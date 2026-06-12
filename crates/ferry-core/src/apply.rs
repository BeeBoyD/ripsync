//! Execute a [`SyncPlan`]: atomic file copies, metadata preservation, contained
//! symlinks, and guarded deletes.
//!
//! Files are written to a temporary name in the destination directory, flushed
//! with `fsync`, given the source's mode and mtime, then atomically `rename`d over
//! the target — so a crash mid-copy never leaves a half-written file in place.
//! Every write/symlink/delete target is checked for destination containment first.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::copy::{FsyncMode, ReflinkMode, copy_file_into};
use crate::meta::{
    canonical_root, check_relative, contained_target, set_mode, set_mtime, set_symlink_mtime,
};
use crate::plan::{Action, SyncPlan};
use crate::report::{Event, Reporter, Stats};
use crate::walk::{Entry, EntryKind};
use crate::{Error, Result};

/// Knobs controlling how a plan is applied.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyOptions {
    /// Plan only: emit events, change nothing on disk.
    pub dry_run: bool,
    /// Confirm destructive actions (required for deletions to happen).
    pub yes: bool,
    /// Whether `--delete` was requested (deletions only run with `yes` too).
    pub delete: bool,
    /// Worker threads for the parallel copy phase (0 ⇒ rayon default).
    pub threads: usize,
    /// Copy-on-write reflink strategy.
    pub reflink: ReflinkMode,
    /// Per-file fsync strategy.
    pub fsync: FsyncMode,
}

/// Thread-safe running totals.
#[derive(Default)]
struct Counters {
    copied: AtomicU64,
    updated: AtomicU64,
    skipped: AtomicU64,
    deleted: AtomicU64,
    errors: AtomicU64,
    bytes: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> Stats {
        Stats {
            copied: self.copied.load(Ordering::Relaxed),
            updated: self.updated.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            deleted: self.deleted.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
        }
    }
    fn bump_action(&self, action: Action) {
        match action {
            Action::Copy => self.copied.fetch_add(1, Ordering::Relaxed),
            Action::Update => self.updated.fetch_add(1, Ordering::Relaxed),
            Action::Skip => self.skipped.fetch_add(1, Ordering::Relaxed),
        };
    }
}

/// Apply `plan`, mirroring `src` into `dst`.
///
/// Returns the [`Stats`] tally. Per-entry failures are reported via `reporter`
/// and counted in [`Stats::errors`]; only setup-level problems (e.g. a
/// containment violation establishing the root) abort the whole run.
///
/// # Errors
///
/// Returns an error if the destination root cannot be created/canonicalized, or
/// a relative path fails its safety check.
pub fn apply_plan<R: Reporter>(
    plan: &SyncPlan,
    src: &Path,
    dst: &Path,
    opts: ApplyOptions,
    reporter: &R,
) -> Result<Stats> {
    reporter.event(Event::Planned {
        total_files: plan
            .actions
            .iter()
            .filter(|a| a.action != Action::Skip && a.entry.is_file())
            .count(),
        total_bytes: plan.bytes_to_transfer(),
        deletions: plan.deletions.len(),
    });

    let counters = Counters::default();

    if opts.dry_run {
        for pa in &plan.actions {
            emit_planned_event(reporter, &pa.entry, pa.action);
            counters.bump_action(pa.action);
            if pa.action != Action::Skip && pa.entry.is_file() {
                counters.bytes.fetch_add(pa.entry.len, Ordering::Relaxed);
            }
        }
        for del in &plan.deletions {
            reporter.event(Event::Deleted {
                rel: del.rel.clone(),
            });
            counters.deleted.fetch_add(1, Ordering::Relaxed);
        }
        return Ok(counters.snapshot());
    }

    apply_real(plan, src, dst, opts, reporter, &counters)?;
    Ok(counters.snapshot())
}

