//! Detail panel for the selected group: state, countdown, request sparkline, containers,
//! warnings, last error and the group's recent events.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::history::{Countdown, RateHistory, bar_cells, countdown, fmt_compact, sparkline};
use crate::controller::{EventKind, GroupSnapshot, MemberSnapshot, SdsEvent, StateKind};
use crate::docker::HealthState;

const LABEL: usize = 11;
/// Notes (warnings, errors) wrap onto at most this many lines each.
const NOTE_LINES: usize = 2;

pub fn state_color(state: StateKind) -> Color {
    match state {
        StateKind::Running => Color::Green,
        StateKind::Starting | StateKind::Stopping => Color::Yellow,
        StateKind::Failed => Color::Red,
        StateKind::Stopped => Color::DarkGray,
    }
}

pub fn ratio_color(ratio: f64) -> Color {
    if ratio > 0.5 {
        Color::Green
    } else if ratio > 0.2 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn draw_detail(
    f: &mut Frame,
    area: Rect,
    group: Option<&GroupSnapshot>,
    history: Option<&RateHistory>,
    events: &[SdsEvent],
    now_ms: u64,
) {
    let title = group.map_or_else(|| " details ".to_owned(), |g| format!(" {} ", g.name));
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = match group {
        Some(g) => detail_lines(g, history, events, now_ms, inner.width, inner.height),
        None => vec![Line::styled("nothing selected", dim())],
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// The panel's content for an inner area of `width` x `height`. Priorities when space is
/// short: summary, then warnings and errors, then containers, then events.
pub fn detail_lines(
    g: &GroupSnapshot,
    history: Option<&RateHistory>,
    events: &[SdsEvent],
    now_ms: u64,
    width: u16,
    height: u16,
) -> Vec<Line<'static>> {
    let (w, h) = (usize::from(width), usize::from(height));
    let mut lines = vec![state_line(g, now_ms), countdown_line(g, now_ms, w), request_line(g, history, w)];
    let mut left = h.saturating_sub(lines.len());

    let mut notes: Vec<Line<'static>> = Vec::new();
    for warning in &g.warnings {
        notes.extend(wrap(warning, "! ", w, Color::Yellow));
    }
    if let Some(error) = &g.last_error {
        notes.extend(wrap(error, "✗ ", w, Color::Red));
    }
    notes.truncate(left);
    left -= notes.len();

    let members = member_lines(&g.members, left);
    left -= members.len();

    let mine: Vec<&SdsEvent> = events.iter().filter(|e| e.group == g.name).collect();
    let mut event_lines = Vec::new();
    // A heading only when there is room for it and at least one event under it.
    if left >= 2 && !mine.is_empty() {
        event_lines.push(Line::styled("recent events", dim()));
        left -= 1;
    }
    let skip = mine.len().saturating_sub(left);
    if !event_lines.is_empty() {
        event_lines.extend(mine[skip..].iter().map(|e| event_line(e, now_ms)));
    }
    lines.extend(notes);
    lines.extend(members);
    lines.extend(event_lines);
    lines
}

fn label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<LABEL$}"), dim())
}

fn state_line(g: &GroupSnapshot, now_ms: u64) -> Line<'static> {
    let mut state = g.state.to_string();
    if g.held {
        state.push_str(" (held)");
    }
    Line::from(vec![
        label("state"),
        Span::styled(state, Style::default().fg(state_color(g.state)).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  for {}", fmt_compact(now_ms.saturating_sub(g.state_since_ms))), dim()),
        Span::styled(format!("  woke {}x, slept {}x", g.wakes_total, g.sleeps_total), dim()),
    ])
}

fn countdown_line(g: &GroupSnapshot, now_ms: u64, width: usize) -> Line<'static> {
    let head = label("sleeps in");
    match countdown(g, now_ms) {
        Countdown::Running { remaining_ms, timeout_ms, ratio, inflight } => {
            let text = if remaining_ms == 0 && inflight > 0 {
                format!(" idle, but {inflight} open connection(s)")
            } else {
                format!(" {} of {}", fmt_compact(remaining_ms), fmt_compact(timeout_ms))
            };
            let bar_w = width.saturating_sub(LABEL + text.chars().count()).min(40);
            if bar_w < 4 {
                return Line::from(vec![head, Span::raw(text.trim_start().to_owned())]);
            }
            let filled = bar_cells(ratio, bar_w);
            Line::from(vec![
                head,
                Span::styled("█".repeat(filled), Style::default().fg(ratio_color(ratio))),
                Span::styled("░".repeat(bar_w - filled), dim()),
                Span::raw(text),
            ])
        }
        Countdown::Held => Line::from(vec![head, Span::styled("on hold, requests do not wake it", dim())]),
        Countdown::Inactive => {
            let text = match g.state {
                StateKind::Stopped => "asleep, the next request wakes it",
                StateKind::Starting => "waking up",
                StateKind::Stopping => "going to sleep",
                StateKind::Failed => "start failed, the next request retries",
                StateKind::Running => "",
            };
            Line::from(vec![head, Span::styled(text, dim())])
        }
    }
}

