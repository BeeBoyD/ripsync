//! Parallel filesystem walk built on `jwalk`.
//!
//! Produces a flat, sorted list of [`Entry`] values describing a tree relative to
//! its root. Symlinks are recorded as symlinks (their target is read but never
//! followed), which keeps the walk safe and cheap.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use filetime::FileTime;
use globset::GlobSet;

use crate::{Error, Result};

/// What a walked entry is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory.
    Dir,
    /// A regular file of the given byte length.
    File,
    /// A symbolic link with the given target (recorded verbatim, never followed).
    Symlink(PathBuf),
}

/// One entry in a walked tree.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Path relative to the walk root (never contains `..`).
    pub rel: PathBuf,
    /// What kind of entry this is.
    pub kind: EntryKind,
    /// Byte length (0 for directories and symlinks).
    pub len: u64,
    /// Modification time (of the link itself for symlinks).
    pub mtime: FileTime,
    /// Unix permission bits (0 on platforms without them).
    pub mode: u32,
}

impl Entry {
    /// Whether this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        matches!(self.kind, EntryKind::File)
    }

    /// Whether this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir)
    }
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0
}

/// Walk `root` in parallel, returning entries sorted by relative path (so every
/// parent directory precedes its children).
///
/// `threads` sets the worker-pool size (0 ⇒ `jwalk` default). `excludes` drops
/// any entry whose relative path matches.
///
/// # Errors
///
/// Returns an error if `root` cannot be read, or if any entry's metadata cannot
/// be stat-ed.
pub fn walk(root: &Path, threads: usize, excludes: &GlobSet) -> Result<Vec<Entry>> {
    let parallelism = if threads == 0 {
        jwalk::Parallelism::RayonDefaultPool {
            busy_timeout: std::time::Duration::from_secs(1),
        }
    } else {
        jwalk::Parallelism::RayonNewPool(threads)
    };

    // Collect into a map keyed by rel path to get a deterministic sorted order.
    let mut map: BTreeMap<PathBuf, Entry> = BTreeMap::new();

    for dent in jwalk::WalkDir::new(root)
        .parallelism(parallelism)
        .skip_hidden(false)
        .follow_links(false)
    {
        let dent = dent.map_err(|e| Error::io(root, std::io::Error::other(e.to_string())))?;
        let path = dent.path();
        if path == root {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| Error::Containment(path.clone()))?
            .to_path_buf();

        if !excludes.is_empty() && excludes.is_match(&rel) {
            continue;
        }

        let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
        let ftype = meta.file_type();
        let mtime = FileTime::from_last_modification_time(&meta);
        let mode = mode_of(&meta);

        let kind = if ftype.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|e| Error::io(&path, e))?;
            EntryKind::Symlink(target)
        } else if ftype.is_dir() {
            EntryKind::Dir
        } else if ftype.is_file() {
            EntryKind::File
        } else {
            // Sockets/FIFOs/devices: skip — not supported in this milestone.
            continue;
        };

        let len = if matches!(kind, EntryKind::File) {
            meta.len()
        } else {
            0
        };

        map.insert(
            rel.clone(),
            Entry {
                rel,
                kind,
                len,
                mtime,
                mode,
            },
        );
    }

    Ok(map.into_values().collect())
}
