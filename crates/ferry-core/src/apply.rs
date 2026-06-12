//! Execute a [`SyncPlan`]: atomic file copies, metadata preservation, contained
//! symlinks, and guarded deletes.
//!
//! Files are written to a temporary name in the destination directory, flushed
//! with `fsync`, given the source's mode and mtime, then atomically `rename`d over
//! the target — so a crash mid-copy never leaves a half-written file in place.
//! Every write/symlink/delete target is checked for destination containment first.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;

use crate::control::RunControl;
use crate::copy::{FsyncMode, ReflinkMode, copy_file_into};
use crate::meta::{
    canonical_root, check_relative, contained_target, copy_xattrs, set_mode, set_mtime,
    set_owner_group, set_symlink_mtime,
};
use crate::plan::{Action, SyncPlan};
use crate::report::{Event, Reporter, RunPhase, Stats};
use crate::walk::{Entry, EntryKind};
use crate::{Error, Result};

/// Which file-copy backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// Portable backend (conservative default).
    #[default]
    Auto,
    /// Force the `io_uring` batched backend (Linux).
    Uring,
    /// Force the portable reflink/`copy_file_range`/buffered backend.
    Portable,
}

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
    /// File-copy backend selection.
    pub backend: Backend,
    /// Optional metadata and file-layout preservation.
    pub metadata: MetadataOptions,
}

/// Optional metadata and file-layout preservation controls.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct MetadataOptions {
    /// Preserve hardlink groups.
    pub hard_links: bool,
    /// Preserve sparse-file holes.
    pub sparse: bool,
    /// Preserve non-ACL extended attributes.
    pub xattrs: bool,
    /// Preserve POSIX ACL attributes.
    pub acls: bool,
    /// Preserve numeric owner id.
    pub owner: bool,
    /// Preserve numeric group id.
    pub group: bool,
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
    apply_plan_controlled(plan, src, dst, opts, reporter, &RunControl::default())
}

