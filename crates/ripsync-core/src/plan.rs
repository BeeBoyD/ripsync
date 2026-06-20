//! Build a [`SyncPlan`]: classify every source entry as copy/update/skip and,
//! when `--delete` is set, collect destination entries to remove.

use std::path::{Path, PathBuf};

// Plan classification keys are local relative paths and (dev, ino) pairs, never
// attacker-controlled, so foldhash's faster non-DoS-hardened hashing is safe.
use foldhash::{HashMap, HashMapExt, HashSet};
use rayon::prelude::*;

use crate::filter::Filter;

use crate::index::{Manifest, RIPSYNC_DIR};
use crate::report::{Event, NullReporter, Reporter, RunPhase};
use crate::walk::{Entry, EntryKind, walk_controlled};
use crate::{Error, Result, RunControl};

/// What ripsync intends to do with a single entry.
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
#[allow(clippy::struct_excessive_bools)]
pub struct PlanOptions {
    /// Compare by content hash rather than size+mtime.
    pub checksum: bool,
    /// Mirror deletions (entries in dest but not source).
    pub delete: bool,
    /// Worker threads for the walk (0 ⇒ default).
    pub threads: usize,
    /// Use the persistent index (manifest) for fast incremental re-syncs.
    pub index: bool,
    /// Preserve source hardlink groups.
    pub hard_links: bool,
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

/// BLAKE3 digest of a file, or `None` on read error. Large files use a
/// memory-mapped, rayon-parallel pass; small files read once into memory.
fn file_hash(path: &Path) -> Option<[u8; 32]> {
    const MMAP_HASH_THRESHOLD: u64 = 16 * 1024 * 1024;
    let len = std::fs::metadata(path).ok()?.len();
    let mut hasher = blake3::Hasher::new();
    if len >= MMAP_HASH_THRESHOLD {
        hasher.update_mmap_rayon(path).ok()?;
    } else {
        let bytes = std::fs::read(path).ok()?;
        hasher.update(&bytes);
    }
    Some(*hasher.finalize().as_bytes())
}

/// Decide whether a source and destination file differ.
fn files_differ(
    src_root: &Path,
    dst_root: &Path,
    entry: &Entry,
    dst: &Entry,
    checksum: bool,
) -> bool {
    let metadata_differs = entry.mode & 0o7777 != dst.mode & 0o7777
        || entry.mtime.unix_seconds() != dst.mtime.unix_seconds();
    if checksum {
        let sp = src_root.join(&entry.rel);
        let dp = dst_root.join(&dst.rel);
        match (file_hash(&sp), file_hash(&dp)) {
            (Some(a), Some(b)) => metadata_differs || a != b,
            _ => true,
        }
    } else {
        // Quick check: size, then mtime at whole-second resolution.
        entry.len != dst.len || metadata_differs
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
    filter: &Filter,
) -> Result<SyncPlan> {
    build_plan_controlled(
        src,
        dst,
        opts,
        filter,
        &RunControl::default(),
        &NullReporter,
    )
}

/// Build a plan with cooperative control and lifecycle reporting.
///
/// # Errors
///
/// Returns an I/O, containment, empty-source, or cancellation error.
pub fn build_plan_controlled<R: Reporter>(
    src: &Path,
    dst: &Path,
    opts: PlanOptions,
    filter: &Filter,
    control: &RunControl,
    reporter: &R,
) -> Result<SyncPlan> {
    reporter.event(Event::Phase(RunPhase::Planning));
    control.checkpoint()?;
    let mut src_entries = walk_controlled(src, opts.threads, filter, control)?;
    control.checkpoint()?;
    reporter.event(Event::PlanningProgress {
        entries: src_entries.len(),
    });
    // Never sync or delete ripsync's own metadata directory at the dst root.
    src_entries.retain(|e| !e.rel.starts_with(RIPSYNC_DIR));

    // Empty-source guard: never mirror deletions from nothing.
    if opts.delete && src_entries.is_empty() {
        return Err(Error::EmptySource(src.to_path_buf()));
    }

    // `--delete` still needs a live walk to discover files created outside ripsync.
    if opts.index && !opts.delete {
        if let Some(manifest) = Manifest::load(dst) {
            return Ok(plan_from_manifest(src, dst, &src_entries, &manifest, opts));
        }
    }

    let mut dst_entries = if dst.exists() {
        walk_controlled(dst, opts.threads, filter, control)?
    } else {
        Vec::new()
    };
    control.checkpoint()?;
    reporter.event(Event::PlanningProgress {
        entries: src_entries.len() + dst_entries.len(),
    });
    dst_entries.retain(|e| !e.rel.starts_with(RIPSYNC_DIR));
    let dst_map: HashMap<&Path, &Entry> =
        dst_entries.iter().map(|e| (e.rel.as_path(), e)).collect();

    // Classify in parallel — the checksum path reads file contents, so spreading
    // the work across cores matters; the size+mtime path is cheap either way.
    let mut actions: Vec<PlannedAction> = src_entries
        .par_iter()
        .map(|entry| {
            let action = match dst_map.get(entry.rel.as_path()) {
                None => Action::Copy,
                Some(dst_entry) => classify_existing(src, dst, entry, dst_entry, opts.checksum),
            };
            PlannedAction {
                entry: entry.clone(),
                action,
            }
        })
        .collect();
    if opts.hard_links {
        enforce_hardlink_actions(&mut actions, |rel| {
            dst_map.get(rel).map(|entry| (entry.dev, entry.ino))
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

/// Build a plan by diffing the source against the persistent index, skipping the
/// destination walk entirely. Deletions are manifest entries no longer in source
/// (apply still containment-checks every removal).
fn plan_from_manifest(
    src: &Path,
    dst: &Path,
    src_entries: &[Entry],
    manifest: &Manifest,
    opts: PlanOptions,
) -> SyncPlan {
    let mut actions: Vec<PlannedAction> = src_entries
        .par_iter()
        .map(|entry| PlannedAction {
            entry: entry.clone(),
            action: manifest.classify(entry, opts.checksum, src, dst),
        })
        .collect();
    if opts.hard_links {
        enforce_hardlink_actions(&mut actions, |rel| {
            crate::meta::meta_min(&dst.join(rel))
                .ok()
                .map(|entry| (entry.dev, entry.ino))
        });
    }

    let mut deletions = Vec::new();
    if opts.delete {
        let src_set: HashSet<&Path> = src_entries.iter().map(|e| e.rel.as_path()).collect();
        for (rel, rec) in &manifest.entries {
            if !src_set.contains(rel.as_path()) {
                deletions.push(Deletion {
                    rel: rel.clone(),
                    is_dir: rec.kind == crate::index::Kind::Dir,
                });
            }
        }
        deletions.sort_by(|a, b| b.rel.cmp(&a.rel));
    }

    SyncPlan { actions, deletions }
}

/// A duplicate source inode must point at the same destination inode as the
/// first member of its group. Force an update when the topology differs.
fn enforce_hardlink_actions(
    actions: &mut [PlannedAction],
    destination_identity: impl Fn(&Path) -> Option<(u64, u64)>,
) {
    let mut first: HashMap<(u64, u64), Option<(u64, u64)>> = HashMap::new();
    for planned in actions {
        if !planned.entry.is_file() {
            continue;
        }
        let source_id = (planned.entry.dev, planned.entry.ino);
        if let Some(canonical_destination) = first.get(&source_id) {
            if destination_identity(&planned.entry.rel) != *canonical_destination {
                planned.action = Action::Update;
            }
        } else {
            first.insert(source_id, destination_identity(&planned.entry.rel));
        }
    }
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
        (EntryKind::Dir, EntryKind::Dir) => {
            if entry.mode & 0o7777 == dst_entry.mode & 0o7777
                && entry.mtime.unix_seconds() == dst_entry.mtime.unix_seconds()
            {
                Action::Skip
            } else {
                Action::Update
            }
        }
        (EntryKind::Symlink(a), EntryKind::Symlink(b)) => {
            if a == b && entry.mtime.unix_seconds() == dst_entry.mtime.unix_seconds() {
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
