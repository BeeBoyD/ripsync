//! Cross-process remote-protocol test: spawn the real `ripsync --server` binary
//! and drive it over OS pipes with the core initiator. This validates the
//! `--server` argv intercept, the server dispatch, and the framing over actual
//! pipes (not the in-process channel used by the core's `net_pipe` test).

use std::fs;
use std::process::{Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use globset::GlobSet;
use ripsync_core::RunControl;
use ripsync_core::net::proto::{NetOptions, Role};
use ripsync_core::net::run_initiator;
use ripsync_core::net::transport::IoDuplex;
use ripsync_core::report::NullReporter;

#[test]
fn server_binary_push_over_pipes() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir_all(src.join("a")).unwrap();
    fs::write(src.join("a/x.txt"), "hello over a real pipe\n").unwrap();
    fs::write(src.join("top.bin"), vec![3u8; 2048]).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // Spawn the actual binary as the far-end peer.
    let mut child = Command::cargo_bin("ripsync")
        .unwrap()
        .arg("--server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn ripsync --server");

    let stdout = child.stdout.take().unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut conn = IoDuplex::new(stdout, stdin);

    let ctrl = RunControl::default();
    let excludes = GlobSet::empty();
    let options = NetOptions {
        preserve_mtime: true,
        ..NetOptions::default()
    };
    let stats = run_initiator(
        &mut conn,
        Role::Push,
        &src,
        &dst,
        options,
        &excludes,
        0,
        &ctrl,
        &NullReporter,
    )
    .expect("push to ripsync --server");

    drop(conn);
    let _ = child.wait();

    assert_eq!(stats.errors, 0, "transfer reported errors");
    assert_eq!(
        fs::read_to_string(dst.join("a/x.txt")).unwrap(),
        "hello over a real pipe\n"
    );
    assert_eq!(fs::read(dst.join("top.bin")).unwrap(), vec![3u8; 2048]);
}
