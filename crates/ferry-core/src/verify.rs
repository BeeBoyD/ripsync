//! Optional post-apply verification.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::GlobSetBuilder;

use crate::apply::MetadataOptions;
use crate::control::RunControl;
use crate::index::FERRY_DIR;
use crate::meta::{FileTypeKind, meta_min};
use crate::plan::{Action, SyncPlan};
use crate::report::{Event, Reporter, RunPhase};
use crate::walk::{Entry, EntryKind, walk_controlled};
use crate::{Error, Result};

/// Verification scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerifyMode {
    /// Do not verify.
    #[default]
    None,
    /// Verify entries changed by this run.
    Changed,
    /// Verify the complete source and destination trees.
    All,
}

/// One source/destination mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationMismatch {
    /// Relative path.
    pub rel: PathBuf,
    /// Human-readable mismatch category.
    pub detail: String,
}

/// Verification summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerificationSummary {
    /// Number of entries checked.
    pub checked: usize,
    /// Mismatch details.
    pub mismatches: Vec<VerificationMismatch>,
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = std::fs::File::open(path).map_err(|error| Error::io(path, error))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| Error::io(path, error))?;
        if read == 0 {
            return Ok(*hasher.finalize().as_bytes());
        }
        hasher.update(&buffer[..read]);
    }
}

fn mismatch(rel: &Path, detail: impl Into<String>) -> VerificationMismatch {
    VerificationMismatch {
        rel: rel.to_path_buf(),
        detail: detail.into(),
    }
}

fn live_entry(root: &Path, rel: &Path) -> Result<Entry> {
    let path = root.join(rel);
    let metadata = meta_min(&path)?;
    let kind = match metadata.kind {
        FileTypeKind::File => EntryKind::File,
        FileTypeKind::Dir => EntryKind::Dir,
        FileTypeKind::Symlink => {
            EntryKind::Symlink(std::fs::read_link(&path).map_err(|error| Error::io(&path, error))?)
        }
        FileTypeKind::Other => {
            return Err(Error::io(
                &path,
                std::io::Error::other("unsupported file kind"),
            ));
        }
    };
    Ok(Entry {
        rel: rel.to_path_buf(),
        len: if matches!(kind, EntryKind::File) {
            metadata.len
        } else {
            0
        },
        kind,
        mtime: metadata.mtime,
        mode: metadata.mode,
        ino: metadata.ino,
        dev: metadata.dev,
        uid: metadata.uid,
        gid: metadata.gid,
    })
}

fn compare_entry(
    source: &Entry,
    destination: Option<&Entry>,
    src: &Path,
    dst: &Path,
    metadata: MetadataOptions,
) -> Result<Option<VerificationMismatch>> {
    let Some(destination) = destination else {
        return Ok(Some(mismatch(&source.rel, "missing from destination")));
    };
    if std::mem::discriminant(&source.kind) != std::mem::discriminant(&destination.kind) {
        return Ok(Some(mismatch(&source.rel, "file kind differs")));
    }
    if source.mode & 0o7777 != destination.mode & 0o7777 {
        return Ok(Some(mismatch(&source.rel, "mode differs")));
    }
    if source.mtime != destination.mtime {
        return Ok(Some(mismatch(&source.rel, "mtime differs")));
    }
    if metadata.owner && source.uid != destination.uid {
        return Ok(Some(mismatch(&source.rel, "uid differs")));
    }
    if metadata.group && source.gid != destination.gid {
        return Ok(Some(mismatch(&source.rel, "gid differs")));
    }
    match (&source.kind, &destination.kind) {
        (EntryKind::File, EntryKind::File) => {
            if source.len != destination.len
                || hash_file(&src.join(&source.rel))? != hash_file(&dst.join(&source.rel))?
            {
                return Ok(Some(mismatch(&source.rel, "content differs")));
            }
            if metadata.sparse
                && sparse_blocks(&src.join(&source.rel)) != sparse_blocks(&dst.join(&source.rel))
            {
                return Ok(Some(mismatch(&source.rel, "sparse allocation differs")));
            }
        }
        (EntryKind::Symlink(a), EntryKind::Symlink(b)) if a != b => {
            return Ok(Some(mismatch(&source.rel, "symlink target differs")));
        }
        _ => {}
    }
    if (metadata.xattrs || metadata.acls)
        && extended_attributes(&src.join(&source.rel), metadata)
            != extended_attributes(&dst.join(&source.rel), metadata)
    {
        return Ok(Some(mismatch(&source.rel, "extended attributes differ")));
    }
    Ok(None)
}

#[cfg(unix)]
fn sparse_blocks(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.blocks())
}

#[cfg(not(unix))]
fn sparse_blocks(_path: &Path) -> Option<u64> {
    None
}