fn request_line(g: &GroupSnapshot, history: Option<&RateHistory>, width: usize) -> Line<'static> {
    let head = label("requests");
    let samples = history.map(RateHistory::samples).unwrap_or_default();
    let now = history.and_then(RateHistory::latest);
    let peak = samples.iter().copied().max().unwrap_or(0);
    let tail = match now {
        Some(n) => format!("  {n}/s now, {peak}/s peak, {} total", g.requests_total),
        None => format!("  {} total", g.requests_total),
    };
    let spark_w = width.saturating_sub(LABEL + tail.chars().count()).min(super::history::HISTORY);
    if spark_w < 8 {
        return Line::from(vec![head, Span::raw(format!("{} total", g.requests_total))]);
    }
    Line::from(vec![
        head,
        Span::styled(sparkline(&samples, spark_w), Style::default().fg(Color::Cyan)),
        Span::styled(tail, dim()),
    ])
}

fn member_lines(members: &[MemberSnapshot], room: usize) -> Vec<Line<'static>> {
    let shown = if members.len() > room { room.saturating_sub(1) } else { members.len() };
    let name_w = members.iter().map(|m| m.name.chars().count()).max().unwrap_or(0).min(28);
    let mut lines: Vec<Line<'static>> = members
        .iter()
        .take(shown)
        .map(|m| {
            let (dot, color, text) = match (m.running, m.health) {
                (false, _) => ("○", Color::DarkGray, "stopped"),
                (true, HealthState::Healthy) => ("●", Color::Green, "running, healthy"),
                (true, HealthState::Unhealthy) => ("●", Color::Red, "running, unhealthy"),
                (true, HealthState::Starting) => ("●", Color::Yellow, "running, health check starting"),
                (true, HealthState::None) => ("●", Color::Green, "running"),
            };
            Line::from(vec![
                Span::styled(dot, Style::default().fg(color)),
                Span::raw(format!(" {:<name_w$}  ", m.name)),
                Span::styled(text, dim()),
            ])
        })
        .collect();
    if shown < members.len() && room > 0 {
        lines.push(Line::styled(format!("+{} more", members.len() - shown), dim()));
    }
    lines
}

fn kind_label(kind: EventKind) -> (&'static str, Color) {
    match kind {
        EventKind::Discovered => ("discovered", Color::Reset),
        EventKind::Removed => ("removed", Color::Reset),
        EventKind::Waking => ("waking", Color::Yellow),
        EventKind::Running => ("running", Color::Green),
        EventKind::StartFailed => ("start failed", Color::Red),
        EventKind::Sleeping => ("sleeping", Color::Yellow),
        EventKind::Stopped => ("stopped", Color::DarkGray),
        EventKind::StopFailed => ("stop failed", Color::Red),
        EventKind::External => ("external", Color::Reset),
        EventKind::Held => ("held", Color::Yellow),
        EventKind::Released => ("released", Color::Reset),
        EventKind::Warning => ("warning", Color::Yellow),
    }
}

fn event_line(e: &SdsEvent, now_ms: u64) -> Line<'static> {
    let (kind, color) = kind_label(e.kind);
    let age = format!("{} ago", fmt_compact(now_ms.saturating_sub(e.ts_ms)));
    Line::from(vec![
        Span::styled(format!("{age:>10}  "), dim()),
        Span::styled(format!("{kind:<13}"), Style::default().fg(color)),
        Span::raw(e.message.clone()),
    ])
}

