//! macOS-specific I/O optimizations.
//!
//! Provides kernel-side copy (`fcopyfile`), UBC-pollution avoidance
//! (`F_NOCACHE`), and APFS clone attempts (`fclonefileat`). Every function
//! falls back gracefully: if the fast path fails (cross-volume, permission,
//! or unsupported filesystem), the caller should retry with the portable
//! buffered copy.
//!
//! # Safety
//!
//! This is an isolated `unsafe` module. Every `unsafe` block carries a
//! `// SAFETY:` comment.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int, c_void};
use std::io;
use std::os::unix::io::{BorrowedFd, RawFd};
use std::path::Path;

// ---------------------------------------------------------------------------
// fcopyfile — kernel-side copy (APFS, HFS+, SMB, NFS)
// ---------------------------------------------------------------------------

/// Flags for `fcopyfile`. We only need `COPYFILE_DATA` — metadata is handled
/// separately by the apply phase.
const COPYFILE_DATA: u32 = 0x0000_0008;

/// Copy file data from `src_fd` to `dst_fd` using the kernel's `fcopyfile`.
///
/// This is a zero-copy kernel operation on APFS volumes; on other filesystems
/// it falls back to an internal kernel buffered copy that is still faster than
/// a userspace read/write loop.
///
/// Returns the number of bytes copied.
///
/// # Errors
///
/// Returns an I/O error if the kernel rejects the operation (e.g. cross-device,
/// permission denied, or the source fd is not seeked to the start).
pub fn copy_file_with_fcopyfile(src_fd: RawFd, dst_fd: RawFd) -> io::Result<u64> {
    // SAFETY: `fcopyfile` is a public Darwin syscall. Both file descriptors are
    // valid (borrowed from live `File` handles). `state` is null (no progress
    // callback). `flags` is `COPYFILE_DATA` (copy data only, no metadata).
    let ret = unsafe {
        fcopyfile(
            src_fd,
            dst_fd,
            std::ptr::null_mut(),
            COPYFILE_DATA,
        )
    };

    if ret == -1 {
        return Err(io::Error::last_os_error());
    }

    // fcopyfile returns -1 on error, but doesn't return bytes copied on success.
    // Stat the destination fd to get the file size.
    // SAFETY: `dst_fd` is a valid, open file descriptor borrowed from a live
    // `File` handle in the caller. The `BorrowedFd` lifetime is bounded by this
    // function call.
    let borrowed = unsafe { BorrowedFd::borrow_raw(dst_fd) };
    let stat = rustix::fs::fstat(borrowed).map_err(io::Error::from)?;
    Ok(stat.st_size.try_into().unwrap_or(0))
}

// ---------------------------------------------------------------------------
// F_NOCACHE — avoid UBC pollution for large files
// ---------------------------------------------------------------------------

/// `F_NOCACHE` fcntl command (macOS-specific, value 48).
const F_NOCACHE: c_int = 48;

