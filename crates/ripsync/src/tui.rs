//! Interactive lifecycle dashboard.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use globset::GlobSet;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Tabs, Wrap};
use ripsync_core::apply::{ApplyOptions, MetadataOptions, apply_plan_controlled};
use ripsync_core::index::Manifest;
use ripsync_core::plan::{Action, PlanOptions, build_plan_controlled};
use ripsync_core::report::{Event, Reporter, RunPhase, RunStatus};
use ripsync_core::verify::{VerificationSummary, verify};
use ripsync_core::{Error, RunControl};

use crate::args::Args;

const LOG_CAP: usize = 4000;

#[derive(Clone)]
struct Approval {
    state: Arc<(Mutex<Option<bool>>, Condvar)>,
}

impl Approval {
    fn new() -> Self {
        Self {
            state: Arc::new((Mutex::new(None), Condvar::new())),
        }
    }

    fn decide(&self, approved: bool) {
        let (lock, wake) = &*self.state;
        *lock.lock().unwrap() = Some(approved);
        wake.notify_all();
    }

    fn wait(&self, control: &RunControl) -> ripsync_core::Result<bool> {
        let (lock, wake) = &*self.state;
        let mut decision = lock.lock().unwrap();
        while decision.is_none() {
            if control.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let result = wake
                .wait_timeout(decision, Duration::from_millis(100))
                .unwrap();
            decision = result.0;
        }
        Ok(decision.unwrap_or(false))
    }
}

#[derive(Debug, Clone)]
struct LogLine {
    kind: LogKind,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogKind {
    Activity,
    Delete,
    Error,
    Verify,
}

struct TuiState {
    phase: RunPhase,
    backend: String,
    backend_reason: String,
    planning_entries: usize,
    total_files: usize,
    total_bytes: u64,
    files_done: u64,
    bytes_done: u64,
    copied: u64,
    updated: u64,
    deleted: u64,
    skipped: u64,
    errors: u64,
    verify_checked: usize,
    verify_total: usize,
    verify_errors: usize,
    active: Vec<String>,
    deletions: Vec<String>,
    log: VecDeque<LogLine>,
    started: Instant,
    finished: bool,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            phase: RunPhase::Planning,
            backend: "pending".into(),
            backend_reason: String::new(),
            planning_entries: 0,
            total_files: 0,
            total_bytes: 0,
            files_done: 0,
            bytes_done: 0,
            copied: 0,
            updated: 0,
            deleted: 0,
            skipped: 0,
            errors: 0,
            verify_checked: 0,
            verify_total: 0,
            verify_errors: 0,
            active: Vec::new(),
            deletions: Vec::new(),
            log: VecDeque::new(),
            started: Instant::now(),
            finished: false,
        }
    }
}

impl TuiState {
    fn log(&mut self, kind: LogKind, text: String) {
        if self.log.len() == LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(LogLine { kind, text });
    }
}

struct TuiReporter {
    state: Arc<Mutex<TuiState>>,
}

