//! Advisory persistent manifest for fast incremental re-syncs.
//!
//! A missing, corrupt, incompatible, or stale manifest causes normal copy/update
//! actions. Writes and deletes still pass through the apply layer's containment
//! checks.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::meta::{FileTypeKind, canonical_root, contained_target, meta_min};
use crate::plan::{Action, SyncPlan};
use crate::walk::{Entry, EntryKind};

/// Directory under the destination root that holds Ferry metadata.
pub const FERRY_DIR: &str = ".ferry";
/// Manifest filename within [`FERRY_DIR`].
pub const MANIFEST_FILE: &str = "manifest.bin";
const FORMAT_VERSION: u32 = 2;

/// Recorded manifest entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
}

/// One recorded destination entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Entry kind.
    pub kind: Kind,
    /// File length in bytes.
    pub size: u64,
    /// Modification time, whole seconds.
    pub mtime_s: i64,
    /// Modification time, nanosecond part.
    pub mtime_ns: u32,
    /// Destination inode number.
    pub ino: u64,
    /// Destination device id.
    pub dev: u64,
    /// Permission and type bits.
    pub mode: u32,
    /// Strong hash, when the manifest was created with `--checksum`.
    pub hash: Option<[u8; 32]>,
    /// Symlink target, for symlinks.
    pub target: Option<PathBuf>,
}

/// Loaded manifest, keyed by destination-relative path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// On-disk format version.
    pub version: u32,
    /// Recorded entries.
    pub entries: BTreeMap<PathBuf, ManifestEntry>,
}

