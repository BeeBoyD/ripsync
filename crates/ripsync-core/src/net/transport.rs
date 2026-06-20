//! Framing and byte-stream plumbing for the remote protocol.
//!
//! A *connection* is any value that is both [`Read`] and [`Write`]: an
//! [`IoDuplex`] over an ssh child's stdout/stdin, over the server's stdin/stdout,
//! or over the in-process [`duplex_pair`] used by tests and the local self-test.
//!
//! Messages are framed as `[u64 little-endian length][bincode bytes]`. A 4 GiB
//! frame cap bounds peak allocation (whole-file transfers above that are out of
//! scope for v1 — the delta path keeps large mostly-unchanged files cheap).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};

use crate::net::proto::Msg;
use crate::{Error, Result};

/// Maximum accepted frame length (4 GiB). Guards against a corrupt or hostile
/// length prefix forcing an unbounded allocation.
pub const MAX_FRAME: u64 = 4 * 1024 * 1024 * 1024;

/// Serialize and write one framed message, flushing the stream.
///
/// # Errors
///
/// Returns [`Error::Protocol`] if the message cannot be serialized or exceeds
/// [`MAX_FRAME`], or an I/O error if the write fails.
pub fn send_msg<W: Write>(w: &mut W, msg: &Msg) -> Result<()> {
    let bytes = bincode::serialize(msg).map_err(|e| Error::Protocol(format!("serialize: {e}")))?;
    let len = bytes.len() as u64;
    if len > MAX_FRAME {
        return Err(Error::Protocol(format!("frame too large: {len} bytes")));
    }
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

/// Read one framed message.
///
/// # Errors
///
/// Returns [`Error::Protocol`] on a bad/oversized frame or a deserialization
/// failure, or an I/O error (including EOF mid-frame) if the read fails.
pub fn recv_msg<R: Read>(r: &mut R) -> Result<Msg> {
    let mut len_buf = [0u8; 8];
    r.read_exact(&mut len_buf)?;
    let len = u64::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(Error::Protocol(format!("frame too large: {len} bytes")));
    }
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX)];
    r.read_exact(&mut buf)?;
    bincode::deserialize(&buf).map_err(|e| Error::Protocol(format!("deserialize: {e}")))
}

/// A bidirectional stream built from a separate reader and writer. Implements
/// both [`Read`] and [`Write`] by delegating to each half.
pub struct IoDuplex<R: Read, W: Write> {
    /// The read half (e.g. ssh child stdout, or server stdin).
    pub r: R,
    /// The write half (e.g. ssh child stdin, or server stdout).
    pub w: W,
}

impl<R: Read, W: Write> IoDuplex<R, W> {
    /// Pair a reader and a writer into one duplex stream.
    pub fn new(r: R, w: W) -> Self {
        Self { r, w }
    }
}

impl<R: Read, W: Write> Read for IoDuplex<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.r.read(buf)
    }
}

impl<R: Read, W: Write> Write for IoDuplex<R, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.w.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

/// Shared state of one in-process byte channel.
struct PipeState {
    buf: VecDeque<u8>,
    closed: bool,
}

/// The write half of an in-process byte pipe.
pub struct PipeWriter {
    inner: Arc<(Mutex<PipeState>, Condvar)>,
}

/// The read half of an in-process byte pipe.
pub struct PipeReader {
    inner: Arc<(Mutex<PipeState>, Condvar)>,
}

fn new_pipe() -> (PipeWriter, PipeReader) {
    let inner = Arc::new((
        Mutex::new(PipeState {
            buf: VecDeque::new(),
            closed: false,
        }),
        Condvar::new(),
    ));
    (
        PipeWriter {
            inner: Arc::clone(&inner),
        },
        PipeReader { inner },
    )
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        state.buf.extend(buf.iter().copied());
        cvar.notify_all();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        state.closed = true;
        cvar.notify_all();
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let (lock, cvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        loop {
            if !state.buf.is_empty() {
                let n = state.buf.len().min(buf.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = state.buf.pop_front().unwrap();
                }
                return Ok(n);
            }
            if state.closed {
                return Ok(0); // EOF
            }
            state = cvar.wait(state).unwrap();
        }
    }
}

/// A connected pair of in-process duplex endpoints. Whatever endpoint A writes,
/// endpoint B reads, and vice versa. Used to drive a full sender/receiver
/// exchange in two threads without spawning a process or touching a socket.
#[must_use]
pub fn duplex_pair() -> (
    IoDuplex<PipeReader, PipeWriter>,
    IoDuplex<PipeReader, PipeWriter>,
) {
    let (a_to_b_w, a_to_b_r) = new_pipe();
    let (b_to_a_w, b_to_a_r) = new_pipe();
    let end_a = IoDuplex::new(b_to_a_r, a_to_b_w);
    let end_b = IoDuplex::new(a_to_b_r, b_to_a_w);
    (end_a, end_b)
}