impl Reporter for TuiReporter {
    fn event(&self, event: Event) {
        let mut state = self.state.lock().unwrap();
        match event {
            Event::Phase(phase) => state.phase = phase,
            Event::PlanningProgress { entries } => state.planning_entries = entries,
            Event::BackendSelected { backend, reason } => {
                state.backend = backend.into();
                state.backend_reason = reason.into();
            }
            Event::Planned {
                total_files,
                total_bytes,
                deletions,
            } => {
                state.total_files = total_files;
                state.total_bytes = total_bytes;
                state.deletions.reserve(deletions);
            }
            Event::FileStart { rel, len } => {
                state
                    .active
                    .push(format!("{} ({})", rel.display(), size(len)));
            }
            Event::FileDone { rel, action, bytes } => {
                let prefix = rel.display().to_string();
                if let Some(index) = state
                    .active
                    .iter()
                    .position(|path| path.starts_with(&prefix))
                {
                    state.active.swap_remove(index);
                }
                state.files_done += 1;
                state.bytes_done += bytes;
                match action {
                    Action::Copy => state.copied += 1,
                    Action::Update => state.updated += 1,
                    Action::Skip => state.skipped += 1,
                }
                state.log(
                    LogKind::Activity,
                    format!(
                        "{} {} ({})",
                        action_word(action),
                        rel.display(),
                        size(bytes)
                    ),
                );
            }
            Event::DirDone { rel, action } | Event::SymlinkDone { rel, action } => {
                match action {
                    Action::Copy => state.copied += 1,
                    Action::Update => state.updated += 1,
                    Action::Skip => state.skipped += 1,
                }
                state.log(
                    LogKind::Activity,
                    format!("{} {}", action_word(action), rel.display()),
                );
            }
            Event::Skipped { .. } => state.skipped += 1,
            Event::Deleted { rel } => {
                state.deleted += 1;
                state.log(LogKind::Delete, format!("delete {}", rel.display()));
            }
            Event::Failed { rel, error } => {
                state.errors += 1;
                state.log(LogKind::Error, format!("{}: {error}", rel.display()));
            }
            Event::VerificationProgress {
                checked,
                total,
                mismatches,
            } => {
                state.verify_checked = checked;
                state.verify_total = total;
                state.verify_errors = mismatches;
            }
            Event::VerificationFailed { rel, detail } => {
                state.log(LogKind::Verify, format!("{}: {detail}", rel.display()));
            }
            Event::Finished { status } => {
                state.finished = true;
                state.phase = match status {
                    RunStatus::Success => RunPhase::Done,
                    RunStatus::Cancelled => RunPhase::Cancelled,
                    RunStatus::Failed => RunPhase::Failed,
                };
            }
        }
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[derive(Default)]
struct Ui {
    tab: usize,
    scroll: usize,
    filter: String,
    editing_filter: bool,
    event_filter: usize,
    help: bool,
    cancel_confirm: bool,
    delete_input: String,
}

/// Open the TUI before planning and execute the complete run lifecycle.
pub fn run(args: &Args, threads: usize, excludes: &GlobSet) -> Result<()> {
    let state = Arc::new(Mutex::new(TuiState::default()));
    let control = RunControl::default();
    let approval = Approval::new();
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    spawn_worker(
        args.clone(),
        threads,
        excludes.clone(),
        Arc::clone(&state),
        control.clone(),
        approval.clone(),
        result_tx,
    );

    let mut terminal = TerminalGuard::enter()?;
    let mut ui = Ui::default();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let result = loop {
        terminal
            .terminal
            .draw(|frame| draw(frame, args, threads, &state.lock().unwrap(), &ui, no_color))?;
        if let Ok(result) = result_rx.try_recv() {
            state.lock().unwrap().finished = true;
            loop {
                terminal.terminal.draw(|frame| {
                    draw(frame, args, threads, &state.lock().unwrap(), &ui, no_color);
                })?;
                if event::poll(Duration::from_millis(100))? {
                    if let CtEvent::Key(key) = event::read()? {
                        if key.kind == KeyEventKind::Press
                            && matches!(
                                key.code,
                                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter
                            )
                        {
                            break;
                        }
                    }
                }
            }
            break result;
        }
        if event::poll(Duration::from_millis(50))? {
            if let CtEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(key, &mut ui, &state, &control, &approval);
                }
            }
        }
    };
    drop(terminal);
    result.map_err(anyhow::Error::from)
}

fn spawn_worker(
    args: Args,
    threads: usize,
    excludes: GlobSet,
    state: Arc<Mutex<TuiState>>,
    control: RunControl,
    approval: Approval,
    result: mpsc::SyncSender<ripsync_core::Result<()>>,
) {
    std::thread::spawn(move || {
        let reporter = TuiReporter {
            state: Arc::clone(&state),
        };
        let outcome = run_worker(
            &args, threads, &excludes, &state, &control, &approval, &reporter,
        );
        let status = match outcome {
            Ok(()) => RunStatus::Success,
            Err(Error::Cancelled) => RunStatus::Cancelled,
            Err(_) => RunStatus::Failed,
        };
        reporter.event(Event::Finished { status });
        let _ = result.send(outcome);
    });
}

