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

/// Ring buffer that receives `tracing` output while the dashboard owns the terminal.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
    keep: usize,
}

impl LogBuffer {
    pub fn new(keep: usize) -> Self {
        Self { inner: Arc::new(Mutex::new(VecDeque::with_capacity(keep))), keep }
    }

    pub fn lines(&self) -> Vec<String> {
        self.inner.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    fn push(&self, line: String) {
        if let Ok(mut l) = self.inner.lock() {
            if l.len() == self.keep {
                l.pop_front();
            }
            l.push_back(line);
        }
    }
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
    registry: Arc<Registry>,
    logs: LogBuffer,
    bind: SocketAddr,
    table: TableState,
}

impl Dashboard {
    pub fn new(registry: Arc<Registry>, logs: LogBuffer, bind: SocketAddr) -> Self {
        Self { registry, logs, bind, table: TableState::default().with_selected(0) }
    }

    /// Blocking event loop; returns when the user quits or `cancel` fires.
    pub fn run(mut self, cancel: CancellationToken) -> Result<()> {
        let mut terminal = ratatui::init();
        let result = self.event_loop(&mut terminal, &cancel);
        ratatui::restore();
        result
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal, cancel: &CancellationToken) -> Result<()> {
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let groups = self.registry.snapshots();
            let logs = self.logs.lines();
            terminal.draw(|f| self.draw(f, &groups, &logs))?;
            if event::poll(Duration::from_millis(500))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let selected = self.table.selected().and_then(|i| groups.get(i));
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                        KeyCode::Down | KeyCode::Char('j') => self.select(groups.len(), 1),
                        KeyCode::Up | KeyCode::Char('k') => self.select(groups.len(), -1),
                        KeyCode::Char('w') => {
                            if let Some(g) = selected {
                                if let Some(h) = self.registry.get(&g.name) {
                                    h.request_wake();
                                }
                            }
                        }
                        KeyCode::Char('s') => {
                            if let Some(g) = selected {
                                if let Some(h) = self.registry.get(&g.name) {
                                    h.request_sleep();
                                }
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
        let help = Line::from(vec![
            Span::styled(" q ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" quit  "),
            Span::styled(" ↑↓ ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" select  "),
            Span::styled(" w ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" wake  "),
            Span::styled(" s ", Style::default().add_modifier(Modifier::REVERSED)),
            Span::raw(" sleep  "),
            Span::styled(format!(" web overview: http://{} ", self.bind), Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(help), chunks[3]);
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
            Span::styled(format!("docker {}", self.registry.docker_endpoint), Style::default().fg(Color::DarkGray)),
        ]);
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
                    Cell::from(g.state.to_string()).style(state_style),
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
