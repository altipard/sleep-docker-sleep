use serde::{Deserialize, Serialize};

use super::time::epoch_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Discovered,
    Removed,
    Waking,
    Running,
    StartFailed,
    Sleeping,
    Stopped,
    StopFailed,
    External,
    Warning,
}

/// Something an operator might want to see: shown in the web overview, the TUI log and
/// streamed over SSE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdsEvent {
    pub ts_ms: u64,
    pub group: String,
    pub kind: EventKind,
    pub message: String,
}

impl SdsEvent {
    pub fn new(group: impl Into<String>, kind: EventKind, message: impl Into<String>) -> Self {
        Self { ts_ms: epoch_ms(), group: group.into(), kind, message: message.into() }
    }
}

/// Fan-out of [`SdsEvent`]s plus a bounded history for late subscribers (web UI, TUI).
#[derive(Debug)]
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<SdsEvent>,
    recent: std::sync::Mutex<std::collections::VecDeque<SdsEvent>>,
    keep: usize,
}

impl EventBus {
    pub fn new(keep: usize) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(512);
        Self { tx, recent: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(keep)), keep }
    }

    pub fn emit(&self, event: SdsEvent) {
        match event.kind {
            EventKind::StartFailed | EventKind::StopFailed | EventKind::Warning => {
                tracing::warn!(group = %event.group, kind = ?event.kind, "{}", event.message);
            }
            _ => tracing::info!(group = %event.group, kind = ?event.kind, "{}", event.message),
        }
        if let Ok(mut recent) = self.recent.lock() {
            if recent.len() == self.keep {
                recent.pop_front();
            }
            recent.push_back(event.clone());
        }
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SdsEvent> {
        self.tx.subscribe()
    }

    pub fn recent(&self) -> Vec<SdsEvent> {
        self.recent.lock().map(|r| r.iter().cloned().collect()).unwrap_or_default()
    }
}
