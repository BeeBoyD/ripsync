//! Remote (ssh) transfers: spawning the far-end `ripsync --server`, parsing
//! `[user@]host:path` specs, and driving the protocol from the local side.
//!
//! Mirrors rsync's model: the source/destination may be a local path or a remote
//! `host:path`. Exactly one side may be remote per run. The transport is the
//! user's own `ssh` (so keys, agent, `~/.ssh/config`, and `known_hosts` all just
//! work); ripsync speaks its own protocol over the ssh pipe.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use ripsync_core::Filter;
use ripsync_core::RunControl;
use ripsync_core::net::proto::{NetOptions, Role};
use ripsync_core::net::transport::IoDuplex;
use ripsync_core::net::{run_initiator, server::run_server as core_run_server};
use ripsync_core::report::{Event, NullReporter, Reporter, RunStatus};
use ripsync_core::verify::VerificationSummary;

use crate::args::Args;
use crate::reporter::CliReporter;

/// A source or destination location: a local path or a remote `host:path`.
pub enum Location {
    /// A path on the local filesystem.
    Local(PathBuf),
    /// A remote endpoint reached over ssh.
    Remote {
        /// Optional `user@` part.
        user: Option<String>,
        /// Hostname or ssh-config alias.
        host: String,
        /// Path on the remote host.
        path: String,
    },
}

impl Location {
    /// Whether this location is remote.
    pub fn is_remote(&self) -> bool {
        matches!(self, Location::Remote { .. })
    }
}

/// Parse a CLI argument into a [`Location`].
///
/// A spec is **remote** when it contains a `:` whose left side has no path
/// separator and is not a single-letter Windows drive (so `C:\dir` and `./a:b`
/// stay local, while `host:/srv`, `user@host:rel`, and `box:.` are remote).
#[must_use]
pub fn parse_location(s: &str) -> Location {
    if let Some(idx) = s.find(':') {
        let before = &s[..idx];
        let after = &s[idx + 1..];
        let is_drive = before.len() == 1
            && before
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic());
        let looks_remote =
            !before.is_empty() && !before.contains('/') && !before.contains('\\') && !is_drive;
        if looks_remote {
            let (user, host) = match before.split_once('@') {
                Some((u, h)) => (Some(u.to_string()), h.to_string()),
                None => (None, before.to_string()),
            };
            return Location::Remote {
                user,
                host,
                path: after.to_string(),
            };
        }
    }
    Location::Local(PathBuf::from(s))
}

/// Run as the far-end protocol peer (`ripsync --server`), reading/writing the
/// protocol on stdin/stdout. Diagnostics and logs go to stderr only.
///
/// # Errors
///
/// Propagates any handshake, protocol, walk, or I/O error.
pub fn run_server() -> Result<()> {
    let control = RunControl::default();
    core_run_server(0, &control, &NullReporter).context("ripsync --server")?;
    Ok(())
}

/// Drive a remote transfer (exactly one of `src`/`dst` is remote).
///
/// # Errors
///
/// Returns an error if both/neither side is remote, if `--delete`/`--dry-run`
/// constraints are unmet, if ssh cannot be spawned, or on any protocol error.
pub fn run_remote(
    args: &Args,
    threads: usize,
    filter: &Filter,
    src: &Location,
    dst: &Location,
) -> Result<()> {
    if args.dry_run {
        bail!("--dry-run is not yet supported for remote transfers");
    }
    if args.delete && !args.yes {
        bail!("--delete over a remote transfer requires --yes (no interactive preview yet)");
    }

    let (role, local_root, remote) = match (src.is_remote(), dst.is_remote()) {
        (true, true) => bail!("remote-to-remote transfers are not supported"),
        (false, false) => bail!("run_remote called with two local paths"),
        // Pull: remote source → local destination.
        (true, false) => {
            let Location::Local(local) = dst else {
                unreachable!()
            };
            (Role::Pull, local.clone(), src)
        }
        // Push: local source → remote destination.
        (false, true) => {
            let Location::Local(local) = src else {
                unreachable!()
            };
            (Role::Push, local.clone(), dst)
        }
    };

    let Location::Remote { user, host, path } = remote else {
        unreachable!()
    };

    let options = NetOptions {
        delete: args.delete,
        checksum: args.checksum,
        preserve_mode: true,
        preserve_mtime: true,
        whole_file: args.whole_file,
        compress: args.compress,
        compress_level: args.compress_level,
    };

    let mut child = spawn_ssh(&args.rsh, user.as_deref(), host)?;
    let stdout = child.stdout.take().context("ssh child has no stdout")?;
    let stdin = child.stdin.take().context("ssh child has no stdin")?;
    let conn = IoDuplex::new(stdout, stdin);

    let control = RunControl::default();
    let reporter = CliReporter::new(args.output, args.quiet, args.verbose, args.dry_run);
    let remote_root = PathBuf::from(path);

    // Optionally throttle the local→remote byte rate (rsync's --bwlimit).
    let result = if let Some(spec) = &args.bwlimit {
        let rate = parse_bwlimit(spec)?;
        let mut throttled = Throttle::new(conn, rate);
        run_initiator(
            &mut throttled,
            role,
            &local_root,
            &remote_root,
            options,
            filter,
            threads,
            &control,
            &reporter,
        )
    } else {
        let mut conn = conn;
        run_initiator(
            &mut conn,
            role,
            &local_root,
            &remote_root,
            options,
            filter,
            threads,
            &control,
            &reporter,
        )
    };
    // Always reap the ssh child so it does not linger.
    let _ = child.wait();

    let stats = result.context("remote sync failed")?;

    let status = if stats.errors == 0 {
        RunStatus::Success
    } else {
        RunStatus::Failed
    };
    reporter.event(Event::Finished { status });
    reporter.finish(
        &stats,
        &args.src.to_string_lossy(),
        &args.dst.to_string_lossy(),
        status,
        &VerificationSummary::default(),
    );

    if stats.errors > 0 {
        bail!(
            "remote sync completed with {} per-entry error(s)",
            stats.errors
        );
    }
    Ok(())
}

