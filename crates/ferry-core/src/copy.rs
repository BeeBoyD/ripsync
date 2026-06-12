//! The regular-file copy ladder: reflink (`CoW` clone) → `copy_file_range`
//! (kernel-side copy) → buffered fallback. Each strategy writes into the caller's
//! temporary path so the atomic-rename invariant is preserved.

use std::fs::File;
use std::io::{self, BufReader, BufWriter};
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
pub fn copy_file_into(src: &Path, tmp: &Path, reflink: ReflinkMode) -> io::Result<u64> {
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

/// Portable buffered copy with a large buffer.
fn buffered_copy(src: &Path, tmp: &Path) -> io::Result<u64> {
    let reader = File::open(src)?;
    let writer = File::create(tmp)?;
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
