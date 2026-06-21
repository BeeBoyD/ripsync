//! The regular-file copy ladder: reflink (`CoW` clone) → kernel-side copy
//! (`fcopyfile` on macOS, `copy_file_range` on Linux) → buffered fallback with
//! page-aligned I/O buffers. Each strategy writes into the caller's temporary
//! path so the atomic-rename invariant is preserved.

#[cfg(not(windows))]
use std::fs::File;
use std::io;
#[cfg(not(windows))]
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::util::AlignedBuf;

/// How aggressively to attempt reflink (copy-on-write) clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReflinkMode {
    /// Try reflink, silently fall back when unsupported/cross-filesystem.
    #[default]
    Auto,
    /// Require reflink; error if it cannot be done.
    Always,
    /// Never attempt reflink.
    Never,
}

/// When to `fsync` file data before the rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsyncMode {
    /// Skip per-file fsync; the apply phase fsyncs touched directories once.
    #[default]
    Auto,
    /// Fsync every file before rename (paranoid; survives power loss per file).
    Always,
    /// Skip all fsync, including directories.
    Never,
}

/// Default buffer size for the portable fallback copy (1 MiB).
const DEFAULT_BUF: usize = 1 << 20;

/// Copy `src` into the (not-yet-existing) `tmp`, returning bytes written.
///
/// Strategy ladder: reflink → kernel copy → buffered. `tmp` must not exist
/// yet (reflink/clone requires creating the destination).
///
/// Uses the default 1 MiB buffer for the fallback path. For tuned buffer
/// sizes, use [`copy_file_into_sized`].
///
/// # Errors
///
/// Returns the underlying I/O error if every applicable strategy fails, or if
/// [`ReflinkMode::Always`] is set and the clone is unsupported.
pub fn copy_file_into(
    src: &Path,
    tmp: &Path,
    reflink: ReflinkMode,
    sparse: bool,
) -> io::Result<u64> {
    copy_file_into_sized(src, tmp, reflink, sparse, DEFAULT_BUF)
}

/// Copy `src` into `tmp` using a caller-specified buffer size for the
/// fallback path. All other behavior is identical to [`copy_file_into`].
///
/// # Errors
///
/// Returns the underlying I/O error if every applicable strategy fails.
pub fn copy_file_into_sized(
    src: &Path,
    tmp: &Path,
    reflink: ReflinkMode,
    sparse: bool,
    buffer_size: usize,
) -> io::Result<u64> {
    // Windows has its own ladder (block clone → CopyFileExW → buffered); sparse
    // preservation is not offered on that backend.
    #[cfg(not(windows))]
    {
        copy_file_into_portable(src, tmp, reflink, sparse, buffer_size)
    }
    #[cfg(windows)]
    {
        let _ = sparse;
        let _ = buffer_size;
        crate::io::windows::copy_file_into(src, tmp, reflink)
    }
}

