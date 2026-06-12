//! Differential parity vs real `rsync`.
//!
//! proptest generates random directory trees (nested dirs, varied files, a few
//! symlinks); we sync the same source into `dest_rsync` with `rsync -a` and into
//! `dest_ferry` with `ferry`, then assert the two destinations are identical in
//! structure, content, modes, mtimes (±1s), and symlink targets.
//!
//! If `rsync` is not installed the tests no-op (they print a note and pass).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use proptest::prelude::*;

/// One generated entry: a relative path and what lives there.
#[derive(Debug, Clone)]
enum Spec {
    File(Vec<u8>),
    Symlink(String),
}

fn rsync_available() -> bool {
    Command::new("rsync")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ferry_bin() -> PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for integration tests.
    PathBuf::from(env!("CARGO_BIN_EXE_ferry"))
}

/// A path component from a small alphabet, so collisions (and thus dir/file
/// conflicts to resolve) actually happen.
fn comp() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("a".to_string()),
        Just("b".to_string()),
        Just("c".to_string()),
        Just("sub".to_string()),
        Just("d".to_string()),
    ]
}

fn rel_path() -> impl Strategy<Value = PathBuf> {
    proptest::collection::vec(comp(), 1..4).prop_map(|parts| parts.iter().collect())
}

fn spec() -> impl Strategy<Value = Spec> {
    prop_oneof![
        8 => proptest::collection::vec(any::<u8>(), 0..2048).prop_map(Spec::File),
        1 => "a|b|sub/a|../oops".prop_map(|s| Spec::Symlink(s.to_string())),
    ]
}

/// Materialize a spec map into `root`, resolving dir/file conflicts by skipping.
/// Returns the set of paths actually created (so the generator is deterministic
/// per source, not per destination).
fn build_tree(root: &Path, entries: &BTreeMap<PathBuf, Spec>) {
    let mut files: Vec<&PathBuf> = Vec::new();
    'outer: for path in entries.keys() {
        // Skip if any ancestor was already created as a file, or this path is an
        // ancestor of an existing file (would be a dir).
        for existing in &files {
            if path.starts_with(existing) || existing.starts_with(path) {
                continue 'outer;
            }
        }
        files.push(path);
    }

    for path in files {
        let full = root.join(path);
        if let Some(parent) = full.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match &entries[path] {
            Spec::File(bytes) => {
                let _ = fs::write(&full, bytes);
            }
            Spec::Symlink(target) => {
                let _ = std::os::unix::fs::symlink(target, &full);
            }
        }
    }
}

/// Snapshot a tree as rel-path → comparable fingerprint.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    walk_into(root, root, &mut out);
    out
}

fn walk_into(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mode = meta.permissions().mode() & 0o7777;
        let mtime = filetime::FileTime::from_last_modification_time(&meta).unix_seconds();
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap_or_default();
            out.insert(rel, format!("symlink:{}", target.display()));
        } else if meta.is_dir() {
            out.insert(rel.clone(), format!("dir:mode={mode:o}:mtime={mtime}"));
            walk_into(root, &path, out);
        } else {
            let content = fs::read(&path).unwrap_or_default();
            let hash = blake3::hash(&content);
            out.insert(
                rel,
                format!("file:{}:mode={mode:o}:mtime={mtime}", hash.to_hex()),
            );
        }
    }
}

/// Compare two snapshots, allowing a ±1s mtime tolerance.
fn assert_identical(a: &BTreeMap<PathBuf, String>, b: &BTreeMap<PathBuf, String>) {
    let a_keys: Vec<_> = a.keys().collect();
    let b_keys: Vec<_> = b.keys().collect();
    assert_eq!(
        a_keys, b_keys,
        "different path sets\nrsync={a_keys:?}\nferry={b_keys:?}"
    );

    for (path, av) in a {
        let bv = &b[path];
        if av == bv {
            continue;
        }
        // Allow a 1-second mtime difference; everything else must match exactly.
        let (am, bm) = (strip_mtime(av), strip_mtime(bv));
        assert_eq!(
            am.0,
            bm.0,
            "mismatch at {}: rsync={av} ferry={bv}",
            path.display()
        );
        let dt = (am.1 - bm.1).abs();
        assert!(
            dt <= 1,
            "mtime drift >1s at {}: {av} vs {bv}",
            path.display()
        );
    }
}

/// Split a fingerprint into (everything-but-mtime, mtime).
fn strip_mtime(s: &str) -> (String, i64) {
    if let Some(idx) = s.find(":mtime=") {
        let (head, tail) = s.split_at(idx);
        let secs: i64 = tail.trim_start_matches(":mtime=").parse().unwrap_or(0);
        (head.to_string(), secs)
    } else {
        (s.to_string(), 0)
    }
}

fn run_rsync(src: &Path, dst: &Path, delete: bool) {
    fs::create_dir_all(dst).unwrap();
    let mut cmd = Command::new("rsync");
    cmd.arg("-a");
    if delete {
        cmd.arg("--delete");
    }
    // Trailing slash on src ⇒ copy contents into dst (matches ferry semantics).
    cmd.arg(format!("{}/", src.display())).arg(dst);
    let status = cmd.status().expect("spawn rsync");
    assert!(status.success(), "rsync failed");
}

fn run_ferry(src: &Path, dst: &Path, delete: bool) {
    let mut cmd = Command::new(ferry_bin());
    cmd.arg(src).arg(dst).arg("--no-tui").arg("-q");
    // Exercise a specific backend when requested (the harness must pass on both).
    if let Ok(backend) = std::env::var("FERRY_TEST_BACKEND") {
        cmd.arg("--backend").arg(backend);
    }
    if delete {
        cmd.arg("--delete").arg("--yes");
    }
    let status = cmd.status().expect("spawn ferry");
    assert!(status.success(), "ferry failed");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn parity_initial_sync(
        entries in proptest::collection::btree_map(rel_path(), spec(), 0..20),
    ) {
        if !rsync_available() {
            return Ok(());
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        build_tree(&src, &entries);

        let dst_r = tmp.path().join("dst_rsync");
        let dst_f = tmp.path().join("dst_ferry");
        run_rsync(&src, &dst_r, false);
        run_ferry(&src, &dst_f, false);

        assert_identical(&snapshot(&dst_r), &snapshot(&dst_f));
    }

    #[test]
    fn parity_with_delete_after_mutation(
        entries in proptest::collection::btree_map(rel_path(), spec(), 1..20),
        stale in proptest::collection::vec(rel_path(), 1..6),
    ) {
        if !rsync_available() {
            return Ok(());
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        build_tree(&src, &entries);

        let dst_r = tmp.path().join("dst_rsync");
        let dst_f = tmp.path().join("dst_ferry");

        // Seed both destinations with identical stale content to be deleted.
        for dst in [&dst_r, &dst_f] {
            for rel in &stale {
                let p = dst.join(rel);
                if let Some(parent) = p.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(&p, b"stale");
            }
        }

        run_rsync(&src, &dst_r, true);
        run_ferry(&src, &dst_f, true);

        assert_identical(&snapshot(&dst_r), &snapshot(&dst_f));
    }
}