fn run_worker<R: Reporter>(
    args: &Args,
    threads: usize,
    excludes: &GlobSet,
    state: &Arc<Mutex<TuiState>>,
    control: &RunControl,
    approval: &Approval,
    reporter: &R,
) -> ripsync_core::Result<()> {
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
        excludes,
        control,
        reporter,
    )?;
    state.lock().unwrap().deletions = plan
        .deletions
        .iter()
        .map(|deletion| deletion.rel.display().to_string())
        .collect();
    let mut yes = args.yes;
    if args.delete && !args.yes && !args.dry_run && !plan.deletions.is_empty() {
        reporter.event(Event::Phase(RunPhase::Review));
        yes = approval.wait(control)?;
        if !yes {
            return Err(Error::Cancelled);
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
        reporter,
        control,
    )?;
    let verification = if args.dry_run || stats.errors > 0 {
        VerificationSummary::default()
    } else {
        verify(
            &plan,
            &args.src,
            &args.dst,
            args.verify.into(),
            metadata,
            threads,
            control,
            reporter,
        )?
    };
    if stats.errors > 0 {
        return Err(Error::Verification(stats.errors as usize));
    }
    if !verification.mismatches.is_empty() {
        return Err(Error::Verification(verification.mismatches.len()));
    }
    if args.index && !args.dry_run {
        reporter.event(Event::Phase(RunPhase::Finalizing));
        control.checkpoint()?;
        Manifest::persist_after_plan(&plan, &args.dst, args.checksum)?;
    }
    reporter.event(Event::Phase(RunPhase::Done));
    Ok(())
}

fn handle_key(
    key: KeyEvent,
    ui: &mut Ui,
    state: &Arc<Mutex<TuiState>>,
    control: &RunControl,
    approval: &Approval,
) {
    if ui.editing_filter {
        match key.code {
            KeyCode::Esc => {
                ui.editing_filter = false;
                ui.filter.clear();
            }
            KeyCode::Enter => ui.editing_filter = false,
            KeyCode::Backspace => {
                ui.filter.pop();
            }
            KeyCode::Char(ch) => ui.filter.push(ch),
            _ => {}
        }
        return;
    }
    if ui.cancel_confirm {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => control.cancel(),
            KeyCode::Char('n') | KeyCode::Esc => ui.cancel_confirm = false,
            _ => {}
        }
        return;
    }
    if state.lock().unwrap().phase == RunPhase::Review {
        match key.code {
            KeyCode::Esc => approval.decide(false),
            KeyCode::Backspace => {
                ui.delete_input.pop();
            }
            KeyCode::Enter if ui.delete_input == "DELETE" => approval.decide(true),
            KeyCode::Char(ch) => ui.delete_input.push(ch),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Tab => ui.tab = (ui.tab + 1) % 4,
        KeyCode::Char(ch @ '1'..='4') => ui.tab = ch as usize - '1' as usize,
        KeyCode::Char('p') => {
            if control.is_paused() {
                control.resume();
            } else {
                control.pause();
            }
        }
        KeyCode::Char('q') => ui.cancel_confirm = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            ui.cancel_confirm = true;
        }
        KeyCode::Char('/') => ui.editing_filter = true,
        KeyCode::Char('f') => ui.event_filter = (ui.event_filter + 1) % 5,
        KeyCode::Char('?') => ui.help = !ui.help,
        KeyCode::Esc => {
            ui.help = false;
            ui.filter.clear();
        }
        KeyCode::Down | KeyCode::Char('j') => ui.scroll = ui.scroll.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => ui.scroll = ui.scroll.saturating_sub(1),
        KeyCode::PageDown => ui.scroll = ui.scroll.saturating_add(10),
        KeyCode::PageUp => ui.scroll = ui.scroll.saturating_sub(10),
        KeyCode::Home => ui.scroll = 0,
        KeyCode::End => ui.scroll = usize::MAX / 2,
        _ => {}
    }
}

