//! Persistent destination index: an atomic v3 snapshot plus append-only deltas.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

// foldhash is a faster, non-DoS-hardened hasher; manifest keys are local paths,
// never attacker-controlled network input, so the trade-off is pure win.
use foldhash::{HashMap, HashMapExt};
use serde::{Deserialize, Serialize};

use crate::meta::{FileTypeKind, canonical_root, contained_target, meta_min};
use crate::plan::{Action, SyncPlan};
use crate::walk::{Entry, EntryKind};

/// Directory under the destination root that holds ripsync metadata.
pub const RIPSYNC_DIR: &str = ".ripsync";
/// Snapshot filename within [`RIPSYNC_DIR`].
pub const MANIFEST_FILE: &str = "manifest.bin";
/// Delta journal filename within [`RIPSYNC_DIR`].
pub const JOURNAL_FILE: &str = "manifest.journal";
const FORMAT_VERSION: u32 = 3;
const COMPACT_BYTES: u64 = 64 * 1024 * 1024;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Strong hash, when created with `--checksum`.
    pub hash: Option<[u8; 32]>,
    /// Symlink target.
    pub target: Option<PathBuf>,
}

/// Loaded manifest, keyed by destination-relative path.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// On-disk format version.
    pub version: u32,
    /// Recorded entries.
    pub entries: HashMap<PathBuf, ManifestEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
enum Delta {
    Upsert(PathBuf, ManifestEntry),
    Delete(PathBuf),
}

/// Path to the snapshot under `dst`.
#[must_use]
pub fn manifest_path(dst: &Path) -> PathBuf {
    dst.join(RIPSYNC_DIR).join(MANIFEST_FILE)
}

/// Path to the append-only journal under `dst`.
#[must_use]
pub fn journal_path(dst: &Path) -> PathBuf {
    dst.join(RIPSYNC_DIR).join(JOURNAL_FILE)
}

fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            return Ok(*hasher.finalize().as_bytes());
        }
        hasher.update(&buf[..read]);
    }
}

fn metadata_dir(dst: &Path) -> crate::Result<(PathBuf, PathBuf)> {
    let root = canonical_root(dst)?;
    let requested = root.join(RIPSYNC_DIR);
    match std::fs::symlink_metadata(&requested) {
        Ok(meta) if !meta.is_dir() => return Err(crate::Error::Containment(requested)),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir(&requested)
                .map_err(|create_error| crate::Error::io(&requested, create_error))?;
        }
        Err(error) => return Err(crate::Error::io(&requested, error)),
    }
    let dir =
        std::fs::canonicalize(&requested).map_err(|error| crate::Error::io(&requested, error))?;
    if !dir.starts_with(&root) {
        return Err(crate::Error::Containment(requested));
    }
    Ok((root, dir))
}

fn entry_from_destination(entry: &Entry, dst: &Path, hash: bool) -> crate::Result<ManifestEntry> {
    let path = dst.join(&entry.rel);
    let meta = meta_min(&path)?;
    let (kind, target) = match entry.kind {
        EntryKind::File => (Kind::File, None),
        EntryKind::Dir => (Kind::Dir, None),
        EntryKind::Symlink(_) => (
            Kind::Symlink,
            Some(std::fs::read_link(&path).map_err(|error| crate::Error::io(&path, error))?),
        ),
    };
    let digest = if hash && kind == Kind::File {
        Some(hash_file(&path).map_err(|error| crate::Error::io(&path, error))?)
    } else {
        None
    };
    Ok(ManifestEntry {
        kind,
        size: meta.len,
        mtime_s: meta.mtime.unix_seconds(),
        mtime_ns: meta.mtime.nanoseconds(),
        ino: meta.ino,
        dev: meta.dev,
        mode: meta.mode,
        hash: digest,
        target,
    })
}

impl Manifest {
    /// Load a compatible snapshot and replay complete, valid journal records.
    #[must_use]
    pub fn load(dst: &Path) -> Option<Self> {
        let root = std::fs::canonicalize(dst).ok()?;
        let snapshot = manifest_path(&root);
        let parent = std::fs::canonicalize(snapshot.parent()?).ok()?;
        if !parent.starts_with(&root) {
            return None;
        }
        let bytes = std::fs::read(snapshot).ok()?;
        let mut manifest: Self = bincode::deserialize(&bytes).ok()?;
        if manifest.version != FORMAT_VERSION {
            return None;
        }
        manifest.replay_journal(&journal_path(&root));
        Some(manifest)
    }

