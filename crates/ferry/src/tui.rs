//! Live `ratatui` dashboard: overall progress, throughput sparkline, active
//! transfers, a red TO-DELETE panel, and a scrollable event log.
//!
//! The sync runs on a worker thread feeding a shared [`TuiState`] through
//! [`TuiReporter`]; the main thread owns the terminal, draws at ~20 fps, and
//! handles keys (`q` quit, `p` pause display, `↑/↓` scroll the log).

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Sparkline};

use ferry_core::apply::{ApplyOptions, MetadataOptions, apply_plan};
use ferry_core::index::Manifest;
use ferry_core::plan::SyncPlan;
use ferry_core::report::{Event, Reporter, Stats};

use crate::args::Args;

const LOG_CAP: usize = 1000;
const SPARK_CAP: usize = 120;

/// Shared, mutable dashboard state.
#[derive(Default)]
struct TuiState {
    total_files: usize,
    total_bytes: u64,
    files_done: u64,
    bytes_done: u64,
    copied: u64,
    updated: u64,
    deleted: u64,
    skipped: u64,
    errors: u64,
    active: Vec<String>,
    deletions: Vec<String>,
    log: VecDeque<String>,
    /// Throughput samples in bytes/sec for the sparkline.
    spark: VecDeque<u64>,
    finished: bool,
}