/// Apply a plan with cooperative pause and cancellation.
///
/// # Errors
///
/// Returns setup, containment, or cancellation errors. Per-entry operation
/// failures continue to be counted in the returned statistics.
pub fn apply_plan_controlled<R: Reporter>(
    plan: &SyncPlan,
    src: &Path,
    dst: &Path,
    opts: ApplyOptions,
    reporter: &R,
    control: &RunControl,
) -> Result<Stats> {
    control.checkpoint()?;
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
            control.checkpoint()?;
            emit_planned_event(reporter, &pa.entry, pa.action);
            counters.bump_action(pa.action);
            if pa.action != Action::Skip && pa.entry.is_file() {
                counters.bytes.fetch_add(pa.entry.len, Ordering::Relaxed);
            }
        }
        for del in &plan.deletions {
            control.checkpoint()?;
            reporter.event(Event::Deleted {
                rel: del.rel.clone(),
            });
            counters.deleted.fetch_add(1, Ordering::Relaxed);
        }
        return Ok(counters.snapshot());
    }

    apply_real(plan, src, dst, opts, reporter, &counters, control)?;
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
    control: &RunControl,
) -> Result<()> {
    reporter.event(Event::Phase(RunPhase::Copying));
    control.checkpoint()?;
    let (backend, reason) = selected_backend(opts);
    reporter.event(Event::BackendSelected { backend, reason });
    let root_canon = canonical_root(dst)?;

    // Partition work by kind. Directories are batched by depth, files run in
    // parallel, and symlinks run sequentially.
    let mut dirs: Vec<&Entry> = Vec::new();
    let mut mtime_dirs: Vec<&Entry> = Vec::new();
    let mut files: Vec<(&Entry, Action)> = Vec::new();
    let mut hardlinks: Vec<(&Entry, Action, PathBuf)> = Vec::new();
    let mut symlinks: Vec<(&Entry, Action)> = Vec::new();
    let mut first_hardlink: HashMap<(u64, u64), PathBuf> = HashMap::new();

    for pa in &plan.actions {
        if pa.entry.is_dir() {
            mtime_dirs.push(&pa.entry);
        }
        if opts.metadata.hard_links && pa.entry.is_file() {
            let id = (pa.entry.dev, pa.entry.ino);
            if let Some(canonical) = first_hardlink.get(&id) {
                if pa.action == Action::Skip {
                    reporter.event(Event::Skipped {
                        rel: pa.entry.rel.clone(),
                    });
                    counters.skipped.fetch_add(1, Ordering::Relaxed);
                } else {
                    check_relative(&pa.entry.rel)?;
                    hardlinks.push((&pa.entry, pa.action, canonical.clone()));
                }
                continue;
            }
            first_hardlink.insert(id, pa.entry.rel.clone());
        }
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

    let pool = build_pool(opts.threads);

    // 1. Directories, parent-depth batches. Peers at one depth are independent.
    control.checkpoint()?;
    let run_dirs = || {
        create_directories(
            &dirs,
            src,
            dst,
            &root_canon,
            opts,
            reporter,
            counters,
            control,
        )
    };
    match &pool {
        Some(thread_pool) => thread_pool.install(run_dirs)?,
        None => run_dirs()?,
    }

    // 2. Files.
    control.checkpoint()?;
    let run_files = || copy_files(&files, src, &root_canon, opts, reporter, counters, control);
    match &pool {
        Some(p) => p.install(run_files)?,
        None => run_files()?,
    }

    // 3. Duplicate hardlinks after their canonical files exist.
    copy_hardlinks(&hardlinks, &root_canon, reporter, counters, control)?;

    // 4. Symlinks, sequentially.
    for (entry, action) in &symlinks {
        control.checkpoint()?;
        if let EntryKind::Symlink(link_target) = &entry.kind {
            match create_symlink(src, &root_canon, entry, link_target, opts) {
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

    // 5. Directory mtimes last (deepest first) — children writes bump parent times.
    for entry in mtime_dirs.iter().rev() {
        control.checkpoint()?;
        let target = root_canon.join(&entry.rel);
        let _ = set_mtime(&target, entry.mtime);
    }

    // 6. Guarded deletions (deepest first).
    if opts.delete && opts.yes {
        reporter.event(Event::Phase(RunPhase::Deleting));
        run_deletions(plan, &root_canon, reporter, counters, control)?;
    }

    // 7. Batched directory fsync for rename durability (auto mode). `always`
    // already fsynced each file; `never` skips even this.
    if opts.fsync == FsyncMode::Auto {
        control.checkpoint()?;
        fsync_touched_dirs(&root_canon, &dirs, &files);
    }

    control.checkpoint()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_directories<R: Reporter>(
    dirs: &[&Entry],
    src: &Path,
    dst: &Path,
    root_canon: &Path,
    opts: ApplyOptions,
    reporter: &R,
    counters: &Counters,
    control: &RunControl,
) -> Result<()> {
    let mut start = 0;
    while start < dirs.len() {
        control.checkpoint()?;
        let depth = dirs[start].rel.components().count();
        let end = dirs[start..]
            .iter()
            .position(|entry| entry.rel.components().count() != depth)
            .map_or(dirs.len(), |offset| start + offset);
        dirs[start..end].par_iter().for_each(|entry| {
            create_directory(entry, src, dst, root_canon, opts, reporter, counters);
        });
        control.checkpoint()?;
        start = end;
    }
    Ok(())
}

fn create_directory<R: Reporter>(
    entry: &Entry,
    src: &Path,
    dst: &Path,
    root_canon: &Path,
    opts: ApplyOptions,
    reporter: &R,
    counters: &Counters,
) {
    let action = action_for_existing(dst, entry);
    let target = root_canon.join(&entry.rel);
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if !meta.is_dir() {
            if let Err(error) =
                std::fs::remove_file(&target).map_err(|error| Error::io(&target, error))
            {
                report_fail(reporter, counters, &entry.rel, &error);
                return;
            }
        }
    }
    if let Err(error) = std::fs::create_dir_all(&target).map_err(|error| Error::io(&target, error))
    {
        report_fail(reporter, counters, &entry.rel, &error);
        return;
    }
    let result = contained_target(root_canon, &target).and_then(|contained| {
        let source = src.join(&entry.rel);
        set_owner_group(
            &contained,
            entry.uid,
            entry.gid,
            opts.metadata.owner,
            opts.metadata.group,
            true,
        )?;
        set_mode(&contained, entry.mode)?;
        copy_xattrs(
            &source,
            &contained,
            opts.metadata.xattrs,
            opts.metadata.acls,
        )
    });
    if let Err(error) = result {
        report_fail(reporter, counters, &entry.rel, &error);
        return;
    }
    reporter.event(Event::DirDone {
        rel: entry.rel.clone(),
        action,
    });
    counters.bump_action(action);
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
    control: &RunControl,
) -> Result<()> {
    for del in &plan.deletions {
        control.checkpoint()?;
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
    Ok(())
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

/// Resolve the concrete `(target, tmp)` paths for a file, enforcing containment.
fn prepare_paths(
    root_canon: &Path,
    entry: &Entry,
) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let dst_path = root_canon.join(&entry.rel);
    let target = contained_target(root_canon, &dst_path)?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::Containment(target.clone()))?;
    let tmp = parent.join(format!(".ferry-tmp-{:016x}", rand::random::<u64>()));
    Ok((target, tmp))
}

/// Apply metadata to `tmp`, then atomically rename it over `target`.
fn finalize_file(
    src: &Path,
    target: &Path,
    tmp: &Path,
    entry: &Entry,
    opts: ApplyOptions,
) -> Result<()> {
    set_owner_group(
        tmp,
        entry.uid,
        entry.gid,
        opts.metadata.owner,
        opts.metadata.group,
        true,
    )?;
    set_mode(tmp, entry.mode)?;
    copy_xattrs(src, tmp, opts.metadata.xattrs, opts.metadata.acls)?;
    set_mtime(tmp, entry.mtime)?;
    if opts.fsync == FsyncMode::Always {
        if let Ok(f) = std::fs::File::open(tmp) {
            let _ = f.sync_all();
        }
    }
    // A directory in the way (type change dir → file): rename can't replace it.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(target) {
                let _ = std::fs::remove_file(tmp);
                return Err(Error::io(target, e));
            }
        }
    }
    if let Err(e) = std::fs::rename(tmp, target) {
        let _ = std::fs::remove_file(tmp);
        return Err(Error::io(target, e));
    }
    Ok(())
}

/// Portable single-file copy: prepare → ladder → finalize. Returns bytes written.
fn copy_file_atomic(
    src: &Path,
    root_canon: &Path,
    entry: &Entry,
    opts: ApplyOptions,
) -> Result<u64> {
    let src_path = src.join(&entry.rel);
    let (target, tmp) = prepare_paths(root_canon, entry)?;
    // Copy ladder: reflink → copy_file_range → buffered. `tmp` must not pre-exist.
    let bytes = match copy_file_into(&src_path, &tmp, opts.reflink, opts.metadata.sparse) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::io(&src_path, e));
        }
    };
    finalize_file(&src_path, &target, &tmp, entry, opts)?;
    Ok(bytes)
}