    fn replay_journal(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        loop {
            let mut length = [0_u8; 4];
            if file.read_exact(&mut length).is_err() {
                break;
            }
            let length = u32::from_le_bytes(length) as usize;
            let mut expected = [0_u8; 32];
            if file.read_exact(&mut expected).is_err() {
                break;
            }
            let mut payload = vec![0_u8; length];
            if file.read_exact(&mut payload).is_err()
                || *blake3::hash(&payload).as_bytes() != expected
            {
                break;
            }
            let Ok(delta) = bincode::deserialize::<Delta>(&payload) else {
                break;
            };
            match delta {
                Delta::Upsert(path, entry) => {
                    self.entries.insert(path, entry);
                }
                Delta::Delete(path) => {
                    self.entries.remove(&path);
                }
            }
        }
    }

    /// Build a complete initial snapshot from the applied plan.
    ///
    /// # Errors
    ///
    /// Returns an error when a destination entry cannot be inspected or hashed.
    pub fn from_destination(plan: &SyncPlan, dst: &Path, hash: bool) -> crate::Result<Self> {
        let mut entries = HashMap::with_capacity(plan.actions.len());
        for planned in &plan.actions {
            entries.insert(
                planned.entry.rel.clone(),
                entry_from_destination(&planned.entry, dst, hash)?,
            );
        }
        Ok(Self {
            version: FORMAT_VERSION,
            entries,
        })
    }

    /// Persist an initial snapshot or append only changed/deleted records.
    ///
    /// # Errors
    ///
    /// Returns an error when destination metadata cannot be read or the
    /// snapshot/journal cannot be written safely.
    pub fn persist_after_plan(plan: &SyncPlan, dst: &Path, hash: bool) -> crate::Result<()> {
        let Some(mut manifest) = Self::load(dst) else {
            return Self::from_destination(plan, dst, hash)?.save_snapshot(dst);
        };
        let mut deltas = Vec::new();
        for planned in &plan.actions {
            if planned.action == Action::Skip {
                continue;
            }
            let record = entry_from_destination(&planned.entry, dst, hash)?;
            manifest
                .entries
                .insert(planned.entry.rel.clone(), record.clone());
            deltas.push(Delta::Upsert(planned.entry.rel.clone(), record));
        }
        for deletion in &plan.deletions {
            if manifest.entries.remove(&deletion.rel).is_some() {
                deltas.push(Delta::Delete(deletion.rel.clone()));
            }
        }
        Self::append(dst, &deltas)?;
        if Self::needs_compaction(dst) {
            manifest.save_snapshot(dst)?;
        }
        Ok(())
    }