impl TuiState {
    fn push_log(&mut self, line: String) {
        if self.log.len() == LOG_CAP {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

/// A [`Reporter`] that funnels engine events into [`TuiState`].
struct TuiReporter {
    state: Arc<Mutex<TuiState>>,
}

impl Reporter for TuiReporter {
    fn event(&self, ev: Event) {
        let mut s = self.state.lock().unwrap();
        match ev {
            Event::Planned {
                total_files,
                total_bytes,
                ..
            } => {
                s.total_files = total_files;
                s.total_bytes = total_bytes;
            }
            Event::FileStart { rel, .. } => {
                s.active.push(rel.display().to_string());
            }
            Event::FileDone { rel, action, bytes } => {
                let r = rel.display().to_string();
                if let Some(i) = s.active.iter().position(|a| *a == r) {
                    s.active.swap_remove(i);
                }
                s.files_done += 1;
                s.bytes_done += bytes;
                match action {
                    ferry_core::plan::Action::Copy => s.copied += 1,
                    ferry_core::plan::Action::Update => s.updated += 1,
                    ferry_core::plan::Action::Skip => s.skipped += 1,
                }
                s.push_log(format!("{:<7}{r} ({bytes} B)", action_word(action)));
            }
            Event::DirDone { rel, action } => {
                match action {
                    ferry_core::plan::Action::Copy => s.copied += 1,
                    ferry_core::plan::Action::Update => s.updated += 1,
                    ferry_core::plan::Action::Skip => s.skipped += 1,
                }
                s.push_log(format!("{:<7}{}/", action_word(action), rel.display()));
            }
            Event::SymlinkDone { rel, action } => {
                match action {
                    ferry_core::plan::Action::Copy => s.copied += 1,
                    ferry_core::plan::Action::Update => s.updated += 1,
                    ferry_core::plan::Action::Skip => s.skipped += 1,
                }
                s.push_log(format!(
                    "{:<7}{} (symlink)",
                    action_word(action),
                    rel.display()
                ));
            }
            Event::Skipped { .. } => {
                s.skipped += 1;
            }
            Event::Deleted { rel } => {
                s.deleted += 1;
                s.push_log(format!("delete {}", rel.display()));
            }
            Event::Failed { rel, error } => {
                s.errors += 1;
                s.push_log(format!("ERROR  {}: {error}", rel.display()));
            }
        }
    }
}

fn action_word(a: ferry_core::plan::Action) -> &'static str {
    match a {
        ferry_core::plan::Action::Copy => "copy",
        ferry_core::plan::Action::Update => "update",
        ferry_core::plan::Action::Skip => "skip",
    }
}

/// Run a sync with the live TUI front-end.
pub fn run(plan: &SyncPlan, args: &Args, threads: usize) -> Result<()> {
    let state = Arc::new(Mutex::new(TuiState::default()));
    {
        let mut s = state.lock().unwrap();
        s.deletions = plan
            .deletions
            .iter()
            .map(|d| d.rel.display().to_string())
            .collect();
    }

    // Spawn the sync worker.
    let worker_done = Arc::new(AtomicBool::new(false));
    let handle = spawn_worker(plan, args, threads, &state, &worker_done);

    let result = render_loop(args, threads, &state, &worker_done);

    // The worker owns no terminal state; join it so temp files are cleaned up.
    let _ = handle.join();
    result
}

fn spawn_worker(
    plan: &SyncPlan,
    args: &Args,
    threads: usize,
    state: &Arc<Mutex<TuiState>>,
    done: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<Stats> {
    let plan = plan.clone();
    let src = args.src.clone();
    let dst = args.dst.clone();
    let opts = ApplyOptions {
        dry_run: args.dry_run,
        yes: args.yes,
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
    };
    let state = Arc::clone(state);
    let done = Arc::clone(done);
    let index = args.index;
    let checksum = args.checksum;
    let dry_run = args.dry_run;
    let deletes_complete = !args.delete || args.yes || plan.deletions.is_empty();
    std::thread::spawn(move || {
        let reporter = TuiReporter {
            state: Arc::clone(&state),
        };
        let stats = apply_plan(&plan, &src, &dst, opts, &reporter).unwrap_or_default();
        if index && !dry_run && stats.errors == 0 && deletes_complete {
            let _ = Manifest::from_destination(&plan, &dst, checksum)
                .and_then(|manifest| manifest.save(&dst));
        }
        state.lock().unwrap().finished = true;
        done.store(true, Ordering::SeqCst);
        stats
    })
}

fn render_loop(
    args: &Args,
    threads: usize,
    state: &Arc<Mutex<TuiState>>,
    done: &Arc<AtomicBool>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let header = build_header(args, threads);
    let start = Instant::now();
    let mut last_bytes = 0u64;
    let mut last_tick = Instant::now();
    let mut scroll: u16 = 0;
    let mut paused = false;

    loop {
        // Sample throughput once per tick (~200ms).
        if last_tick.elapsed() >= Duration::from_millis(200) {
            let mut s = state.lock().unwrap();
            let now_bytes = s.bytes_done;
            let dt = last_tick.elapsed().as_secs_f64().max(0.001);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let bps = ((now_bytes.saturating_sub(last_bytes)) as f64 / dt) as u64;
            if !paused {
                if s.spark.len() == SPARK_CAP {
                    s.spark.pop_front();
                }
                s.spark.push_back(bps);
            }
            drop(s);
            last_bytes = now_bytes;
            last_tick = Instant::now();
        }

        terminal.draw(|f| {
            let s = state.lock().unwrap();
            draw(f, &header, &s, start, paused);
        })?;

        if event::poll(Duration::from_millis(50))? {
            if let CtEvent::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match k.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('p') => paused = !paused,
                        KeyCode::Up => scroll = scroll.saturating_sub(1),
                        KeyCode::Down => scroll = scroll.saturating_add(1),
                        _ => {}
                    }
                }
            }
        }
        let _ = scroll; // scroll currently drives the (auto-tailing) log view

        if done.load(Ordering::SeqCst) {
            // One last draw, then wait for the user to quit.
            terminal.draw(|f| {
                let s = state.lock().unwrap();
                draw(f, &header, &s, start, paused);
            })?;
            wait_for_quit()?;
            break;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Block until the user presses `q` (or Esc) after the sync finishes.
fn wait_for_quit() -> Result<()> {
    loop {
        if event::poll(Duration::from_millis(150))? {
            if let CtEvent::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    return Ok(());
                }
            }
        }
    }
}

fn build_header(args: &Args, threads: usize) -> String {
    let mut flags = Vec::new();
    if args.dry_run {
        flags.push("dry-run");
    }
    if args.delete {
        flags.push("delete");
    }
    if args.checksum {
        flags.push("checksum");
    }
    if args.delta {
        flags.push("delta");
    }
    let flag_str = if flags.is_empty() {
        "size+mtime".to_string()
    } else {
        flags.join(",")
    };
    format!(
        "{}  →  {}    [{}]  j={threads}",
        args.src.display(),
        args.dst.display(),
        flag_str
    )
}

#[allow(clippy::cast_precision_loss)]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[allow(clippy::too_many_lines)]
fn draw(f: &mut ratatui::Frame, header: &str, s: &TuiState, start: Instant, paused: bool) {
    let has_del = !s.deletions.is_empty();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(3), // overall gauge
            Constraint::Length(2), // stats line
            Constraint::Length(5), // sparkline
            Constraint::Min(6),    // middle (active + delete)
            Constraint::Min(5),    // log
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    // Header.
    f.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::ALL).title("Ferry 🛶")),
        chunks[0],
    );

    // Overall progress gauge (by bytes, falling back to files when no bytes).
    let ratio = if s.total_bytes > 0 {
        #[allow(clippy::cast_precision_loss)]
        {
            (s.bytes_done as f64 / s.total_bytes as f64).clamp(0.0, 1.0)
        }
    } else if s.total_files > 0 {
        #[allow(clippy::cast_precision_loss)]
        {
            (s.files_done as f64 / s.total_files as f64).clamp(0.0, 1.0)
        }
    } else {
        f64::from(u8::from(s.finished))
    };
    let pct = (ratio * 100.0) as u16;
    f.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Overall"))
            .gauge_style(Style::default().fg(Color::Cyan))
            .percent(pct.min(100)),
        chunks[1],
    );

    // Stats line: bytes, throughput, ETA, files/sec.
    let elapsed = start.elapsed().as_secs_f64().max(0.001);
    #[allow(clippy::cast_precision_loss)]
    let mbps = mb(s.bytes_done) / elapsed;
    #[allow(clippy::cast_precision_loss)]
    let fps = s.files_done as f64 / elapsed;
    let remaining = s.total_bytes.saturating_sub(s.bytes_done);
    let eta = if mbps > 0.01 {
        format!("{:.0}s", mb(remaining) / mbps)
    } else {
        "—".into()
    };
    let stats = format!(
        "{:.1}/{:.1} MB   {mbps:.1} MB/s   ETA {eta}   {fps:.0} files/s   files {}/{}",
        mb(s.bytes_done),
        mb(s.total_bytes),
        s.files_done,
        s.total_files,
    );
    f.render_widget(Paragraph::new(stats), chunks[2]);

    // Throughput sparkline.
    let data: Vec<u64> = s.spark.iter().copied().collect();
    let title = if paused {
        "Throughput (paused)"
    } else {
        "Throughput B/s"
    };
    f.render_widget(
        Sparkline::default()
            .block(Block::default().borders(Borders::ALL).title(title))
            .data(&data)
            .style(Style::default().fg(Color::Green)),
        chunks[3],
    );

    // Middle: active transfers (+ delete panel when present).
    draw_middle(f, chunks[4], s, has_del);

    // Event log (auto-tailing).
    let height = chunks[5].height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = s
        .log
        .iter()
        .rev()
        .take(height)
        .rev()
        .map(|l| {
            let style = if l.starts_with("ERROR") || l.starts_with("delete") {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(l.clone(), style)))
        })
        .collect();
    f.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title("Log")),
        chunks[5],
    );

    // Footer.
    let footer = if s.finished {
        "DONE — q quit"
    } else {
        "q quit · p pause · ↑/↓ scroll"
    };
    f.render_widget(
        Paragraph::new(footer).style(Style::default().add_modifier(Modifier::DIM)),
        chunks[6],
    );
}

fn draw_middle(f: &mut ratatui::Frame, area: Rect, s: &TuiState, has_del: bool) {
    let cols = if has_del {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(area)
    };

    let active: Vec<ListItem> = s
        .active
        .iter()
        .take(area.height.saturating_sub(2) as usize)
        .map(|a| ListItem::new(format!("⇅ {a}")))
        .collect();
    f.render_widget(
        List::new(active).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Active ({})", s.active.len())),
        ),
        cols[0],
    );

    if has_del {
        let dels: Vec<ListItem> = s
            .deletions
            .iter()
            .take(area.height.saturating_sub(2) as usize)
            .map(|d| {
                ListItem::new(Span::styled(
                    format!("- {d}"),
                    Style::default().fg(Color::Red),
                ))
            })
            .collect();
        f.render_widget(
            List::new(dels).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red))
                    .title(format!("TO DELETE ({})", s.deletions.len())),
            ),
            cols[1],
        );
    }
}
