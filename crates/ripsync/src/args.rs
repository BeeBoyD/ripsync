//! Command-line interface definition (clap derive).

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// ripsync — a fast, memory-safe rsync alternative for local and remote sync.
#[derive(Debug, Clone, Parser)]
#[command(name = "ripsync", version, about, long_about = None)]
pub struct Args {
    /// Source directory.
    pub src: PathBuf,

    /// Destination directory (made an exact mirror of the source).
    pub dst: PathBuf,

    /// Plan only: print a readable summary and change nothing.
    #[arg(short = 'n', long)]
    pub dry_run: bool,

    /// Mirror deletions (entries in dest but not source). Gated by `--yes`.
    #[arg(long)]
    pub delete: bool,

    /// Confirm destructive actions (required for `--delete` to remove anything).
    #[arg(long)]
    pub yes: bool,

    /// Compare by content hash instead of size+mtime.
    #[arg(short = 'c', long)]
    pub checksum: bool,

    /// Force delta transfer even locally (demo/bench).
    #[arg(long)]
    pub delta: bool,

    /// Copy-on-write reflink strategy (CoW filesystems: btrfs/XFS/APFS/ReFS).
    /// `auto` tries it and falls back; `always` requires it; `never` skips it.
    #[arg(long, value_enum, default_value_t = ReflinkArg::Auto)]
    pub reflink: ReflinkArg,

    /// Durability vs speed. `auto`/`never` skip per-file fsync (fast); `auto`
    /// still fsyncs each touched directory once so renames survive a crash;
    /// `always` fsyncs every file before rename (slowest, strongest).
    #[arg(long, value_enum, default_value_t = FsyncArg::Auto)]
    pub fsync: FsyncArg,

    /// File-copy backend. `auto` selects the portable path;
    /// `uring` remains available explicitly on Linux.
    #[arg(long, value_enum, default_value_t = BackendArg::Auto)]
    pub backend: BackendArg,

    /// Post-copy verification scope.
    #[arg(long, value_enum, default_value_t = VerifyArg::None)]
    pub verify: VerifyArg,

    /// Disable the persistent destination index used for fast incremental runs.
    #[arg(long = "no-index", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub index: bool,

    /// Preserve hardlinked source files as hardlinks in the destination.
    #[arg(short = 'H', long)]
    pub hard_links: bool,

    /// Preserve sparse-file holes using SEEK_DATA/SEEK_HOLE where supported.
    #[arg(short = 'S', long)]
    pub sparse: bool,

    /// Preserve non-ACL extended attributes.
    #[arg(long)]
    pub xattrs: bool,

    /// Preserve POSIX ACL attributes.
    #[arg(long)]
    pub acls: bool,

    /// Preserve numeric owner id (requires suitable privileges).
    #[arg(long)]
    pub owner: bool,

    /// Preserve numeric group id (requires suitable privileges).
    #[arg(long)]
    pub group: bool,

    /// Exclude paths matching this glob (repeatable). Also excludes everything
    /// beneath a matching directory.
    #[arg(long, value_name = "PAT")]
    pub exclude: Vec<String>,

    /// Include paths matching this glob (repeatable). Checked before `--exclude`,
    /// so `--include '*.rs' --exclude '*'` keeps only Rust files.
    #[arg(long, value_name = "PAT")]
    pub include: Vec<String>,

    /// Ordered filter rule, highest priority: `"+ PAT"` keeps, `"- PAT"` drops
    /// (repeatable). Evaluated before `--include`/`--exclude`.
    #[arg(long, value_name = "RULE", allow_hyphen_values = true)]
    pub filter: Vec<String>,

    /// Read the set of paths to transfer (relative to SRC, one per line; blank
    /// lines and `#` comments ignored) from FILE.
    #[arg(long, value_name = "FILE")]
    pub files_from: Option<PathBuf>,

    /// Throttle the remote upload rate (like rsync's `--bwlimit`). A bare number
    /// is KiB/s; `K`/`M`/`G` suffixes set the unit. No effect on local copies.
    #[arg(long, value_name = "RATE")]
    pub bwlimit: Option<String>,

    /// Keep partially transferred files (accepted for rsync compatibility).
    /// Writes are always atomic via a temp file and rename; resume-from-partial
    /// is not yet implemented.
    #[arg(long)]
    pub partial: bool,

    /// Watch SRC and re-sync on every change (local transfers only). Runs the
    /// fast incremental sync after each debounced batch of filesystem events;
    /// press Ctrl-C to stop.
    #[arg(long)]
    pub watch: bool,

    /// Debounce window in milliseconds for `--watch`: events are coalesced for
    /// this long before a sync runs.
    #[arg(long, value_name = "MS", default_value_t = 300)]
    pub debounce: u64,

    /// Plain line output instead of the TUI.
    #[arg(long)]
    pub no_tui: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    pub output: OutputFormat,