/// The on-disk apply phases (everything except dry-run bookkeeping).
fn apply_real<R: Reporter>(
    plan: &SyncPlan,
    src: &Path,
    dst: &Path,
    opts: ApplyOptions,
    reporter: &R,
    counters: &Counters,
) -> Result<()> {
    let root_canon = canonical_root(dst)?;

    // Partition work by kind. Directories and symlinks run sequentially (cheap,
    // and dirs must exist before their children); files run in parallel.
    let mut dirs: Vec<&Entry> = Vec::new();
    let mut files: Vec<(&Entry, Action)> = Vec::new();
    let mut symlinks: Vec<(&Entry, Action)> = Vec::new();

    for pa in &plan.actions {
        if pa.action == Action::Skip {
            reporter.event(Event::Skipped {
                rel: pa.entry.rel.clone(),
            });
            counters.skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        check_relative(&pa.entry.rel)?;
        match pa.entry.kind {
            EntryKind::Dir => dirs.push(&pa.entry),
            EntryKind::File => files.push((&pa.entry, pa.action)),
            EntryKind::Symlink(_) => symlinks.push((&pa.entry, pa.action)),
        }
    }

    // 1. Directories, parent-first (actions are already sorted by rel path).
    for entry in &dirs {
        let action = action_for_existing(dst, entry);
        let target = root_canon.join(&entry.rel);
        // A non-directory in the way (type change file/symlink → dir): clear it.
        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if !meta.is_dir() {
                if let Err(e) = std::fs::remove_file(&target).map_err(|e| Error::io(&target, e)) {
                    report_fail(reporter, counters, &entry.rel, &e);
                    continue;
                }
            }
        }
        if let Err(e) = std::fs::create_dir_all(&target).map_err(|e| Error::io(&target, e)) {
            report_fail(reporter, counters, &entry.rel, &e);
            continue;
        }
        // Confirm the created dir is contained, then set its mode.
        if let Err(e) = contained_target(&root_canon, &target).and_then(|t| {
            set_mode(&t, entry.mode)?;
            Ok(())
        }) {
            report_fail(reporter, counters, &entry.rel, &e);
            continue;
        }
        reporter.event(Event::DirDone {
            rel: entry.rel.clone(),
            action,
        });
        counters.bump_action(action);
    }

    // 2. Files in parallel.
    let pool = build_pool(opts.threads);
    let run_files = || {
        files.par_iter().for_each(|(entry, action)| {
            reporter.event(Event::FileStart {
                rel: entry.rel.clone(),
                len: entry.len,
            });
            match copy_file_atomic(src, &root_canon, entry, opts.reflink, opts.fsync) {
                Ok(bytes) => {
                    counters.bytes.fetch_add(bytes, Ordering::Relaxed);
                    counters.bump_action(*action);
                    reporter.event(Event::FileDone {
                        rel: entry.rel.clone(),
                        action: *action,
                        bytes,
                    });
                }
                Err(e) => report_fail(reporter, counters, &entry.rel, &e),
            }
        });
    };
    match &pool {
        Some(p) => p.install(run_files),
        None => run_files(),
    }

    // 3. Symlinks, sequentially.
    for (entry, action) in &symlinks {
        if let EntryKind::Symlink(link_target) = &entry.kind {
            match create_symlink(&root_canon, &entry.rel, link_target, entry.mtime) {
                Ok(()) => {
                    reporter.event(Event::SymlinkDone {
                        rel: entry.rel.clone(),
                        action: *action,
                    });
                    counters.bump_action(*action);
                }
                Err(e) => report_fail(reporter, counters, &entry.rel, &e),
            }
        }
    }

    // 4. Directory mtimes last (deepest first) — children writes bump parent times.
    for entry in dirs.iter().rev() {
        let target = root_canon.join(&entry.rel);
        let _ = set_mtime(&target, entry.mtime);
    }

    // 5. Guarded deletions (deepest first).
    if opts.delete && opts.yes {
        run_deletions(plan, &root_canon, reporter, counters);
    }

    // 6. Batched directory fsync for rename durability (auto mode). `always`
    // already fsynced each file; `never` skips even this.
    if opts.fsync == FsyncMode::Auto {
        fsync_touched_dirs(&root_canon, &dirs, &files);
    }

    Ok(())
}

/// Fsync every directory that received a create/rename, once each, so the
/// directory entries (and thus the atomic renames) are durable.
fn fsync_touched_dirs(root_canon: &Path, dirs: &[&Entry], files: &[(&Entry, Action)]) {
    let mut targets: std::collections::HashSet<std::path::PathBuf> =
        std::collections::HashSet::new();
    targets.insert(root_canon.to_path_buf());
    for entry in dirs {
        targets.insert(root_canon.join(&entry.rel));
    }
    for (entry, _) in files {
        if let Some(parent) = root_canon.join(&entry.rel).parent() {
            targets.insert(parent.to_path_buf());
        }
    }
    for dir in targets {
        if let Ok(f) = std::fs::File::open(&dir) {
            let _ = f.sync_all();
        }
    }
}

