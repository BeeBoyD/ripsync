//! CLI-side rendering of engine [`Event`]s: plain line output and a JSON final
//! report. (The live TUI is a separate reporter, added in Phase 3.)

use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

use ripsync_core::plan::{Action, SyncPlan};
use ripsync_core::report::{Event, Reporter, RunPhase, RunStatus, Stats};
use ripsync_core::verify::VerificationSummary;

use crate::args::OutputFormat;

/// Renders sync progress to the terminal or as JSON.
pub struct CliReporter {
    format: OutputFormat,
    quiet: bool,
    verbose: u8,
    dry_run: bool,
    /// Serializes plain-text writes from parallel workers.
    lock: Mutex<()>,
    /// Accumulated per-entry records for the JSON report.
    json: Mutex<Vec<serde_json::Value>>,
    lifecycle: Mutex<Lifecycle>,
}

struct Lifecycle {
    phase: RunPhase,
    phase_started: Instant,
    timings: serde_json::Map<String, serde_json::Value>,
    backend: Option<String>,
    backend_reason: Option<String>,
}

impl CliReporter {
    /// Build a reporter for the given format and verbosity.
    #[must_use]
    pub fn new(format: OutputFormat, quiet: bool, verbose: u8, dry_run: bool) -> Self {
        Self {
            format,
            quiet,
            verbose,
            dry_run,
            lock: Mutex::new(()),
            json: Mutex::new(Vec::new()),
            lifecycle: Mutex::new(Lifecycle {
                phase: RunPhase::Planning,
                phase_started: Instant::now(),
                timings: serde_json::Map::new(),
                backend: None,
                backend_reason: None,
            }),
        }
    }

    fn prefix(&self) -> &'static str {
        if self.dry_run { "[dry] " } else { "" }
    }

    fn line(&self, msg: &str) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}{msg}", self.prefix());
    }

    fn record(&self, kind: &str, rel: &str, extra: serde_json::Value) {
        let mut obj = serde_json::Map::new();
        obj.insert("event".into(), kind.into());
        obj.insert("path".into(), rel.into());
        if let serde_json::Value::Object(m) = extra {
            obj.extend(m);
        }
        self.json
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(serde_json::Value::Object(obj));
    }

    fn action_word(action: Action) -> &'static str {
        match action {
            Action::Copy => "copy",
            Action::Update => "update",
            Action::Skip => "skip",
        }
    }

    /// Print the delete-preview panel (plain mode) before applying.
    pub fn print_delete_preview(&self, plan: &SyncPlan) {
        if plan.deletions.is_empty() || self.format == OutputFormat::Json || self.quiet {
            return;
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "\n\x1b[1;31mTO DELETE ({}):\x1b[0m",
            plan.deletions.len()
        );
        for d in &plan.deletions {
            let _ = writeln!(out, "  \x1b[31m- {}\x1b[0m", d.rel.display());
        }
        let _ = writeln!(out);
    }

    /// Emit the final report (human summary or JSON document).
    pub fn finish(
        &self,
        stats: &Stats,
        src: &str,
        dst: &str,
        status: RunStatus,
        verification: &VerificationSummary,
    ) {
        match self.format {
            OutputFormat::Human => self.finish_human(stats),
            OutputFormat::Json => self.finish_json(stats, src, dst, status, verification),
        }
    }

    fn finish_human(&self, s: &Stats) {
        if self.quiet {
            return;
        }
        let verb = if self.dry_run { "would " } else { "" };
        println!(
            "\n{}copy {} · {}update {} · {}delete {} · skip {} · errors {} · {:.2} MB",
            verb,
            s.copied,
            verb,
            s.updated,
            verb,
            s.deleted,
            s.skipped,
            s.errors,
            bytes_to_mb(s.bytes),
        );
    }

    fn finish_json(
        &self,
        s: &Stats,
        src: &str,
        dst: &str,
        status: RunStatus,
        verification: &VerificationSummary,
    ) {
        let events = self.json.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        let report = serde_json::json!({
            "src": src,
            "dst": dst,
            "dry_run": self.dry_run,
            "status": status_word(status),
            "cancelled": status == RunStatus::Cancelled,
            "phase_timings_ms": lifecycle.timings,
            "backend": {
                "selected": lifecycle.backend,
                "reason": lifecycle.backend_reason,
            },
            "verification": {
                "checked": verification.checked,
                "mismatches": verification.mismatches.iter().map(|m| serde_json::json!({
                    "path": m.rel,
                    "detail": m.detail,
                })).collect::<Vec<_>>(),
            },
            "summary": {
                "copied": s.copied,
                "updated": s.updated,
                "deleted": s.deleted,
                "skipped": s.skipped,
                "errors": s.errors,
                "bytes": s.bytes,
            },
            "events": events,
        });
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }
}

