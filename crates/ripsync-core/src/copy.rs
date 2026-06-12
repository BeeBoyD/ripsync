//! The regular-file copy ladder: reflink (`CoW` clone) → `copy_file_range`
//! (kernel-side copy) → buffered fallback. Each strategy writes into the caller's
//! temporary path so the atomic-rename invariant is preserved.

#[cfg(not(windows))]
use std::fs::File;
use std::io;
#[cfg(not(windows))]
use std::io::{BufReader, BufWriter};
use std::path::Path;

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

/// Buffer size for the portable fallback copy.
#[cfg(not(windows))]
const BUF: usize = 1 << 20; // 1 MiB

/// Copy `src` into the (not-yet-existing) `tmp`, returning bytes written.
///
/// Strategy ladder: reflink → `copy_file_range` → buffered. `tmp` must not exist
/// yet (reflink/clone requires creating the destination).
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
    // Windows has its own ladder (block clone → CopyFileExW → buffered); sparse
    // preservation is not offered on that backend.
    #[cfg(not(windows))]
    {
        copy_file_into_portable(src, tmp, reflink, sparse)
    }
    #[cfg(windows)]
    {
        let _ = sparse;
        crate::io::windows::copy_file_into(src, tmp, reflink)
    }
}

/// The POSIX copy ladder: reflink → sparse → `copy_file_range` → buffered.
#[cfg(not(windows))]
fn copy_file_into_portable(
    src: &Path,
    tmp: &Path,
    reflink: ReflinkMode,
    sparse: bool,
) -> io::Result<u64> {
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

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "solaris"
    ))]
    if sparse {
        match sparse_copy(src, tmp) {
            Ok(n) => return Ok(n),
            Err(_) => {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }

    // Kernel-side copy_file_range (Linux); falls back on EXDEV/ENOSYS.
    #[cfg(target_os = "linux")]
    {
        match copy_file_range_all(src, tmp) {
            Ok(n) => return Ok(n),
            Err(_) => {
                let _ = std::fs::remove_file(tmp);
            }
        }
    }

    buffered_copy(src, tmp)
}

/// Copy allocated extents only, preserving holes in sparse files.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "solaris"
))]
fn sparse_copy(src: &Path, tmp: &Path) -> io::Result<u64> {
    use std::os::unix::fs::FileExt;

    use rustix::fs::SeekFrom;

    let infile = File::open(src)?;
    let outfile = File::create(tmp)?;
    let len = infile.metadata()?.len();
    outfile.set_len(len)?;

    let mut offset = 0_u64;
    let mut buf = vec![0_u8; BUF];
    while offset < len {
        let data = match rustix::fs::seek(&infile, SeekFrom::Data(offset)) {
            Ok(data) => data,
            Err(error) if error == rustix::io::Errno::NXIO => break,
            Err(error) => return Err(io::Error::from(error)),
        };
        let hole = rustix::fs::seek(&infile, SeekFrom::Hole(data))?.min(len);
        let mut position = data;
        while position < hole {
            let wanted = usize::try_from((hole - position).min(BUF as u64)).unwrap_or(BUF);
            let read = infile.read_at(&mut buf[..wanted], position)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short sparse extent read",
                ));
            }
            let mut written = 0;
            while written < read {
                let count = outfile.write_at(&buf[written..read], position + written as u64)?;
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
        offset = hole;
    }
    Ok(len)
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

/// Portable buffered copy with a large buffer.
#[cfg(not(windows))]
fn buffered_copy(src: &Path, tmp: &Path) -> io::Result<u64> {
    let reader = File::open(src)?;
    let writer = File::create(tmp)?;
    advise_sequential(&reader, reader.metadata()?.len());
    let mut r = BufReader::with_capacity(BUF, reader);
    let mut w = BufWriter::with_capacity(BUF, writer);
    let n = io::copy(&mut r, &mut w)?;
    w.into_inner()?.sync_all().ok(); // best-effort; durability handled by caller
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
            break; // source shorter than expected; stop cleanly
        }
        remaining -= copied as u64;
    }
    Ok(len - remaining)
}