/// Execute the (already gated) deletion phase, deepest paths first.
fn run_deletions<R: Reporter>(
    plan: &SyncPlan,
    root_canon: &Path,
    reporter: &R,
    counters: &Counters,
) {
    for del in &plan.deletions {
        match delete_entry(root_canon, &del.rel, del.is_dir) {
            Ok(()) => {
                reporter.event(Event::Deleted {
                    rel: del.rel.clone(),
                });
                counters.deleted.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => report_fail(reporter, counters, &del.rel, &e),
        }
    }
}

fn build_pool(threads: usize) -> Option<rayon::ThreadPool> {
    if threads == 0 {
        None
    } else {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .ok()
    }
}

fn action_for_existing(dst: &Path, entry: &Entry) -> Action {
    if dst.join(&entry.rel).exists() {
        Action::Update
    } else {
        Action::Copy
    }
}

fn emit_planned_event<R: Reporter>(reporter: &R, entry: &Entry, action: Action) {
    if action == Action::Skip {
        reporter.event(Event::Skipped {
            rel: entry.rel.clone(),
        });
        return;
    }
    match entry.kind {
        EntryKind::Dir => reporter.event(Event::DirDone {
            rel: entry.rel.clone(),
            action,
        }),
        EntryKind::File => reporter.event(Event::FileDone {
            rel: entry.rel.clone(),
            action,
            bytes: entry.len,
        }),
        EntryKind::Symlink(_) => reporter.event(Event::SymlinkDone {
            rel: entry.rel.clone(),
            action,
        }),
    }
}

fn report_fail<R: Reporter>(reporter: &R, counters: &Counters, rel: &Path, err: &Error) {
    counters.errors.fetch_add(1, Ordering::Relaxed);
    reporter.event(Event::Failed {
        rel: rel.to_path_buf(),
        error: err.to_string(),
    });
}

/// Atomically copy one file, preserving mode and mtime. Returns bytes written.
fn copy_file_atomic(
    src: &Path,
    root_canon: &Path,
    entry: &Entry,
    reflink: ReflinkMode,
    fsync: FsyncMode,
) -> Result<u64> {
    let src_path = src.join(&entry.rel);
    let dst_path = root_canon.join(&entry.rel);
    let target = contained_target(root_canon, &dst_path)?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::Containment(target.clone()))?;

    let tmp = parent.join(format!(".ferry-tmp-{:016x}", rand::random::<u64>()));

    // Copy ladder: reflink → copy_file_range → buffered. `tmp` must not pre-exist.
    let bytes = match copy_file_into(&src_path, &tmp, reflink) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::io(&src_path, e));
        }
    };

    // Metadata onto the temp file.
    set_mode(&tmp, entry.mode)?;
    set_mtime(&tmp, entry.mtime)?;
    // Optional per-file durability.
    if fsync == FsyncMode::Always {
        if let Ok(f) = std::fs::File::open(&tmp) {
            let _ = f.sync_all();
        }
    }
    // A directory in the way (type change dir → file): rename can't replace it.
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&target) {
                let _ = std::fs::remove_file(&tmp);
                return Err(Error::io(&target, e));
            }
        }
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(&target, e));
    }
    Ok(bytes)
}

/// Create (or replace) a symlink, copying its target verbatim — never followed.
fn create_symlink(
    root_canon: &Path,
    rel: &Path,
    link_target: &Path,
    mtime: filetime::FileTime,
) -> Result<()> {
    let link_path = root_canon.join(rel);
    let target = contained_target(root_canon, &link_path)?;

    // Remove anything already at the path (file, dir, or stale link).
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.is_dir() {
            std::fs::remove_dir_all(&target).map_err(|e| Error::io(&target, e))?;
        } else {
            std::fs::remove_file(&target).map_err(|e| Error::io(&target, e))?;
        }
    }

    symlink_impl(link_target, &target)?;
    let _ = set_symlink_mtime(&target, mtime);
    Ok(())
}

#[cfg(unix)]
fn symlink_impl(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| Error::io(link, e))
}

#[cfg(not(unix))]
fn symlink_impl(_target: &Path, link: &Path) -> Result<()> {
    Err(Error::io(
        link,
        io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks unsupported on this platform",
        ),
    ))
}

/// Whether any strict ancestor of `rel` (under `root_canon`) is a symlink.
fn has_symlink_ancestor(root_canon: &Path, rel: &Path) -> bool {
    let mut prefix = root_canon.to_path_buf();
    let comps: Vec<_> = rel.components().collect();
    // Iterate ancestors only (exclude the final component itself).
    for comp in &comps[..comps.len().saturating_sub(1)] {
        prefix.push(comp);
        match std::fs::symlink_metadata(&prefix) {
            Ok(m) if m.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return true, // ancestor vanished ⇒ entry is gone too
        }
    }
    false
}

/// Delete one destination entry, after confirming containment.
///
/// A previously planned entry may already be gone (e.g. it was inside a
/// directory that a type-change replacement removed); that is treated as success
/// since the goal is its absence.
fn delete_entry(root_canon: &Path, rel: &Path, is_dir: bool) -> Result<()> {
    check_relative(rel)?;
    // If any ancestor component is a symlink, this logical path no longer points
    // at the originally-planned entry (e.g. a dir was replaced by a symlink) —
    // deleting through it would clobber a sibling. The entry is already gone.
    if has_symlink_ancestor(root_canon, rel) {
        return Ok(());
    }
    let path = root_canon.join(rel);
    // Already absent (or its parent vanished)? Nothing to do.
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(());
    }
    let target = contained_target(root_canon, &path)?;
    let result = if is_dir {
        // Directory should be empty by now (deepest-first order); fall back to
        // recursive removal if not.
        std::fs::remove_dir(&target).or_else(|_| std::fs::remove_dir_all(&target))
    } else {
        std::fs::remove_file(&target)
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(&target, e)),
    }
}
