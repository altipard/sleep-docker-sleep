use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use comfy_table::{Cell, Color, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use owo_colors::OwoColorize;

use super::{ApiClient, ClientArgs};
use crate::controller::{GroupSnapshot, StateKind};

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[command(flatten)]
    pub client: ClientArgs,
    /// Machine-readable output
    #[arg(long)]
    pub json: bool,
    /// Also list recent events
    #[arg(long)]
    pub events: bool,
}

#[derive(Debug, Args)]
pub struct WakeArgs {
    #[command(flatten)]
    pub client: ClientArgs,
    /// Group name (see `sds status`)
    pub group: String,
    /// Block until the group is ready
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct TopArgs {
    #[command(flatten)]
    pub client: ClientArgs,
}

#[derive(Debug, Args)]
pub struct SleepArgs {
    #[command(flatten)]
    pub client: ClientArgs,
    /// Group name (see `sds status`)
    pub group: String,
}

pub fn fmt_age(ms: Option<u64>, now: u64) -> String {
    match ms {
        None => "—".into(),
        Some(t) => {
            let secs = now.saturating_sub(t) / 1000;
            format!("{} ago", humantime::format_duration(std::time::Duration::from_secs(secs)))
        }
    }
}

fn fmt_ms(ms: Option<u64>) -> String {
    match ms {
        None => "—".into(),
        Some(ms) => humantime::format_duration(std::time::Duration::from_secs(ms / 1000)).to_string(),
    }
}

fn state_cell(state: StateKind) -> Cell {
    let color = match state {
        StateKind::Running => Color::Green,
        StateKind::Starting | StateKind::Stopping => Color::Yellow,
        StateKind::Failed => Color::Red,
        StateKind::Stopped => Color::DarkGrey,
    };
    Cell::new(state.to_string()).fg(color)
}

pub fn render_table(groups: &[GroupSnapshot], now: u64) -> Table {
    let mut table = Table::new();
    table.load_style(UTF8_FULL_CONDENSED).set_content_arrangement(ContentArrangement::Dynamic).set_header([
        "group",
        "state",
        "containers",
        "requests",
        "last request",
        "sleeps in",
        "idle timeout",
        "notes",
    ]);
    for g in groups {
        let containers = g
            .members
            .iter()
            .map(|m| format!("{}{}", if m.running { "●" } else { "○" }, m.name))
            .collect::<Vec<_>>()
            .join("\n");
        let mut notes: Vec<String> = g.warnings.clone();
        if let Some(e) = &g.last_error {
            notes.push(e.clone());
        }
        table.add_row(vec![
            Cell::new(&g.name),
            state_cell(g.state),
            Cell::new(containers),
            Cell::new(g.requests_total.to_string()),
            Cell::new(fmt_age(g.last_activity_ms, now)),
            Cell::new(fmt_ms(g.sleeps_in_ms(now))),
            Cell::new(humantime::format_duration(g.settings.idle_timeout).to_string()),
            Cell::new(notes.join("\n")),
        ]);
    }
    table
}

pub async fn status(cfg: Option<&Path>, args: StatusArgs) -> Result<()> {
    let client = ApiClient::from_args(cfg, &args.client)?;
    let status = client.status().await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    println!(
        "{} {} · docker {} · uptime {} · {}",
        "sds".bold(),
        status.version,
        status.docker,
        humantime::format_duration(std::time::Duration::from_secs(status.uptime_ms / 1000)),
        client.base().dimmed()
    );
    for w in &status.warnings {
        println!("{} {w}", "warning:".yellow().bold());
    }
    if status.groups.is_empty() {
        println!("no groups discovered (label containers with sds.enable=true or add [groups.*] to sds.toml)");
    } else {
        println!("{}", render_table(&status.groups, status.now_ms));
    }
    if args.events {
        println!();
        for ev in status.events.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev() {
            let secs = ev.ts_ms / 1000;
            println!(
                "{} {:<12} {:<13} {}",
                humantime::format_rfc3339_seconds(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
                    .to_string()
                    .dimmed(),
                ev.group.bold(),
                format!("{:?}", ev.kind).to_lowercase(),
                ev.message
            );
        }
    }
    Ok(())
}

pub async fn wake(cfg: Option<&Path>, args: WakeArgs) -> Result<()> {
    let client = ApiClient::from_args(cfg, &args.client)?;
    let (code, body) = client.wake(&args.group, args.wait).await?;
    match code.as_u16() {
        200 | 202 => {
            let result = body.get("result").and_then(|v| v.as_str()).unwrap_or("wake requested");
            println!("{} {}: {result}", "ok".green().bold(), args.group);
            Ok(())
        }
        404 => bail!("unknown group `{}`", args.group),
        _ => {
            bail!("{}: {}", code, body.get("result").or(body.get("error")).and_then(|v| v.as_str()).unwrap_or("failed"))
        }
    }
}

pub async fn sleep(cfg: Option<&Path>, args: SleepArgs) -> Result<()> {
    let client = ApiClient::from_args(cfg, &args.client)?;
    let (code, body) = client.sleep(&args.group).await?;
    match code.as_u16() {
        202 => {
            println!("{} {}: sleep requested", "ok".green().bold(), args.group);
            Ok(())
        }
        404 => bail!("unknown group `{}`", args.group),
        _ => bail!("{}: {}", code, body.get("error").and_then(|v| v.as_str()).unwrap_or("failed")),
    }
}

/// `sds top`: the dashboard over the API of a running daemon.
pub async fn top(cfg: Option<&Path>, args: TopArgs) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        bail!("`sds top` needs a terminal; use `sds status` for scripts");
    }
    let client = ApiClient::from_args(cfg, &args.client)?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let source = crate::tui::RemoteSource::connect(client, cancel.clone()).await?;
    let ui = crate::tui::Dashboard::new(Box::new(source));
    let cancel_ui = cancel.clone();
    let outcome = tokio::task::spawn_blocking(move || ui.run(cancel_ui)).await;
    cancel.cancel();
    match outcome {
        Ok(result) => result,
        Err(e) => bail!("dashboard panicked: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_formatting() {
        assert_eq!(fmt_age(None, 0), "—");
        assert_eq!(fmt_age(Some(1_000), 61_000), "1m ago");
    }
}
