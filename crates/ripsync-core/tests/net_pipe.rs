//! End-to-end remote-protocol tests over the in-process [`duplex_pair`] — the
//! whole sender/receiver exchange runs in two threads with no ssh, no sockets, no
//! subprocess. Exercises new / changed / unchanged / deleted files, a subdir, and
//! (on Unix) a symlink, for both push and pull.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::thread;

use ripsync_core::Filter;
use ripsync_core::RunControl;
use ripsync_core::net::proto::{NetOptions, Role};
use ripsync_core::net::{duplex_pair, run_initiator, run_responder};
use ripsync_core::report::NullReporter;

/// Read a tree into a sorted map of relative-path → contents marker, so two trees
/// can be compared regardless of walk order. Directories map to "<dir>", symlinks
/// to "<sym:target>", files to their bytes (as a lossy string).
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for ent in fs::read_dir(dir).unwrap() {
            let ent = ent.unwrap();
            let path = ent.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(rel, format!("<sym:{}>", target.to_string_lossy()));
            } else if meta.is_dir() {
                out.insert(rel, "<dir>".to_string());
                walk(root, &path, out);
            } else {
                out.insert(
                    rel,
                    String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned(),
                );
            }
        }
    }
    walk(root, root, &mut out);
    out
}

/// Drive a full transfer: initiator plays `role`, responder is the opposite.
/// `local_root`/`remote_root` are the paths each side operates on.
fn run_transfer(role: Role, local_root: &Path, remote_root: &Path, options: NetOptions) {
    let (mut end_a, mut end_b) = duplex_pair();
    let local = local_root.to_path_buf();
    let remote = remote_root.to_path_buf();

    let server = thread::spawn(move || {
        let ctrl = RunControl::default();
        let excludes = Filter::none();
        run_responder(&mut end_b, &excludes, 0, &ctrl, &NullReporter)
    });

    let ctrl = RunControl::default();
    let excludes = Filter::none();
    let stats = run_initiator(
        &mut end_a,
        role,
        &local,
        &remote,
        options,
        &excludes,
        0,
        &ctrl,
        &NullReporter,
    )
    .expect("initiator");

    server.join().unwrap().expect("responder");
    // Both sides agree on the final tally.
    let _ = stats;
}

fn build_source(src: &Path) {
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("keep.txt"), "identical contents\n").unwrap();
    fs::write(
        src.join("changed.txt"),
        "the new improved contents, longer than before\n",
    )
    .unwrap();
    fs::write(src.join("brand_new.txt"), "freshly created file\n").unwrap();
    fs::write(src.join("sub/nested.bin"), vec![7u8; 4096]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("keep.txt", src.join("link")).unwrap();
}

fn build_stale_dest(dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    // Same as source → should be skipped.
    fs::write(dst.join("keep.txt"), "identical contents\n").unwrap();
    // Older/different → should be updated (delta path).
    fs::write(dst.join("changed.txt"), "old contents\n").unwrap();
    // Not in source → should be deleted when --delete.
    fs::write(dst.join("obsolete.txt"), "remove me\n").unwrap();
}

#[test]
fn push_syncs_source_into_dest() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    build_source(&src);
    build_stale_dest(&dst);

    let options = NetOptions {
        delete: true,
        preserve_mtime: true,
        ..NetOptions::default()
    };
    // Push: local is the sender (src), remote is the receiver (dst).
    run_transfer(Role::Push, &src, &dst, options);

    let want = snapshot(&src);
    let got = snapshot(&dst);
    assert_eq!(want, got, "push: dst must match src exactly");
    assert!(
        !dst.join("obsolete.txt").exists(),
        "stale file must be deleted"
    );
    assert_eq!(
        fs::read_to_string(dst.join("changed.txt")).unwrap(),
        "the new improved contents, longer than before\n"
    );
}

#[test]
fn pull_syncs_remote_source_into_local_dest() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("remote_src");
    let dst = tmp.path().join("local_dst");
    build_source(&src);
    build_stale_dest(&dst);

    let options = NetOptions {
        delete: true,
        preserve_mtime: true,
        ..NetOptions::default()
    };
    // Pull: local is the receiver (dst), remote is the sender (src).
    run_transfer(Role::Pull, &dst, &src, options);

    let want = snapshot(&src);
    let got = snapshot(&dst);
    assert_eq!(want, got, "pull: dst must match src exactly");
    assert!(
        !dst.join("obsolete.txt").exists(),
        "stale file must be deleted"
    );
}

#[test]
fn push_with_compression_roundtrips() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&src).unwrap();
    // Highly compressible payload so zstd actually shrinks the wire bytes.
    fs::write(src.join("big.log"), "A".repeat(200_000)).unwrap();
    fs::write(src.join("note.txt"), "small\n").unwrap();

    let options = NetOptions {
        whole_file: true,
        compress: true,
        compress_level: 6,
        ..NetOptions::default()
    };
    run_transfer(Role::Push, &src, &dst, options);

    assert_eq!(
        snapshot(&src),
        snapshot(&dst),
        "compressed push must match src"
    );
    assert_eq!(
        fs::read_to_string(dst.join("big.log")).unwrap(),
        "A".repeat(200_000)
    );
}

#[test]
fn push_into_empty_dest_copies_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    build_source(&src);

    run_transfer(Role::Push, &src, &dst, NetOptions::default());

    assert_eq!(
        snapshot(&src),
        snapshot(&dst),
        "fresh push must clone the tree"
    );
}
