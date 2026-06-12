//! Windows file-copy backend.
//!
//! This is one of the two isolated `unsafe` modules in `ripsync-core` (the other
//! is the Linux `io::uring` backend). Every `unsafe` block carries a `// SAFETY:`
//! comment; nothing outside these modules touches raw FFI.
//!
//! Copy ladder, fastest first:
//!   1. `ReFS` / Dev Drive block clone via `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
//!      (copy-on-write; only works when source and destination share a volume).
//!   2. `CopyFileExW` — the OS copy primitive, which itself performs block
//!      cloning on supporting volumes.
//!   3. A portable buffered copy.
//!
//! Atomic replace uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING |
//! MOVEFILE_WRITE_THROUGH`, preserving the temp-then-rename invariant.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io::{self, BufReader, BufWriter};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CopyFileExW, CreateFileW, MoveFileExW, SetEndOfFile, SetFilePointerEx,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::copy::ReflinkMode;

// --- ABI constants (stable Win32 values; defined locally to keep the
// `windows-sys` import surface minimal and resilient to feature renames). ---
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const CREATE_ALWAYS: u32 = 2;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
const FILE_BEGIN: u32 = 0;
const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
const FSCTL_DUPLICATE_EXTENTS_TO_FILE: u32 = 0x0009_8344;

/// Cluster-alignment used for block-clone ranges. `ReFS` clusters are 4 KiB or
/// 64 KiB; 64 KiB is a multiple of both, so aligning to it satisfies the
/// FSCTL's cluster-boundary requirement on either layout.
const CLUSTER_ALIGN: i64 = 64 * 1024;

/// Largest range duplicated per `DeviceIoControl`, keeping each FSCTL bounded.
const MAX_CLONE_CHUNK: i64 = 256 * 1024 * 1024;

/// `DUPLICATE_EXTENTS_DATA` (declared locally as `repr(C)` to match the Win32
/// header layout exactly).
#[repr(C)]
struct DuplicateExtentsData {
    file_handle: HANDLE,
    source_file_offset: i64,
    target_file_offset: i64,
    byte_count: i64,
}

/// Encode a path as a NUL-terminated wide string for the `*W` APIs.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn last_error() -> io::Error {
    // SAFETY: `GetLastError` reads a thread-local error code and has no
    // preconditions.
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(-1))
}

/// RAII wrapper so a Win32 `HANDLE` is always closed, including on early return.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE && !self.0.is_null() {
            // SAFETY: we own this handle (returned by `CreateFileW`) and it is
            // valid and non-null; closing it exactly once here is correct.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn create_handle(path_w: &[u16], access: u32, disposition: u32) -> io::Result<OwnedHandle> {
    // SAFETY: `path_w` is a valid NUL-terminated wide string for the lifetime of
    // the call; the security-attributes and template-file arguments are null,
    // which the API documents as "use defaults / none".
    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            disposition,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    Ok(OwnedHandle(handle))
}

fn set_file_size(handle: HANDLE, size: i64) -> io::Result<()> {
    let mut new_pos = 0_i64;
    // SAFETY: `handle` is a valid file handle opened for write; `new_pos` is a
    // valid out-pointer for the resulting file position.
    let moved = unsafe { SetFilePointerEx(handle, size, &raw mut new_pos, FILE_BEGIN) };
    if moved == 0 {
        return Err(last_error());
    }
    // SAFETY: `handle` is valid and the file pointer is positioned at `size`.
    let ended = unsafe { SetEndOfFile(handle) };
    if ended == 0 {
        return Err(last_error());
    }
    Ok(())
}

#[inline]
fn round_up(value: i64, align: i64) -> i64 {
    (value + align - 1) / align * align
}

