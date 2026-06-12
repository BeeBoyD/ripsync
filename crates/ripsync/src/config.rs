//! Optional config file supplying flag *defaults* (never overriding the CLI).
//!
//! Location: `$XDG_CONFIG_HOME/ripsync/config.toml` (or `~/.config/ripsync/…`)
//! on Unix, `%APPDATA%\ripsync\config.toml` on Windows. Only the keys below are
//! honored; the CLI always wins.
//!
//! ```toml
//! reflink = "always"     # auto | always | never
//! fsync   = "never"      # auto | always | never
//! backend = "uring"      # auto | uring | portable
//! threads = 8
//! exclude = ["*.tmp", "node_modules"]
//! ```

use std::path::PathBuf;

use clap::ValueEnum;
use clap::parser::ValueSource;
use serde::Deserialize;

use crate::args::{Args, BackendArg, FsyncArg, ReflinkArg};

/// Deserialized config file. Every field is optional; unknown keys are rejected
/// so typos surface instead of silently doing nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    reflink: Option<String>,
    fsync: Option<String>,
    backend: Option<String>,
    threads: Option<usize>,
    exclude: Option<Vec<String>>,
}

/// Resolve the config file path for this platform, if a base dir is known.
fn config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("ripsync/config.toml"))
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(xdg).join("ripsync/config.toml"));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/ripsync/config.toml"))
    }
}

/// Apply config-file values to `args` for every flag the user did **not** pass
/// on the command line. A missing file is silently ignored; a malformed file is
/// reported as a warning and otherwise ignored.
pub fn apply_defaults(args: &mut Args, matches: &clap::ArgMatches) {
    let Some(path) = config_path() else {
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return, // no file (or unreadable): use built-in defaults
    };
    let cfg: FileConfig = match toml::from_str(&text) {
        Ok(cfg) => cfg,
        Err(error) => {
            tracing::warn!("ignoring {}: {error}", path.display());
            return;
        }
    };

    if !is_from_cli(matches, "reflink") {
        if let Some(value) = parse_enum::<ReflinkArg>(cfg.reflink.as_deref(), "reflink") {
            args.reflink = value;
        }
    }
    if !is_from_cli(matches, "fsync") {
        if let Some(value) = parse_enum::<FsyncArg>(cfg.fsync.as_deref(), "fsync") {
            args.fsync = value;
        }
    }
    if !is_from_cli(matches, "backend") {
        if let Some(value) = parse_enum::<BackendArg>(cfg.backend.as_deref(), "backend") {
            args.backend = value;
        }
    }
    if !is_from_cli(matches, "threads") && args.threads.is_none() {
        args.threads = cfg.threads;
    }
    if !is_from_cli(matches, "exclude") {
        if let Some(globs) = cfg.exclude {
            args.exclude = globs;
        }
    }
}

/// Whether `id` was supplied on the command line (as opposed to a default).
fn is_from_cli(matches: &clap::ArgMatches, id: &str) -> bool {
    matches.value_source(id) == Some(ValueSource::CommandLine)
}

/// Parse an optional string into a `ValueEnum`, warning on an invalid value.
fn parse_enum<T: ValueEnum>(value: Option<&str>, key: &str) -> Option<T> {
    let raw = value?;
    match T::from_str(raw, true) {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            tracing::warn!("config: invalid value for `{key}`: {raw:?}");
            None
        }
    }
}
