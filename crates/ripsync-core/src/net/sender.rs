//! The **sender**: the side that holds the source tree.
//!
//! It streams its file list, then answers the receiver's [`Request`]s one at a
//! time until the receiver sends [`Msg::Finished`]. It never initiates a read on
//! its own — it is purely reactive — which is what keeps the single duplex stream
//! deadlock-free.

use std::io::{Read, Write};
use std::path::Path;

use crate::delta::encode_with_signature;
use crate::filter::Filter;
use crate::net::proto::{Data, Msg, NetEntry, NetKind, NetOptions, Request};
use crate::net::transport::{recv_msg, send_msg};
use crate::report::Stats;
use crate::walk::{EntryKind, walk_controlled};
use crate::{Error, Result, RunControl};

/// Build a whole-file [`Data`] response, compressing with zstd when negotiated.
fn whole_data(raw: Vec<u8>, options: NetOptions) -> Data {
    if options.compress {
        if let Ok(packed) = zstd::encode_all(raw.as_slice(), options.compress_level) {
            return Data::Whole {
                bytes: packed,
                zstd: true,
            };
        }
    }
    Data::Whole {
        bytes: raw,
        zstd: false,
    }
}

/// Run the sender half over `conn` for the source tree at `root`.
///
/// # Errors
///
/// Returns a walk, protocol, or I/O error, or [`Error::Cancelled`] on cancellation.
pub fn run_sender<C: Read + Write>(
    conn: &mut C,
    root: &Path,
    filter: &Filter,
    options: NetOptions,
    threads: usize,
    control: &RunControl,
) -> Result<Stats> {
    let entries = walk_controlled(root, threads, filter, control)?;

    for entry in &entries {
        control.checkpoint()?;
        let kind = match &entry.kind {
            EntryKind::Dir => NetKind::Dir,
            EntryKind::File => NetKind::File,
            EntryKind::Symlink(target) => NetKind::Symlink(target.clone()),
        };
        let net = NetEntry {
            rel: entry.rel.clone(),
            kind,
            len: entry.len,
            mtime_s: entry.mtime.unix_seconds(),
            mtime_ns: entry.mtime.nanoseconds(),
            mode: entry.mode,
        };
        send_msg(conn, &Msg::Entry(net))?;
    }
    send_msg(conn, &Msg::ListDone)?;

    // Reactive loop: one request → one response, until the receiver finishes.
    loop {
        control.checkpoint()?;
        match recv_msg(conn)? {
            Msg::Request(Request::Whole(rel)) => {
                let data = match std::fs::read(root.join(&rel)) {
                    Ok(bytes) => whole_data(bytes, options),
                    Err(_) => Data::NotFound,
                };
                send_msg(conn, &Msg::Data(data))?;
            }
            Msg::Request(Request::Delta(rel, sig)) => {
                let data = match std::fs::read(root.join(&rel)) {
                    Ok(bytes) => Data::Delta(encode_with_signature(&sig, &bytes)),
                    Err(_) => Data::NotFound,
                };
                send_msg(conn, &Msg::Data(data))?;
            }
            Msg::Finished(stats) => return Ok(stats),
            Msg::Error(e) => return Err(Error::Protocol(format!("peer error: {e}"))),
            other => {
                return Err(Error::Protocol(format!(
                    "sender: unexpected message {other:?}"
                )));
            }
        }
    }
}
