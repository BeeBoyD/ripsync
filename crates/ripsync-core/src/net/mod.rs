//! Remote sync over a single duplex byte stream (ssh, or an in-process pipe).
//!
//! Both peers are ripsync. One side is the **initiator** (the locally-invoked
//! `ripsync`, which spawns `ssh host ripsync --server`); the other is the
//! **responder** (`ripsync --server`). After a versioned handshake the connection
//! carries the receiver-driven lock-step protocol described in [`proto`].
//!
//! Role mapping:
//! * [`proto::Role::Push`] — local → remote: local is the **sender**, the remote
//!   server is the **receiver**.
//! * [`proto::Role::Pull`] — remote → local: the remote server is the sender,
//!   local is the receiver.

pub mod proto;
pub mod receiver;
pub mod sender;
pub mod server;
pub mod transport;

use std::io::{Read, Write};
use std::path::Path;

use crate::filter::Filter;
use crate::net::proto::{Init, Msg, NetOptions, PROTO_VERSION, Role};
use crate::net::transport::{recv_msg, send_msg};
use crate::report::{Reporter, Stats};
use crate::{Error, Result, RunControl};

pub use proto::MAGIC;
pub use receiver::run_receiver;
pub use sender::run_sender;
pub use transport::{IoDuplex, duplex_pair};

/// Perform the initiator side of the handshake: send magic+version, check the
/// server's hello, send [`Init`], await the ack.
fn handshake_initiator<C: Read + Write>(
    conn: &mut C,
    role: Role,
    root: &Path,
    options: NetOptions,
) -> Result<()> {
    let mut hello = Vec::with_capacity(11);
    hello.extend_from_slice(MAGIC);
    hello.extend_from_slice(&PROTO_VERSION.to_be_bytes());
    conn.write_all(&hello)?;
    conn.flush()?;

    match recv_msg(conn)? {
        Msg::ServerHello { version, ok } => {
            if !ok || version != PROTO_VERSION {
                return Err(Error::Protocol(format!(
                    "remote ripsync protocol v{version} is incompatible (need v{PROTO_VERSION})"
                )));
            }
        }
        other => {
            return Err(Error::Protocol(format!(
                "expected ServerHello, got {other:?}"
            )));
        }
    }

    send_msg(
        conn,
        &Msg::Init(Init {
            role,
            root: root.to_path_buf(),
            options,
        }),
    )?;
    match recv_msg(conn)? {
        Msg::InitAck => Ok(()),
        other => Err(Error::Protocol(format!("expected InitAck, got {other:?}"))),
    }
}

/// Perform the responder side of the handshake: validate magic+version, send the
/// hello, read [`Init`], ack it.
pub(crate) fn handshake_responder<C: Read + Write>(conn: &mut C) -> Result<Init> {
    let mut buf = [0u8; 11];
    conn.read_exact(&mut buf)?;
    let magic_ok = &buf[..7] == MAGIC.as_slice();
    let version = u32::from_be_bytes([buf[7], buf[8], buf[9], buf[10]]);
    let ok = magic_ok && version == PROTO_VERSION;

    send_msg(
        conn,
        &Msg::ServerHello {
            version: PROTO_VERSION,
            ok,
        },
    )?;
    if !ok {
        return Err(Error::Protocol(format!(
            "bad handshake (magic_ok={magic_ok}, version={version})"
        )));
    }

    match recv_msg(conn)? {
        Msg::Init(init) => {
            send_msg(conn, &Msg::InitAck)?;
            Ok(init)
        }
        other => Err(Error::Protocol(format!("expected Init, got {other:?}"))),
    }
}

/// Drive the responder side over an established connection: perform the
/// responder handshake, then play the opposite role to the initiator. Used by
/// `ripsync --server` (over stdio) and by tests (over an in-process pipe).
///
/// # Errors
///
/// Returns a handshake, protocol, walk, or I/O error.
pub fn run_responder<C: Read + Write, R: Reporter>(
    conn: &mut C,
    filter: &Filter,
    threads: usize,
    control: &RunControl,
    reporter: &R,
) -> Result<Stats> {
    let init = handshake_responder(conn)?;
    match init.role {
        // Initiator pushed to us: we are the receiver.
        Role::Push => run_receiver(
            conn,
            &init.root,
            filter,
            init.options,
            threads,
            control,
            reporter,
        ),
        // Initiator pulled from us: we are the sender.
        Role::Pull => run_sender(conn, &init.root, filter, init.options, threads, control),
    }
}

/// Drive a remote sync from the local (initiator) side over an established
/// connection. `local_root` is the local path; `remote_root` is the path on the
/// peer. Returns the final [`Stats`] (for push, these come back from the remote
/// receiver via [`Msg::Finished`]).
///
/// # Errors
///
/// Returns a handshake, protocol, walk, or I/O error.
#[allow(clippy::too_many_arguments)]
pub fn run_initiator<C: Read + Write, R: Reporter>(
    conn: &mut C,
    role: Role,
    local_root: &Path,
    remote_root: &Path,
    options: NetOptions,
    filter: &Filter,
    threads: usize,
    control: &RunControl,
    reporter: &R,
) -> Result<Stats> {
    handshake_initiator(conn, role, remote_root, options)?;
    match role {
        Role::Push => run_sender(conn, local_root, filter, options, threads, control),
        Role::Pull => run_receiver(
            conn, local_root, filter, options, threads, control, reporter,
        ),
    }
}
