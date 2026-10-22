use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;
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
}

pub async fn run(explicit: Option<&Path>, args: RunArgs) -> Result<()> {
    let (mut cfg, source) = config::load(explicit)?;
    if let Some(bind) = args.bind {
        cfg.admin.bind = bind;
    }
    let use_tui = !args.headless && std::io::IsTerminal::is_terminal(&std::io::stdout());

    let log_buffer = crate::tui::LogBuffer::new(500);
    init_tracing(&cfg, use_tui, &log_buffer);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %source, "sds starting");

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
    let admin_task = tokio::spawn(admin::serve(Arc::clone(&state), cancel.clone()));
    let supervisor_task = tokio::spawn(Supervisor::new(Arc::clone(&registry), Arc::clone(&cfg), cancel.clone()).run());

    // Reload on SIGHUP is reserved for a later version; today the daemon logs the request.
    let signal_task = tokio::spawn(wait_for_shutdown(cancel.clone(), explicit.map(Path::to_path_buf)));

    if use_tui {
        let source =
            crate::tui::LocalSource { registry: Arc::clone(&registry), logs: log_buffer.clone(), bind: cfg.admin.bind };
        let ui = crate::tui::Dashboard::new(Box::new(source));
        let cancel_ui = cancel.clone();
        let outcome = tokio::task::spawn_blocking(move || ui.run(cancel_ui)).await;
        match outcome {
            Ok(Ok(crate::tui::Exit::Detach)) if !cancel.is_cancelled() => {
                log_buffer.detach();
                eprintln!("sds: dashboard detached, daemon keeps running headless (reattach: `sds top`, stop: Ctrl-C)");
                cancel.cancelled().await;
            }
            Ok(Ok(crate::tui::Exit::Quit | crate::tui::Exit::Detach)) => cancel.cancel(),
            Ok(Err(e)) => {
                cancel.cancel();
                tracing::error!("dashboard failed: {e}");
            }
            Err(e) => {
                cancel.cancel();
                tracing::error!("dashboard panicked: {e}");
            }
        }
    } else {
        cancel.cancelled().await;
    }

    tracing::info!("shutting down (containers are left in their current state)");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), supervisor_task).await;
    if let Ok(Ok(Err(e))) = tokio::time::timeout(std::time::Duration::from_secs(5), admin_task).await {
        return Err(e);
    }
    signal_task.abort();
    Ok(())
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
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => { tracing::info!("SIGINT received"); break; }
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