fn draw(
    frame: &mut ratatui::Frame,
    args: &Args,
    threads: usize,
    state: &TuiState,
    ui: &Ui,
    no_color: bool,
) {
    let area = frame.area();
    let compact = area.height < 24 || area.width < 80;
    let rows = if compact {
        vec![
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(1),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(rows)
        .split(area);
    let color = |wanted| if no_color { Color::Reset } else { wanted };
    let options = format!(
        "phase={} backend={} verify={:?} profile={} j={}{}{}",
        phase_word(state.phase),
        state.backend,
        args.verify,
        args.resolve_profile().name(),
        threads,
        if args.delete { " delete" } else { "" },
        if args.dry_run { " dry-run" } else { "" }
    );
    frame.render_widget(
        Paragraph::new(format!(
            "{} -> {}\n{}",
            args.src.display(),
            args.dst.display(),
            options
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(concat!("ripsync ", env!("CARGO_PKG_VERSION"))),
        ),
        chunks[0],
    );
    let entry_ratio = ratio(state.files_done, state.total_files as u64);
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Entries"))
            .ratio(entry_ratio)
            .gauge_style(Style::default().fg(color(Color::Cyan))),
        chunks[1],
    );
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Bytes"))
            .ratio(ratio(state.bytes_done, state.total_bytes))
            .gauge_style(Style::default().fg(color(Color::Green))),
        chunks[2],
    );
    let elapsed = state.started.elapsed().as_secs_f64().max(0.001);
    let throughput = state.bytes_done as f64 / elapsed;
    let eta = if throughput > 0.0 {
        format!(
            "{:.1}s",
            state.total_bytes.saturating_sub(state.bytes_done) as f64 / throughput
        )
    } else {
        "-".into()
    };
    let metrics = format!(
        "{} / {} | {}/s | {:.0} files/s | elapsed {:.1}s | ETA {} | errors {} | planning {}",
        size(state.bytes_done),
        size(state.total_bytes),
        size(throughput as u64),
        state.files_done as f64 / elapsed,
        elapsed,
        eta,
        state.errors + state.verify_errors as u64,
        state.planning_entries
    );
    let tab_row = if compact { 3 } else { 4 };
    if !compact {
        frame.render_widget(Paragraph::new(metrics), chunks[3]);
    }
    frame.render_widget(
        Tabs::new(["Summary", "Activity", "Deletes", "Errors/Verify"])
            .select(ui.tab)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[tab_row],
    );
    let body = chunks[tab_row + 1];
    draw_body(frame, body, state, ui, color);
    let footer = if state.phase == RunPhase::Review {
        format!(
            "Type DELETE and Enter to approve; Esc cancels [{}]",
            ui.delete_input
        )
    } else if ui.editing_filter {
        format!("filter: {}", ui.filter)
    } else if control_hint(state) {
        "Done. q closes/cancels | ? keys".into()
    } else {
        "Tab/1-4 views | p pause | q/Ctrl-C cancel | / filter | f event filter | ? keys".into()
    };
    frame.render_widget(Paragraph::new(footer), *chunks.last().unwrap());
    if ui.help {
        overlay(
            frame,
            area,
            "Keys",
            "Tab/1-4 views\np pause/resume\nq or Ctrl-C cancel\nj/k/arrows/Page/Home/End navigate\n/ filter\nf event type\nEsc close/clear",
        );
    } else if ui.cancel_confirm {
        overlay(
            frame,
            area,
            "Cancel",
            "Cancel gracefully? Enter/y confirms; Esc/n returns.",
        );
    }
}

fn draw_body(
    frame: &mut ratatui::Frame,
    area: Rect,
    state: &TuiState,
    ui: &Ui,
    color: impl Fn(Color) -> Color,
) {
    match ui.tab {
        0 => {
            let text = format!(
                "copied {}  updated {}  skipped {}  deleted {}\nverification {}/{} mismatches {}\nbackend reason: {}\nactive: {}",
                state.copied,
                state.updated,
                state.skipped,
                state.deleted,
                state.verify_checked,
                state.verify_total,
                state.verify_errors,
                state.backend_reason,
                state.active.join(", ")
            );
            frame.render_widget(
                Paragraph::new(text)
                    .block(Block::default().borders(Borders::ALL))
                    .wrap(Wrap { trim: true }),
                area,
            );
        }
        2 => draw_strings(
            frame,
            area,
            "Deletes",
            &state.deletions,
            ui,
            color(Color::Red),
        ),
        _ => {
            let wanted = if ui.tab == 3 {
                Some([LogKind::Error, LogKind::Verify].as_slice())
            } else {
                None
            };
            let event_kind = match ui.event_filter {
                1 => Some(LogKind::Activity),
                2 => Some(LogKind::Delete),
                3 => Some(LogKind::Error),
                4 => Some(LogKind::Verify),
                _ => None,
            };
            let lines: Vec<String> = state
                .log
                .iter()
                .filter(|line| wanted.is_none_or(|kinds| kinds.contains(&line.kind)))
                .filter(|line| event_kind.is_none_or(|kind| line.kind == kind))
                .filter(|line| ui.filter.is_empty() || line.text.contains(&ui.filter))
                .map(|line| line.text.clone())
                .collect();
            draw_strings(frame, area, "Events", &lines, ui, Color::Reset);
        }
    }
}

fn draw_strings(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    values: &[String],
    ui: &Ui,
    color: Color,
) {
    let height = area.height.saturating_sub(2) as usize;
    let max_scroll = values.len().saturating_sub(height);
    let scroll = ui.scroll.min(max_scroll);
    let items = values
        .iter()
        .skip(scroll)
        .take(height)
        .map(|value| ListItem::new(value.as_str()).style(Style::default().fg(color)));
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{title} ({})", values.len())),
        ),
        area,
    );
}

