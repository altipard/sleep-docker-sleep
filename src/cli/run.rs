use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::admin::{self, AppState};
use crate::config::{self, Config};
use crate::controller::{EventBus, Registry, Supervisor};
use crate::docker::{Backend, DockerEngine};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// No terminal dashboard; log to stderr (automatic when stdout is not a TTY)
    #[arg(long)]
    pub headless: bool,
    /// Override the admin bind address from sds.toml
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<std::net::SocketAddr>,
    /// Explicitly allow a non-loopback admin bind without a token (private networks only)
    #[arg(long)]
    pub insecure: bool,
}

fn validate_admin_exposure(cfg: &Config, allow_insecure: bool) -> Result<()> {
    anyhow::ensure!(
        cfg.admin.bind.ip().is_loopback() || cfg.admin.token.is_some() || allow_insecure,
        "admin.bind is not loopback but admin.token is unset; set a token or pass --insecure for a trusted private network"
    );
    Ok(())
}

pub async fn run(explicit: Option<&Path>, args: RunArgs) -> Result<()> {
    let (mut cfg, source) = config::load(explicit)?;
    if let Some(bind) = args.bind {
        cfg.admin.bind = bind;
    }
    let use_tui = !args.headless && std::io::IsTerminal::is_terminal(&std::io::stdout());
    validate_admin_exposure(&cfg, args.insecure)?;

    let log_buffer = crate::tui::LogBuffer::new(500);
    init_tracing(&cfg, use_tui, &log_buffer);
    tracing::info!(version = crate::VERSION, config = %source, "sds starting");
    if !cfg.admin.bind.ip().is_loopback() && cfg.admin.token.is_none() {
        tracing::warn!(
            bind = %cfg.admin.bind,
            "admin API is reachable from the network without a token because --insecure was explicitly set"
        );
    }

    let backend: Arc<dyn Backend> = Arc::new(DockerEngine::connect(cfg.docker.host.as_deref())?);
    let version = backend
        .ping()
        .await
        .context("docker daemon not reachable (check docker.host / DOCKER_HOST and socket permissions)")?;
    tracing::info!(docker = %backend.describe(), "{version}");

    let cfg = Arc::new(cfg);
    let bus = Arc::new(EventBus::new(200));
    let registry = Arc::new(Registry::new(Arc::clone(&backend), Arc::clone(&bus)));
    let cancel = CancellationToken::new();

    let state =
        Arc::new(AppState { registry: Arc::clone(&registry), config: Arc::clone(&cfg), started_at: Instant::now() });
    let mut admin_task = Some(tokio::spawn(admin::serve(Arc::clone(&state), cancel.clone())));
    let mut supervisor_task =
        Some(tokio::spawn(Supervisor::new(Arc::clone(&registry), Arc::clone(&cfg), cancel.clone()).run()));

    // Reload on SIGHUP is reserved for a later version; today the daemon logs the request.
    let signal_task = tokio::spawn(wait_for_shutdown(cancel.clone(), explicit.map(Path::to_path_buf)));

    let result = if use_tui {
        let source =
            crate::tui::LocalSource { registry: Arc::clone(&registry), logs: log_buffer.clone(), bind: cfg.admin.bind };
        let ui = crate::tui::Dashboard::new(Box::new(source));
        let cancel_ui = cancel.clone();
        let mut ui_task = tokio::task::spawn_blocking(move || ui.run(cancel_ui));
        tokio::select! {
            outcome = &mut ui_task => match outcome {
                Ok(Ok(crate::tui::Exit::Detach)) if !cancel.is_cancelled() => {
                    log_buffer.detach();
                    eprintln!("sds: dashboard detached, daemon keeps running headless (reattach: `sds top`, stop: Ctrl-C)");
                    run_until_exit(&mut admin_task, &mut supervisor_task, &cancel).await
                }
                Ok(Ok(crate::tui::Exit::Quit | crate::tui::Exit::Detach)) => {
                    cancel.cancel();
                    Ok(())
                }
                Ok(Err(e)) => {
                    cancel.cancel();
                    Err(e.context("dashboard failed"))
                }
                Err(e) => {
                    cancel.cancel();
                    Err(anyhow::anyhow!("dashboard panicked: {e}"))
                }
            },
            // A dead admin server or supervisor ends the process even while the dashboard
            // is open; the dashboard observes the cancellation and restores the terminal.
            early = run_until_exit(&mut admin_task, &mut supervisor_task, &cancel) => {
                let _ = ui_task.await;
                early
            }
        }
    } else {
        run_until_exit(&mut admin_task, &mut supervisor_task, &cancel).await
    };

    tracing::info!("shutting down (containers are left in their current state)");
    if let Some(task) = supervisor_task.take() {
        // The registry waits for running `docker stop`s; give it its full budget plus slack.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(40), task).await;
    }
    if let Some(task) = admin_task.take()
        && let Ok(Ok(Err(e))) = tokio::time::timeout(std::time::Duration::from_secs(5), task).await
        && result.is_ok()
    {
        signal_task.abort();
        return Err(e);
    }
    signal_task.abort();
    result
}

