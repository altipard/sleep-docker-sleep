//! Terminal dashboard for `sds run`, `sds top` and `sds top --demo` (interactive sessions only).

mod demo;
mod detail;
mod history;

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use tokio_util::sync::CancellationToken;

use crate::controller::{EventKind, GroupSnapshot, Registry, SdsEvent, StateKind, epoch_ms};
pub use demo::DemoSource;
use history::{Countdown, RateHistory, bar_cells, countdown, fmt_compact};

/// Where the dashboard gets its data from: the in-process registry (`sds run`) or a
/// running daemon's API (`sds top`).
pub trait Source: Send {
    fn snapshots(&self) -> Vec<GroupSnapshot>;
    fn logs(&self) -> Vec<String>;
    /// Structured recent events of all groups, oldest first (the detail panel filters them).
    fn events(&self) -> Vec<SdsEvent> {
        Vec::new()
    }
    /// The source's idea of "now" in epoch milliseconds; countdowns and ages are relative to it.
    fn now_ms(&self) -> u64 {
        epoch_ms()
    }
    fn wake(&self, group: &str);
    fn sleep(&self, group: &str);
    /// `true` puts the group on hold, `false` releases it.
    fn set_hold(&self, group: &str, held: bool);
    fn docker_endpoint(&self) -> String;
    /// Shown in the footer, e.g. the web overview URL.
    fn location(&self) -> String;
    /// `Some(reason)` while the source is unreachable.
    fn problem(&self) -> Option<String> {
        None
    }
    /// Whether `x` (detach, keep running) is meaningful for this source.
    fn supports_detach(&self) -> bool {
        false
    }
    /// Simulated data: the UI marks itself so it cannot be mistaken for a real daemon.
    fn is_demo(&self) -> bool {
        false
    }
}

/// One line of the log pane for a structured event.
fn format_event(ev: &SdsEvent) -> String {
    let secs = ev.ts_ms / 1000;
    let t = humantime::format_rfc3339_seconds(std::time::UNIX_EPOCH + Duration::from_secs(secs));
    let level = match ev.kind {
        EventKind::StartFailed | EventKind::StopFailed | EventKind::Warning => " WARN",
        _ => " INFO",
    };
    format!("{t}{level} {} [{}] {}", ev.group, format!("{:?}", ev.kind).to_lowercase(), ev.message)
}

/// In-process source for `sds run`.
pub struct LocalSource {
    pub registry: Arc<Registry>,
    pub logs: LogBuffer,
    pub bind: SocketAddr,
}

impl Source for LocalSource {
    fn snapshots(&self) -> Vec<GroupSnapshot> {
        self.registry.snapshots()
    }
    fn logs(&self) -> Vec<String> {
        self.logs.lines()
    }
    fn events(&self) -> Vec<SdsEvent> {
        self.registry.bus().recent()
    }
    fn wake(&self, group: &str) {
        if let Some(h) = self.registry.get(group) {
            h.force_wake();
        }
    }
    fn sleep(&self, group: &str) {
        if let Some(h) = self.registry.get(group) {
            h.request_sleep();
        }
    }
    fn set_hold(&self, group: &str, held: bool) {
        self.registry.set_hold(group, held);
    }
    fn docker_endpoint(&self) -> String {
        self.registry.docker_endpoint.clone()
    }
    fn location(&self) -> String {
        format!("web overview: http://{}", self.bind)
    }
    fn supports_detach(&self) -> bool {
        true
    }
}

/// Remote source for `sds top`: a background task polls `/api/v1/status` and executes
/// wake/sleep/hold requests; the dashboard thread only reads the latest copy.
pub struct RemoteSource {
    latest: Arc<Mutex<Option<crate::admin::api::StatusResponse>>>,
    error: Arc<Mutex<Option<String>>>,
    actions: tokio::sync::mpsc::UnboundedSender<(Action, String)>,
    base: String,
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Wake,
    Sleep,
    Hold(bool),
}

impl RemoteSource {
    /// Start polling. The first poll has already succeeded when this returns.
    pub async fn connect(client: crate::cli::ApiClient, cancel: CancellationToken) -> anyhow::Result<Self> {
        let first = client.status().await?;
        let latest = Arc::new(Mutex::new(Some(first)));
        let error = Arc::new(Mutex::new(None));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Action, String)>();
        let base = client.base().to_owned();
        let (latest_bg, error_bg) = (Arc::clone(&latest), Arc::clone(&error));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tick.tick() => match client.status().await {
                        Ok(s) => {
                            if let Ok(mut l) = latest_bg.lock() { *l = Some(s); }
                            if let Ok(mut e) = error_bg.lock() { *e = None; }
                        }
                        Err(e) => {
                            if let Ok(mut err) = error_bg.lock() { *err = Some(e.to_string()); }
                        }
                    },
                    Some((action, group)) = rx.recv() => {
                        let result = match action {
                            Action::Wake => client.wake(&group, false).await,
                            Action::Sleep => client.sleep(&group).await,
                            Action::Hold(held) => client.set_hold(&group, held).await,
                        };
                        if let Err(e) = result
                            && let Ok(mut err) = error_bg.lock()
                        {
                            *err = Some(e.to_string());
                        }
                    }
                }
            }
        });
        Ok(Self { latest, error, actions: tx, base })
    }
}

