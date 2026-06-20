//! Wire types for the ripsync↔ripsync remote-sync protocol.
//!
//! Both peers are ripsync; the protocol is **not** wire-compatible with real
//! rsync. Control messages are bincode-serialized inside length-prefixed frames
//! (see [`super::transport`]). The flow is **receiver-driven lock-step**: the
//! side that holds the *source* ("sender") streams its file list, then becomes
//! purely reactive — it answers one [`Request`] with one [`Data`] response at a
//! time. The side that holds the *destination* ("receiver") drives: it requests
//! the content it needs, applies each response, and finally sends [`Msg::Finished`]
//! to terminate. Because exactly one side is ever waiting to read while the other
//! computes-then-writes, the single duplex stream cannot deadlock.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::delta::{Delta, Signature};
use crate::report::Stats;

/// Magic prefix sent raw (unframed) at the very start of a connection.
pub const MAGIC: &[u8; 7] = b"ripsync";

/// Protocol version. Bump on any incompatible wire change.
pub const PROTO_VERSION: u32 = 1;

/// Which direction the *local* (initiating) side asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Local → remote: local is the sender, the remote `--server` is the receiver.
    Push,
    /// Remote → local: the remote `--server` is the sender, local is the receiver.
    Pull,
}

/// Options that affect how the transfer is performed, negotiated once.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct NetOptions {
    /// Mirror deletions (remove destination entries absent from the source).
    pub delete: bool,
    /// Compare files by content hash rather than size+mtime.
    pub checksum: bool,
    /// Preserve Unix permission bits.
    pub preserve_mode: bool,
    /// Preserve modification times.
    pub preserve_mtime: bool,
    /// Always transfer whole files; never compute a delta.
    pub whole_file: bool,
    /// Compress whole-file payloads on the wire with zstd.
    pub compress: bool,
    /// zstd compression level (1–22) used when `compress` is set.
    pub compress_level: i32,
}

/// The first framed message the initiator sends after the raw handshake: it tells
/// the responder which role to play and over which root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Init {
    /// The local side's intent.
    pub role: Role,
    /// The remote root path (destination for [`Role::Push`], source for [`Role::Pull`]).
    pub root: PathBuf,
    /// Negotiated transfer options.
    pub options: NetOptions,
}

/// What a listed entry is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symlink with the given verbatim target.
    Symlink(PathBuf),
}

/// One entry in the sender's file list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetEntry {
    /// Path relative to the transfer root (never contains `..`).
    pub rel: PathBuf,
    /// Kind of entry.
    pub kind: NetKind,
    /// Byte length (0 for dirs/symlinks).
    pub len: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime_s: i64,
    /// Sub-second part of the modification time, in nanoseconds.
    pub mtime_ns: u32,
    /// Unix permission+type bits (0 where unavailable).
    pub mode: u32,
}

/// Receiver → sender: the content the receiver needs for one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Send the whole file at this relative path.
    Whole(PathBuf),
    /// Send a delta of this file against the supplied signature of the receiver's
    /// existing copy.
    Delta(PathBuf, Signature),
}

/// Sender → receiver: the response to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Data {
    /// The whole file contents. `zstd` is true when `bytes` is zstd-compressed.
    Whole {
        /// File bytes (possibly compressed).
        bytes: Vec<u8>,
        /// Whether `bytes` is zstd-compressed.
        zstd: bool,
    },
    /// A delta to apply to the receiver's existing copy. Deltas are mostly small
    /// literal runs already, so they are not compressed.
    Delta(Delta),
    /// The sender could not read the file (it vanished or is unreadable); the
    /// receiver records a per-entry error and moves on.
    NotFound,
}

/// Every framed message on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Msg {
    /// Responder → initiator: version check result.
    ServerHello {
        /// Responder's protocol version.
        version: u32,
        /// Whether the responder accepts the connection.
        ok: bool,
    },
    /// Initiator → responder: role, root, options.
    Init(Init),
    /// Responder → initiator: handshake complete.
    InitAck,
    /// Sender → receiver: one file-list entry.
    Entry(NetEntry),
    /// Sender → receiver: the file list is complete.
    ListDone,
    /// Receiver → sender: request content for one file.
    Request(Request),
    /// Sender → receiver: content for the requested file.
    Data(Data),
    /// Receiver → sender: terminal message carrying the final tally.
    Finished(Stats),
    /// Either side: a fatal error; the peer should abort.
    Error(String),
}
