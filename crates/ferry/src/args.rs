//! Command-line interface definition (clap derive).

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// Ferry — a fast, memory-safe rsync alternative (local sync milestone).
#[derive(Debug, Parser)]
#[command(name = "ferry", version, about, long_about = None)]
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

    /// Exclude paths matching this glob (repeatable).
    #[arg(long, value_name = "PAT")]
    pub exclude: Vec<String>,

    /// Bandwidth limit (parsed now, enforced in a later phase).
    #[arg(long, value_name = "RATE")]
    pub bwlimit: Option<String>,

    /// Keep partial files for resume (later phase).
    #[arg(long)]
    pub partial: bool,

    /// Plain line output instead of the TUI.
    #[arg(long)]
    pub no_tui: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    pub output: OutputFormat,

    /// Parallelism (worker threads). Defaults to the CPU count.
    #[arg(short = 'j', long, value_name = "N")]
    pub threads: Option<usize>,

    /// Increase verbosity (repeatable).
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Quiet: suppress per-entry output.
    #[arg(short = 'q', long)]
    pub quiet: bool,
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

impl From<ReflinkArg> for ferry_core::copy::ReflinkMode {
    fn from(a: ReflinkArg) -> Self {
        match a {
            ReflinkArg::Auto => Self::Auto,
            ReflinkArg::Always => Self::Always,
            ReflinkArg::Never => Self::Never,
        }
    }
}

impl Args {
    /// Resolve the worker-thread count (CLI override or CPU count).
    #[must_use]
    pub fn thread_count(&self) -> usize {
        self.threads.unwrap_or_else(num_cpus::get)
    }
}
