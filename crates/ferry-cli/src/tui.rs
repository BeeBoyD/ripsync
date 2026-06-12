//! Live TUI dashboard (Phase 3). Until then, this falls back to plain output.

use anyhow::Result;

use ferry_core::apply::{ApplyOptions, apply_plan};
use ferry_core::plan::SyncPlan;

use crate::args::Args;
use crate::reporter::CliReporter;

/// Run a sync with the TUI front-end. (Placeholder: plain output for now.)
pub fn run(plan: &SyncPlan, args: &Args, threads: usize) -> Result<()> {
    let reporter = CliReporter::new(args.output, args.quiet, args.verbose, args.dry_run);
    if args.delete {
        reporter.print_delete_preview(plan);
    }
    let stats = apply_plan(
        plan,
        &args.src,
        &args.dst,
        ApplyOptions {
            dry_run: args.dry_run,
            yes: args.yes,
            delete: args.delete,
            threads,
        },
        &reporter,
    )?;
    reporter.finish(
        &stats,
        &args.src.display().to_string(),
        &args.dst.display().to_string(),
    );
    Ok(())
}