/// Dispatch the file-copy phase to the selected backend.
fn copy_files<R: Reporter>(
    files: &[(&Entry, Action)],
    src: &Path,
    root_canon: &Path,
    opts: ApplyOptions,
    reporter: &R,
    counters: &Counters,
    control: &RunControl,
) -> Result<()> {
    #[cfg(all(target_os = "linux", feature = "io-uring"))]
    {
        if opts.backend == Backend::Uring && !opts.metadata.sparse {
            control.checkpoint()?;
            copy_files_uring(files, src, root_canon, opts, reporter, counters);
            control.checkpoint()?;
            return Ok(());
        }
    }
    copy_files_portable(files, src, root_canon, opts, reporter, counters, control)
}

/// Portable backend: one atomic copy per file across the rayon pool.
fn copy_files_portable<R: Reporter>(
    files: &[(&Entry, Action)],
    src: &Path,
    root_canon: &Path,
    opts: ApplyOptions,
    reporter: &R,
    counters: &Counters,
    control: &RunControl,
) -> Result<()> {
    let chunk_size = opts.threads.max(1).saturating_mul(2);
    for chunk in files.chunks(chunk_size) {
        control.checkpoint()?;
        chunk.par_iter().for_each(|(entry, action)| {
            reporter.event(Event::FileStart {
                rel: entry.rel.clone(),
                len: entry.len,
            });
            match copy_file_atomic(src, root_canon, entry, opts) {
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
        control.checkpoint()?;
    }
    Ok(())
}

/// `io_uring` backend: batch the data copy through one ring, then finalize each
/// file in parallel. Anything the ring rejects falls back to the portable copy.
#[cfg(all(target_os = "linux", feature = "io-uring"))]
fn copy_files_uring<R: Reporter>(
    files: &[(&Entry, Action)],
    src: &Path,
    root_canon: &Path,
    opts: ApplyOptions,
    reporter: &R,
    counters: &Counters,
) {
    use crate::io::uring::{self, Job};

    // Prepare paths sequentially (containment), reporting prep failures.
    struct Prepared<'a> {
        entry: &'a Entry,
        action: Action,
        src_path: std::path::PathBuf,
        target: std::path::PathBuf,
        tmp: std::path::PathBuf,
    }
    let mut prepared: Vec<Prepared> = Vec::with_capacity(files.len());
    for (entry, action) in files {
        reporter.event(Event::FileStart {
            rel: entry.rel.clone(),
            len: entry.len,
        });
        match prepare_paths(root_canon, entry) {
            Ok((target, tmp)) => prepared.push(Prepared {
                entry,
                action: *action,
                src_path: src.join(&entry.rel),
                target,
                tmp,
            }),
            Err(e) => report_fail(reporter, counters, &entry.rel, &e),
        }
    }

    let jobs: Vec<Job> = prepared
        .iter()
        .map(|p| Job {
            src: &p.src_path,
            tmp: &p.tmp,
            len: p.entry.len,
        })
        .collect();
    let batch = uring::copy_batch(&jobs);

    // Finalize in parallel; uring rejects fall back to the portable ladder.
    prepared.par_iter().zip(batch).for_each(|(p, res)| {
        let outcome = if let Ok(bytes) = res {
            finalize_file(&p.src_path, &p.target, &p.tmp, p.entry, opts).map(|()| bytes)
        } else {
            // Fall back: remove any partial temp, then portable copy.
            let _ = std::fs::remove_file(&p.tmp);
            copy_file_into(&p.src_path, &p.tmp, opts.reflink, opts.metadata.sparse)
                .map_err(|e| Error::io(&p.src_path, e))
                .and_then(|bytes| {
                    finalize_file(&p.src_path, &p.target, &p.tmp, p.entry, opts).map(|()| bytes)
                })
        };
        match outcome {
            Ok(bytes) => {
                counters.bytes.fetch_add(bytes, Ordering::Relaxed);
                counters.bump_action(p.action);
                reporter.event(Event::FileDone {
                    rel: p.entry.rel.clone(),
                    action: p.action,
                    bytes,
                });
            }
            Err(e) => report_fail(reporter, counters, &p.entry.rel, &e),
        }
    });
}

/// Materialize duplicate members after their canonical hardlink targets exist.
fn copy_hardlinks<R: Reporter>(
    hardlinks: &[(&Entry, Action, PathBuf)],
    root_canon: &Path,
    reporter: &R,
    counters: &Counters,
    control: &RunControl,
) -> Result<()> {
    for (entry, action, canonical) in hardlinks {
        control.checkpoint()?;
        reporter.event(Event::FileStart {
            rel: entry.rel.clone(),
            len: entry.len,
        });
        match create_hardlink(root_canon, entry, canonical) {
            Ok(()) => {
                counters.bump_action(*action);
                reporter.event(Event::FileDone {
                    rel: entry.rel.clone(),
                    action: *action,
                    bytes: 0,
                });
            }
            Err(error) => report_fail(reporter, counters, &entry.rel, &error),
        }
    }
    Ok(())
}

fn selected_backend(opts: ApplyOptions) -> (&'static str, &'static str) {
    match opts.backend {
        Backend::Auto => ("portable", "auto is portable-first"),
        Backend::Portable => ("portable", "explicitly requested"),
        Backend::Uring if opts.metadata.sparse => {
            ("portable", "sparse preservation requires portable")
        }
        Backend::Uring => ("uring", "explicitly requested"),
    }
}

/// Atomically create a hardlink to an already materialized canonical file.
fn create_hardlink(root_canon: &Path, entry: &Entry, canonical_rel: &Path) -> Result<()> {
    let canonical = root_canon.join(canonical_rel);
    let canonical =
        std::fs::canonicalize(&canonical).map_err(|error| Error::io(&canonical, error))?;
    if !canonical.starts_with(root_canon) {
        return Err(Error::Containment(canonical));
    }
    let (target, tmp) = prepare_paths(root_canon, entry)?;
    std::fs::hard_link(&canonical, &tmp).map_err(|error| Error::io(&tmp, error))?;
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        let result = if meta.is_dir() {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_file(&target)
        };
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::io(&target, error));
        }
    }
    if let Err(error) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(&target, error));
    }
    Ok(())
}

/// Create (or replace) a symlink, copying its target verbatim — never followed.
fn create_symlink(
    src: &Path,
    root_canon: &Path,
    entry: &Entry,
    link_target: &Path,
    opts: ApplyOptions,
) -> Result<()> {
    let link_path = root_canon.join(&entry.rel);
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
    set_owner_group(
        &target,
        entry.uid,
        entry.gid,
        opts.metadata.owner,
        opts.metadata.group,
        false,
    )?;
    copy_xattrs(
        &src.join(&entry.rel),
        &target,
        opts.metadata.xattrs,
        opts.metadata.acls,
    )?;
    let _ = set_symlink_mtime(&target, entry.mtime);
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
