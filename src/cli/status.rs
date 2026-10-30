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

#[derive(Debug, Args)]
pub struct HoldArgs {
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

fn state_cell(g: &GroupSnapshot) -> Cell {
    let color = match g.state {
        StateKind::Running => Color::Green,
        StateKind::Starting | StateKind::Stopping => Color::Yellow,
        StateKind::Failed => Color::Red,
        StateKind::Stopped => Color::DarkGrey,
    };
    let text = if g.held { format!("{} (held)", g.state) } else { g.state.to_string() };
    Cell::new(text).fg(color)
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
            state_cell(g),
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

pub async fn set_hold(cfg: Option<&Path>, args: HoldArgs, held: bool) -> Result<()> {
    let client = ApiClient::from_args(cfg, &args.client)?;
    let (code, body) = client.set_hold(&args.group, held).await?;
    match code.as_u16() {
        202 => {
            let what = if held { "on hold" } else { "released" };
            println!("{} {}: {what}", "ok".green().bold(), args.group);
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
        Ok(result) => result.map(|_| ()),
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

    #[test]
    fn table_renders_states_activity_timeouts_and_notes() {
        let mut running = crate::test_support::snapshot("shop", StateKind::Running);
        running.inflight = 2;
        running.warnings.push("configuration warning".into());
        running.last_error = Some("last failure".into());
        let mut held = crate::test_support::snapshot("db", StateKind::Stopped);
        held.held = true;
        let failed = crate::test_support::snapshot("web", StateKind::Failed);
        let output = render_table(&[running, held, failed], crate::controller::epoch_ms()).to_string();
        assert!(output.contains("shop"));
        assert!(output.contains("running"));
        assert!(output.contains("stopped (held)"));
        assert!(output.contains("failed"));
        assert!(output.contains("configuration warning"));
        assert!(output.contains("last failure"));
        assert!(output.contains("15m"));
    }

    #[tokio::test]
    async fn commands_render_status_and_execute_operator_actions() {
        use axum::http::{HeaderMap, StatusCode, Uri};
        use axum::response::{IntoResponse, Response};
        use axum::{Json, Router};

        async fn api(method: axum::http::Method, uri: Uri, _headers: HeaderMap) -> Response {
            match (method.as_str(), uri.path()) {
                ("GET", "/api/v1/status") => Json(crate::admin::api::StatusResponse {
                    version: "test".into(),
                    now_ms: crate::controller::epoch_ms(),
                    uptime_ms: 1_000,
                    docker: "mock".into(),
                    last_discovery_ms: 1,
                    warnings: vec!["test warning".into()],
                    groups: vec![crate::test_support::snapshot("shop", StateKind::Running)],
                    events: vec![crate::controller::SdsEvent::new(
                        "shop",
                        crate::controller::EventKind::Running,
                        "ready",
                    )],
                })
                .into_response(),
                ("POST", "/api/v1/groups/shop/wake") => {
                    let (code, result) = if uri.query().is_some() {
                        (StatusCode::OK, "running")
                    } else {
                        (StatusCode::ACCEPTED, "wake requested")
                    };
                    (code, Json(serde_json::json!({"result": result}))).into_response()
                }
                ("POST", "/api/v1/groups/shop/sleep" | "/api/v1/groups/shop/hold" | "/api/v1/groups/shop/release") => {
                    (StatusCode::ACCEPTED, Json(serde_json::json!({"requested": "ok"}))).into_response()
                }
                _ => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "unknown group"}))).into_response(),
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, Router::new().fallback(api)).await.unwrap() });
        let client = || super::super::ClientArgs { url: Some(format!("http://{addr}")), token: None };
        status(None, StatusArgs { client: client(), json: false, events: true }).await.unwrap();
        status(None, StatusArgs { client: client(), json: true, events: false }).await.unwrap();
        wake(None, WakeArgs { client: client(), group: "shop".into(), wait: false }).await.unwrap();
        wake(None, WakeArgs { client: client(), group: "shop".into(), wait: true }).await.unwrap();
        sleep(None, SleepArgs { client: client(), group: "shop".into() }).await.unwrap();
        set_hold(None, HoldArgs { client: client(), group: "shop".into() }, true).await.unwrap();
        set_hold(None, HoldArgs { client: client(), group: "shop".into() }, false).await.unwrap();
        assert!(wake(None, WakeArgs { client: client(), group: "missing".into(), wait: false }).await.is_err());
        assert!(sleep(None, SleepArgs { client: client(), group: "missing".into() }).await.is_err());
        server.abort();
    }
}