/// Attempt a ReFS/Dev Drive block clone of `src` into the freshly created `dst`.
///
/// Returns `Ok(len)` on success. Any failure (unsupported volume, cross-volume,
/// misalignment) is reported as an error so the caller can fall back; the
/// partially written `dst` is cleaned up by the caller.
fn block_clone(src_w: &[u16], dst_w: &[u16], len: u64) -> io::Result<u64> {
    let src = create_handle(src_w, GENERIC_READ, OPEN_EXISTING)?;
    let dst = create_handle(dst_w, GENERIC_READ | GENERIC_WRITE, CREATE_ALWAYS)?;

    let len_i = i64::try_from(len).map_err(|_| io::Error::other("file too large to clone"))?;
    let aligned = round_up(len_i, CLUSTER_ALIGN);
    // The destination must be at least as large as the duplicated range.
    set_file_size(dst.0, aligned)?;

    // Duplicate in cluster-aligned chunks. The final chunk's `byte_count` is
    // rounded up to a cluster.
    let mut offset = 0_i64;
    while offset < aligned {
        let chunk = (aligned - offset).min(MAX_CLONE_CHUNK);
        let data = DuplicateExtentsData {
            file_handle: src.0,
            source_file_offset: offset,
            target_file_offset: offset,
            byte_count: chunk,
        };
        let mut returned = 0_u32;
        // SAFETY: `dst.0` is a valid writable handle; `data` is a correctly laid
        // out, fully initialized `DUPLICATE_EXTENTS_DATA` living for the call;
        // its size is passed exactly; the output buffer is null with size 0,
        // which this FSCTL permits; `returned` is a valid out-pointer.
        let ok = unsafe {
            DeviceIoControl(
                dst.0,
                FSCTL_DUPLICATE_EXTENTS_TO_FILE,
                ptr::from_ref(&data).cast::<c_void>(),
                u32::try_from(size_of::<DuplicateExtentsData>()).unwrap_or(0),
                ptr::null_mut(),
                0,
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        offset += chunk;
    }

    // Trim back to the exact logical size.
    set_file_size(dst.0, len_i)?;
    Ok(len)
}

/// `CopyFileExW`: the OS copy primitive. Performs block cloning itself on
/// supporting volumes; otherwise a regular copy.
fn copy_file_ex(src_w: &[u16], dst_w: &[u16]) -> io::Result<()> {
    // SAFETY: both wide strings are valid and NUL-terminated for the call; the
    // progress-routine and its data are null (no callback), the cancel flag is
    // null (non-cancellable), and no copy flags are set.
    let ok = unsafe {
        CopyFileExW(
            src_w.as_ptr(),
            dst_w.as_ptr(),
            None,
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Portable buffered fallback (no FFI).
fn buffered_copy(src: &Path, dst: &Path) -> io::Result<u64> {
    const BUF: usize = 1 << 20;
    let reader = std::fs::File::open(src)?;
    let writer = std::fs::File::create(dst)?;
    let mut r = BufReader::with_capacity(BUF, reader);
    let mut w = BufWriter::with_capacity(BUF, writer);
    let n = io::copy(&mut r, &mut w)?;
    w.into_inner()?.sync_all().ok();
    Ok(n)
}

/// Copy `src` into the not-yet-existing `dst`, returning bytes written.
///
/// Ladder: block clone → `CopyFileExW` → buffered. `reflink` controls the block
/// clone: [`ReflinkMode::Never`] skips it, [`ReflinkMode::Always`] requires it.
///
/// # Errors
///
/// Returns an error if every applicable strategy fails, or if
/// [`ReflinkMode::Always`] is set and the clone is unsupported.
pub fn copy_file_into(src: &Path, dst: &Path, reflink: ReflinkMode) -> io::Result<u64> {
    let src_w = wide(src);
    let dst_w = wide(dst);
    let len = std::fs::metadata(src)?.len();

    if reflink != ReflinkMode::Never {
        match block_clone(&src_w, &dst_w, len) {
            Ok(n) => return Ok(n),
            Err(e) => {
                let _ = std::fs::remove_file(dst);
                if reflink == ReflinkMode::Always {
                    return Err(e);
                }
            }
        }
    }

    if copy_file_ex(&src_w, &dst_w).is_ok() {
        Ok(len)
    } else {
        let _ = std::fs::remove_file(dst);
        buffered_copy(src, dst)
    }
}

/// Atomically replace `target` with `tmp` using `MoveFileExW`
/// (`MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`). Works whether or not
/// `target` already exists. `target` must not be a directory.
///
/// # Errors
///
/// Returns the OS error if the move fails.
pub fn replace_file(tmp: &Path, target: &Path) -> io::Result<()> {
    let tmp_w = wide(tmp);
    let target_w = wide(target);
    // SAFETY: both wide strings are valid and NUL-terminated for the duration of
    // the call.
    let ok = unsafe {
        MoveFileExW(
            tmp_w.as_ptr(),
            target_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}
