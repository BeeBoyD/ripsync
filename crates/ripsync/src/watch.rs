//! `--watch`: re-sync the source into the destination whenever it changes.
//!
//! A recursive filesystem watcher feeds change events into a channel. After the
//! first event, further events are coalesced for the debounce window, then one
//! incremental sync runs (reusing the persistent index, so unchanged files are
//! skipped cheaply). A failed cycle is reported but does not stop the watch —
//! the next change triggers another attempt. Ctrl-C ends the loop.

use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use notify::{RecursiveMode, Watcher};
use ripsync_core::Filter;

use crate::args::Args;
use crate::run_local_once;

/// Run an initial sync, then watch `args.src` and re-sync on change until the
/// process is interrupted.
pub fn run_watch(args: &Args, threads: usize, filter: &Filter) -> Result<()> {
    if args.dry_run {
        bail!("--watch cannot be combined with --dry-run");
    }
    let debounce = Duration::from_millis(args.debounce.max(1));

    // Initial sync so the destination is current before we start watching.
    sync_cycle(args, threads, filter);

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        // A send failure only means the receiver is gone (we are shutting down).
        let _ = tx.send(res);
    })
    .context("creating filesystem watcher")?;
    watcher
        .watch(&args.src, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", args.src.display()))?;

    eprintln!(
        "watching {} → {} (debounce {} ms; Ctrl-C to stop)",
        args.src.display(),
        args.dst.display(),
        debounce.as_millis()
    );

    loop {
        // Block until the first change, then coalesce the burst that follows.
        match rx.recv() {
            Ok(Ok(event)) if !relevant(&event) => continue,
            Ok(_) => {}
            Err(_) => break, // watcher dropped
        }
        drain_for(&rx, debounce);
        sync_cycle(args, threads, filter);
    }
    Ok(())
}

/// Whether an event should trigger a sync. ripsync's own metadata writes under
/// `.ripsync/` are ignored so a sync does not retrigger itself.
fn relevant(event: &notify::Event) -> bool {
    !event
        .paths
        .iter()
        .all(|p| p.components().any(|c| c.as_os_str() == ".ripsync"))
}

/// Swallow events until the channel has been quiet for `window`, so a burst of
/// edits collapses into a single sync.
fn drain_for(rx: &std::sync::mpsc::Receiver<notify::Result<notify::Event>>, window: Duration) {
    let deadline = Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match rx.recv_timeout(remaining) {
            Ok(_) => {} // reset is implicit: we keep draining until quiet
            Err(RecvTimeoutError::Timeout) => return,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Run one sync, reporting (but not propagating) a per-cycle failure.
fn sync_cycle(args: &Args, threads: usize, filter: &Filter) {
    let start = Instant::now();
    match run_local_once(args, threads, filter) {
        Ok(()) => log_synced(&args.dst, start),
        Err(error) => eprintln!("sync failed: {error:#}"),
    }
}

fn log_synced(dst: &Path, start: Instant) {
    eprintln!(
        "synced {} in {:.3}s",
        dst.display(),
        start.elapsed().as_secs_f64()
    );
}