    /// Parallelism (worker threads). Overrides the `--profile` thread count.
    #[arg(short = 'j', long, value_name = "N")]
    pub threads: Option<usize>,

    /// Performance tier. `auto` detects from CPU count and RAM; `low` conserves
    /// resources on small devices, `high` uses a workstation fully.
    #[arg(long, value_enum, default_value_t = ProfileArg::Auto)]
    pub profile: ProfileArg,

    /// Increase verbosity (repeatable).
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Quiet: suppress per-entry output.
    #[arg(short = 'q', long)]
    pub quiet: bool,

    /// Print a final summary block (counts, bytes, backend, elapsed) for
    /// non-TUI runs, even with `--quiet`. Honors `NO_COLOR` and piping.
    #[arg(long)]
    pub stats: bool,

    /// Internal: act as the remote protocol peer over stdin/stdout. Spawned on
    /// the far host by the local side's ssh command; not for direct use.
    #[arg(long, hide = true)]
    pub server: bool,

    /// Remote shell program for `host:path` transfers (like rsync's `-e`).
    /// Split on whitespace, e.g. `--rsh "ssh -p 2222"`.
    #[arg(short = 'e', long, value_name = "CMD", default_value = "ssh")]
    pub rsh: String,

    /// Transfer whole files instead of computing deltas (remote transfers).
    #[arg(short = 'W', long = "whole-file")]
    pub whole_file: bool,

    /// Compress whole-file payloads on the wire with zstd (remote transfers),
    /// like rsync's `-z`.
    #[arg(short = 'z', long)]
    pub compress: bool,

    /// zstd level for `--compress` (1–22; higher = smaller but slower).
    #[arg(long, value_name = "N", default_value_t = 3)]
    pub compress_level: i32,
}

/// CLI form of the device performance tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProfileArg {
    /// Detect the tier automatically from CPU count and RAM.
    Auto,
    /// Conserve resources for small/constrained devices.
    Low,
    /// Balanced desktop/laptop defaults.
    Balanced,
    /// Use workstation/server hardware fully.
    High,
}

/// How to render output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable text.
    Human,
    /// Machine-readable JSON (final report).
    Json,
}

/// CLI form of the reflink strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReflinkArg {
    /// Try reflink, fall back when unsupported.
    Auto,
    /// Require reflink; error if unavailable.
    Always,
    /// Never attempt reflink.
    Never,
}

impl From<ReflinkArg> for ripsync_core::copy::ReflinkMode {
    fn from(a: ReflinkArg) -> Self {
        match a {
            ReflinkArg::Auto => Self::Auto,
            ReflinkArg::Always => Self::Always,
            ReflinkArg::Never => Self::Never,
        }
    }
}

/// CLI form of the fsync strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FsyncArg {
    /// Skip per-file fsync; fsync touched directories once at the end.
    Auto,
    /// Fsync every file before rename.
    Always,
    /// Skip all fsync.
    Never,
}

impl From<FsyncArg> for ripsync_core::copy::FsyncMode {
    fn from(a: FsyncArg) -> Self {
        match a {
            FsyncArg::Auto => Self::Auto,
            FsyncArg::Always => Self::Always,
            FsyncArg::Never => Self::Never,
        }
    }
}

/// CLI form of the copy backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    /// Portable-first automatic selection.
    Auto,
    /// Force the io_uring backend.
    Uring,
    /// Force the portable backend.
    Portable,
}

/// CLI form of post-copy verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerifyArg {
    /// Do not verify.
    None,
    /// Verify copied and updated entries.
    Changed,
    /// Compare the complete source and destination trees.
    All,
}

impl From<VerifyArg> for ripsync_core::verify::VerifyMode {
    fn from(value: VerifyArg) -> Self {
        match value {
            VerifyArg::None => Self::None,
            VerifyArg::Changed => Self::Changed,
            VerifyArg::All => Self::All,
        }
    }
}

impl From<BackendArg> for ripsync_core::apply::Backend {
    fn from(a: BackendArg) -> Self {
        match a {
            BackendArg::Auto => Self::Auto,
            BackendArg::Uring => Self::Uring,
            BackendArg::Portable => Self::Portable,
        }
    }
}

impl Args {
    /// Resolve the device performance tier (`--profile`, or auto-detect).
    #[must_use]
    pub fn resolve_profile(&self) -> ripsync_core::tune::Profile {
        use ripsync_core::tune::Profile;
        match self.profile {
            ProfileArg::Auto => ripsync_core::tune::detect(),
            ProfileArg::Low => Profile::Low,
            ProfileArg::Balanced => Profile::Balanced,
            ProfileArg::High => Profile::High,
        }
    }

    /// Resolve the worker-thread count: an explicit `--threads`, else the count
    /// implied by the resolved performance tier.
    #[must_use]
    pub fn thread_count(&self) -> usize {
        if let Some(t) = self.threads {
            return t;
        }
        self.resolve_profile().params(num_cpus::get()).threads
    }
}