/// Break `text` into lines of at most `width` characters with `prefix` on the first line and
/// matching indent on the rest; whatever does not fit in [`NOTE_LINES`] ends in `…`.
fn wrap(text: &str, prefix: &str, width: usize, color: Color) -> Vec<Line<'static>> {
    let indent = " ".repeat(prefix.chars().count());
    let room = width.saturating_sub(indent.chars().count()).max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    for (i, chunk) in chars.chunks(room).enumerate() {
        if i == NOTE_LINES {
            break;
        }
        let mut body: String = chunk.iter().collect();
        if i == NOTE_LINES - 1 && chars.len() > room * NOTE_LINES {
            body.pop();
            body.push('…');
        }
        let lead = if i == 0 { prefix } else { &indent };
        out.push(Line::styled(format!("{lead}{body}"), Style::default().fg(color)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn wrap_limits_lines_and_marks_truncation() {
        let short = wrap("abc", "! ", 20, Color::Red);
        assert_eq!(text(&short), "! abc");
        let long = wrap(&"x".repeat(100), "✗ ", 12, Color::Red);
        assert_eq!(long.len(), NOTE_LINES);
        assert!(text(&long).ends_with('…'));
        assert!(text(&long).lines().all(|l| l.chars().count() <= 12));
        // Zero width must not loop or panic.
        assert!(wrap("abc", "! ", 0, Color::Red).len() <= NOTE_LINES);
        assert!(wrap("", "! ", 10, Color::Red).is_empty());
    }

    #[test]
    fn panel_prefers_summary_then_notes_then_containers_then_events() {
        let mut g = test_support::snapshot("shop", StateKind::Running);
        g.warnings.push("warn".into());
        g.last_error = Some("boom".into());
        let events =
            vec![SdsEvent { ts_ms: 1, group: "shop".into(), kind: EventKind::Running, message: "ready".into() }];
        let now = g.state_since_ms + 5_000;
        let full = text(&detail_lines(&g, None, &events, now, 80, 20));
        for want in ["state", "sleeps in", "requests", "! warn", "✗ boom", "app", "recent events", "ready"] {
            assert!(full.contains(want), "missing `{want}` in\n{full}");
        }
        // Three rows: only the summary survives.
        let tight = detail_lines(&g, None, &events, now, 80, 3);
        assert_eq!(tight.len(), 3);
        // Five rows: summary plus the two notes, no containers.
        let mid = text(&detail_lines(&g, None, &events, now, 80, 5));
        assert!(mid.contains("✗ boom") && !mid.contains("app"));
        // Nothing fits in an empty panel.
        assert!(detail_lines(&g, None, &events, now, 0, 0).len() <= 3);
    }

    #[test]
    fn only_the_groups_own_events_are_listed_newest_last() {
        let g = test_support::snapshot("shop", StateKind::Stopped);
        let ev = |group: &str, ts: u64, msg: &str| SdsEvent {
            ts_ms: ts,
            group: group.into(),
            kind: EventKind::Waking,
            message: msg.into(),
        };
        let events = vec![ev("shop", 1, "first"), ev("other", 2, "foreign"), ev("shop", 3, "second")];
        let out = text(&detail_lines(&g, None, &events, 10_000, 80, 12));
        assert!(!out.contains("foreign"));
        assert!(out.find("first").unwrap() < out.find("second").unwrap());
        // With room for one event only the newest remains.
        let one = text(&detail_lines(&g, None, &events, 10_000, 80, 6));
        assert!(one.contains("second") && !one.contains("first"));
    }

    #[test]
    fn many_containers_are_summarised() {
        let mut g = test_support::snapshot("big", StateKind::Running);
        let proto = g.members[0].clone();
        g.members = (0..6).map(|i| MemberSnapshot { name: format!("svc-{i}"), ..proto.clone() }).collect();
        let out = text(&detail_lines(&g, None, &[], 0, 80, 7));
        assert!(out.contains("+3 more"), "{out}");
    }

    #[test]
    fn countdown_line_covers_every_state() {
        let now = test_support::snapshot("g", StateKind::Running).state_since_ms;
        let line = |g: &GroupSnapshot| text(&[countdown_line(g, now, 60)]);
        let mut running = test_support::snapshot("g", StateKind::Running);
        running.settings.idle_timeout = std::time::Duration::from_secs(100);
        assert!(line(&running).contains("1m 40s of 1m 40s"));
        assert!(line(&running).contains('█'));
        running.inflight = 2;
        running.last_activity_ms = Some(now - 200_000);
        assert!(line(&running).contains("2 open connection(s)"));
        running.held = true;
        assert!(line(&running).contains("on hold"));
        assert!(line(&test_support::snapshot("g", StateKind::Stopped)).contains("asleep"));
        assert!(line(&test_support::snapshot("g", StateKind::Starting)).contains("waking up"));
        assert!(line(&test_support::snapshot("g", StateKind::Stopping)).contains("going to sleep"));
        assert!(line(&test_support::snapshot("g", StateKind::Failed)).contains("retries"));
        // Too narrow for a bar: text only.
        let narrow = text(&[countdown_line(&test_support::snapshot("g", StateKind::Running), now, 20)]);
        assert!(!narrow.contains('█') && narrow.contains("of"));
    }

    #[test]
    fn request_line_shows_sparkline_when_there_is_history() {
        let mut h = RateHistory::default();
        h.observe(0, 0);
        h.observe(1_000, 3);
        h.observe(2_000, 3);
        let g = test_support::snapshot("g", StateKind::Running);
        let out = text(&[request_line(&g, Some(&h), 80)]);
        assert!(out.contains("0/s now") && out.contains("3/s peak") && out.contains("3 total"), "{out}");
        assert!(out.contains('·'));
        assert!(text(&[request_line(&g, None, 80)]).contains("3 total"));
        assert!(text(&[request_line(&g, Some(&h), 12)]).contains("3 total"));
    }
}
