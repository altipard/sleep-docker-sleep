//! Terminal dashboard for `sds run` (interactive sessions only).

use std::collections::VecDeque;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use tokio_util::sync::CancellationToken;

use crate::controller::{GroupSnapshot, Registry, StateKind, epoch_ms};

/// Where the dashboard gets its data from: the in-process registry (`sds run`) or a
/// running daemon's API (`sds top`).
pub trait Source: Send {
    fn snapshots(&self) -> Vec<GroupSnapshot>;
    fn logs(&self) -> Vec<String>;
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
                        if let Err(e) = result {
                            if let Ok(mut err) = error_bg.lock() { *err = Some(e.to_string()); }
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
        self.latest
            .lock()
            .ok()
            .and_then(|l| {
                l.as_ref().map(|s| {
                    s.events
                        .iter()
                        .map(|ev| {
                            let secs = ev.ts_ms / 1000;
                            let t =
                                humantime::format_rfc3339_seconds(std::time::UNIX_EPOCH + Duration::from_secs(secs));
                            let level = match ev.kind {
                                crate::controller::EventKind::StartFailed
                                | crate::controller::EventKind::StopFailed
                                | crate::controller::EventKind::Warning => " WARN",
                                _ => " INFO",
                            };
                            format!(
                                "{t}{level} {} [{}] {}",
                                ev.group,
                                format!("{:?}", ev.kind).to_lowercase(),
                                ev.message
                            )
                        })
                        .collect()
                })
            })
            .unwrap_or_default()
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
}

impl Dashboard {
    pub fn new(source: Box<dyn Source>) -> Self {
        Self { source, table: TableState::default().with_selected(0), confirm_quit: false }
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
            terminal.draw(|f| self.draw(f, &groups, &logs))?;
            if event::poll(Duration::from_millis(500))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let selected = self.table.selected().and_then(|i| groups.get(i));
                    if self.confirm_quit {
                        self.confirm_quit = false;
                        if matches!(key.code, KeyCode::Char('q')) {
                            return Ok(Exit::Quit);
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc if self.source.supports_detach() => self.confirm_quit = true,
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(Exit::Quit),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(Exit::Quit),
                        KeyCode::Char('x') | KeyCode::Char('d') => return Ok(Exit::Detach),
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
                }
            }
        }
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

    fn draw(&mut self, f: &mut Frame, groups: &[GroupSnapshot], logs: &[String]) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(6), Constraint::Length(10), Constraint::Length(1)])
            .split(area);
        self.draw_header(f, chunks[0], groups);
        self.draw_table(f, chunks[1], groups);
        draw_logs(f, chunks[2], logs);
        if self.confirm_quit {
            let prompt = Line::from(vec![
                Span::styled(" q ", Style::default().add_modifier(Modifier::REVERSED).fg(Color::Red)),
                Span::styled(
                    " press q again to STOP THE DAEMON (sleeping groups cannot wake without it), any other key to cancel ",
                    Style::default().fg(Color::Red),
                ),
            ]);
            f.render_widget(Paragraph::new(prompt), chunks[3]);
            return;
        }
        let mut help = vec![
            Span::styled(" q ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(if self.source.supports_detach() { " stop daemon  " } else { " detach  " }),
        ];
        if self.source.supports_detach() {
            help.push(Span::styled(" x ", Style::default().add_modifier(Modifier::REVERSED)));
            help.push(Span::raw(" detach, keep running headless  "));
        }
        help.extend([
            Span::styled(" ↑↓ ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" select  "),
            Span::styled(" w ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" wake  "),
            Span::styled(" s ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" sleep  "),
            Span::styled(" h ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" hold/release  "),
            Span::styled(format!(" {} ", self.source.location()), Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(Line::from(help)), chunks[3]);
    }

    fn draw_header(&self, f: &mut Frame, area: Rect, groups: &[GroupSnapshot]) {
        let running = groups.iter().filter(|g| g.state == StateKind::Running).count();
        let asleep = groups.iter().filter(|g| g.state == StateKind::Stopped).count();
        let failed = groups.iter().filter(|g| g.state == StateKind::Failed).count();
        let line = Line::from(vec![
            Span::styled(" sleep-docker-sleep ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("v{}  ", env!("CARGO_PKG_VERSION"))),
            Span::styled(format!("{running} running"), Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(format!("{asleep} asleep"), Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                format!("{failed} failed"),
                Style::default().fg(if failed > 0 { Color::Red } else { Color::DarkGray }),
            ),
            Span::raw("  "),
            Span::styled(format!("docker {}", self.source.docker_endpoint()), Style::default().fg(Color::DarkGray)),
        ]);
        let line = match self.source.problem() {
            Some(p) => {
                let mut spans = line.spans;
                spans.push(Span::styled(format!("  ⚠ {p}"), Style::default().fg(Color::Red)));
                Line::from(spans)
            }
            None => line,
        };
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_table(&mut self, f: &mut Frame, area: Rect, groups: &[GroupSnapshot]) {
        let now = epoch_ms();
        let header = Row::new(["group", "state", "containers", "req", "last request", "sleeps in", "notes"])
            .style(Style::default().fg(Color::DarkGray))
            .height(1);
        let rows: Vec<Row> = groups
            .iter()
            .map(|g| {
                let state_style = match g.state {
                    StateKind::Running => Style::default().fg(Color::Green),
                    StateKind::Starting | StateKind::Stopping => Style::default().fg(Color::Yellow),
                    StateKind::Failed => Style::default().fg(Color::Red),
                    StateKind::Stopped => Style::default().fg(Color::DarkGray),
                };
                let containers = g
                    .members
                    .iter()
                    .map(|m| format!("{}{}", if m.running { "●" } else { "○" }, m.name))
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut notes = g.warnings.clone();
                if let Some(e) = &g.last_error {
                    notes.push(e.clone());
                }
                let height = u16::try_from(notes.len().max(1)).unwrap_or(1);
                Row::new(vec![
                    Cell::from(g.name.clone()).bold(),
                    Cell::from(if g.held { format!("{} (held)", g.state) } else { g.state.to_string() })
                        .style(state_style),
                    Cell::from(containers),
                    Cell::from(g.requests_total.to_string()),
                    Cell::from(crate::cli::fmt_age(g.last_activity_ms, now)),
                    Cell::from(
                        g.sleeps_in_ms(now)
                            .map(|ms| humantime::format_duration(Duration::from_secs(ms / 1000)).to_string())
                            .unwrap_or_else(|| "—".into()),
                    ),
                    Cell::from(notes.join("\n")).style(Style::default().fg(Color::Yellow)),
                ])
                .height(height)
            })
            .collect();
        let widths = [
            Constraint::Min(14),
            Constraint::Length(9),
            Constraint::Min(20),
            Constraint::Length(6),
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Min(16),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(" groups "))
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        if groups.is_empty() {
            let msg = Paragraph::new(
                "no groups discovered yet — label containers with sds.enable=true or add [groups.*] to sds.toml",
            )
            .block(Block::default().borders(Borders::ALL).title(" groups "))
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

    #[test]
    fn log_buffer_splits_lines_and_caps() {
        let buf = LogBuffer::new(2);
        let mut w = tracing_subscriber::fmt::MakeWriter::make_writer(&buf);
        w.write_all(b"one\ntwo\nthr").unwrap();
        w.write_all(b"ee\n").unwrap();
        assert_eq!(buf.lines(), vec!["two".to_string(), "three".to_string()]);
    }
}
