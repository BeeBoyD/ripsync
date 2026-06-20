//! The `ripsync --server` peer: speaks the protocol over stdin/stdout.
//!
//! Spawned on the remote host by the initiator's ssh command. It plays the
//! opposite role to whatever the initiator asked for: for [`Role::Push`] it is
//! the receiver (writing into the remote destination), for [`Role::Pull`] it is
//! the sender (reading the remote source). Every path it touches is confined under
//! the negotiated root by the receiver's containment checks.

use crate::filter::Filter;
use crate::net::run_responder;
use crate::net::transport::IoDuplex;
use crate::report::{Reporter, Stats};
use crate::{Result, RunControl};

/// Run the server peer to completion, reading the protocol from stdin and writing
/// it to stdout. Diagnostics must go to stderr (never stdout, which is the wire).
///
/// # Errors
///
/// Returns a handshake, protocol, walk, or I/O error.
pub fn run_server<R: Reporter>(
    threads: usize,
    control: &RunControl,
    reporter: &R,
) -> Result<Stats> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut conn = IoDuplex::new(stdin.lock(), stdout.lock());
    // The server applies no local filter; the initiator filters its own walk.
    let filter = Filter::none();
    run_responder(&mut conn, &filter, threads, control, reporter)
}