/// Path to the manifest under `dst`.
#[must_use]
pub fn manifest_path(dst: &Path) -> PathBuf {
    dst.join(FERRY_DIR).join(MANIFEST_FILE)
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

impl Manifest {
    /// Load a compatible manifest, rejecting metadata paths outside `dst`.
    #[must_use]
    pub fn load(dst: &Path) -> Option<Self> {
        let root = std::fs::canonicalize(dst).ok()?;
        let path = manifest_path(&root);
        let parent = std::fs::canonicalize(path.parent()?).ok()?;
        if !parent.starts_with(&root) {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let manifest: Self = bincode::deserialize(&bytes).ok()?;
        (manifest.version == FORMAT_VERSION).then_some(manifest)
    }

    /// Build a manifest from the live destination after a successful apply.
    ///
    /// # Errors
    ///
    /// Returns an error if an applied destination entry cannot be inspected.
    pub fn from_destination(plan: &SyncPlan, dst: &Path, hash_files: bool) -> crate::Result<Self> {
        let mut entries = BTreeMap::new();
        for planned in &plan.actions {
            let entry = &planned.entry;
            let path = dst.join(&entry.rel);
            let meta = meta_min(&path)?;
            let (kind, target) = match &entry.kind {
                EntryKind::File => (Kind::File, None),
                EntryKind::Dir => (Kind::Dir, None),
                EntryKind::Symlink(_) => (
                    Kind::Symlink,
                    Some(
                        std::fs::read_link(&path)
                            .map_err(|error| crate::Error::io(&path, error))?,
                    ),
                ),
            };
            let hash = if hash_files && kind == Kind::File {
                Some(hash_file(&path).map_err(|error| crate::Error::io(&path, error))?)
            } else {
                None
            };
            entries.insert(
                entry.rel.clone(),
                ManifestEntry {
                    kind,
                    size: entry.len,
                    mtime_s: meta.mtime.unix_seconds(),
                    mtime_ns: meta.mtime.nanoseconds(),
                    ino: meta.ino,
                    dev: meta.dev,
                    mode: meta.mode,
                    hash,
                    target,
                },
            );
        }
        Ok(Self {
            version: FORMAT_VERSION,
            entries,
        })
    }

    /// Atomically save this manifest inside a contained `.ferry` directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the metadata directory is unsafe or cannot be written.
    pub fn save(&self, dst: &Path) -> crate::Result<()> {
        let root = canonical_root(dst)?;
        let requested_dir = root.join(FERRY_DIR);
        match std::fs::symlink_metadata(&requested_dir) {
            Ok(meta) if !meta.is_dir() => {
                return Err(crate::Error::Containment(requested_dir));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir(&requested_dir)
                    .map_err(|create_error| crate::Error::io(&requested_dir, create_error))?;
            }
            Err(error) => return Err(crate::Error::io(&requested_dir, error)),
        }
        let dir = std::fs::canonicalize(&requested_dir)
            .map_err(|error| crate::Error::io(&requested_dir, error))?;
        if !dir.starts_with(&root) {
            return Err(crate::Error::Containment(requested_dir));
        }

        let bytes = bincode::serialize(self)
            .map_err(|error| crate::Error::Pattern(format!("manifest: {error}")))?;
        let tmp = dir.join(format!(".manifest-tmp-{:016x}", rand::random::<u64>()));
        std::fs::write(&tmp, bytes).map_err(|error| crate::Error::io(&tmp, error))?;
        let final_path = contained_target(&root, &dir.join(MANIFEST_FILE))?;
        if let Err(error) = std::fs::rename(&tmp, &final_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(crate::Error::io(&final_path, error));
        }
        Ok(())
    }

    /// Classify a source entry against the recorded and live destination state.
    #[must_use]
    pub fn classify(&self, entry: &Entry, checksum: bool, src: &Path, dst: &Path) -> Action {
        let Some(recorded) = self.entries.get(&entry.rel) else {
            return Action::Copy;
        };
        let destination = dst.join(&entry.rel);
        match (&entry.kind, recorded.kind) {
            (EntryKind::Dir, Kind::Dir) => {
                let source_matches = metadata_matches(entry, recorded);
                action_from_match(
                    source_matches && destination_matches(recorded, &destination, None),
                )
            }
            (EntryKind::Symlink(target), Kind::Symlink) => {
                let matches = recorded.target.as_ref() == Some(target)
                    && metadata_matches(entry, recorded)
                    && destination_matches(recorded, &destination, Some(target));
                action_from_match(matches)
            }
            (EntryKind::File, Kind::File) => {
                let source_matches = metadata_matches(entry, recorded);
                if !source_matches || !destination_matches(recorded, &destination, None) {
                    return Action::Update;
                }
                if checksum {
                    match (recorded.hash, hash_file(&src.join(&entry.rel)).ok()) {
                        (Some(expected), Some(live)) if expected == live => Action::Skip,
                        _ => Action::Update,
                    }
                } else {
                    Action::Skip
                }
            }
            _ => Action::Update,
        }
    }
}

fn metadata_matches(entry: &Entry, recorded: &ManifestEntry) -> bool {
    entry.len == recorded.size
        && entry.mtime.unix_seconds() == recorded.mtime_s
        && entry.mtime.nanoseconds() == recorded.mtime_ns
        && entry.mode & 0o7777 == recorded.mode & 0o7777
}

fn action_from_match(matches: bool) -> Action {
    if matches {
        Action::Skip
    } else {
        Action::Update
    }
}

fn destination_matches(
    recorded: &ManifestEntry,
    path: &Path,
    symlink_target: Option<&PathBuf>,
) -> bool {
    let Ok(meta) = meta_min(path) else {
        return false;
    };
    let kind_matches = matches!(
        (recorded.kind, meta.kind),
        (Kind::File, FileTypeKind::File)
            | (Kind::Dir, FileTypeKind::Dir)
            | (Kind::Symlink, FileTypeKind::Symlink)
    );
    if !kind_matches || meta.ino != recorded.ino || meta.dev != recorded.dev {
        return false;
    }
    if (recorded.kind == Kind::File && meta.len != recorded.size)
        || meta.mtime.unix_seconds() != recorded.mtime_s
        || meta.mtime.nanoseconds() != recorded.mtime_ns
        || meta.mode & 0o7777 != recorded.mode & 0o7777
    {
        return false;
    }
    match symlink_target {
        Some(expected) => std::fs::read_link(path).is_ok_and(|target| target == *expected),
        None => true,
    }
}
