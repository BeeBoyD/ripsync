//! ripsync CLI library: argument parsing, planning, applying, reporting.
//!
//! The `ripsync` binary and the opt-in `rs` short alias are both thin wrappers
//! over [`main`], so the entire program lives here in the library.

pub mod args;
mod config;
mod remote;
mod reporter;
mod tui;
mod watch;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches};

use ripsync_core::apply::{ApplyOptions, MetadataOptions, apply_plan_controlled};
use ripsync_core::filter::{Filter, FilterBuilder};
use ripsync_core::index::Manifest;
use ripsync_core::plan::{PlanOptions, build_plan_controlled};
use ripsync_core::report::{Event, Reporter, RunPhase, RunStatus};
use ripsync_core::verify::{VerificationSummary, verify};
use ripsync_core::{Error, RunControl};

use args::{Args, OutputFormat};
use reporter::CliReporter;

/// The fully-built clap [`clap::Command`] for ripsync. Used by the man-page and
/// shell-completion generators (the hidden `_gen` subcommand and the `xtask`).
#[must_use]
pub fn command() -> clap::Command {
    Args::command()
}

/// Write the man page (`ripsync.1`) and bash/zsh/fish/powershell completions
/// into `dir`, creating it if needed.
///
/// # Errors
///
/// Returns an I/O error if the directory or any asset cannot be written.
pub fn write_assets(dir: &std::path::Path) -> std::io::Result<()> {
    use clap_complete::Shell;

    std::fs::create_dir_all(dir)?;

    let mut man_out = Vec::new();
    clap_mangen::Man::new(command()).render(&mut man_out)?;
    std::fs::write(dir.join("ripsync.1"), man_out)?;

    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
        clap_complete::generate_to(shell, &mut command(), "ripsync", dir)?;
    }
    Ok(())
}

/// Handle the hidden `ripsync _gen <man|completions> [shell]` subcommand by
/// streaming the requested asset to stdout. `rest` is argv after `_gen`.
fn run_gen(rest: &[String]) -> Result<()> {
    use std::io::stdout;

    match rest.first().map(String::as_str) {
        Some("man") => {
            clap_mangen::Man::new(command())
                .render(&mut stdout())
                .context("rendering man page")?;
        }
        Some("completions") => {
            let shell: clap_complete::Shell = rest
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: ripsync _gen completions <shell>"))?
                .parse()
                .map_err(|e| anyhow::anyhow!("unknown shell: {e}"))?;
            clap_complete::generate(shell, &mut command(), "ripsync", &mut stdout());
        }
        _ => bail!("usage: ripsync _gen <man|completions> [shell]"),
    }
    Ok(())
}

