//! Progress reporting plumbing shared by the plain CLI output and the TUI.
//!
//! The engine emits [`Event`]s through a [`Reporter`]; the CLI decides how to
//! render them (line output, JSON, or a live `ratatui` dashboard). The trait is
//! `Sync` so the parallel apply phase can report from worker threads.

use std::path::PathBuf;

use crate::plan::Action;

/// A progress event emitted while applying a plan.
#[derive(Debug, Clone)]
pub enum Event {
    /// Emitted once before work starts.
    Planned {
        /// Number of files that will be copied or updated.
        total_files: usize,
        /// Total bytes those files comprise.
        total_bytes: u64,
        /// Number of pending deletions.
        deletions: usize,
    },
    /// A file transfer began.
    FileStart {
        /// Path relative to the destination root.
        rel: PathBuf,
        /// File length in bytes.
        len: u64,
    },
    /// A file transfer finished.
    FileDone {
        /// Path relative to the destination root.
        rel: PathBuf,
        /// Whether it was a copy or an update.
        action: Action,
        /// Bytes written.
        bytes: u64,
    },
    /// A directory was created or already present.
    DirDone {
        /// Path relative to the destination root.
        rel: PathBuf,
        /// The action taken.
        action: Action,
    },
    /// A symlink was created or updated.
    SymlinkDone {
        /// Path relative to the destination root.
        rel: PathBuf,
        /// The action taken.
        action: Action,
    },
    /// An entry was skipped (already up to date).
    Skipped {
        /// Path relative to the destination root.
        rel: PathBuf,
    },
    /// An entry was deleted.
    Deleted {
        /// Path relative to the destination root.
        rel: PathBuf,
    },
    /// An operation failed for a single entry (the sync continues).
    Failed {
        /// Path relative to the destination root.
        rel: PathBuf,
        /// Human-readable error text.
        error: String,
    },
}

/// Tally of everything a sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Files/dirs/symlinks newly created.
    pub copied: u64,
    /// Entries overwritten.
    pub updated: u64,
    /// Entries left untouched.
    pub skipped: u64,
    /// Entries removed.
    pub deleted: u64,
    /// Per-entry failures.
    pub errors: u64,
    /// Bytes written for copies and updates.
    pub bytes: u64,
}

/// A sink for [`Event`]s. Implementations must be cheap and thread-safe.
pub trait Reporter: Sync {
    /// Handle a single event.
    fn event(&self, ev: Event);
}

/// A [`Reporter`] that ignores everything (useful for tests and benches).
pub struct NullReporter;

impl Reporter for NullReporter {
    fn event(&self, _ev: Event) {}
}
