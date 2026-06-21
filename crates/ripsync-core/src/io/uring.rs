//! `io_uring` batched small-file copy backend (Linux only).
//!
//! This is the one place in `ripsync-core` where raw `unsafe` is justified: the
//! `io_uring` submission interface requires pointing the kernel at user buffers.
//! The rest of the crate keeps `#![forbid(unsafe_code)]`; here we opt in locally
//! and annotate every `unsafe` block with a `// SAFETY:` rationale.
//!
//! Strategy: copy a chunk of files by (1) opening each source and temp file,
//! (2) submitting one `read` per file and reaping all completions, then
//! (3) submitting one `write` per file and reaping. Two ring round-trips replace
//! `2·N` read/write syscalls. Files larger than [`MAX_FILE`] but at most
//! [`MAX_LARGE_FILE`] are handled via linked [`Splice`] SQEs when the kernel
//! supports it (≥5.19). Anything larger or unsupported is reported back so the
//! caller can fall back to the portable path.
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
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use io_uring::{opcode, squeue, types, IoUring};

/// Submission/completion queue depth, and the per-chunk batch size.
const QD: u32 = 256;

/// Largest file handled by the single-shot uring path; bigger files fall back
/// or use the splice path.
pub const MAX_FILE: u64 = 1 << 20; // 1 MiB

/// Largest file handled by the linked-splice uring path (64 MiB).
pub const MAX_LARGE_FILE: u64 = 64 * 1024 * 1024; // 64 MiB

/// Default chunk size for splice operations (1 MiB, matches typical
/// `/proc/sys/fs/pipe-max-size`).
const SPLICE_CHUNK: usize = 1 * 1024 * 1024; // 1 MiB

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

static LARGE_FILE_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static LARGE_FILE_SUCCESSES: AtomicU64 = AtomicU64::new(0);

/// Return (attempts, successes) counters for large-file splice copies.
#[must_use]
pub fn large_file_stats() -> (u64, u64) {
    (
        LARGE_FILE_ATTEMPTS.load(Ordering::Relaxed),
        LARGE_FILE_SUCCESSES.load(Ordering::Relaxed),
    )
}

// ---------------------------------------------------------------------------
// Kernel probe
// ---------------------------------------------------------------------------

static SPLICE_SUPPORTED: OnceLock<bool> = OnceLock::new();

/// Check whether the running kernel supports `IORING_OP_SPLICE` (≥5.19).
///
/// The probe runs once and caches the result. On non-Linux or pre-5.19 kernels
/// this returns `false` and callers should fall back to the portable path.
#[must_use]
pub fn kernel_supports_splice() -> bool {
    *SPLICE_SUPPORTED.get_or_init(probe_kernel_splice)
}

fn probe_kernel_splice() -> bool {
    let uname = rustix::system::uname();
    let release = uname.release().to_str().unwrap_or("0.0");
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    // Splice opcode exists since 5.7; we require 5.19 for reliability.
    major > 5 || (major == 5 && minor >= 19)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

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
///
/// Small files (≤[`MAX_FILE`]) use the existing read/write batching path.
/// Medium files (≤[`MAX_LARGE_FILE`]) use linked splice SQEs when the kernel
/// supports it. Larger files are rejected so the caller falls back.
#[must_use]
pub fn copy_batch(jobs: &[Job]) -> Vec<io::Result<u64>> {
    let mut results: Vec<io::Result<u64>> = Vec::with_capacity(jobs.len());
    for _ in jobs {
        results.push(Ok(0));
    }

    let mut ring = match IoUring::new(QD) {
        Ok(r) => r,
        Err(e) => {
            for r in &mut results {
                *r = Err(io::Error::other(format!("io_uring init: {e}")));
            }
            return results;
        }
    };

    // Partition: small (≤1 MiB), large (1–64 MiB), huge (>64 MiB).
    let mut small_indices: Vec<usize> = Vec::new();
    let mut large_indices: Vec<usize> = Vec::new();

    for (i, job) in jobs.iter().enumerate() {
        if job.len <= MAX_FILE {
            small_indices.push(i);
        } else if job.len <= MAX_LARGE_FILE {
            large_indices.push(i);
        } else {
            results[i] = Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "file too large for uring path",
            ));
        }
    }

    // Process small files in chunks (existing path).
    for chunk in chunk_indices(small_indices.len(), QD as usize) {
        let mapped: Vec<usize> = chunk.iter().map(|&idx| small_indices[idx]).collect();
        copy_chunk(&mut ring, jobs, &mapped, &mut results);
    }

    // Process large files via linked splice (one at a time).
    let splice_ok = kernel_supports_splice();
    for &job_idx in &large_indices {
        if !splice_ok {
            results[job_idx] = Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "kernel too old for io_uring splice; upgrade to ≥5.19",
            ));
            continue;
        }
        LARGE_FILE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        match copy_large_file_splice(&mut ring, &jobs[job_idx]) {
            Ok(bytes) => {
                LARGE_FILE_SUCCESSES.fetch_add(1, Ordering::Relaxed);
                results[job_idx] = Ok(bytes);
            }
            Err(e) => {
                results[job_idx] = Err(e);
            }
        }
    }

    results
}