#[allow(clippy::cast_precision_loss)]
fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

impl Reporter for CliReporter {
    fn event(&self, ev: Event) {
        let json = self.format == OutputFormat::Json;
        match ev {
            Event::Phase(phase) => {
                let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
                let elapsed = u64::try_from(lifecycle.phase_started.elapsed().as_millis())
                    .unwrap_or(u64::MAX);
                let previous = phase_word(lifecycle.phase).to_string();
                lifecycle.timings.insert(previous, elapsed.into());
                lifecycle.phase = phase;
                lifecycle.phase_started = Instant::now();
            }
            Event::BackendSelected { backend, reason } => {
                let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
                lifecycle.backend = Some(backend.into());
                lifecycle.backend_reason = Some(reason.into());
            }
            Event::Planned { .. }
            | Event::PlanningProgress { .. }
            | Event::FileStart { .. }
            | Event::VerificationProgress { .. }
            | Event::Finished { .. } => {}
            Event::FileDone { rel, action, bytes } => {
                if json {
                    self.record(
                        Self::action_word(action),
                        &rel.display().to_string(),
                        serde_json::json!({ "bytes": bytes }),
                    );
                } else if !self.quiet {
                    self.line(&format!(
                        "{:<7}{} ({bytes} B)",
                        Self::action_word(action),
                        rel.display()
                    ));
                }
            }
            Event::DirDone { rel, action } => {
                if json {
                    self.record("mkdir", &rel.display().to_string(), serde_json::json!({}));
                } else if !self.quiet && self.verbose >= 1 {
                    self.line(&format!(
                        "{:<7}{}/",
                        Self::action_word(action),
                        rel.display()
                    ));
                }
            }
            Event::SymlinkDone { rel, action } => {
                if json {
                    self.record("symlink", &rel.display().to_string(), serde_json::json!({}));
                } else if !self.quiet {
                    self.line(&format!(
                        "{:<7}{} (symlink)",
                        Self::action_word(action),
                        rel.display()
                    ));
                }
            }
            Event::Skipped { rel } => {
                if json {
                    self.record("skip", &rel.display().to_string(), serde_json::json!({}));
                } else if !self.quiet && self.verbose >= 2 {
                    self.line(&format!("{:<7}{}", "skip", rel.display()));
                }
            }
            Event::Deleted { rel } => {
                if json {
                    self.record("delete", &rel.display().to_string(), serde_json::json!({}));
                } else if !self.quiet {
                    self.line(&format!("\x1b[31m{:<7}{}\x1b[0m", "delete", rel.display()));
                }
            }
            Event::Failed { rel, error } => {
                if json {
                    self.record(
                        "error",
                        &rel.display().to_string(),
                        serde_json::json!({ "error": error }),
                    );
                } else {
                    let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                    let mut err = std::io::stderr().lock();
                    let _ = writeln!(err, "\x1b[31merror\x1b[0m  {}: {error}", rel.display());
                }
            }
            Event::VerificationFailed { rel, detail } => {
                if json {
                    self.record(
                        "verification_error",
                        &rel.display().to_string(),
                        serde_json::json!({ "detail": detail }),
                    );
                } else {
                    let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
                    eprintln!("verify  {}: {detail}", rel.display());
                }
            }
        }
    }
}

fn phase_word(phase: RunPhase) -> &'static str {
    match phase {
        RunPhase::Planning => "planning",
        RunPhase::Review => "review",
        RunPhase::Copying => "copying",
        RunPhase::Deleting => "deleting",
        RunPhase::Verifying => "verifying",
        RunPhase::Finalizing => "finalizing",
        RunPhase::Done => "done",
        RunPhase::Cancelled => "cancelled",
        RunPhase::Failed => "failed",
    }
}

fn status_word(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Success => "success",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
    }
}
