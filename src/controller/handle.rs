use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::activity::{Activity, InflightGuard};
use super::actor::Command;
use super::snapshot::{GroupSnapshot, StateKind};
use super::{FAILED_RETRY_INTERVAL_MS, epoch_ms};
use crate::config::WakeMode;

/// What a request handler should do after asking for the group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The group is running: forward the request.
    Ready,
    /// The group is starting and `wake = page`: answer with the waiting page.
    Starting,
    /// `hold_timeout` elapsed before the group became ready.
    Timeout,
    /// The last start attempt failed (and will be retried after a pause).
    Failed(String),
    /// `max_waiters` held requests already: shed load.
    Overloaded,
    /// The group disappeared while waiting.
    Gone,
}

/// Cheap, cloneable access to one group for request handlers, the API and the UI.
#[derive(Debug)]
pub struct GroupHandle {
    name: String,
    activity: Arc<Activity>,
    state: watch::Receiver<GroupSnapshot>,
    cmd: mpsc::Sender<Command>,
}

impl GroupHandle {
    pub(super) fn new(
        name: String,
        activity: Arc<Activity>,
        state: watch::Receiver<GroupSnapshot>,
        cmd: mpsc::Sender<Command>,
    ) -> Self {
        Self { name, activity, state, cmd }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn activity(&self) -> &Arc<Activity> {
        &self.activity
    }

    pub fn snapshot(&self) -> GroupSnapshot {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<GroupSnapshot> {
        self.state.clone()
    }

    pub fn is_running(&self) -> bool {
        self.state.borrow().state == StateKind::Running
    }

    /// Record a request (hot path: two atomic stores).
    pub fn touch(&self) {
        self.activity.touch();
    }

    pub fn begin_inflight(&self) -> InflightGuard {
        self.activity.begin_inflight()
    }

    /// Ask the actor to start the group. Non-blocking; a full queue means a wake is
    /// already pending, which is fine.
    pub fn request_wake(&self) {
        let _ = self.cmd.try_send(Command::Wake);
    }

    pub fn request_sleep(&self) {
        let _ = self.cmd.try_send(Command::Sleep);
    }

    /// Record activity and make sure the group is (or becomes) running.
    ///
    /// Returns [`WakeOutcome::Ready`] immediately when it already runs. Otherwise triggers a
    /// wake-up and, depending on `wake`, either returns [`WakeOutcome::Starting`] right away
    /// or holds until the group is ready, fails, or `hold_timeout` elapses.
    pub async fn ensure_ready(&self) -> WakeOutcome {
        self.touch();
        let before = self.snapshot();
        if before.state == StateKind::Running {
            return WakeOutcome::Ready;
        }
        if before.state == StateKind::Failed
            && epoch_ms().saturating_sub(before.state_since_ms) < FAILED_RETRY_INTERVAL_MS
        {
            return WakeOutcome::Failed(before.last_error.unwrap_or_else(|| "start failed".into()));
        }
        self.request_wake();
        if before.settings.wake == WakeMode::Page {
            return WakeOutcome::Starting;
        }
        let Some(_slot) = self.activity.try_wait(before.settings.max_waiters) else {
            return WakeOutcome::Overloaded;
        };
        self.wait_ready(before.settings.hold_timeout, before.wakes_total).await
    }

    /// Wait until the group is running or a *new* start attempt (one more than
    /// `wakes_before`) has failed.
    pub async fn wait_ready(&self, timeout: Duration, wakes_before: u64) -> WakeOutcome {
        let mut rx = self.state.clone();
        let waited = tokio::time::timeout(
            timeout,
            rx.wait_for(|s| {
                s.state == StateKind::Running || (s.state == StateKind::Failed && s.wakes_total > wakes_before)
            }),
        )
        .await;
        match waited {
            Ok(Ok(snap)) => match snap.state {
                StateKind::Running => WakeOutcome::Ready,
                _ => WakeOutcome::Failed(snap.last_error.clone().unwrap_or_else(|| "start failed".into())),
            },
            Ok(Err(_)) => WakeOutcome::Gone,
            Err(_) => WakeOutcome::Timeout,
        }
    }
}