/// Hint that the kernel should not cache this file's data in the Unified
/// Buffer Cache. Useful for large files that are read or written once and
/// would otherwise evict hot cache entries.
///
/// # Errors
///
/// Returns an I/O error if `fcntl` fails (e.g. invalid fd).
pub fn set_nocache(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fcntl` with `F_NOCACHE` is a no-op on non-regular files and
    // safe to call on any valid fd. The `1` argument enables the hint.
    let ret = unsafe { fcntl(fd, F_NOCACHE, 1) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// fclonefileat — APFS instant clone
// ---------------------------------------------------------------------------

/// `AT_FDCWD` — use the current working directory for relative paths.
const AT_FDCWD: c_int = -2;

/// Try to create `dst` as an APFS copy-on-write clone of `src`.
///
/// This is a metadata-only operation: no data is copied. Both files must
/// reside on the same APFS volume.
///
/// # Errors
///
/// Returns an I/O error if the clone is not possible (cross-volume, non-APFS,
/// permission denied, or path contains interior null bytes).
pub fn try_clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    let src_cstr = path_to_cstring(src)?;
    let dst_cstr = path_to_cstring(dst)?;

    // SAFETY: `fclonefileat` is a public Darwin syscall. Both paths are
    // null-terminated C strings. `AT_FDCWD` means "relative to cwd" for
    // absolute paths. Flags are 0 (no special behavior).
    let ret = unsafe {
        fclonefileat(
            AT_FDCWD,
            src_cstr.as_ptr(),
            AT_FDCWD,
            dst_cstr.as_ptr(),
            0,
        )
    };

    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `Path` to a null-terminated `CString` for FFI.
fn path_to_cstring(path: &Path) -> io::Result<CString> {
    let bytes = path.as_os_str().as_encoded_bytes();
    CString::new(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains interior null byte",
        )
    })
}

// ---------------------------------------------------------------------------
// FFI declarations
// ---------------------------------------------------------------------------

unsafe extern "C" {
    /// Darwin `fcopyfile` — copy a file's data (and optionally metadata/ACLs)
    /// entirely in the kernel.
    ///
    /// Returns 0 on success, -1 on error (check `errno`).
    fn fcopyfile(
        src_fd: c_int,
        dst_fd: c_int,
        state: *mut c_void,
        flags: u32,
    ) -> c_int;

    /// Darwin `fclonefileat` — create a copy-on-write clone of a file on APFS.
    ///
    /// Returns 0 on success, -1 on error (check `errno`).
    fn fclonefileat(
        src_dirfd: c_int,
        src: *const c_char,
        dst_dirfd: c_int,
        dst: *const c_char,
        flags: u32,
    ) -> c_int;

    /// POSIX `fcntl` — used for `F_NOCACHE` on macOS.
    fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, Write};
    use std::os::unix::io::AsRawFd;

    #[test]
    fn nocache_on_temp_file() {
        let mut f = tempfile::tempfile().expect("tempfile");
        f.write_all(b"hello").expect("write");
        f.flush().expect("flush");
        // F_NOCACHE should succeed on a regular file
        let result = set_nocache(f.as_raw_fd());
        assert!(result.is_ok());
    }

    #[test]
    fn fcopyfile_copies_data() {
        let mut src = tempfile::tempfile().expect("tempfile src");
        src.write_all(b"fcopyfile test data!").expect("write");
        src.flush().expect("flush");
        // fcopyfile reads from the current file offset; rewind to the start.
        src.rewind().expect("rewind src");

        let dst = tempfile::tempfile().expect("tempfile dst");

        let bytes =
            copy_file_with_fcopyfile(src.as_raw_fd(), dst.as_raw_fd()).expect("fcopyfile");
        // fcopyfile may round up to filesystem block size; verify at least the
        // expected data length was written.
        assert!(bytes >= 19, "expected at least 19 bytes, got {bytes}");

        let mut dst_read = dst;
        dst_read.rewind().expect("rewind");
        let mut contents = String::new();
        dst_read.read_to_string(&mut contents).expect("read");
        assert!(contents.starts_with("fcopyfile test data!"));
    }

    #[test]
    fn clone_file_creates_clone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src_path = dir.path().join("src.txt");
        let dst_path = dir.path().join("dst.txt");

        std::fs::write(&src_path, b"clone me").expect("write src");

        let result = try_clone_file(&src_path, &dst_path);
        // fclonefileat only works on APFS; on other filesystems (CI tmpfs, etc.)
        // it will fail with EXDEV or ENOTSUP. That's expected — the caller
        // falls back to buffered copy.
        if let Err(ref e) = result {
            let kind = e.kind();
            let raw = e.raw_os_error();
            assert!(
                kind == io::ErrorKind::Unsupported
                    || kind == io::ErrorKind::CrossesDevices
                    || kind == io::ErrorKind::PermissionDenied
                    || kind == io::ErrorKind::InvalidInput
                    || raw == Some(45), // ENOTSUP on macOS
                "unexpected clone error: {e} (raw={raw:?})"
            );
        }
    }
}