    fn append(dst: &Path, deltas: &[Delta]) -> crate::Result<()> {
        if deltas.is_empty() {
            return Ok(());
        }
        let (root, dir) = metadata_dir(dst)?;
        let path = contained_target(&root, &dir.join(JOURNAL_FILE))?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| crate::Error::io(&path, error))?;
        for delta in deltas {
            let payload = bincode::serialize(delta)
                .map_err(|error| crate::Error::Pattern(format!("manifest journal: {error}")))?;
            let length = u32::try_from(payload.len())
                .map_err(|_| crate::Error::Pattern("manifest journal record too large".into()))?;
            file.write_all(&length.to_le_bytes())
                .and_then(|()| file.write_all(blake3::hash(&payload).as_bytes()))
                .and_then(|()| file.write_all(&payload))
                .map_err(|error| crate::Error::io(&path, error))?;
        }
        file.sync_data()
            .map_err(|error| crate::Error::io(&path, error))
    }

    fn needs_compaction(dst: &Path) -> bool {
        let journal = std::fs::metadata(journal_path(dst))
            .map(|meta| meta.len())
            .unwrap_or(0);
        let snapshot = std::fs::metadata(manifest_path(dst))
            .map(|meta| meta.len())
            .unwrap_or(0);
        journal > COMPACT_BYTES || (snapshot > 0 && journal > snapshot / 10)
    }

    /// Atomically replace the full snapshot and clear the journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the metadata directory is unsafe or the snapshot
    /// cannot be written and renamed.
    pub fn save(&self, dst: &Path) -> crate::Result<()> {
        self.save_snapshot(dst)
    }

    fn save_snapshot(&self, dst: &Path) -> crate::Result<()> {
        let (root, dir) = metadata_dir(dst)?;
        let bytes = bincode::serialize(self)
            .map_err(|error| crate::Error::Pattern(format!("manifest: {error}")))?;
        let tmp = dir.join(format!(".manifest-tmp-{:016x}", rand::random::<u64>()));
        std::fs::write(&tmp, bytes).map_err(|error| crate::Error::io(&tmp, error))?;
        let final_path = contained_target(&root, &dir.join(MANIFEST_FILE))?;
        if let Err(error) = std::fs::rename(&tmp, &final_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(crate::Error::io(&final_path, error));
        }
        let journal = contained_target(&root, &dir.join(JOURNAL_FILE))?;
        match std::fs::remove_file(&journal) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(crate::Error::io(&journal, error)),
        }
        Ok(())
    }

    /// Classify against recorded and live destination state.
    #[must_use]
    pub fn classify(&self, entry: &Entry, checksum: bool, src: &Path, dst: &Path) -> Action {
        let Some(recorded) = self.entries.get(&entry.rel) else {
            return Action::Copy;
        };
        let destination = dst.join(&entry.rel);
        match (&entry.kind, recorded.kind) {
            (EntryKind::Dir, Kind::Dir) => action_from_match(
                metadata_matches(entry, recorded)
                    && destination_matches(recorded, &destination, None),
            ),
            (EntryKind::Symlink(target), Kind::Symlink) => action_from_match(
                recorded.target.as_ref() == Some(target)
                    && metadata_matches(entry, recorded)
                    && destination_matches(recorded, &destination, Some(target)),
            ),
            (EntryKind::File, Kind::File) => {
                if !metadata_matches(entry, recorded)
                    || !destination_matches(recorded, &destination, None)
                {
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use foldhash::{HashMap, HashMapExt};

    use super::{Delta, Manifest, ManifestEntry};

    #[test]
    fn incomplete_journal_tail_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let dst = temp.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        Manifest {
            version: 3,
            entries: HashMap::new(),
        }
        .save(&dst)
        .unwrap();
        let journal = super::journal_path(&dst);
        std::fs::write(&journal, [12, 0, 0, 0, 1, 2, 3]).unwrap();
        assert!(Manifest::load(&dst).is_some());
        let _ = Delta::Delete("unused".into());
    }

    #[test]
    fn valid_journal_replays_and_old_versions_miss() {
        let temp = tempfile::tempdir().unwrap();
        let dst = temp.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        Manifest {
            version: 3,
            entries: HashMap::new(),
        }
        .save(&dst)
        .unwrap();
        let delta = Delta::Upsert(
            "file".into(),
            ManifestEntry {
                kind: super::Kind::File,
                size: 4,
                mtime_s: 1,
                mtime_ns: 2,
                ino: 3,
                dev: 4,
                mode: 0o100_644,
                hash: None,
                target: None,
            },
        );
        let payload = bincode::serialize(&delta).unwrap();
        let mut journal = std::fs::File::create(super::journal_path(&dst)).unwrap();
        journal
            .write_all(&u32::try_from(payload.len()).unwrap().to_le_bytes())
            .unwrap();
        journal
            .write_all(blake3::hash(&payload).as_bytes())
            .unwrap();
        journal.write_all(&payload).unwrap();
        assert!(
            Manifest::load(&dst)
                .unwrap()
                .entries
                .contains_key(Path::new("file"))
        );

        Manifest {
            version: 2,
            entries: HashMap::new(),
        }
        .save(&dst)
        .unwrap();
        assert!(Manifest::load(&dst).is_none());
    }

    #[test]
    fn oversized_journal_requests_compaction() {
        let temp = tempfile::tempdir().unwrap();
        let dst = temp.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let manifest = Manifest {
            version: 3,
            entries: HashMap::new(),
        };
        manifest.save(&dst).unwrap();
        let journal = std::fs::File::create(super::journal_path(&dst)).unwrap();
        journal.set_len(super::COMPACT_BYTES + 1).unwrap();
        assert!(Manifest::needs_compaction(&dst));
    }
}