/// Spawn `<rsh> [opts] <target> ripsync --server`, piping stdin/stdout and
/// letting the remote's stderr pass through to ours.
fn spawn_ssh(rsh: &str, user: Option<&str>, host: &str) -> Result<Child> {
    let mut parts = rsh.split_whitespace();
    let program = parts.next().context("--rsh is empty")?;
    let target = match user {
        Some(u) => format!("{u}@{host}"),
        None => host.to_string(),
    };

    let mut cmd = Command::new(program);
    for opt in parts {
        cmd.arg(opt);
    }
    cmd.arg(target)
        .arg("ripsync")
        .arg("--server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    cmd.spawn()
        .with_context(|| format!("spawning remote shell `{rsh}`"))
}

/// Parse a `--bwlimit` value into bytes per second. A bare number is KiB/s (as in
/// rsync); a `K`/`M`/`G` suffix multiplies by 1024/1024²/1024³.
///
/// # Errors
///
/// Returns an error if the number cannot be parsed.
fn parse_bwlimit(spec: &str) -> Result<u64> {
    let spec = spec.trim();
    let (num, mult) = match spec.chars().last() {
        Some('k' | 'K') => (&spec[..spec.len() - 1], 1024.0),
        Some('m' | 'M') => (&spec[..spec.len() - 1], 1024.0 * 1024.0),
        Some('g' | 'G') => (&spec[..spec.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (spec, 1024.0), // bare number = KiB/s, matching rsync
    };
    let value: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("invalid --bwlimit value: {spec}"))?;
    if value <= 0.0 {
        bail!("--bwlimit must be positive");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((value * mult) as u64)
}

/// A [`Read`]+[`Write`] wrapper that caps write throughput with a token bucket.
/// Reads pass through untouched; only the outbound (local→remote) byte rate is
/// limited, which is the side a push uploads on.
struct Throttle<S> {
    inner: S,
    rate: f64,
    allowance: f64,
    last: Instant,
}

impl<S> Throttle<S> {
    fn new(inner: S, bytes_per_sec: u64) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let rate = bytes_per_sec as f64;
        Self {
            inner,
            rate,
            allowance: rate, // allow an initial one-second burst
            last: Instant::now(),
        }
    }

    /// Refill the bucket for elapsed time, capping the burst at one second.
    fn replenish(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.allowance = (self.allowance + dt * self.rate).min(self.rate);
    }
}

impl<S: Read> Read for Throttle<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<S: Write> Write for Throttle<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.replenish();
        if self.allowance <= 0.0 {
            let wait = (-self.allowance + 1.0) / self.rate;
            std::thread::sleep(Duration::from_secs_f64(wait));
            self.replenish();
        }
        let n = self.inner.write(buf)?;
        #[allow(clippy::cast_precision_loss)]
        {
            self.allowance -= n as f64;
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bwlimit_parses_units() {
        assert_eq!(parse_bwlimit("1024").unwrap(), 1024 * 1024); // bare = KiB/s
        assert_eq!(parse_bwlimit("1K").unwrap(), 1024);
        assert_eq!(parse_bwlimit("2M").unwrap(), 2 * 1024 * 1024);
        assert!(parse_bwlimit("nonsense").is_err());
    }

    #[test]
    fn local_paths_stay_local() {
        assert!(matches!(parse_location("./src"), Location::Local(_)));
        assert!(matches!(parse_location("/abs/path"), Location::Local(_)));
        assert!(matches!(parse_location("relative/dir"), Location::Local(_)));
        assert!(matches!(parse_location("C:\\Users\\x"), Location::Local(_)));
        assert!(matches!(parse_location("C:/Users/x"), Location::Local(_)));
    }

    #[test]
    fn remote_specs_parse() {
        match parse_location("host:/srv/data") {
            Location::Remote { user, host, path } => {
                assert_eq!(user, None);
                assert_eq!(host, "host");
                assert_eq!(path, "/srv/data");
            }
            Location::Local(_) => panic!("expected remote"),
        }
        match parse_location("alice@box:backups") {
            Location::Remote { user, host, path } => {
                assert_eq!(user.as_deref(), Some("alice"));
                assert_eq!(host, "box");
                assert_eq!(path, "backups");
            }
            Location::Local(_) => panic!("expected remote"),
        }
    }
}