#[cfg(unix)]
fn extended_attributes(path: &Path, metadata: MetadataOptions) -> Option<Vec<(String, Vec<u8>)>> {
    let mut attributes = Vec::new();
    for name in xattr::list(path).ok()? {
        let text = name.to_string_lossy();
        let is_acl = text.starts_with("system.posix_acl_");
        if (is_acl && !metadata.acls) || (!is_acl && !metadata.xattrs) {
            continue;
        }
        attributes.push((
            text.into_owned(),
            xattr::get(path, &name).ok().flatten().unwrap_or_default(),
        ));
    }
    attributes.sort_unstable();
    Some(attributes)
}

#[cfg(not(unix))]
fn extended_attributes(_path: &Path, _metadata: MetadataOptions) -> Option<Vec<(String, Vec<u8>)>> {
    None
}

/// Verify a completed apply before manifest persistence.
///
/// # Errors
///
/// Returns an I/O, containment, metadata, or cancellation error.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn verify<R: Reporter>(
    plan: &SyncPlan,
    src: &Path,
    dst: &Path,
    mode: VerifyMode,
    metadata: MetadataOptions,
    threads: usize,
    control: &RunControl,
    reporter: &R,
) -> Result<VerificationSummary> {
    if mode == VerifyMode::None {
        return Ok(VerificationSummary::default());
    }
    reporter.event(Event::Phase(RunPhase::Verifying));
    control.checkpoint()?;
    let excludes = GlobSetBuilder::new()
        .build()
        .map_err(|error| Error::Pattern(error.to_string()))?;
    let source_entries = if mode == VerifyMode::All {
        walk_controlled(src, threads, &excludes, control)?
    } else {
        Vec::new()
    };
    let mut destination_entries = if mode == VerifyMode::All {
        walk_controlled(dst, threads, &excludes, control)?
    } else {
        plan.actions
            .iter()
            .filter(|planned| metadata.hard_links || planned.action != Action::Skip)
            .filter_map(|planned| live_entry(dst, &planned.entry.rel).ok())
            .collect()
    };
    destination_entries.retain(|entry| !entry.rel.starts_with(FERRY_DIR));
    let destination: HashMap<&Path, &Entry> = destination_entries
        .iter()
        .map(|entry| (entry.rel.as_path(), entry))
        .collect();
    let selected: Vec<&Entry> = match mode {
        VerifyMode::None => Vec::new(),
        VerifyMode::Changed => plan
            .actions
            .iter()
            .filter(|planned| planned.action != Action::Skip)
            .map(|planned| &planned.entry)
            .collect(),
        VerifyMode::All => source_entries.iter().collect(),
    };
    let total = selected.len();
    let mut summary = VerificationSummary::default();
    for entry in &selected {
        control.checkpoint()?;
        summary.checked += 1;
        if let Some(problem) = compare_entry(
            entry,
            destination.get(entry.rel.as_path()).copied(),
            src,
            dst,
            metadata,
        )? {
            reporter.event(Event::VerificationFailed {
                rel: problem.rel.clone(),
                detail: problem.detail.clone(),
            });
            summary.mismatches.push(problem);
        }
        reporter.event(Event::VerificationProgress {
            checked: summary.checked,
            total,
            mismatches: summary.mismatches.len(),
        });
    }
    if metadata.hard_links {
        let mut groups: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
        let hardlink_entries: Vec<&Entry> = if mode == VerifyMode::Changed {
            plan.actions.iter().map(|planned| &planned.entry).collect()
        } else {
            selected.clone()
        };
        for entry in hardlink_entries {
            if !entry.is_file() {
                continue;
            }
            let Some(destination) = destination.get(entry.rel.as_path()) else {
                continue;
            };
            let source_id = (entry.dev, entry.ino);
            let destination_id = (destination.dev, destination.ino);
            let expected = groups.entry(source_id).or_insert(destination_id);
            if *expected != destination_id {
                let problem = mismatch(&entry.rel, "hardlink identity differs");
                reporter.event(Event::VerificationFailed {
                    rel: problem.rel.clone(),
                    detail: problem.detail.clone(),
                });
                summary.mismatches.push(problem);
            }
        }
    }
    if mode == VerifyMode::All {
        let source: std::collections::HashSet<&Path> = source_entries
            .iter()
            .map(|entry| entry.rel.as_path())
            .collect();
        for entry in destination_entries {
            control.checkpoint()?;
            if !source.contains(entry.rel.as_path()) {
                let problem = mismatch(&entry.rel, "extra destination entry");
                reporter.event(Event::VerificationFailed {
                    rel: problem.rel.clone(),
                    detail: problem.detail.clone(),
                });
                summary.mismatches.push(problem);
            }
        }
    }
    Ok(summary)
}
