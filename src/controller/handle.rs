use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::activity::{Activity, InflightGuard};
use super::signals::Signals;
use super::snapshot::{GroupSnapshot, StateKind};
use super::{FAILED_RETRY_INTERVAL, mono_ms};
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
///
/// Everything here is lock-free and never touches Docker: atomics on [`Activity`], a
/// `watch` receiver for the state and flags on [`Signals`] to poke the actor.
#[derive(Debug)]
pub struct GroupHandle {
    name: String,
    activity: Arc<Activity>,
    state: watch::Receiver<GroupSnapshot>,
    signals: Arc<Signals>,
}

impl GroupHandle {
    pub(super) fn new(
        name: String,
        activity: Arc<Activity>,
        state: watch::Receiver<GroupSnapshot>,
        signals: Arc<Signals>,
    ) -> Self {
        Self { name, activity, state, signals }
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

    /// Ask the actor to start the group because traffic arrived. Never blocks, never lost;
    /// the actor ignores it while a failed start is still inside its retry pause.
    pub fn request_wake(&self) {
        self.signals.request_wake();
    }

    /// Operator wake (API, TUI, CLI): starts even during the retry pause after a failure.
    pub fn force_wake(&self) {
        self.signals.force_wake();
    }

    pub fn request_sleep(&self) {
        self.signals.request_sleep();
    }

    /// Record activity and make sure the group is (or becomes) running, using the group's
    /// own `wake` setting.
    pub async fn ensure_ready(&self) -> WakeOutcome {
        self.ensure_ready_with(None).await
    }

    /// Like [`ensure_ready`](Self::ensure_ready), with `mode` overriding the group's
    /// `wake` setting (forward-auth `?mode=`, TCP proxy).
    ///
    /// Returns [`WakeOutcome::Ready`] immediately when the group runs. Otherwise triggers a
    /// wake-up and, depending on the mode, either returns [`WakeOutcome::Starting`] right
    /// away or holds until the group is ready, fails, or `hold_timeout` elapses.
    pub async fn ensure_ready_with(&self, mode: Option<WakeMode>) -> WakeOutcome {
        self.touch();
        let before = self.snapshot();
        if before.state == StateKind::Running {
            return WakeOutcome::Ready;
        }
        if before.in_retry_pause() {
            // The actor would ignore the wake anyway; answer without holding the request.
            return WakeOutcome::Failed(before.last_error.clone().unwrap_or_else(|| "start failed".into()));
        }
        self.request_wake();
        if mode.unwrap_or(before.settings.wake) == WakeMode::Page {
            return WakeOutcome::Starting;
        }
        self.hold(&before).await
    }

    /// Operator wake that blocks until the group is ready (`POST …/wake?wait=true`).
    pub async fn wake_and_wait(&self) -> WakeOutcome {
        let before = self.snapshot();
        if before.state == StateKind::Running {
            return WakeOutcome::Ready;
        }
        self.force_wake();
        self.hold(&before).await
    }

    /// Take a waiter slot and wait for the start triggered after `before` was taken.
    async fn hold(&self, before: &GroupSnapshot) -> WakeOutcome {
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

impl GroupSnapshot {
    /// `true` while a failed start is inside [`FAILED_RETRY_INTERVAL`]; traffic-driven wakes
    /// are ignored by the actor during that window.
    pub fn in_retry_pause(&self) -> bool {
        self.state == StateKind::Failed
            && Duration::from_millis(mono_ms().saturating_sub(self.state_since_mono_ms)) < FAILED_RETRY_INTERVAL
    }
}
