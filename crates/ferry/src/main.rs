//! Ferry CLI entry point: parse args, build a plan, apply it.

/// Global allocator: mimalloc speeds up the alloc-heavy parallel walk. Opt out
/// with `--features system-malloc`.
#[cfg(not(feature = "system-malloc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod args;
mod reporter;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use globset::{Glob, GlobSet, GlobSetBuilder};

use ferry_core::apply::{ApplyOptions, MetadataOptions, apply_plan};
use ferry_core::index::Manifest;
use ferry_core::plan::{PlanOptions, build_plan};

use args::{Args, OutputFormat};
use reporter::CliReporter;

fn main() -> Result<()> {
    let args = Args::parse();
    init_tracing(args.verbose);

    if args.bwlimit.is_some() && args.verbose > 0 {
        eprintln!("note: --bwlimit is parsed but not yet enforced (later phase)");
    }
    if args.partial && args.verbose > 0 {
        eprintln!("note: --partial is parsed but not yet enforced (later phase)");
    }

    let threads = args.thread_count();
    let excludes = build_excludes(&args.exclude)?;

    let plan = build_plan(
        &args.src,
        &args.dst,
        PlanOptions {
            checksum: args.checksum,
            delete: args.delete,
            threads,
            index: args.index,
            hard_links: args.hard_links,
        },
        &excludes,
    )
    .with_context(|| {
        format!(
            "planning sync {} -> {}",
            args.src.display(),
            args.dst.display()
        )
    })?;

    // TUI path (Phase 3) — falls back to plain output with --no-tui, --output json,
    // or when stdout is not a terminal.
    let use_tui = !args.no_tui
        && args.output == OutputFormat::Human
        && !args.quiet
        && std::io::IsTerminal::is_terminal(&std::io::stdout());

    if use_tui {
        return tui::run(&plan, &args, threads);
    }

    let reporter = CliReporter::new(args.output, args.quiet, args.verbose, args.dry_run);

    // Delete guard: show the preview, and only actually delete with --yes.
    let mut yes = args.yes;
    if args.delete {
        reporter.print_delete_preview(&plan);
        if !yes && !args.dry_run && !plan.deletions.is_empty() {
            eprintln!(
                "refusing to delete {} entries without --yes (planning only)",
                plan.deletions.len()
            );
            yes = false;
        }
    }

    let stats = apply_plan(
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
            metadata: MetadataOptions {
                hard_links: args.hard_links,
                sparse: args.sparse,
                xattrs: args.xattrs,
                acls: args.acls,
                owner: args.owner,
                group: args.group,
            },
        },
        &reporter,
    )
    .context("applying sync plan")?;

    let deletes_complete = !args.delete || args.yes || plan.deletions.is_empty();
    if args.index && !args.dry_run && stats.errors == 0 && deletes_complete {
        Manifest::from_destination(&plan, &args.dst, args.checksum)?
            .save(&args.dst)
            .context("saving persistent index")?;
    }

    reporter.finish(
        &stats,
        &args.src.display().to_string(),
        &args.dst.display().to_string(),
    );

    if stats.errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Compile exclude globs. Each pattern matches both at the root and nested
/// (`PAT` and `**/PAT`), so `*.log` excludes log files anywhere in the tree.
fn build_excludes(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        builder.add(Glob::new(pat).with_context(|| format!("invalid exclude pattern: {pat}"))?);
        if !pat.contains("**") {
            let nested = format!("**/{pat}");
            builder.add(
                Glob::new(&nested).with_context(|| format!("invalid exclude pattern: {nested}"))?,
            );
        }
    }
    builder.build().context("compiling exclude patterns")
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
