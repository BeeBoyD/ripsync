//! The **receiver**: the side that holds the destination tree.
//!
//! It reads the sender's file list, creates directories and symlinks directly,
//! and for each file decides skip / whole / delta. It drives the exchange — one
//! [`Request`] then one [`Data`] response at a time — applies each result with a
//! containment-checked atomic write, mirrors deletions, and finally sends
//! [`Msg::Finished`] to end the session.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use filetime::FileTime;
use globset::GlobSet;

use crate::delta::{Signature, apply};
use crate::meta::{
    canonical_root, check_relative, contained_target, set_mode, set_mtime, set_symlink_mtime,
};
use crate::net::proto::{Data, Msg, NetEntry, NetKind, NetOptions, Request};
use crate::net::transport::{recv_msg, send_msg};
use crate::plan::Action;
use crate::report::{Event, Reporter, RunPhase, Stats};
use crate::walk::{Entry, walk_controlled};
use crate::{Error, Result, RunControl};

/// Run the receiver half over `conn` for the destination tree at `root`.
///
/// # Errors
///
/// Returns a walk, protocol, or I/O error, or [`Error::Cancelled`] on cancellation.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn run_receiver<C: Read + Write, R: Reporter>(
    conn: &mut C,
    root: &Path,
    excludes: &GlobSet,
    options: NetOptions,
    threads: usize,
    control: &RunControl,
    reporter: &R,
) -> Result<Stats> {
    reporter.event(Event::Phase(RunPhase::Copying));
    let root_canon = canonical_root(root)?;

    // 1. Receive the sender's full file list (parents precede children).
    let mut src_entries: Vec<NetEntry> = Vec::new();
    loop {
        control.checkpoint()?;
        match recv_msg(conn)? {
            Msg::Entry(e) => src_entries.push(e),
            Msg::ListDone => break,
            Msg::Error(e) => return Err(Error::Protocol(format!("peer error: {e}"))),
            other => {
                return Err(Error::Protocol(format!(
                    "receiver: expected entry/list-done, got {other:?}"
                )));
            }
        }
    }

    // 2. Walk our own tree once for skip and delete decisions.
    let existing = walk_controlled(root, threads, excludes, control)?;
    let existing_map: HashMap<PathBuf, Entry> = existing
        .iter()
        .map(|e| (e.rel.clone(), e.clone()))
        .collect();
    let src_set: HashSet<&PathBuf> = src_entries.iter().map(|e| &e.rel).collect();

    let mut stats = Stats::default();

    // 3. Apply every source entry, in order.
    for e in &src_entries {
        control.checkpoint()?;
        if check_relative(&e.rel).is_err() {
            fail(
                reporter,
                &mut stats,
                &e.rel,
                "path escapes destination root",
            );
            continue;
        }
        let mtime = FileTime::from_unix_time(e.mtime_s, e.mtime_ns);
        let existing_entry = existing_map.get(&e.rel);
        match &e.kind {
            NetKind::Dir => {
                let full = root_canon.join(&e.rel);
                if let Err(err) = std::fs::create_dir_all(&full) {
                    fail(reporter, &mut stats, &e.rel, &err.to_string());
                    continue;
                }
                apply_meta(&full, e.mode, mtime, options, false);
                let action = bump(&mut stats, existing_entry.is_some());
                reporter.event(Event::DirDone {
                    rel: e.rel.clone(),
                    action,
                });
            }
            NetKind::Symlink(target) => {
                let dst = match ensure_and_contain(&root_canon, &e.rel) {
                    Ok(p) => p,
                    Err(err) => {
                        fail(reporter, &mut stats, &e.rel, &err.to_string());
                        continue;
                    }
                };
                let _ = std::fs::remove_file(&dst);
                if let Err(err) = symlink_create(target, &dst) {
                    fail(reporter, &mut stats, &e.rel, &err.to_string());
                    continue;
                }
                if options.preserve_mtime {
                    let _ = set_symlink_mtime(&dst, mtime);
                }
                let action = bump(&mut stats, existing_entry.is_some());
                reporter.event(Event::SymlinkDone {
                    rel: e.rel.clone(),
                    action,
                });
            }
            NetKind::File => {
                let is_existing_file = existing_entry.is_some_and(Entry::is_file);
                let unchanged = !options.checksum
                    && is_existing_file
                    && existing_entry
                        .is_some_and(|x| x.len == e.len && x.mtime.unix_seconds() == e.mtime_s);
                if unchanged {
                    stats.skipped += 1;
                    reporter.event(Event::Skipped { rel: e.rel.clone() });
                    continue;
                }
                let dst = match ensure_and_contain(&root_canon, &e.rel) {
                    Ok(p) => p,
                    Err(err) => {
                        fail(reporter, &mut stats, &e.rel, &err.to_string());
                        continue;
                    }
                };
                let Some(new_bytes) = fetch_bytes(conn, e, &dst, is_existing_file, options)? else {
                    fail(
                        reporter,
                        &mut stats,
                        &e.rel,
                        "source unreadable or delta failed",
                    );
                    continue;
                };
                if let Err(err) = write_atomic(&dst, &new_bytes, e.mode, mtime, options) {
                    fail(reporter, &mut stats, &e.rel, &err.to_string());
                    continue;
                }
                stats.bytes += new_bytes.len() as u64;
                let action = bump(&mut stats, is_existing_file);
                reporter.event(Event::FileDone {
                    rel: e.rel.clone(),
                    action,
                    bytes: new_bytes.len() as u64,
                });
            }
        }
    }

    // 4. Mirror deletions (deepest paths first so directories empty before removal).
    if options.delete {
        let mut victims: Vec<&Entry> = existing
            .iter()
            .filter(|x| !src_set.contains(&x.rel))
            .collect();
        victims.sort_by(|a, b| b.rel.cmp(&a.rel));
        for victim in victims {
            control.checkpoint()?;
            let full = root_canon.join(&victim.rel);
            let removed = if victim.is_dir() {
                std::fs::remove_dir(&full).or_else(|_| std::fs::remove_dir_all(&full))
            } else {
                std::fs::remove_file(&full)
            };
            if removed.is_ok() {
                stats.deleted += 1;
                reporter.event(Event::Deleted {
                    rel: victim.rel.clone(),
                });
            }
        }
    }

    // 5. Terminate the session: the sender returns these stats to its caller.
    send_msg(conn, &Msg::Finished(stats.clone()))?;
    Ok(stats)
}

