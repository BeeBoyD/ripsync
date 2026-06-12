//! `io_uring` batched small-file copy backend (Linux only).
//!
//! This is the one place in `ferry-core` where raw `unsafe` is justified: the
//! `io_uring` submission interface requires pointing the kernel at user buffers.
//! The rest of the crate keeps `#![forbid(unsafe_code)]`; here we opt in locally
//! and annotate every `unsafe` block with a `// SAFETY:` rationale.
//!
//! Strategy: copy a chunk of files by (1) opening each source and temp file,
//! (2) submitting one `read` per file and reaping all completions, then
//! (3) submitting one `write` per file and reaping. Two ring round-trips replace
//! `2·N` read/write syscalls. Files larger than [`MAX_FILE`] or anything that
//! errors are reported back so the caller can fall back to the portable path.
#![allow(unsafe_code)]
// Lengths are bounded by `MAX_FILE` (1 MiB) and completion results are checked
// non-negative before use, so these casts are provably lossless here.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use io_uring::{IoUring, opcode, types};

/// Submission/completion queue depth, and the per-chunk batch size.
const QD: u32 = 256;

/// Largest file handled by the single-shot uring path; bigger files fall back.
pub const MAX_FILE: u64 = 1 << 20; // 1 MiB

/// One copy request: read all of `src`, write it to the fresh temp file `tmp`.
pub struct Job<'a> {
    /// Source file path.
    pub src: &'a Path,
    /// Destination temporary path (created by this backend).
    pub tmp: &'a Path,
    /// Expected source length in bytes.
    pub len: u64,
}

/// Copy every job, returning a per-job result (bytes written, or the error that
/// should trigger a portable-path fallback for that single file).
#[must_use]
pub fn copy_batch(jobs: &[Job]) -> Vec<io::Result<u64>> {
    let mut results: Vec<io::Result<u64>> = Vec::with_capacity(jobs.len());
    for _ in jobs {
        results.push(Ok(0));
    }

    let mut ring = match IoUring::new(QD) {
        Ok(r) => r,
        Err(e) => {
            // No ring: signal fallback for everything.
            for r in &mut results {
                *r = Err(io::Error::other(format!("io_uring init: {e}")));
            }
            return results;
        }
    };

    for chunk in chunk_indices(jobs.len(), QD as usize) {
        copy_chunk(&mut ring, jobs, &chunk, &mut results);
    }
    results
}

/// Yield index ranges of at most `size` over `0..n`.
fn chunk_indices(n: usize, size: usize) -> impl Iterator<Item = Vec<usize>> {
    (0..n)
        .step_by(size)
        .map(move |start| (start..(start + size).min(n)).collect())
}

/// State for one opened job within a chunk.
struct Open {
    job_idx: usize,
    src: File,
    tmp: File,
    buf: Vec<u8>,
    read: usize,
    failed: bool,
}

fn copy_chunk(ring: &mut IoUring, jobs: &[Job], chunk: &[usize], results: &mut [io::Result<u64>]) {
    let mut opens: Vec<Open> = Vec::with_capacity(chunk.len());

    for &job_idx in chunk {
        let job = &jobs[job_idx];
        if job.len > MAX_FILE {
            results[job_idx] = Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "file too large for uring path",
            ));
            continue;
        }
        match open_pair(job) {
            Ok((src, tmp)) => {
                let len = usize::try_from(job.len).unwrap_or(0);
                opens.push(Open {
                    job_idx,
                    src,
                    tmp,
                    buf: vec![0u8; len],
                    read: 0,
                    failed: false,
                });
            }
            Err(e) => results[job_idx] = Err(e),
        }
    }

    submit_reads(ring, &mut opens, results);
    submit_writes(ring, &mut opens, results);

    // Empty files and any survivors succeed with their full length.
    for o in &opens {
        if !o.failed {
            results[o.job_idx] = Ok(o.buf.len() as u64);
        }
    }
}

fn open_pair(job: &Job) -> io::Result<(File, File)> {
    let src = File::open(job.src)?;
    let tmp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(job.tmp)?;
    Ok((src, tmp))
}

fn submit_reads(ring: &mut IoUring, opens: &mut [Open], results: &mut [io::Result<u64>]) {
    let mut submitted = 0u32;
    for (slot, o) in opens.iter().enumerate() {
        if o.buf.is_empty() {
            continue; // zero-length file: nothing to read
        }
        // SAFETY: `o.buf` lives in `opens` for the whole call (not moved until
        // after we reap completions), so the kernel-visible pointer/length stay
        // valid until the matching CQE is consumed below.
        let sqe = opcode::Read::new(
            types::Fd(o.src.as_raw_fd()),
            o.buf.as_ptr().cast_mut(),
            o.buf.len() as u32,
        )
        .offset(0)
        .build()
        .user_data(slot as u64);
        // SAFETY: the SQE references `o.buf` which outlives submission/reaping.
        if unsafe { ring.submission().push(&sqe) }.is_err() {
            break;
        }
        submitted += 1;
    }
    reap(ring, submitted, opens, results, true);
}

fn submit_writes(ring: &mut IoUring, opens: &mut [Open], results: &mut [io::Result<u64>]) {
    let mut submitted = 0u32;
    for (slot, o) in opens.iter().enumerate() {
        if o.failed || o.read == 0 {
            continue;
        }
        // SAFETY: `o.buf[..o.read]` stays valid in `opens` until reaping.
        let sqe = opcode::Write::new(types::Fd(o.tmp.as_raw_fd()), o.buf.as_ptr(), o.read as u32)
            .offset(0)
            .build()
            .user_data(slot as u64);
        // SAFETY: same lifetime argument as the read phase.
        if unsafe { ring.submission().push(&sqe) }.is_err() {
            break;
        }
        submitted += 1;
    }
    reap(ring, submitted, opens, results, false);
}

/// Wait for `count` completions and record their results.
fn reap(
    ring: &mut IoUring,
    count: u32,
    opens: &mut [Open],
    results: &mut [io::Result<u64>],
    is_read: bool,
) {
    if count == 0 {
        return;
    }
    if let Err(e) = ring.submit_and_wait(count as usize) {
        for o in opens.iter_mut() {
            o.failed = true;
            results[o.job_idx] = Err(io::Error::other(format!("uring submit: {e}")));
        }
        return;
    }
    let cqes: Vec<(usize, i32)> = ring
        .completion()
        .map(|c| (c.user_data() as usize, c.result()))
        .collect();
    for (slot, res) in cqes {
        let Some(o) = opens.get_mut(slot) else {
            continue;
        };
        if res < 0 {
            o.failed = true;
            results[o.job_idx] = Err(io::Error::from_raw_os_error(-res));
        } else if is_read {
            o.read = res as usize;
            if o.read != o.buf.len() {
                // Short read (file changed under us): fall back for this file.
                o.failed = true;
                results[o.job_idx] =
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
            }
        } else if res as usize != o.read {
            o.failed = true;
            results[o.job_idx] = Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
        }
    }
}