impl Source for RemoteSource {
    fn snapshots(&self) -> Vec<GroupSnapshot> {
        self.latest.lock().ok().and_then(|l| l.as_ref().map(|s| s.groups.clone())).unwrap_or_default()
    }
    fn logs(&self) -> Vec<String> {
        self.events().iter().map(format_event).collect()
    }
    fn events(&self) -> Vec<SdsEvent> {
        self.latest.lock().ok().and_then(|l| l.as_ref().map(|s| s.events.clone())).unwrap_or_default()
    }
    fn wake(&self, group: &str) {
        let _ = self.actions.send((Action::Wake, group.to_owned()));
    }
    fn sleep(&self, group: &str) {
        let _ = self.actions.send((Action::Sleep, group.to_owned()));
    }
    fn set_hold(&self, group: &str, held: bool) {
        let _ = self.actions.send((Action::Hold(held), group.to_owned()));
    }
    fn docker_endpoint(&self) -> String {
        self.latest.lock().ok().and_then(|l| l.as_ref().map(|s| s.docker.clone())).unwrap_or_default()
    }
    fn location(&self) -> String {
        format!("attached to {}", self.base)
    }
    fn problem(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
}

/// Ring buffer that receives `tracing` output while the dashboard owns the terminal.
/// After [`LogBuffer::detach`] lines go to stderr instead (headless mode).
#[derive(Clone, Debug)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
    keep: usize,
    detached: Arc<std::sync::atomic::AtomicBool>,
}

impl LogBuffer {
    pub fn new(keep: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(keep))),
            keep,
            detached: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.inner.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    /// Switch to headless output: every further line is written to stderr with a
    /// timestamp, and the buffer keeps collecting for a later `sds top`.
    pub fn detach(&self) {
        self.detached.store(true, std::sync::atomic::Ordering::Release);
    }

    fn push(&self, line: String) {
        if self.detached.load(std::sync::atomic::Ordering::Acquire) {
            let ts = humantime::format_rfc3339_seconds(std::time::SystemTime::now());
            eprintln!("{ts} {line}");
        }
        if let Ok(mut l) = self.inner.lock() {
            if l.len() == self.keep {
                l.pop_front();
            }
            l.push_back(line);
        }
    }
}

/// How the dashboard was left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// `q`: stop whatever owns the dashboard.
    Quit,
    /// `x`: leave the dashboard but keep the daemon running headless.
    Detach,
}

pub struct LogWriter {
    buffer: LogBuffer,
    pending: Vec<u8>,
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line).trim_end().to_owned();
            if !text.is_empty() {
                self.buffer.push(text);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter { buffer: self.clone(), pending: Vec::new() }
    }
}

pub struct Dashboard {
    source: Box<dyn Source>,
    table: TableState,
    /// `q` was pressed once in a session that owns the daemon; waiting for confirmation.
    confirm_quit: bool,
    /// Requests per second per group, derived from `requests_total` between polls.
    history: HashMap<String, RateHistory>,
}

/// Below this width the detail panel goes under the table instead of next to it.
const WIDE: u16 = 110;
/// Rows of the log pane; it is dropped when the terminal is too short to spare them.
const LOG_ROWS: u16 = 10;

struct Areas {
    header: Rect,
    table: Rect,
    detail: Rect,
    log: Option<Rect>,
    footer: Rect,
}

fn areas(area: Rect, groups: usize) -> Areas {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(area);
    // The panel replaces most of what the log pane showed per group, so on 80x24 it gets
    // the space; with 30+ rows the log pane comes back.
    let log_rows = match body.height {
        0..28 => 0,
        28..36 => LOG_ROWS - 3,
        _ => (body.height / 3).clamp(LOG_ROWS, 16),
    };
    let (main, log) = if log_rows > 0 {
        let [main, log] = Layout::vertical([Constraint::Min(0), Constraint::Length(log_rows)]).areas(body);
        (main, Some(log))
    } else {
        (body, None)
    };
    let (table, detail) = if area.width >= WIDE {
        let [table, detail] = Layout::horizontal([Constraint::Length(68), Constraint::Min(30)]).areas(main);
        (table, detail)
    } else {
        let want = u16::try_from(groups).unwrap_or(u16::MAX).saturating_add(3);
        let rows = want.clamp(5, (main.height / 2).max(5));
        let [table, detail] = Layout::vertical([Constraint::Length(rows), Constraint::Min(0)]).areas(main);
        (table, detail)
    };
    Areas { header, table, detail, log, footer }
}

impl Dashboard {
    pub fn new(source: Box<dyn Source>) -> Self {
        Self { source, table: TableState::default().with_selected(0), confirm_quit: false, history: HashMap::new() }
    }

    /// Blocking event loop; returns when the user quits or detaches, or `cancel` fires.
    pub fn run(mut self, cancel: CancellationToken) -> Result<Exit> {
        let mut terminal = ratatui::init();
        let result = self.event_loop(&mut terminal, &cancel);
        ratatui::restore();
        result
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal, cancel: &CancellationToken) -> Result<Exit> {
        loop {
            if cancel.is_cancelled() {
                return Ok(Exit::Quit);
            }
            let groups = self.source.snapshots();
            let logs = self.source.logs();
            self.observe(&groups, self.source.now_ms());
            terminal.draw(|f| self.draw(f, &groups, &logs))?;
            if event::poll(Duration::from_millis(500))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && let Some(exit) = self.handle_key(key.code, key.modifiers, &groups)
            {
                return Ok(exit);
            }
        }
    }