/// Process entry point shared by every binary: runs [`run`] and maps errors to
/// exit codes (130 on cancellation, 1 otherwise).
pub fn main() {
    if let Err(error) = run() {
        if error
            .downcast_ref::<Error>()
            .is_some_and(|e| matches!(e, Error::Cancelled))
        {
            std::process::exit(130);
        }
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Hidden asset generator, intercepted before clap so it does not appear in
    // help or pollute the flat argument parser: `ripsync _gen <man|completions>`.
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("_gen") {
        return run_gen(&raw[2..]);
    }

    // The far-end protocol peer is launched as `ripsync --server` with no
    // positional paths, so it must be intercepted before clap's required-args
    // parsing. It reads the role/root/options off the wire, not from argv.
    if raw.iter().skip(1).any(|a| a == "--server") {
        init_tracing(raw.iter().filter(|a| a.as_str() == "-v").count().min(255) as u8);
        return remote::run_server();
    }

    // Parse via matches so we can tell which flags were given on the command
    // line, then layer config-file defaults under them.
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;
    init_tracing(args.verbose);
    config::apply_defaults(&mut args, &matches);

    // `--bwlimit` throttles remote transfers (see remote::run_remote); it has no
    // effect on local copies. `--partial` is accepted (writes always go to a temp
    // file and atomically rename) but resume-from-partial is not yet implemented.
    if args.bwlimit.is_some() && !is_remote_run(&args) {
        tracing::warn!("--bwlimit has no effect on local copies; ignoring");
    }

    let threads = args.thread_count();
    let filter = build_filter(&args)?;

    // Remote transfer: exactly one of src/dst is a `[user@]host:path` spec.
    let src_loc = remote::parse_location(&args.src.to_string_lossy());
    let dst_loc = remote::parse_location(&args.dst.to_string_lossy());
    if src_loc.is_remote() || dst_loc.is_remote() {
        return remote::run_remote(&args, threads, &filter, &src_loc, &dst_loc);
    }

    // Watch mode: continuously re-sync on filesystem change (local only). It
    // drives the same one-shot pipeline after each debounced batch of events.
    if args.watch {
        return watch::run_watch(&args, threads, &filter);
    }

    let use_tui = !args.no_tui
        && args.output == OutputFormat::Human
        && !args.quiet
        && std::io::IsTerminal::is_terminal(&std::io::stdout());
    if use_tui {
        return tui::run(&args, threads, &filter);
    }

    run_local_once(&args, threads, &filter)
}

/// Execute one complete local sync: plan → optional delete approval → apply →
/// verify → persist the index. Shared by the one-shot path and `--watch`.
pub(crate) fn run_local_once(args: &Args, threads: usize, filter: &Filter) -> Result<()> {
    let reporter = CliReporter::new(args.output, args.quiet, args.verbose, args.dry_run);
    let control = RunControl::default();
    let started = std::time::Instant::now();
    let plan = build_plan_controlled(
        &args.src,
        &args.dst,
        PlanOptions {
            checksum: args.checksum,
            delete: args.delete,
            threads,
            index: args.index,
            hard_links: args.hard_links,
        },
        filter,
        &control,
        &reporter,
    )
    .with_context(|| {
        format!(
            "planning sync {} -> {}",
            args.src.display(),
            args.dst.display()
        )
    })?;

    // Delete guard: no destination mutation starts before destructive approval.
    let mut yes = args.yes;
    if args.delete {
        reporter.print_delete_preview(&plan);
        if !yes && !args.dry_run && !plan.deletions.is_empty() {
            if args.output == OutputFormat::Human
                && std::io::IsTerminal::is_terminal(&std::io::stdin())
            {
                eprint!("Type DELETE and press Enter to approve: ");
                let mut approval = String::new();
                std::io::stdin()
                    .read_line(&mut approval)
                    .context("reading delete approval")?;
                yes = approval.trim_end() == "DELETE";
                if !yes {
                    bail!("delete approval rejected before mutation");
                }
            } else {
                bail!("--delete requires --yes in noninteractive mode");
            }
        }
    }

    let metadata = MetadataOptions {
        hard_links: args.hard_links,
        sparse: args.sparse,
        xattrs: args.xattrs,
        acls: args.acls,
        owner: args.owner,
        group: args.group,
    };
    let stats = apply_plan_controlled(
        &plan,
        &args.src,
        &args.dst,
        ApplyOptions {
            dry_run: args.dry_run,
            yes,
            delete: args.delete,
            threads,
            reflink: args.reflink.into(),
            fsync: args.fsync.into(),
            backend: args.backend.into(),
            metadata,
        },
        &reporter,
        &control,
    )
    .context("applying sync plan")?;

    let verification = if !args.dry_run && stats.errors == 0 {
        verify(
            &plan,
            &args.src,
            &args.dst,
            args.verify.into(),
            metadata,
            threads,
            &control,
            &reporter,
        )?
    } else {
        VerificationSummary::default()
    };
    let deletes_complete = !args.delete || args.yes || plan.deletions.is_empty();
    let successful = stats.errors == 0 && verification.mismatches.is_empty();
    if args.index && !args.dry_run && successful && deletes_complete {
        reporter.event(Event::Phase(RunPhase::Finalizing));
        Manifest::persist_after_plan(&plan, &args.dst, args.checksum)
            .context("saving persistent index")?;
    }

    let status = if successful {
        RunStatus::Success
    } else {
        RunStatus::Failed
    };
    reporter.event(Event::Phase(if successful {
        RunPhase::Done
    } else {
        RunPhase::Failed
    }));
    reporter.event(Event::Finished { status });
    reporter.finish(
        &stats,
        &args.src.display().to_string(),
        &args.dst.display().to_string(),
        status,
        &verification,
    );

    if args.stats {
        print_stats(args, &stats, &verification, started.elapsed());
    }

    if !verification.mismatches.is_empty() {
        return Err(Error::Verification(verification.mismatches.len()).into());
    }
    if stats.errors > 0 {
        bail!("sync completed with {} per-entry error(s)", stats.errors);
    }
    Ok(())
}

/// Print a compact, color-free summary block for `--stats` (non-TUI runs).
fn print_stats(
    args: &Args,
    stats: &ripsync_core::report::Stats,
    verification: &VerificationSummary,
    elapsed: std::time::Duration,
) {
    let backend = match args.backend {
        args::BackendArg::Auto => "auto",
        args::BackendArg::Uring => "uring",
        args::BackendArg::Portable => "portable",
    };
    println!(
        "stats: copied {} updated {} skipped {} deleted {} errors {}",
        stats.copied, stats.updated, stats.skipped, stats.deleted, stats.errors
    );
    println!(
        "stats: {} in {:.3}s ({} backend, {} threads)",
        human_bytes(stats.bytes),
        elapsed.as_secs_f64(),
        backend,
        args.thread_count(),
    );
    if !verification.mismatches.is_empty() {
        println!(
            "stats: {} verification mismatch(es)",
            verification.mismatches.len()
        );
    }
}

/// Format a byte count with a binary unit suffix.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Build the path [`Filter`] from `--filter`/`--include`/`--exclude`/`--files-from`.
fn build_filter(args: &Args) -> Result<Filter> {
    let mut builder = FilterBuilder::default();
    for rule in &args.filter {
        builder.filter_rule(rule)?;
    }
    for pat in &args.include {
        builder.include(pat.clone());
    }
    for pat in &args.exclude {
        builder.exclude(pat.clone());
    }
    if let Some(path) = &args.files_from {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading --files-from {}", path.display()))?;
        let paths: Vec<PathBuf> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(PathBuf::from)
            .collect();
        builder.files_from(paths);
    }
    Ok(builder.build()?)
}

/// Whether either side of the transfer is a remote `[user@]host:path` spec.
fn is_remote_run(args: &Args) -> bool {
    remote::parse_location(&args.src.to_string_lossy()).is_remote()
        || remote::parse_location(&args.dst.to_string_lossy()).is_remote()
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