/// Copy a single large file via linked splice SQEs.
///
/// This is the entry point used by the portable copy ladder in [`crate::copy`]
/// when io_uring is available and the file is 1–64 MiB.
pub fn copy_single_large(src: &Path, tmp: &Path, len: u64) -> io::Result<u64> {
    if !kernel_supports_splice() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "kernel too old for io_uring splice; upgrade to ≥5.19",
        ));
    }
    if len > MAX_LARGE_FILE {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file too large for uring splice path",
        ));
    }
    LARGE_FILE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let mut ring = IoUring::new(QD)?;
    match copy_large_file_splice(&mut ring, &Job { src, tmp, len }) {
        Ok(bytes) => {
            LARGE_FILE_SUCCESSES.fetch_add(1, Ordering::Relaxed);
            Ok(bytes)
        }
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Small-file batching (existing path, unchanged logic)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Large-file splice path
// ---------------------------------------------------------------------------

/// Copy a single large file using linked `IORING_OP_SPLICE` SQEs.
///
/// Strategy:
/// 1. Create a pipe and maximize its capacity.
/// 2. For each chunk, submit a linked pair: splice(src→pipe) → splice(pipe→dst).
/// 3. All pairs are submitted in one batch, then all completions are reaped.
/// 4. If any SQE fails, the whole operation fails (caller falls back).
fn copy_large_file_splice(ring: &mut IoUring, job: &Job) -> io::Result<u64> {
    let src_file = File::open(job.src)?;
    let dst_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(job.tmp)?;

    let src_fd = src_file.as_raw_fd();
    let dst_fd = dst_file.as_raw_fd();

    // Create pipe and maximize its size.
    let (pipe_read, pipe_write) = rustix::pipe::pipe()?;
    let pipe_size = maximize_pipe_size(&pipe_write);

    let chunk_size = pipe_size.min(SPLICE_CHUNK);
    let num_chunks = ((job.len as usize) + chunk_size - 1) / chunk_size;
    let total_sqes = num_chunks * 2; // each chunk = src→pipe + pipe→dst

    if total_sqes > QD as usize {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "too many splice SQEs needed ({total_sqes}) for {} byte file; \
                 increase QD or reduce file size",
                job.len
            ),
        ));
    }

    let pipe_read_fd = pipe_read.as_raw_fd();
    let pipe_write_fd = pipe_write.as_raw_fd();

    // Submit all linked splice pairs.
    let mut submitted: u32 = 0;
    for chunk_idx in 0..num_chunks {
        let offset = (chunk_idx * chunk_size) as u64;
        let remaining = job.len.saturating_sub(offset);
        let this_chunk = (remaining as usize).min(chunk_size);

        if this_chunk == 0 {
            break;
        }

        // SQE 1: splice src → pipe (fill pipe from source file).
        let sqe1 = opcode::Splice::new(
            types::Fd(src_fd),
            offset as i64,       // off_in: read from this file offset
            types::Fd(pipe_write_fd),
            -1_i64,              // off_out: pipe, must be -1
            this_chunk as u32,
        )
        .build()
        .flags(squeue::Flags::IO_LINK)
        .user_data((chunk_idx * 2) as u64);

        // SAFETY: pipe_write_fd and src_fd are valid for the duration.
        if unsafe { ring.submission().push(&sqe1) }.is_err() {
            return Err(io::Error::other("uring submission queue full (splice src→pipe)"));
        }
        submitted += 1;

        // SQE 2: splice pipe → dst (drain pipe to destination file).
        let sqe2 = opcode::Splice::new(
            types::Fd(pipe_read_fd),
            -1_i64,              // off_in: pipe, must be -1
            types::Fd(dst_fd),
            offset as i64,       // off_out: write to this file offset
            this_chunk as u32,
        )
        .build()
        .user_data((chunk_idx * 2 + 1) as u64);

        // SAFETY: pipe_read_fd and dst_fd are valid for the duration.
        if unsafe { ring.submission().push(&sqe2) }.is_err() {
            return Err(io::Error::other("uring submission queue full (splice pipe→dst)"));
        }
        submitted += 1;
    }

    // Submit all SQEs and reap completions.
    if submitted == 0 {
        return Ok(0);
    }

    if let Err(e) = ring.submit_and_wait(submitted as usize) {
        return Err(io::Error::other(format!("uring splice submit: {e}")));
    }

    let mut total_copied: u64 = 0;
    let mut first_error: Option<io::Error> = None;

    for cqe in ring.completion() {
        let res = cqe.result();
        if res < 0 {
            let err = io::Error::from_raw_os_error(-res);
            if first_error.is_none() {
                first_error = Some(io::Error::other(format!(
                    "uring splice SQE failed: {err}; consider upgrading kernel to ≥5.19"
                )));
            }
        } else {
            total_copied += res as u64;
        }
    }

    if let Some(e) = first_error {
        // Log warning about splice failure.
        tracing::warn!(
            "io_uring splice failed for {} ({} bytes): {e}; falling back to portable copy",
            job.src.display(),
            job.len
        );
        // Clean up partial destination.
        drop(dst_file);
        let _ = std::fs::remove_file(job.tmp);
        return Err(e);
    }

    // Verify we copied the expected amount.
    if total_copied != job.len {
        tracing::warn!(
            "io_uring splice short copy for {}: expected {} bytes, got {total_copied}",
            job.src.display(),
            job.len
        );
        drop(dst_file);
        let _ = std::fs::remove_file(job.tmp);
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "splice short copy: expected {} bytes, got {total_copied}",
                job.len
            ),
        ));
    }

    Ok(total_copied)
}

/// Maximize the pipe buffer size up to the system limit.
///
/// Returns the actual pipe size in bytes.
fn maximize_pipe_size(pipe_fd: &OwnedFd) -> usize {
    // Try to set to 1 MiB; the kernel will clamp to `/proc/sys/fs/pipe-max-size`.
    let desired = SPLICE_CHUNK;
    if rustix::pipe::fcntl_setpipe_size(pipe_fd, desired).is_ok() {
        rustix::pipe::fcntl_getpipe_size(pipe_fd).unwrap_or(desired)
    } else {
        // Fall back to whatever the kernel gave us.
        rustix::pipe::fcntl_getpipe_size(pipe_fd).unwrap_or(65536)
    }
}
