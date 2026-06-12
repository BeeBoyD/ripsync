//! CLI integration tests (assert_cmd + tempfile).

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt};

fn ferry() -> Command {
    Command::cargo_bin("ferry").expect("binary builds")
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn basic_sync_mirrors_tree() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("a.txt"), "alpha");
    write(&src.join("nested/b.txt"), "beta");

    ferry()
        .args([src.to_str().unwrap(), dst.to_str().unwrap(), "--no-tui"])
        .assert()
        .success();

    assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "alpha");
    assert_eq!(
        fs::read_to_string(dst.join("nested/b.txt")).unwrap(),
        "beta"
    );
}

#[test]
fn dry_run_changes_nothing() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("a.txt"), "alpha");

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--dry-run",
        ])
        .assert()
        .success();

    assert!(!dst.exists(), "dry-run must not create the destination");
}

#[test]
fn delete_without_yes_deletes_nothing() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("keep.txt"), "keep");
    write(&dst.join("keep.txt"), "keep");
    write(&dst.join("stale.txt"), "stale");

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--delete",
        ])
        .assert()
        .success();

    assert!(
        dst.join("stale.txt").exists(),
        "must not delete without --yes"
    );
}

#[test]
fn delete_with_yes_removes_stale() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("keep.txt"), "keep");
    write(&dst.join("keep.txt"), "keep");
    write(&dst.join("stale.txt"), "stale");

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--delete",
            "--yes",
        ])
        .assert()
        .success();

    assert!(
        !dst.join("stale.txt").exists(),
        "stale file must be deleted"
    );
    assert!(dst.join("keep.txt").exists());
}

#[test]
fn empty_source_with_delete_aborts() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&src).unwrap();
    write(&dst.join("precious.txt"), "do not lose me");

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--delete",
            "--yes",
        ])
        .assert()
        .failure();

    assert!(
        dst.join("precious.txt").exists(),
        "must not wipe dest from empty source"
    );
}

#[test]
fn exclude_skips_matching_files() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("keep.txt"), "keep");
    write(&src.join("skip.log"), "log");
    write(&src.join("nested/deep.log"), "deep log");

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--exclude",
            "*.log",
        ])
        .assert()
        .success();

    assert!(dst.join("keep.txt").exists());
    assert!(!dst.join("skip.log").exists(), "top-level *.log excluded");
    assert!(
        !dst.join("nested/deep.log").exists(),
        "nested *.log excluded"
    );
}

#[test]
fn json_output_is_valid() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("a.txt"), "alpha");

    let out = ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--output",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(parsed["summary"]["copied"], 1);
}

#[test]
fn index_detects_destination_modified_after_sync() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("data.txt"), "source");

    ferry()
        .args([src.to_str().unwrap(), dst.to_str().unwrap(), "--no-tui"])
        .assert()
        .success();
    write(&dst.join("data.txt"), "tampered destination");

    ferry()
        .args([src.to_str().unwrap(), dst.to_str().unwrap(), "--no-tui"])
        .assert()
        .success();

    assert_eq!(fs::read_to_string(dst.join("data.txt")).unwrap(), "source");
}

#[test]
fn corrupt_index_falls_back_to_full_scan() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("data.txt"), "first");

    ferry()
        .args([src.to_str().unwrap(), dst.to_str().unwrap(), "--no-tui"])
        .assert()
        .success();
    write(&dst.join(".ferry/manifest.bin"), "not a manifest");
    write(&src.join("data.txt"), "second version");

    ferry()
        .args([src.to_str().unwrap(), dst.to_str().unwrap(), "--no-tui"])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(dst.join("data.txt")).unwrap(),
        "second version"
    );
}

#[cfg(unix)]
#[test]
fn hard_links_preserve_and_repair_inode_groups() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("original"), "shared");
    fs::hard_link(src.join("original"), src.join("alias")).unwrap();

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--hard-links",
        ])
        .assert()
        .success();
    assert_eq!(
        fs::metadata(dst.join("original")).unwrap().ino(),
        fs::metadata(dst.join("alias")).unwrap().ino()
    );

    fs::remove_file(dst.join("alias")).unwrap();
    write(&dst.join("alias"), "shared");
    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--hard-links",
        ])
        .assert()
        .success();
    assert_eq!(
        fs::metadata(dst.join("original")).unwrap().ino(),
        fs::metadata(dst.join("alias")).unwrap().ino()
    );
}

#[cfg(unix)]
#[test]
fn sparse_copy_preserves_holes() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&src).unwrap();
    let source = fs::File::create(src.join("sparse.bin")).unwrap();
    source.set_len(64 * 1024 * 1024).unwrap();
    source.write_at(b"start", 0).unwrap();
    source.write_at(b"end", 64 * 1024 * 1024 - 3).unwrap();

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--sparse",
            "--reflink",
            "never",
            "--backend",
            "portable",
        ])
        .assert()
        .success();

    let source_meta = fs::metadata(src.join("sparse.bin")).unwrap();
    let dest_meta = fs::metadata(dst.join("sparse.bin")).unwrap();
    assert_eq!(source_meta.len(), dest_meta.len());
    assert!(
        dest_meta.blocks() * 512 < dest_meta.len() / 2,
        "destination should remain sparse"
    );
}

#[cfg(unix)]
#[test]
fn xattrs_round_trip_when_supported() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    write(&src.join("data"), "content");
    if xattr::set(src.join("data"), "user.ferry-test", b"value").is_err() {
        return;
    }

    ferry()
        .args([
            src.to_str().unwrap(),
            dst.to_str().unwrap(),
            "--no-tui",
            "--xattrs",
        ])
        .assert()
        .success();

    assert_eq!(
        xattr::get(dst.join("data"), "user.ferry-test").unwrap(),
        Some(b"value".to_vec())
    );
}
