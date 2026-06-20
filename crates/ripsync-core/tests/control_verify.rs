use std::path::Path;

use ripsync_core::Filter;
use ripsync_core::apply::{ApplyOptions, MetadataOptions, apply_plan_controlled};
use ripsync_core::plan::{PlanOptions, build_plan_controlled};
use ripsync_core::report::{Event, NullReporter, Reporter, RunPhase};
use ripsync_core::verify::{VerifyMode, verify};
use ripsync_core::{Error, RunControl};
use tempfile::tempdir;

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

struct CancelOn {
    control: RunControl,
    phase: Option<RunPhase>,
    file_start: bool,
}

impl Reporter for CancelOn {
    fn event(&self, event: Event) {
        let phase_matches = matches!(event, Event::Phase(phase) if Some(phase) == self.phase);
        if phase_matches || (self.file_start && matches!(event, Event::FileStart { .. })) {
            self.control.cancel();
        }
    }
}

#[test]
fn cancellation_during_planning_is_reported() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    write(&src.join("file"), b"content");
    let control = RunControl::default();
    let reporter = CancelOn {
        control: control.clone(),
        phase: Some(RunPhase::Planning),
        file_start: false,
    };
    let excludes = Filter::none();
    let result = build_plan_controlled(
        &src,
        &dst,
        PlanOptions::default(),
        &excludes,
        &control,
        &reporter,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[test]
fn cancellation_after_an_in_flight_copy_leaves_no_temp_file() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    write(&src.join("file"), &[7; 4096]);
    let excludes = Filter::none();
    let plan =
        ripsync_core::plan::build_plan(&src, &dst, PlanOptions::default(), &excludes).unwrap();
    let control = RunControl::default();
    let reporter = CancelOn {
        control: control.clone(),
        phase: None,
        file_start: true,
    };
    let result = apply_plan_controlled(
        &plan,
        &src,
        &dst,
        ApplyOptions {
            threads: 1,
            ..ApplyOptions::default()
        },
        &reporter,
        &control,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    let names: Vec<_> = std::fs::read_dir(&dst)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| !name.to_string_lossy().starts_with(".ripsync-tmp-"))
    );
}

#[test]
fn cancellation_before_deletion_keeps_stale_entry() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    write(&src.join("keep"), b"same");
    write(&dst.join("keep"), b"same");
    write(&dst.join("stale"), b"stale");
    let excludes = Filter::none();
    let plan = ripsync_core::plan::build_plan(
        &src,
        &dst,
        PlanOptions {
            delete: true,
            ..PlanOptions::default()
        },
        &excludes,
    )
    .unwrap();
    let control = RunControl::default();
    let reporter = CancelOn {
        control: control.clone(),
        phase: Some(RunPhase::Deleting),
        file_start: false,
    };
    let result = apply_plan_controlled(
        &plan,
        &src,
        &dst,
        ApplyOptions {
            delete: true,
            yes: true,
            ..ApplyOptions::default()
        },
        &reporter,
        &control,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(dst.join("stale").exists());
}

#[test]
fn verification_reports_content_mismatch_and_can_cancel() {
    let temp = tempdir().unwrap();
    let src = temp.path().join("src");
    let dst = temp.path().join("dst");
    write(&src.join("file"), b"source");
    write(&dst.join("file"), b"destination");
    let excludes = Filter::none();
    let plan =
        ripsync_core::plan::build_plan(&src, &dst, PlanOptions::default(), &excludes).unwrap();
    let summary = verify(
        &plan,
        &src,
        &dst,
        VerifyMode::All,
        MetadataOptions::default(),
        1,
        &RunControl::default(),
        &NullReporter,
    )
    .unwrap();
    assert!(!summary.mismatches.is_empty());

    let control = RunControl::default();
    let reporter = CancelOn {
        control: control.clone(),
        phase: Some(RunPhase::Verifying),
        file_start: false,
    };
    let result = verify(
        &plan,
        &src,
        &dst,
        VerifyMode::All,
        MetadataOptions::default(),
        1,
        &control,
        &reporter,
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}
