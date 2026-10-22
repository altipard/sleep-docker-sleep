//! Keeps the registry in sync with Docker: periodic discovery plus an event stream that
//! triggers immediate re-discovery when containers appear or vanish.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::registry::Registry;
use super::{EventKind, SdsEvent};
use crate::config::Config;
use crate::docker::{EventAction, discover};

const EVENT_DEBOUNCE: Duration = Duration::from_millis(300);
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

pub struct Supervisor {
    registry: Arc<Registry>,
    config: Arc<Config>,
    cancel: CancellationToken,
}

impl Supervisor {
    pub fn new(registry: Arc<Registry>, config: Arc<Config>, cancel: CancellationToken) -> Self {
        Self { registry, config, cancel }
    }

    /// Run one discovery immediately (so `sds run` shows groups right away), then keep
    /// going until `cancel` fires.
    pub async fn run(self) {
        let refresh = Arc::new(Notify::new());
        self.discover_once().await;

        let events_task =
            tokio::spawn(events_loop(Arc::clone(&self.registry), Arc::clone(&refresh), self.cancel.clone()));

        let mut interval = tokio::time::interval(self.config.discovery.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // first tick fires immediately; we already discovered
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = interval.tick() => self.discover_once().await,
                _ = refresh.notified() => {
                    tokio::time::sleep(EVENT_DEBOUNCE).await;
                    self.discover_once().await;
                }
            }
        }
        events_task.abort();
        self.registry.shutdown();
    }

    async fn discover_once(&self) {
        match self.registry.backend().list().await {
            Ok(containers) => {
                let discovery = discover(&containers, &self.config);
                self.registry.apply(discovery);
            }
            Err(e) => {
                tracing::warn!("discovery failed: {e}");
            }
        }
    }
}

async fn events_loop(registry: Arc<Registry>, refresh: Arc<Notify>, cancel: CancellationToken) {
    let fanout = registry.container_events();
    let mut backoff = RECONNECT_MIN;
    loop {
        let mut stream = registry.backend().events();
        tracing::debug!("subscribed to docker events");
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                item = stream.next() => match item {
                    Some(Ok(ev)) => {
                        backoff = RECONNECT_MIN;
                        if matches!(ev.action, EventAction::Create | EventAction::Destroy | EventAction::Rename) {
                            refresh.notify_one();
                        }
                        let _ = fanout.send(ev);
                    }
                    Some(Err(e)) => {
                        tracing::warn!("docker event stream error: {e}");
                        break;
                    }
                    None => {
                        tracing::warn!("docker event stream ended");
                        break;
                    }
                }
            }
        }
        registry.bus().emit(SdsEvent::new(
            "-",
            EventKind::Warning,
            format!("docker event stream lost; reconnecting in {}", humantime::format_duration(backoff)),
        ));
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
        refresh.notify_one();
    }
}