/// Run until `cancel` fires (clean exit) or one of the core tasks ends on its own, which is
/// always an error: a port clash on the admin socket, a panic in the supervisor. Without
/// this the daemon would keep running half-dead while looking healthy to systemd.
async fn run_until_exit(
    admin: &mut Option<JoinHandle<Result<()>>>,
    supervisor: &mut Option<JoinHandle<()>>,
    cancel: &CancellationToken,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        res = join_once(admin) => {
            cancel.cancel();
            match res {
                Ok(Ok(())) => Err(anyhow::anyhow!("admin server stopped unexpectedly")),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(anyhow::anyhow!("admin server panicked: {e}")),
            }
        }
        res = join_once(supervisor) => {
            cancel.cancel();
            match res {
                Ok(()) => Err(anyhow::anyhow!("supervisor stopped unexpectedly")),
                Err(e) => Err(anyhow::anyhow!("supervisor panicked: {e}")),
            }
        }
    }
}

/// Await a task once and forget it afterwards (polling a finished `JoinHandle` panics).
async fn join_once<T>(task: &mut Option<JoinHandle<T>>) -> Result<T, tokio::task::JoinError> {
    match task.as_mut() {
        Some(handle) => {
            let result = handle.await;
            *task = None;
            result
        }
        None => std::future::pending().await,
    }
}

fn init_tracing(cfg: &Config, use_tui: bool, buffer: &crate::tui::LogBuffer) {
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_env("SDS_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_new(&cfg.log.filter))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if use_tui {
        let layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(buffer.clone());
        tracing_subscriber::registry().with(filter).with(layer).init();
    } else if cfg.log.json {
        let layer = tracing_subscriber::fmt::layer().json().with_writer(std::io::stderr);
        tracing_subscriber::registry().with(filter).with(layer).init();
    } else {
        let layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_target(false);
        tracing_subscriber::registry().with(filter).with(layer).init();
    }
}

async fn wait_for_shutdown(cancel: CancellationToken, _config: Option<PathBuf>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cannot listen for SIGTERM: {e}");
                let _ = tokio::signal::ctrl_c().await;
                cancel.cancel();
                return;
            }
        };
        let mut hup = signal(SignalKind::hangup()).ok();
        // Created once: a fresh ctrl_c() per iteration would miss a SIGINT arriving between.
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        loop {
            tokio::select! {
                _ = &mut ctrl_c => { tracing::info!("SIGINT received"); break; }
                _ = term.recv() => { tracing::info!("SIGTERM received"); break; }
                _ = async { match hup.as_mut() { Some(h) => { h.recv().await; } None => std::future::pending::<()>().await } } => {
                    tracing::info!("SIGHUP received: configuration reload is not supported yet; restart the service to apply changes");
                }
                _ = cancel.cancelled() => return,
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    cancel.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_admin_requires_a_token_or_an_explicit_insecure_override() {
        let mut cfg = Config::default();
        cfg.admin.bind = "0.0.0.0:9900".parse().unwrap();
        assert!(validate_admin_exposure(&cfg, false).is_err());
        assert!(validate_admin_exposure(&cfg, true).is_ok());
        cfg.admin.token = Some("secret".into());
        assert!(validate_admin_exposure(&cfg, false).is_ok());
        cfg.admin.bind = "127.0.0.1:9900".parse().unwrap();
        cfg.admin.token = None;
        assert!(validate_admin_exposure(&cfg, false).is_ok());
    }
}
