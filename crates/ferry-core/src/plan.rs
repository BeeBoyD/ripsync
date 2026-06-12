//! Build a [`SyncPlan`]: classify every source entry as copy/update/skip and,
//! when `--delete` is set, collect destination entries to remove.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use globset::GlobSet;

use crate::walk::{Entry, EntryKind, walk};
use crate::{Error, Result};

/// What Ferry intends to do with a single entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Entry is missing in the destination — create it.
    Copy,
    /// Entry exists but differs — overwrite it.
    Update,
    /// Entry is already up to date — do nothing.
    Skip,
}

/// A source entry paired with the action chosen for it.
#[derive(Debug, Clone)]
pub struct PlannedAction {
    /// The source entry.
    pub entry: Entry,
    /// What to do with it.
    pub action: Action,
}

/// A destination entry slated for deletion (deepest paths first).
#[derive(Debug, Clone)]
pub struct Deletion {
    /// Path relative to the destination root.
    pub rel: PathBuf,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// Knobs controlling how the plan is built.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanOptions {
    /// Compare by content hash rather than size+mtime.
    pub checksum: bool,
    /// Mirror deletions (entries in dest but not source).
    pub delete: bool,
    /// Worker threads for the walk (0 ⇒ default).
    pub threads: usize,
}

/// The full plan: per-entry actions plus pending deletions.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    /// Source-side actions, ordered so parents precede children.
    pub actions: Vec<PlannedAction>,
    /// Destination entries to delete (deepest first), only when `delete` is set.
    pub deletions: Vec<Deletion>,
}

impl SyncPlan {
    /// Count of actions equal to `action`.
    #[must_use]
    pub fn count(&self, action: Action) -> usize {
        self.actions.iter().filter(|a| a.action == action).count()
    }

    /// Total bytes to transfer (sum of files being copied or updated).
    #[must_use]
    pub fn bytes_to_transfer(&self) -> u64 {
        self.actions
            .iter()
            .filter(|a| a.action != Action::Skip && a.entry.is_file())
            .map(|a| a.entry.len)
            .sum()
    }
}

/// Read a whole file and return its BLAKE3 digest, or `None` on read error.
fn file_hash(path: &Path) -> Option<[u8; 32]> {
    let bytes = std::fs::read(path).ok()?;
    Some(*blake3::hash(&bytes).as_bytes())
}

/// Decide whether a source and destination file differ.
fn files_differ(
    src_root: &Path,
    dst_root: &Path,
    entry: &Entry,
    dst: &Entry,
    checksum: bool,
) -> bool {
    if checksum {
        let sp = src_root.join(&entry.rel);
        let dp = dst_root.join(&dst.rel);
        match (file_hash(&sp), file_hash(&dp)) {
            (Some(a), Some(b)) => a != b,
            _ => true,
        }
    } else {
        // Quick check: size, then mtime at whole-second resolution.
        entry.len != dst.len || entry.mtime.unix_seconds() != dst.mtime.unix_seconds()
    }
}

/// Build a [`SyncPlan`] for mirroring `src` into `dst`.
///
/// # Errors
///
/// * the source cannot be walked (unreadable);
/// * `opts.delete` is set but the source yields no entries (empty-source guard —
///   refuses to mirror emptiness and wipe the destination).
pub fn build_plan(
    src: &Path,
    dst: &Path,
    opts: PlanOptions,
    excludes: &GlobSet,
) -> Result<SyncPlan> {
    let src_entries = walk(src, opts.threads, excludes)?;

    // Empty-source guard: never mirror deletions from nothing.
    if opts.delete && src_entries.is_empty() {
        return Err(Error::EmptySource(src.to_path_buf()));
    }

    let dst_entries = if dst.exists() {
        walk(dst, opts.threads, excludes)?
    } else {
        Vec::new()
    };
    let dst_map: HashMap<&Path, &Entry> =
        dst_entries.iter().map(|e| (e.rel.as_path(), e)).collect();

    let mut actions = Vec::with_capacity(src_entries.len());
    for entry in &src_entries {
        let action = match dst_map.get(entry.rel.as_path()) {
            None => Action::Copy,
            Some(dst_entry) => classify_existing(src, dst, entry, dst_entry, opts.checksum),
        };
        actions.push(PlannedAction {
            entry: entry.clone(),
            action,
        });
    }

    let mut deletions = Vec::new();
    if opts.delete {
        let src_set: HashSet<&Path> = src_entries.iter().map(|e| e.rel.as_path()).collect();
        for d in &dst_entries {
            if !src_set.contains(d.rel.as_path()) {
                deletions.push(Deletion {
                    rel: d.rel.clone(),
                    is_dir: d.is_dir(),
                });
            }
        }
        // Deepest paths first so directories empty out before removal.
        deletions.sort_by(|a, b| b.rel.cmp(&a.rel));
    }

    Ok(SyncPlan { actions, deletions })
}

/// Classify a source entry whose path also exists in the destination.
fn classify_existing(
    src: &Path,
    dst: &Path,
    entry: &Entry,
    dst_entry: &Entry,
    checksum: bool,
) -> Action {
    match (&entry.kind, &dst_entry.kind) {
        (EntryKind::Dir, EntryKind::Dir) => Action::Skip,
        (EntryKind::Symlink(a), EntryKind::Symlink(b)) => {
            if a == b {
                Action::Skip
            } else {
                Action::Update
            }
        }
        (EntryKind::File, EntryKind::File) => {
            if files_differ(src, dst, entry, dst_entry, checksum) {
                Action::Update
            } else {
                Action::Skip
            }
        }
        // Kind changed (e.g. file ↔ dir ↔ symlink): replace it.
        _ => Action::Update,
    }
}
