//! Error types for `ripsync-core`. Library code returns [`Result`]; it never
//! panics on bad input.

use std::path::PathBuf;

/// Convenience alias for results produced by `ripsync-core`.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can arise while planning or applying a sync.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The run was cancelled cooperatively.
    #[error("run cancelled")]
    Cancelled,

    /// An underlying I/O failure, annotated with the offending path.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Path that triggered the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A bare I/O failure with no associated path.
    #[error(transparent)]
    BareIo(#[from] std::io::Error),

    /// A path escaped the destination root (containment violation).
    #[error("path escapes destination root: {0}")]
    Containment(PathBuf),

    /// `--delete` was requested but the source is empty or unreadable.
    #[error("refusing to mirror deletions: source is empty or unreadable ({0})")]
    EmptySource(PathBuf),

    /// An exclude/glob pattern failed to compile.
    #[error("invalid exclude pattern: {0}")]
    Pattern(String),

    /// An include/exclude/files-from filter was invalid.
    #[error("filter error: {0}")]
    Filter(String),

    /// A delta could not be applied to the supplied basis.
    #[error("delta apply failed: {0}")]
    DeltaApply(String),

    /// The remote-sync wire protocol was violated (bad frame, version mismatch,
    /// unexpected message, oversized frame).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Verification found one or more mismatches.
    #[error("verification failed with {0} mismatch(es)")]
    Verification(usize),
}

impl Error {
    /// Wrap an [`std::io::Error`] together with the path that produced it.
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