/// The POSIX copy ladder:
///   reflink → sparse → fcopyfile (macOS) → `copy_file_range` (Linux) → buffered.
#[cfg(not(windows))]
fn copy_file_into_portable(
    src: &Path,
    tmp: &Path,
    reflink: ReflinkMode,
    sparse: bool,
    buffer_size: usize,
) -> io::Result<u64> {
    // 1. Reflink (CoW clone) — fastest, no data movement.
    if reflink != ReflinkMode::Never {
        match reflink_copy::reflink(src, tmp) {
            Ok(()) => return std::fs::metadata(tmp).map(|m| m.len()),
            Err(e) => {
                let _ = std::fs::remove_file(tmp);
                if reflink == ReflinkMode::Always {
                    return Err(e);
                }
            }
        }
    }

    // 2. Sparse copy — only copy allocated extents.
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "solaris"
    ))]
    if sparse {
        match sparse_copy_sized(src, tmp, buffer_size) {
            Ok(n) => return Ok(n),
            Err(_) => {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }

    // 3. macOS kernel-side copy via fcopyfile.
    #[cfg(target_os = "macos")]
    {
        match macos_fcopyfile_copy(src, tmp) {
            Ok(n) => return Ok(n),
            Err(_) => {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }

    // 4. io_uring splice for medium files (1–64 MiB) when available.
    #[cfg(all(target_os = "linux", feature = "io-uring"))]
    {
        let file_len = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        if file_len > 1_048_576 && file_len <= 67_108_864 {
            match crate::io::uring::copy_single_large(src, tmp, file_len) {
                Ok(n) => return Ok(n),
                Err(_) => {
                    let _ = std::fs::remove_file(tmp);
                }
            }
        }
    }

    // 5. Kernel-side copy_file_range (Linux).
    #[cfg(target_os = "linux")]
    {
        match copy_file_range_all(src, tmp) {
            Ok(n) => return Ok(n),
            Err(_) => {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }

    // 6. Buffered fallback with page-aligned buffer.
    buffered_copy_sized(src, tmp, buffer_size)
}

// ---------------------------------------------------------------------------
// macOS fcopyfile fast path
// ---------------------------------------------------------------------------

/// Try `fcopyfile` for a kernel-side copy on macOS.
#[cfg(target_os = "macos")]
fn macos_fcopyfile_copy(src: &Path, tmp: &Path) -> io::Result<u64> {
    use std::os::unix::io::AsRawFd;

    let infile = File::open(src)?;
    let len = infile.metadata()?.len();

    // For large files, hint the kernel to avoid polluting the UBC.
    if len >= 8 * 1024 * 1024 {
        let _ = crate::io::macos::set_nocache(infile.as_raw_fd());
    }

    let outfile = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)?;

    if len >= 8 * 1024 * 1024 {
        let _ = crate::io::macos::set_nocache(outfile.as_raw_fd());
    }

    crate::io::macos::copy_file_with_fcopyfile(infile.as_raw_fd(), outfile.as_raw_fd())
}

// ---------------------------------------------------------------------------
// Sparse copy
// ---------------------------------------------------------------------------

/// Copy allocated extents only, preserving holes in sparse files.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "solaris"
))]
fn sparse_copy_sized(src: &Path, tmp: &Path, buffer_size: usize) -> io::Result<u64> {
    use std::os::unix::fs::FileExt;

    use rustix::fs::SeekFrom;

    let infile = File::open(src)?;
    let outfile = File::create(tmp)?;
    let len = infile.metadata()?.len();
    outfile.set_len(len)?;

    let mut offset = 0_u64;
    let mut buf = AlignedBuf::new(buffer_size);
    let mut bytes_written = 0_u64;
    while offset < len {
        let data = match rustix::fs::seek(&infile, SeekFrom::Data(offset)) {
            Ok(data) => data,
            Err(error) if error == rustix::io::Errno::NXIO => break,
            Err(error) => return Err(io::Error::from(error)),
        };
        let hole = rustix::fs::seek(&infile, SeekFrom::Hole(data))?.min(len);
        let mut position = data;
        while position < hole {
            let wanted = usize::try_from((hole - position).min(buf.len() as u64))
                .unwrap_or(buf.len());
            let read = infile.read_at(&mut buf[..wanted], position)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short sparse extent read",
                ));
            }
            let mut written = 0;
            while written < read {
                let count =
                    outfile.write_at(&buf[written..read], position + written as u64)?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "short sparse extent write",
                    ));
                }
                written += count;
            }
            position += read as u64;
        }
        bytes_written = hole;
        offset = hole;
    }
    outfile.set_len(bytes_written)?;
    Ok(bytes_written)
}

/// Hint the kernel to read a large source sequentially and prefetch it. No-op
/// off Linux and for small files (where the syscalls are not worth it).
#[cfg(not(windows))]
fn advise_sequential(file: &File, len: u64) {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{Advice, fadvise};
        if len >= 8 * 1024 * 1024 {
            let span = std::num::NonZeroU64::new(len);
            let _ = fadvise(file, 0, span, Advice::Sequential);
            let _ = fadvise(file, 0, span, Advice::WillNeed);
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (file, len);
}

/// Portable buffered copy with a page-aligned buffer.
#[cfg(not(windows))]
fn buffered_copy_sized(src: &Path, tmp: &Path, buffer_size: usize) -> io::Result<u64> {
    let reader = File::open(src)?;
    let writer = File::create(tmp)?;
    let file_len = reader.metadata()?.len();
    advise_sequential(&reader, file_len);

    // On macOS, hint the kernel to avoid UBC pollution for large files.
    #[cfg(target_os = "macos")]
    if file_len >= 8 * 1024 * 1024 {
        use std::os::unix::io::AsRawFd;
        let _ = crate::io::macos::set_nocache(reader.as_raw_fd());
        let _ = crate::io::macos::set_nocache(writer.as_raw_fd());
    }

    let mut r = BufReader::with_capacity(buffer_size, reader);
    let mut w = BufWriter::with_capacity(buffer_size, writer);
    let n = io::copy(&mut r, &mut w)?;
    // Flush the buffer to the OS, but do NOT force a device flush here: per-file
    // durability is the caller's job (`finalize_file` fsyncs each file in
    // `FsyncMode::Always`; `Auto` fsyncs the touched directories once). An
    // unconditional `sync_all` here both double-fsyncs in `Always` and violates
    // the documented `Auto`/`Never` "skip per-file fsync" contract — and on
    // macOS it lowers to `F_FULLFSYNC` (a full drive flush), which made the
    // non-reflink path pathologically slow on many small files.
    w.into_inner()?;
    Ok(n)
}

/// Loop `copy_file_range` until the whole file is copied.
#[cfg(target_os = "linux")]
fn copy_file_range_all(src: &Path, tmp: &Path) -> io::Result<u64> {
    let infile = File::open(src)?;
    let outfile = File::create(tmp)?;
    let len = infile.metadata()?.len();
    advise_sequential(&infile, len);
    let mut remaining = len;
    while remaining > 0 {
        let chunk = usize::try_from(remaining.min(1 << 30)).unwrap_or(usize::MAX);
        let copied = rustix::fs::copy_file_range(&infile, None, &outfile, None, chunk)?;
        if copied == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source file truncated during copy",
            ));
        }
        remaining -= copied as u64;
    }
    Ok(len - remaining)
}