fn overlay(frame: &mut ratatui::Frame, area: Rect, title: &str, text: &str) {
    let popup = centered(area, 60, 40);
    frame.render_widget(ratatui::widgets::Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - height) / 2),
        Constraint::Percentage(height),
        Constraint::Percentage((100 - height) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - width) / 2),
        Constraint::Percentage(width),
        Constraint::Percentage((100 - width) / 2),
    ])
    .split(vertical[1])[1]
}

fn action_word(action: Action) -> &'static str {
    match action {
        Action::Copy => "copy",
        Action::Update => "update",
        Action::Skip => "skip",
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

fn ratio(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn control_hint(state: &TuiState) -> bool {
    state.finished
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::{TuiState, Ui, draw};
    use crate::args::Args;

    #[test]
    fn renders_lifecycle_states_at_supported_sizes() {
        let args = Args::parse_from(["ripsync", "src", "dst", "--verify", "changed"]);
        for (width, height) in [(60, 18), (100, 30), (160, 45)] {
            for phase in [
                ripsync_core::RunPhase::Planning,
                ripsync_core::RunPhase::Review,
                ripsync_core::RunPhase::Copying,
                ripsync_core::RunPhase::Deleting,
                ripsync_core::RunPhase::Verifying,
                ripsync_core::RunPhase::Finalizing,
                ripsync_core::RunPhase::Done,
                ripsync_core::RunPhase::Cancelled,
                ripsync_core::RunPhase::Failed,
            ] {
                let state = TuiState {
                    phase,
                    total_files: 10,
                    files_done: 4,
                    total_bytes: 1000,
                    bytes_done: 400,
                    deletions: vec!["old/file".into()],
                    ..TuiState::default()
                };
                let ui = Ui {
                    tab: phase as usize % 4,
                    help: phase == ripsync_core::RunPhase::Planning,
                    cancel_confirm: phase == ripsync_core::RunPhase::Cancelled,
                    ..Ui::default()
                };
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend).unwrap();
                terminal
                    .draw(|frame| draw(frame, &args, 4, &state, &ui, false))
                    .unwrap();
            }
        }
    }
}