    /// React to one key press; `Some` ends the dashboard.
    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers, groups: &[GroupSnapshot]) -> Option<Exit> {
        let selected = self.table.selected().and_then(|i| groups.get(i));
        if self.confirm_quit {
            self.confirm_quit = false;
            return matches!(code, KeyCode::Char('q')).then_some(Exit::Quit);
        }
        match code {
            KeyCode::Char('q') | KeyCode::Esc if self.source.supports_detach() => self.confirm_quit = true,
            KeyCode::Char('q') | KeyCode::Esc => return Some(Exit::Quit),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return Some(Exit::Quit),
            KeyCode::Char('x') | KeyCode::Char('d') => return Some(Exit::Detach),
            KeyCode::Down | KeyCode::Char('j') => self.select(groups.len(), 1),
            KeyCode::Up | KeyCode::Char('k') => self.select(groups.len(), -1),
            KeyCode::Char('w') => {
                if let Some(g) = selected {
                    self.source.wake(&g.name);
                }
            }
            KeyCode::Char('s') => {
                if let Some(g) = selected {
                    self.source.sleep(&g.name);
                }
            }
            KeyCode::Char('h') => {
                if let Some(g) = selected {
                    self.source.set_hold(&g.name, !g.held);
                }
            }
            _ => {}
        }
        None
    }

    fn select(&mut self, len: usize, delta: isize) {
        if len == 0 {
            self.table.select(None);
            return;
        }
        let cur = self.table.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len as isize) as usize;
        self.table.select(Some(next));
    }

    /// Fold the latest counters into the per-group request history. Groups that vanished
    /// lose their history; while the source is unreachable the counters are stale, so no
    /// bucket is booked and the baseline is dropped.
    fn observe(&mut self, groups: &[GroupSnapshot], now_ms: u64) {
        self.history.retain(|name, _| groups.iter().any(|g| &g.name == name));
        let reachable = self.source.problem().is_none();
        for g in groups {
            let history = self.history.entry(g.name.clone()).or_default();
            if reachable {
                history.observe(now_ms, g.requests_total);
            } else {
                history.pause();
            }
        }
    }

    fn draw(&mut self, f: &mut Frame, groups: &[GroupSnapshot], logs: &[String]) {
        let now = self.source.now_ms();
        let events = self.source.events();
        let a = areas(f.area(), groups.len());
        self.draw_header(f, a.header, groups);
        self.draw_table(f, a.table, groups, now);
        let selected = self.table.selected().and_then(|i| groups.get(i));
        let history = selected.and_then(|g| self.history.get(&g.name));
        detail::draw_detail(f, a.detail, selected, history, &events, now);
        if let Some(log) = a.log {
            draw_logs(f, log, logs);
        }
        if self.confirm_quit {
            let text = if a.footer.width >= 105 {
                " press q again to STOP THE DAEMON (sleeping groups cannot wake without it), any other key to cancel "
            } else {
                " press q again to STOP THE DAEMON, any other key to cancel "
            };
            let prompt = Line::from(vec![
                Span::styled(" q ", Style::default().add_modifier(Modifier::REVERSED).fg(Color::Red)),
                Span::styled(text, Style::default().fg(Color::Red)),
            ]);
            f.render_widget(Paragraph::new(prompt), a.footer);
            return;
        }
        f.render_widget(Paragraph::new(self.help_line(a.footer.width)), a.footer);
    }

    fn help_line(&self, width: u16) -> Line<'static> {
        let wide = width >= 100;
        let detach = self.source.supports_detach();
        let key = |k: &'static str| Span::styled(format!(" {k} "), Style::default().add_modifier(Modifier::REVERSED));
        let mut help = vec![
            key("q"),
            Span::raw(if detach {
                " stop daemon  "
            } else if self.source.is_demo() {
                " quit  "
            } else {
                " detach  "
            }),
        ];
        if detach {
            help.push(key("x"));
            help.push(Span::raw(if wide { " detach, keep running headless  " } else { " detach  " }));
        }
        help.extend([
            key("↑↓"),
            Span::raw(" select  "),
            key("w"),
            Span::raw(" wake  "),
            key("s"),
            Span::raw(" sleep  "),
            key("h"),
            Span::raw(if wide { " hold/release  " } else { " hold  " }),
        ]);
        let location = self.source.location();
        if self.source.is_demo() {
            help.push(Span::styled(format!(" {location} "), Style::default().fg(Color::Yellow)));
            return Line::from(help);
        }
        // A cut-off URL is useless: show the whole text, at least the URL, or nothing.
        let used: usize = help.iter().map(Span::width).sum();
        let url = location.rsplit(' ').next().unwrap_or_default().to_owned();
        let style = Style::default().fg(Color::DarkGray);
        if let Some(text) = [location, url]
            .into_iter()
            .map(|t| Span::styled(format!(" {t} "), style))
            .find(|s| used + s.width() <= usize::from(width))
        {
            help.push(text);
        }
        Line::from(help)
    }

    fn draw_header(&self, f: &mut Frame, area: Rect, groups: &[GroupSnapshot]) {
        let running = groups.iter().filter(|g| g.state == StateKind::Running).count();
        let asleep = groups.iter().filter(|g| g.state == StateKind::Stopped).count();
        let failed = groups.iter().filter(|g| g.state == StateKind::Failed).count();
        let mut spans = Vec::new();
        if self.source.is_demo() {
            spans.push(Span::styled(
                " DEMO ",
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend([
            Span::styled(
                if area.width >= 100 { " sleep-docker-sleep " } else { " sds " },
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("v{}  ", crate::VERSION)),
            Span::styled(format!("{running} running"), Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(format!("{asleep} asleep"), Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                format!("{failed} failed"),
                Style::default().fg(if failed > 0 { Color::Red } else { Color::DarkGray }),
            ),
        ]);
        if let Some(p) = self.source.problem() {
            spans.push(Span::styled(format!("  ⚠ {p}"), Style::default().fg(Color::Red)));
        }
        // The demo has no Docker; the badge and the footer say so. Otherwise the endpoint is
        // shown whole or not at all, and never pushes a problem off the line.
        if !self.source.is_demo() {
            let endpoint = Span::styled(
                format!("  docker {}", self.source.docker_endpoint()),
                Style::default().fg(Color::DarkGray),
            );
            let used: usize = spans.iter().map(Span::width).sum();
            if used + endpoint.width() <= usize::from(area.width) {
                spans.push(endpoint);
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_table(&mut self, f: &mut Frame, area: Rect, groups: &[GroupSnapshot], now: u64) {
        let title = if self.source.is_demo() { " groups (demo data) " } else { " groups " };
        let header = Row::new(["group", "state", "up", "last request", "sleeps in"])
            .style(Style::default().fg(Color::DarkGray))
            .height(1);
        let rows: Vec<Row> = groups
            .iter()
            .map(|g| {
                let mut name = vec![Span::styled(g.name.clone(), Style::default().add_modifier(Modifier::BOLD))];
                // Details of warnings and errors are in the panel; the table only flags them.
                if g.last_error.is_some() {
                    name.push(Span::styled(" !", Style::default().fg(Color::Red)));
                } else if !g.warnings.is_empty() {
                    name.push(Span::styled(" !", Style::default().fg(Color::Yellow)));
                }
                let up = g.members.iter().filter(|m| m.running).count();
                Row::new(vec![
                    Cell::from(Line::from(name)),
                    Cell::from(if g.held { format!("{} (held)", g.state) } else { g.state.to_string() })
                        .style(Style::default().fg(detail::state_color(g.state))),
                    Cell::from(format!("{up}/{}", g.members.len())),
                    Cell::from(
                        g.last_activity_ms
                            .map_or_else(|| "—".to_owned(), |t| format!("{} ago", fmt_compact(now.saturating_sub(t)))),
                    ),
                    Cell::from(sleeps_in_cell(g, now)),
                ])
            })
            .collect();
        let widths = [
            Constraint::Min(12),
            Constraint::Length(15),
            Constraint::Length(5),
            Constraint::Length(13),
            Constraint::Length(15),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(title))
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        if groups.is_empty() {
            let msg = Paragraph::new(
                "no groups discovered yet — label containers with sds.enable=true or add [groups.*] to sds.toml",
            )
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true });
            f.render_widget(msg, area);
            return;
        }
        if self.table.selected().is_none_or(|i| i >= groups.len()) {
            self.table.select(Some(0));
        }
        f.render_stateful_widget(table, area, &mut self.table);
    }
}

/// Mini countdown bar plus the time left, for the table.
fn sleeps_in_cell(g: &GroupSnapshot, now: u64) -> Line<'static> {
    const BAR: usize = 6;
    match countdown(g, now) {
        Countdown::Running { remaining_ms, ratio, .. } => {
            let filled = bar_cells(ratio, BAR);
            Line::from(vec![
                Span::styled("█".repeat(filled), Style::default().fg(detail::ratio_color(ratio))),
                Span::styled("░".repeat(BAR - filled), Style::default().fg(Color::DarkGray)),
                Span::raw(format!(" {}", fmt_compact(remaining_ms))),
            ])
        }
        Countdown::Held => Line::styled("held", Style::default().fg(Color::DarkGray)),
        Countdown::Inactive => Line::raw("—"),
    }
}

fn draw_logs(f: &mut Frame, area: Rect, logs: &[String]) {
    let visible = usize::from(area.height.saturating_sub(2));
    let start = logs.len().saturating_sub(visible);
    let lines: Vec<Line> = logs[start..]
        .iter()
        .map(|l| {
            let style = if l.contains(" WARN") || l.contains(" ERROR") {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            Line::styled(l.clone(), style)
        })
        .collect();
    let widget = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" log "));
    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GroupSettings, HealthSpec};
    use crate::test_support;

    struct FakeSource {
        groups: Vec<GroupSnapshot>,
        logs: Vec<String>,
        problem: Option<String>,
    }

    /// Renders `dashboard` into a `width` x `height` terminal and returns the screen as lines.
    fn render(dashboard: &mut Dashboard, width: u16, height: u16, groups: &[GroupSnapshot]) -> Vec<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        let logs = dashboard.source.logs();
        terminal.draw(|frame| dashboard.draw(frame, groups, &logs)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned())
            .collect()
    }

    fn demo_dashboard() -> (Dashboard, Arc<std::sync::atomic::AtomicU64>) {
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let c = Arc::clone(&clock);
        let source = DemoSource::with_clock(Box::new(move || c.load(std::sync::atomic::Ordering::Relaxed)));
        (Dashboard::new(Box::new(source)), clock)
    }

    impl Source for FakeSource {
        fn snapshots(&self) -> Vec<GroupSnapshot> {
            self.groups.clone()
        }
        fn logs(&self) -> Vec<String> {
            self.logs.clone()
        }
        fn wake(&self, _group: &str) {}
        fn sleep(&self, _group: &str) {}
        fn set_hold(&self, _group: &str, _held: bool) {}
        fn docker_endpoint(&self) -> String {
            "mock://docker".into()
        }
        fn location(&self) -> String {
            "test dashboard".into()
        }
        fn problem(&self) -> Option<String> {
            self.problem.clone()
        }
        fn supports_detach(&self) -> bool {
            true
        }
    }

    #[test]
    fn log_buffer_splits_lines_and_caps() {
        let buf = LogBuffer::new(2);
        let mut w = tracing_subscriber::fmt::MakeWriter::make_writer(&buf);
        w.write_all(b"one\ntwo\nthr").unwrap();
        w.write_all(b"ee\n").unwrap();
        assert_eq!(buf.lines(), vec!["two".to_string(), "three".to_string()]);
        w.flush().unwrap();
        buf.detach();
        w.write_all(b"four\n").unwrap();
        assert_eq!(buf.lines().last().unwrap(), "four");
    }

    #[test]
    fn dashboard_renders_group_states_logs_and_quit_confirmation() {
        let mut running = test_support::snapshot("shop", StateKind::Running);
        running.warnings.push("warning text".into());
        running.last_error = Some("error text".into());
        let mut held = test_support::snapshot("db", StateKind::Stopped);
        held.held = true;
        let groups = vec![
            running,
            held,
            test_support::snapshot("starting", StateKind::Starting),
            test_support::snapshot("stopping", StateKind::Stopping),
            test_support::snapshot("failed", StateKind::Failed),
        ];
        let source = FakeSource {
            groups: groups.clone(),
            logs: vec![" INFO running".into(), " WARN failed".into()],
            problem: Some("daemon unreachable".into()),
        };
        let mut dashboard = Dashboard::new(Box::new(source));
        dashboard.select(groups.len(), 1);
        dashboard.select(groups.len(), -2);
        assert_eq!(dashboard.table.selected(), Some(4));
        dashboard.select(groups.len(), 1);
        let backend = ratatui::backend::TestBackend::new(140, 35);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| dashboard.draw(frame, &groups, &[" INFO running".into(), " WARN failed".into()]))
            .unwrap();
        let content = terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect::<String>();
        assert!(content.contains("shop"));
        assert!(content.contains("stopped (held)"));
        assert!(content.contains("daemon unreachable"));
        // Warnings and errors moved from the table into the panel of the selected group.
        assert!(content.contains("warning text"));
        assert!(content.contains("error text"));
        assert!(content.contains("mock://docker"));

        dashboard.confirm_quit = true;
        terminal.draw(|frame| dashboard.draw(frame, &groups, &[])).unwrap();
        let content = terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect::<String>();
        assert!(content.contains("press q again to STOP THE DAEMON"));
    }

    #[test]
    fn dashboard_handles_empty_groups_and_selection() {
        let source = FakeSource { groups: Vec::new(), logs: Vec::new(), problem: None };
        let mut dashboard = Dashboard::new(Box::new(source));
        dashboard.select(0, 1);
        assert_eq!(dashboard.table.selected(), None);
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| dashboard.draw(frame, &[], &[])).unwrap();
        let content = terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect::<String>();
        assert!(content.contains("no groups discovered yet"));
    }

    #[tokio::test]
    async fn local_source_exposes_registry_and_operator_actions() {
        let settings =
            GroupSettings { start_grace: Duration::ZERO, health: HealthSpec::Grace, ..GroupSettings::default() };
        let (registry, _backend) = test_support::registry("shop", &["app"], true, settings);
        let source = LocalSource { registry, logs: LogBuffer::new(5), bind: "127.0.0.1:9900".parse().unwrap() };
        assert_eq!(source.snapshots().len(), 1);
        assert_eq!(source.docker_endpoint(), "mock://docker");
        assert!(source.location().contains("9900"));
        assert!(source.supports_detach());
        assert!(source.problem().is_none());
        source.wake("shop");
        source.sleep("shop");
        source.set_hold("shop", true);
        source.set_hold("shop", false);
        source.set_hold("missing", true);
        assert!(source.logs().is_empty());
        source.registry.shutdown().await;
    }

    fn busy_groups() -> Vec<GroupSnapshot> {
        let mut shop = test_support::snapshot("shop", StateKind::Running);
        shop.warnings.push("container `web` sets different sds.* values than `db`; the first one wins".into());
        shop.last_error = Some("wake-up failed: container `db` is unhealthy".into());
        shop.settings.idle_timeout = Duration::from_secs(100);
        shop.members.push(crate::controller::MemberSnapshot {
            name: "db".into(),
            id: "ba9876543210".into(),
            running: false,
            health: crate::docker::HealthState::None,
        });
        let mut db = test_support::snapshot("db", StateKind::Stopped);
        db.held = true;
        vec![shop, db, test_support::snapshot("blog", StateKind::Failed)]
    }

    #[test]
    fn narrow_terminal_shows_table_and_panel_stacked_without_log() {
        let groups = busy_groups();
        let source = FakeSource { groups: groups.clone(), logs: vec![" INFO log line".into()], problem: None };
        let mut dashboard = Dashboard::new(Box::new(source));
        let screen = render(&mut dashboard, 80, 24, &groups);
        let text = screen.join("\n");
        assert_eq!(screen.len(), 24);
        assert!(screen.iter().all(|l| l.chars().count() <= 80));
        // Table on top (group names, counts, mini bar), panel below it for the selected group.
        assert!(text.contains("groups") && text.contains("shop !") && text.contains("1/2"), "{text}");
        let table_end = screen.iter().position(|l| l.contains("shop") && l.contains("running")).unwrap();
        let panel_start = screen.iter().position(|l| l.starts_with("┌ shop")).unwrap();
        assert!(panel_start > table_end, "{text}");
        for want in ["sleeps in", "requests", "! container `web`", "✗ wake-up failed", "● app", "○ db"] {
            assert!(text.contains(want), "missing `{want}` in\n{text}");
        }
        // 24 rows have no spare room for the log pane; the footer keeps the keys readable.
        assert!(!text.contains("log line"));
        assert!(screen[23].contains("hold") && screen[23].contains("stop daemon"), "{}", screen[23]);
    }

    #[test]
    fn header_and_footer_show_addresses_whole_or_not_at_all() {
        struct Located(FakeSource);
        impl Source for Located {
            fn snapshots(&self) -> Vec<GroupSnapshot> {
                self.0.snapshots()
            }
            fn logs(&self) -> Vec<String> {
                Vec::new()
            }
            fn wake(&self, _group: &str) {}
            fn sleep(&self, _group: &str) {}
            fn set_hold(&self, _group: &str, _held: bool) {}
            fn docker_endpoint(&self) -> String {
                "unix:///var/run/docker.sock".into()
            }
            fn location(&self) -> String {
                "attached to http://127.0.0.1:9900".into()
            }
            fn problem(&self) -> Option<String> {
                self.0.problem.clone()
            }
        }
        let groups = busy_groups();
        for problem in [None, Some("daemon unreachable".to_owned())] {
            for width in 20..180u16 {
                let fake = FakeSource { groups: groups.clone(), logs: vec![], problem: problem.clone() };
                let mut dashboard = Dashboard::new(Box::new(Located(fake)));
                let screen = render(&mut dashboard, width, 24, &groups);
                let (header, footer) = (&screen[0], &screen[23]);
                assert!(!header.contains("docker u") || header.ends_with("docker.sock"), "{width}: {header}");
                assert!(!footer.contains("http") || footer.ends_with(":9900"), "{width}: {footer}");
                if problem.is_some() && width >= 80 {
                    assert!(header.contains("daemon unreachable"), "{width}: {header}");
                }
            }
        }
        let fake = FakeSource { groups: groups.clone(), logs: vec![], problem: None };
        let screen = render(&mut Dashboard::new(Box::new(Located(fake))), 80, 24, &groups);
        assert!(screen[23].ends_with("http://127.0.0.1:9900"), "{}", screen[23]);
    }

    #[test]
    fn quit_confirmation_fits_80_columns() {
        let groups = busy_groups();
        let fake = FakeSource { groups: groups.clone(), logs: vec![], problem: None };
        let mut dashboard = Dashboard::new(Box::new(fake));
        dashboard.confirm_quit = true;
        for width in [80, 104, 105, 140] {
            let screen = render(&mut dashboard, width, 24, &groups);
            assert!(screen[23].contains("STOP THE DAEMON"), "{}", screen[23]);
            assert!(screen[23].ends_with("any other key to cancel"), "{width}: {}", screen[23]);
        }
    }

    #[test]
    fn wide_terminal_puts_the_panel_next_to_the_table_and_keeps_the_log() {
        let groups = busy_groups();
        let source = FakeSource { groups: groups.clone(), logs: vec![" INFO log line".into()], problem: None };
        let mut dashboard = Dashboard::new(Box::new(source));
        let screen = render(&mut dashboard, 140, 40, &groups);
        let text = screen.join("\n");
        let row = screen.iter().find(|l| l.contains("shop !")).unwrap();
        // The same row carries the table entry and the panel (`state`/`for` summary line).
        let header = screen.iter().find(|l| l.contains("groups") && l.contains("shop")).unwrap();
        assert!(header.find("groups").unwrap() < header.find("shop").unwrap());
        assert!(row.contains("running"));
        assert!(text.contains("log line"), "{text}");
        assert!(screen.iter().any(|l| l.contains("state") && l.contains("running") && l.contains("woke")));
        // Moving the selection changes the panel.
        dashboard.select(groups.len(), 1);
        let text = render(&mut dashboard, 140, 40, &groups).join("\n");
        assert!(text.contains("on hold, requests do not wake it"), "{text}");
        assert!(!text.contains("✗ wake-up failed"));
    }

    #[test]
    fn dashboard_survives_tiny_and_empty_terminals() {
        let groups = busy_groups();
        for (w, h) in [(0, 0), (1, 1), (10, 3), (20, 5), (30, 8), (79, 23), (109, 29), (110, 30), (200, 6), (60, 100)] {
            for selected in [0, 2] {
                let source =
                    FakeSource { groups: groups.clone(), logs: vec![" WARN x".into()], problem: Some("down".into()) };
                let mut dashboard = Dashboard::new(Box::new(source));
                dashboard.table.select(Some(selected));
                render(&mut dashboard, w, h, &groups);
                dashboard.confirm_quit = true;
                render(&mut dashboard, w, h, &groups);
            }
            let mut empty = Dashboard::new(Box::new(FakeSource { groups: vec![], logs: vec![], problem: None }));
            let screen = render(&mut empty, w, h, &[]);
            if w >= 100 && h >= 20 {
                assert!(screen.join("\n").contains("nothing selected"));
            }
        }
    }

    #[test]
    fn layout_gives_the_panel_the_space_and_brings_the_log_back_when_tall() {
        let small = areas(Rect::new(0, 0, 80, 24), 7);
        assert!(small.log.is_none());
        assert_eq!(small.table.width, 80);
        assert!(small.detail.y >= small.table.y + small.table.height && small.detail.height >= 8);
        let tall_narrow = areas(Rect::new(0, 0, 80, 50), 7);
        assert!(tall_narrow.log.is_some_and(|l| l.height >= LOG_ROWS));
        let mid = areas(Rect::new(0, 0, 80, 32), 7);
        assert!(mid.log.is_some_and(|l| l.height < LOG_ROWS));
        let wide = areas(Rect::new(0, 0, 140, 40), 7);
        assert_eq!(wide.table.y, wide.detail.y);
        assert!(wide.detail.x >= wide.table.x + wide.table.width);
        assert!(wide.log.is_some());
        // Many groups: the table never takes more than half of the stacked area.
        let many = areas(Rect::new(0, 0, 80, 24), 200);
        assert!(many.table.height <= 11);
        // Degenerate sizes still yield rectangles.
        let _ = areas(Rect::new(0, 0, 0, 0), 0);
    }

    #[test]
    fn history_follows_polls_resets_and_group_changes() {
        let mut dashboard = Dashboard::new(Box::new(FakeSource { groups: vec![], logs: vec![], problem: None }));
        let mut groups = busy_groups();
        let set = |groups: &mut Vec<GroupSnapshot>, name: &str, total: u64| {
            groups.iter_mut().find(|g| g.name == name).unwrap().requests_total = total;
        };
        set(&mut groups, "shop", 100);
        dashboard.observe(&groups, 0);
        set(&mut groups, "shop", 105);
        dashboard.observe(&groups, 500);
        dashboard.observe(&groups, 1_000);
        // Daemon restart: counter is lower than before.
        set(&mut groups, "shop", 2);
        dashboard.observe(&groups, 2_000);
        assert_eq!(dashboard.history["shop"].samples(), vec![5, 2]);
        // A group disappears: its history goes; it comes back without a spike.
        let shop = groups.remove(0);
        dashboard.observe(&groups, 3_000);
        assert!(!dashboard.history.contains_key("shop"));
        groups.insert(0, GroupSnapshot { requests_total: 9_999, ..shop });
        dashboard.observe(&groups, 4_000);
        dashboard.observe(&groups, 5_000);
        assert_eq!(dashboard.history["shop"].samples(), vec![0]);
        assert_eq!(dashboard.history.len(), 3);
    }

    #[test]
    fn unreachable_source_books_no_requests() {
        let source = FakeSource { groups: vec![], logs: vec![], problem: Some("down".into()) };
        let mut dashboard = Dashboard::new(Box::new(source));
        let mut groups = busy_groups();
        dashboard.observe(&groups, 0);
        dashboard.observe(&groups, 1_000);
        assert!(dashboard.history["shop"].samples().is_empty());
        groups[0].requests_total += 50;
        dashboard.observe(&groups, 2_000);
        assert!(dashboard.history["shop"].samples().is_empty());
    }

    #[test]
    fn panel_shows_sparkline_once_requests_arrive() {
        let (mut dashboard, clock) = demo_dashboard();
        let busiest = |d: &Dashboard| d.source.snapshots().into_iter().max_by_key(|g| g.requests_total).unwrap().name;
        let name = busiest(&dashboard);
        for step in 0..30u64 {
            clock.store(step * 1_000, std::sync::atomic::Ordering::Relaxed);
            let groups = dashboard.source.snapshots();
            dashboard.observe(&groups, dashboard.source.now_ms());
        }
        let groups = dashboard.source.snapshots();
        let index = groups.iter().position(|g| g.name == name).unwrap();
        dashboard.table.select(Some(index));
        let text = render(&mut dashboard, 140, 40, &groups).join("\n");
        assert!(text.contains("/s now") && text.contains("/s peak"), "{text}");
        assert!(text.contains('·') || text.contains('▁') || text.contains('█'));
    }

    #[test]
    fn demo_dashboard_is_marked_and_drives_the_simulation_with_keys() {
        let (mut dashboard, _clock) = demo_dashboard();
        for (w, h) in [(80, 24), (140, 40)] {
            let groups = dashboard.source.snapshots();
            let screen = render(&mut dashboard, w, h, &groups);
            assert!(screen[0].starts_with(" DEMO "), "{}", screen[0]);
            assert!(!screen[0].contains("docker simulated"));
            assert!(screen.join("\n").contains("groups (demo data)"));
            assert!(screen[h as usize - 1].contains("demo"), "{}", screen[h as usize - 1]);
        }
        let none = KeyModifiers::NONE;
        let groups = dashboard.source.snapshots();
        let first_stopped = groups.iter().position(|g| g.state == StateKind::Stopped && !g.held).unwrap();
        dashboard.table.select(Some(first_stopped));
        assert_eq!(dashboard.handle_key(KeyCode::Char('w'), none, &groups), None);
        assert_eq!(dashboard.source.snapshots()[first_stopped].state, StateKind::Starting);

        let running = groups.iter().position(|g| g.state == StateKind::Running).unwrap();
        dashboard.table.select(Some(running));
        dashboard.handle_key(KeyCode::Char('s'), none, &groups);
        assert_eq!(dashboard.source.snapshots()[running].state, StateKind::Stopping);

        let held = groups.iter().position(|g| g.held).unwrap();
        dashboard.table.select(Some(held));
        dashboard.handle_key(KeyCode::Char('h'), none, &groups);
        assert!(!dashboard.source.snapshots()[held].held);
        let groups = dashboard.source.snapshots();
        dashboard.handle_key(KeyCode::Char('h'), none, &groups);
        assert!(dashboard.source.snapshots()[held].held);

        // Navigation and quitting: a source without detach quits at once.
        dashboard.handle_key(KeyCode::Down, none, &groups);
        assert_eq!(dashboard.table.selected(), Some((held + 1) % groups.len()));
        dashboard.handle_key(KeyCode::Char('k'), none, &groups);
        assert_eq!(dashboard.table.selected(), Some(held));
        assert_eq!(dashboard.handle_key(KeyCode::Char('q'), none, &groups), Some(Exit::Quit));
        assert_eq!(dashboard.handle_key(KeyCode::Esc, none, &groups), Some(Exit::Quit));
        assert_eq!(dashboard.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL, &groups), Some(Exit::Quit));
    }

    #[test]
    fn keys_keep_their_meaning_and_quit_needs_confirmation_when_the_daemon_is_owned() {
        let groups = busy_groups();
        let source = FakeSource { groups: groups.clone(), logs: vec![], problem: None };
        let mut dashboard = Dashboard::new(Box::new(source));
        let none = KeyModifiers::NONE;
        assert_eq!(dashboard.handle_key(KeyCode::Char('q'), none, &groups), None);
        assert!(dashboard.confirm_quit);
        assert_eq!(dashboard.handle_key(KeyCode::Char('j'), none, &groups), None, "any other key only cancels");
        assert!(!dashboard.confirm_quit);
        assert_eq!(dashboard.table.selected(), Some(0));
        dashboard.handle_key(KeyCode::Esc, none, &groups);
        assert_eq!(dashboard.handle_key(KeyCode::Char('q'), none, &groups), Some(Exit::Quit));
        assert_eq!(dashboard.handle_key(KeyCode::Char('x'), none, &groups), Some(Exit::Detach));
        assert_eq!(dashboard.handle_key(KeyCode::Char('d'), none, &groups), Some(Exit::Detach));
        assert_eq!(dashboard.handle_key(KeyCode::Char('c'), KeyModifiers::CONTROL, &groups), Some(Exit::Quit));
        dashboard.handle_key(KeyCode::Char('j'), none, &groups);
        dashboard.handle_key(KeyCode::Char('k'), none, &groups);
        dashboard.handle_key(KeyCode::Up, none, &groups);
        assert_eq!(dashboard.table.selected(), Some(2));
    }

    #[test]
    fn table_flags_notes_and_shows_a_countdown_bar() {
        let groups = busy_groups();
        let source = FakeSource { groups: groups.clone(), logs: vec![], problem: None };
        let mut dashboard = Dashboard::new(Box::new(source));
        let text = render(&mut dashboard, 100, 24, &groups).join("\n");
        assert!(text.contains("█") && text.contains("1m 40s"), "{text}");
        assert!(text.contains("held"));
        let line = sleeps_in_cell(&groups[1], epoch_ms());
        assert_eq!(line.spans[0].content, "held");
        let mut zero = groups[0].clone();
        zero.settings.idle_timeout = Duration::ZERO;
        assert!(sleeps_in_cell(&zero, epoch_ms()).spans[0].content.is_empty());
    }

    #[test]
    fn event_lines_keep_the_log_format() {
        let ev = SdsEvent {
            ts_ms: 1_767_225_600_000,
            group: "shop".into(),
            kind: EventKind::StartFailed,
            message: "boom".into(),
        };
        assert_eq!(format_event(&ev), "2026-01-01T00:00:00Z WARN shop [startfailed] boom");
    }
}