/// Request and receive the new contents for one file: a delta against the local
/// copy when possible, otherwise the whole file. Returns `None` if the sender
/// could not supply it or the delta failed to apply.
fn fetch_bytes<C: Read + Write>(
    conn: &mut C,
    e: &NetEntry,
    dst: &Path,
    is_existing_file: bool,
    options: NetOptions,
) -> Result<Option<Vec<u8>>> {
    let use_delta = !options.whole_file && is_existing_file;
    if use_delta {
        let old = std::fs::read(dst).unwrap_or_default();
        let sig = Signature::compute(&old, None);
        send_msg(conn, &Msg::Request(Request::Delta(e.rel.clone(), sig)))?;
        match recv_msg(conn)? {
            Msg::Data(Data::Delta(d)) => Ok(apply(&old, &d).ok()),
            Msg::Data(Data::Whole { bytes, zstd }) => Ok(unpack(bytes, zstd)),
            Msg::Data(Data::NotFound) => Ok(None),
            Msg::Error(e) => Err(Error::Protocol(format!("peer error: {e}"))),
            other => Err(Error::Protocol(format!("receiver: unexpected {other:?}"))),
        }
    } else {
        send_msg(conn, &Msg::Request(Request::Whole(e.rel.clone())))?;
        match recv_msg(conn)? {
            Msg::Data(Data::Whole { bytes, zstd }) => Ok(unpack(bytes, zstd)),
            Msg::Data(Data::NotFound) => Ok(None),
            Msg::Error(e) => Err(Error::Protocol(format!("peer error: {e}"))),
            other => Err(Error::Protocol(format!("receiver: unexpected {other:?}"))),
        }
    }
}

/// Decode a whole-file payload, decompressing when it was zstd-packed. Returns
/// `None` if decompression fails (treated as a per-entry error upstream).
fn unpack(bytes: Vec<u8>, zstd: bool) -> Option<Vec<u8>> {
    if zstd {
        zstd::decode_all(bytes.as_slice()).ok()
    } else {
        Some(bytes)
    }
}

/// Report a per-entry failure and bump the error tally; the run continues.
fn fail<R: Reporter>(reporter: &R, stats: &mut Stats, rel: &Path, error: &str) {
    stats.errors += 1;
    reporter.event(Event::Failed {
        rel: rel.to_path_buf(),
        error: error.to_string(),
    });
}

/// Count a created/updated entry and return the action taken.
fn bump(stats: &mut Stats, existed: bool) -> Action {
    if existed {
        stats.updated += 1;
        Action::Update
    } else {
        stats.copied += 1;
        Action::Copy
    }
}

/// Apply mode/mtime to a just-written path, honoring the preserve options.
fn apply_meta(path: &Path, mode: u32, mtime: FileTime, options: NetOptions, _is_file: bool) {
    if options.preserve_mode {
        let _ = set_mode(path, mode);
    }
    if options.preserve_mtime {
        let _ = set_mtime(path, mtime);
    }
}

/// Ensure the parent directory of `rel` exists, then return the containment-checked
/// concrete path to write.
fn ensure_and_contain(root_canon: &Path, rel: &Path) -> Result<PathBuf> {
    let target = root_canon.join(rel);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    contained_target(root_canon, &target)
}

/// Write `bytes` to a temp file beside `dst`, set metadata, then atomically rename.
fn write_atomic(
    dst: &Path,
    bytes: &[u8],
    mode: u32,
    mtime: FileTime,
    options: NetOptions,
) -> Result<()> {
    let parent = dst
        .parent()
        .ok_or_else(|| Error::Containment(dst.to_path_buf()))?;
    let tmp = parent.join(format!(".ripsync.tmp.{:016x}", rand::random::<u64>()));
    std::fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
    apply_meta(&tmp, mode, mtime, options, true);
    if let Err(e) = atomic_replace(&tmp, dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(dst, e));
    }
    Ok(())
}

/// Atomically replace `dst` with `tmp`.
#[cfg(not(windows))]
fn atomic_replace(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(tmp, dst)
}

#[cfg(windows)]
fn atomic_replace(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    crate::io::windows::replace_file(tmp, dst)
}

/// Create a symlink at `link` pointing to `target` (verbatim).
#[cfg(unix)]
fn symlink_create(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_create(target: &Path, link: &Path) -> std::io::Result<()> {
    // Best effort; needs Developer Mode or privilege. Treat the link as a file link.
    std::os::windows::fs::symlink_file(target, link)
}
