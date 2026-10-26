//! The per-group state machine.
//!
//! ```text
//!            wake                 healthy                idle / sleep
//!  Stopped ───────► Starting ──────────────► Running ─────────────────► Stopping
//!     ▲                │ start failed            │ member died              │
//!     │                ▼                         ▼                          │
//!     │             Failed ──wake (≥10s later)──► Starting                   │
//!     └────────────────────────────────── stopped ◄─────────────────────────┘
//!                                         (wake pending? → Starting again)
//! ```
//!
//! Long operations (`docker start` + health probing, `docker stop`) run in a separate task
//! so the actor keeps reacting to signals and container events meanwhile.
//!
//! Inputs are lossless by construction: the desired [`GroupSpec`] arrives through a
//! `watch` channel (latest value wins), wake and sleep are flags on [`Signals`], and
//! shutdown is a [`CancellationToken`]. Policy such as the retry pause after a failed start
//! lives here, not in the callers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::FAILED_RETRY_INTERVAL;
use super::activity::Activity;
use super::events::{EventBus, EventKind, SdsEvent};
use super::handle::GroupHandle;
use super::health::{Probe, probe};
use super::signals::Signals;
use super::snapshot::{GroupSnapshot, MemberSnapshot, StateKind};
use super::time::{epoch_ms, mono_ms};
use crate::config::GroupSettings;
use crate::docker::{Backend, BackendError, ContainerEvent, EventAction, GroupSpec, HealthState, MemberSpec};

const PROBE_INTERVAL: Duration = Duration::from_millis(500);
/// Upper bound for one `inspect` during reconciliation so a hanging daemon cannot block
/// the actor loop.
const INSPECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Extra time granted to a running `docker stop` when the actor is shut down.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Loop timing; the defaults are production values, tests shorten them.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    pub tick: Duration,
    pub reconcile_every_ticks: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self { tick: Duration::from_secs(1), reconcile_every_ticks: 10 }
    }
}

#[derive(Debug)]
enum Transition {
    /// All listed container ids were started and the group is healthy.
    Started(Vec<String>),
    StartFailed(String),
    /// All listed container ids were stopped.
    Stopped(Vec<String>),
    StopFailed(String),
}

#[derive(Debug, Clone)]
struct MemberStatus {
    spec: MemberSpec,
    running: bool,
    health: HealthState,
}

/// Everything the registry keeps per actor.
pub(super) struct ActorLink {
    pub handle: Arc<GroupHandle>,
    pub spec_tx: watch::Sender<GroupSpec>,
    pub cancel: CancellationToken,
    pub task: JoinHandle<()>,
}

pub(super) struct GroupActor {
    spec: GroupSpec,
    spec_rx: watch::Receiver<GroupSpec>,
    backend: Arc<dyn Backend>,
    activity: Arc<Activity>,
    signals: Arc<Signals>,
    cancel: CancellationToken,
    bus: Arc<EventBus>,
    state_tx: watch::Sender<GroupSnapshot>,
    container_events: broadcast::Receiver<ContainerEvent>,
    http: reqwest::Client,
    timing: Timing,
    state: StateKind,
    state_since_ms: u64,
    state_since_mono_ms: u64,
    members: Vec<MemberStatus>,
    wakes: u64,
    sleeps: u64,
    last_error: Option<String>,
    /// Monotonic ms of the last failed start; the retry pause is anchored here and survives
    /// a cleanup stop (`Failed` → `Stopping` → `Stopped`).
    failed_at_mono_ms: Option<u64>,
    /// Monotonic ms since the group has been "asleep but with members running" (after a
    /// failed/partial start, an external start or a failed stop). Cleanup waits
    /// `idle_timeout` from here, so a failing `docker stop` is retried with backoff instead
    /// of every tick.
    partial_since_mono_ms: Option<u64>,
    /// After a failed `docker stop`, no automatic stop before this monotonic ms.
    stop_backoff_until_mono_ms: Option<u64>,
    pending_wake: bool,
    /// On hold: wakes are ignored until released. Owned by the registry, which passes it
    /// to a respawned actor so a group recreated by Compose stays on hold.
    held: bool,
    transition: Option<JoinHandle<Transition>>,
}

/// Spawn the actor for `spec`.
pub(super) fn spawn(
    spec: GroupSpec,
    backend: Arc<dyn Backend>,
    bus: Arc<EventBus>,
    container_events: broadcast::Receiver<ContainerEvent>,
    http: reqwest::Client,
    timing: Timing,
    held: bool,
) -> ActorLink {
    let activity = Arc::new(Activity::default());
    let signals = Arc::new(Signals::default());
    let cancel = CancellationToken::new();
    let (spec_tx, spec_rx) = watch::channel(spec.clone());
    let members: Vec<MemberStatus> = spec
        .members
        .iter()
        .cloned()
        .map(|spec| MemberStatus { spec, running: false, health: HealthState::None })
        .collect();
    let mut actor = GroupActor {
        spec,
        spec_rx,
        backend,
        activity: Arc::clone(&activity),
        signals: Arc::clone(&signals),
        cancel: cancel.clone(),
        bus,
        // Placeholder; replaced with the real initial snapshot right below.
        state_tx: watch::channel(GroupSnapshot::placeholder()).0,
        container_events,
        http,
        timing,
        state: StateKind::Stopped,
        state_since_ms: epoch_ms(),
        state_since_mono_ms: mono_ms(),
        members,
        wakes: 0,
        sleeps: 0,
        last_error: None,
        failed_at_mono_ms: None,
        partial_since_mono_ms: None,
        stop_backoff_until_mono_ms: None,
        pending_wake: false,
        held,
        transition: None,
    };
    let (state_tx, state_rx) = watch::channel(actor.snapshot());
    actor.state_tx = state_tx;
    let handle = Arc::new(GroupHandle::new(actor.spec.name.clone(), activity, state_rx, signals));
    let task = tokio::spawn(async move { actor.run().await });
    ActorLink { handle, spec_tx, cancel, task }
}

impl GroupActor {
    async fn run(mut self) {
        self.reconcile().await;
        self.publish();
        let signals = Arc::clone(&self.signals);
        let cancel = self.cancel.clone();
        let mut tick = tokio::time::interval(self.timing.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ticks: u32 = 0;
        let mut events_open = true;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = signals.notified() => {
                    let pending = signals.take();
                    // Hold first: a wake that raced with it must already see the hold.
                    if let Some(held) = pending.hold {
                        self.on_hold(held);
                    }
                    if pending.sleep {
                        self.on_sleep(true);
                    }
                    if pending.force_wake {
                        self.on_wake(true);
                    } else if pending.wake {
                        self.on_wake(false);
                    }
                }
                changed = self.spec_rx.changed() => match changed {
                    Ok(()) => {
                        let spec = self.spec_rx.borrow_and_update().clone();
                        self.on_update(spec).await;
                    }
                    // The registry dropped us without cancelling: treat it as shutdown.
                    Err(_) => break,
                },
                ev = self.container_events.recv(), if events_open => match ev {
                    Ok(ev) => self.on_container_event(ev),
                    Err(broadcast::error::RecvError::Lagged(_)) => self.reconcile().await,
                    Err(broadcast::error::RecvError::Closed) => {
                        // No more events: periodic reconciliation is the only observation left.
                        tracing::warn!(group = %self.spec.name, "container event stream closed; relying on reconciliation");
                        events_open = false;
                    }
                },
                result = join_transition(&mut self.transition), if self.transition.is_some() => {
                    self.transition = None;
                    match result {
                        Ok(t) => self.on_transition(t).await,
                        Err(e) => self.on_transition(Transition::StartFailed(format!("internal task failed: {e}"))).await,
                    }
                }
                _ = tick.tick() => {
                    ticks = ticks.wrapping_add(1);
                    self.check_idle();
                    if ticks % self.timing.reconcile_every_ticks == 0 && self.transition.is_none() {
                        self.reconcile().await;
                    }
                }
            }
            self.publish();
        }
        self.finish().await;
    }

    /// Let a running `docker stop` finish; abandon a start (its containers stay as they are).
    async fn finish(mut self) {
        let Some(task) = self.transition.take() else {
            return;
        };
        if self.state == StateKind::Stopping {
            let budget = self.spec.settings.stop_timeout + SHUTDOWN_GRACE;
            if tokio::time::timeout(budget, task).await.is_err() {
                tracing::warn!(group = %self.spec.name, "stop did not finish within {budget:?} at shutdown");
            }
        } else {
            task.abort();
        }
    }

    // ----- commands ---------------------------------------------------------------------

    fn on_wake(&mut self, force: bool) {
        if self.held {
            tracing::debug!(group = %self.spec.name, "wake ignored: group is on hold");
            return;
        }
        match self.state {
            StateKind::Stopped | StateKind::Failed => {
                let since_failure = self.failed_at_mono_ms.map(|t| Duration::from_millis(mono_ms().saturating_sub(t)));
                match since_failure {
                    Some(ago) if !force && ago < FAILED_RETRY_INTERVAL => {
                        tracing::debug!(group = %self.spec.name, "wake ignored: start failed {ago:?} ago");
                    }
                    _ => self.begin_start(),
                }
            }
            StateKind::Stopping => self.pending_wake = true,
            StateKind::Starting | StateKind::Running => {}
        }
    }

    fn on_sleep(&mut self, manual: bool) {
        match self.state {
            StateKind::Running => self.begin_stop(manual),
            // A failed or partial start may have left members running: a sleep cleans up.
            StateKind::Stopped | StateKind::Failed if self.any_running() => self.begin_stop(manual),
            StateKind::Starting => {
                self.emit(EventKind::Warning, "sleep requested while starting; ignored");
            }
            StateKind::Stopped | StateKind::Stopping | StateKind::Failed => {}
        }
    }

    fn on_hold(&mut self, held: bool) {
        if held == self.held {
            return;
        }
        self.held = held;
        if !held {
            self.emit(EventKind::Released, "released: the next request wakes the group");
            return;
        }
        self.pending_wake = false;
        self.emit(EventKind::Held, "on hold: requests no longer wake the group");
        // A start in progress is stopped as soon as it has finished (see `on_transition`).
        if self.state != StateKind::Starting {
            self.on_sleep(true);
        }
    }

    async fn on_update(&mut self, spec: GroupSpec) {
        let members_changed = spec.members != self.spec.members;
        self.spec = spec;
        if !members_changed {
            return;
        }
        let old: Vec<MemberStatus> = std::mem::take(&mut self.members);
        self.members = self
            .spec
            .members
            .iter()
            .cloned()
            .map(|m| {
                let known = old.iter().find(|o| o.spec.id == m.id);
                MemberStatus {
                    spec: m,
                    // A member we have not seen yet (container recreated by Compose) is
                    // assumed to share the group's state until `reconcile` confirms it, so
                    // a daemon that is unreachable right now does not flap a Running group.
                    running: known.map_or(self.state == StateKind::Running, |k| k.running),
                    health: known.map(|k| k.health).unwrap_or_default(),
                }
            })
            .collect();
        // During a transition the result handler notices the changed member list itself.
        if self.transition.is_none() {
            self.reconcile().await;
        }
    }

    // ----- transitions ------------------------------------------------------------------

    fn begin_start(&mut self) {
        // Count the attempt first: waiters resolve on `wakes_total > before`, also when the
        // attempt fails right here.
        self.wakes += 1;
        self.pending_wake = false;
        if self.members.is_empty() {
            self.last_error = Some("no containers in this group".into());
            self.set_state(StateKind::Failed);
            self.emit(EventKind::StartFailed, "cannot wake: no containers in this group");
            return;
        }
        self.set_state(StateKind::Starting);
        self.emit(EventKind::Waking, format!("starting {} container(s)", self.members.len()));
        let backend = Arc::clone(&self.backend);
        let members: Vec<MemberSpec> = self.members.iter().map(|m| m.spec.clone()).collect();
        let settings = self.spec.settings.clone();
        let http = self.http.clone();
        self.transition = Some(tokio::spawn(start_group(backend, members, settings, http)));
    }

    fn begin_stop(&mut self, manual: bool) {
        self.sleeps += 1;
        self.partial_since_mono_ms = None;
        let why = if self.held {
            "put on hold".to_owned()
        } else if manual {
            "sleep requested".to_owned()
        } else if self.state == StateKind::Running {
            format!("idle for {}", humantime::format_duration(self.spec.settings.idle_timeout))
        } else {
            "containers left running by a failed or partial start".to_owned()
        };
        self.set_state(StateKind::Stopping);
        self.emit(EventKind::Sleeping, format!("stopping {} container(s): {why}", self.members.len()));
        let backend = Arc::clone(&self.backend);
        let members: Vec<MemberSpec> = self.members.iter().map(|m| m.spec.clone()).collect();
        let timeout = self.spec.settings.stop_timeout;
        self.transition = Some(tokio::spawn(stop_group(backend, members, timeout)));
    }

    async fn on_transition(&mut self, t: Transition) {
        match t {
            Transition::Started(ids) => {
                for m in &mut self.members {
                    if ids.contains(&m.spec.id) {
                        m.running = true;
                    }
                }
                self.last_error = None;
                self.failed_at_mono_ms = None;
                self.activity.mark();
                if self.all_running() {
                    self.set_state(StateKind::Running);
                    self.emit(EventKind::Running, "ready, forwarding traffic");
                    if self.held {
                        // Put on hold while starting.
                        self.begin_stop(true);
                    }
                } else if self.held {
                    self.begin_stop(true);
                } else {
                    // The member list changed while we were starting (container recreated).
                    self.emit(EventKind::Waking, "members changed during start; starting again");
                    self.begin_start();
                }
            }
            Transition::StartFailed(reason) => {
                self.last_error = Some(reason.clone());
                self.failed_at_mono_ms = Some(mono_ms());
                self.set_state(StateKind::Failed);
                self.emit(EventKind::StartFailed, format!("wake-up failed: {reason}"));
                // Learn which members did come up so the idle check can stop them later.
                // Only the member flags: "running but never became healthy" must stay
                // Failed, so `refresh_members` does not settle.
                self.refresh_members().await;
                self.partial_since_mono_ms = self.any_running().then(mono_ms);
            }
            Transition::Stopped(ids) => {
                self.stop_backoff_until_mono_ms = None;
                self.last_error = None;
                for m in &mut self.members {
                    if ids.contains(&m.spec.id) {
                        m.running = false;
                        m.health = HealthState::None;
                    }
                }
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::Stopped, "asleep");
                if self.members.iter().any(|m| !ids.contains(&m.spec.id)) {
                    // Members changed while stopping: look at the ones we did not touch.
                    self.reconcile().await;
                }
                if self.pending_wake {
                    if self.state == StateKind::Stopped {
                        self.emit(EventKind::Waking, "request arrived while stopping; waking again");
                        self.begin_start();
                    } else {
                        self.pending_wake = false;
                    }
                }
            }
            Transition::StopFailed(reason) => {
                self.last_error = Some(reason.clone());
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::StopFailed, format!("stop failed: {reason}"));
                // Containers are in an unknown mix; look before deciding anything. The next
                // automatic stop waits a full idle_timeout (backoff, not a hot loop).
                let backoff = u64::try_from(self.spec.settings.idle_timeout.as_millis()).unwrap_or(u64::MAX);
                self.stop_backoff_until_mono_ms = Some(mono_ms().saturating_add(backoff));
                self.reconcile().await;
                self.partial_since_mono_ms = self.any_running().then(mono_ms);
                if self.pending_wake {
                    // Only start if the reconcile did not already find the group running.
                    if self.state == StateKind::Stopped {
                        self.begin_start();
                    } else {
                        self.pending_wake = false;
                    }
                }
            }
        }
    }

    fn check_idle(&mut self) {
        if self.transition.is_some() || self.activity.inflight() > 0 {
            return;
        }
        let limit = u64::try_from(self.spec.settings.idle_timeout.as_millis()).unwrap_or(u64::MAX);
        let now = mono_ms();
        if self.stop_backoff_until_mono_ms.is_some_and(|until| now < until) {
            return;
        }
        match self.state {
            StateKind::Running => {
                // Without any request yet, measure from the moment the group became running.
                let idle = self.activity.idle_ms().unwrap_or_else(|| now.saturating_sub(self.state_since_mono_ms));
                if idle >= limit {
                    self.begin_stop(false);
                }
            }
            // Asleep on paper, but members are running (failed/partial start, external
            // start of one member, failed stop): clean up once this has lasted idle_timeout.
            StateKind::Stopped | StateKind::Failed if self.any_running() => {
                let since = *self.partial_since_mono_ms.get_or_insert(now);
                if now.saturating_sub(since) >= limit {
                    self.begin_stop(false);
                }
            }
            StateKind::Stopped | StateKind::Failed | StateKind::Starting | StateKind::Stopping => {
                self.partial_since_mono_ms = None;
            }
        }
    }

    // ----- observation ------------------------------------------------------------------

    fn on_container_event(&mut self, ev: ContainerEvent) {
        let Some(member) = self.members.iter_mut().find(|m| m.spec.id == ev.id) else {
            return;
        };
        match ev.action {
            EventAction::Start => member.running = true,
            EventAction::Die | EventAction::Stop => {
                member.running = false;
                member.health = HealthState::None;
            }
            EventAction::Health(h) => member.health = h,
            EventAction::Create | EventAction::Destroy | EventAction::Rename | EventAction::Other(_) => return,
        }
        let name = member.spec.name.clone();
        self.settle(&format!("container `{name}`"));
    }

    /// Inspect every member (in parallel, each bounded by [`INSPECT_TIMEOUT`]) and align the
    /// state with reality: missed events, restarts, containers recreated by Compose.
    ///
    /// Only `NotFound` means "not running". Any other error (daemon unreachable, timeout)
    /// keeps the previous knowledge; a Docker API blip must not put a busy group to sleep.
    async fn reconcile(&mut self) {
        self.refresh_members().await;
        self.settle("reconciliation");
    }

    /// The inspect half of [`reconcile`](Self::reconcile): update member flags only.
    async fn refresh_members(&mut self) {
        let lookups = self.members.iter().map(|m| {
            let backend = Arc::clone(&self.backend);
            let id = m.spec.id.clone();
            async move { tokio::time::timeout(INSPECT_TIMEOUT, backend.inspect(&id)).await }
        });
        let results = futures_util::future::join_all(lookups).await;
        for (m, result) in self.members.iter_mut().zip(results) {
            match result {
                Ok(Ok(info)) => {
                    m.running = info.running;
                    m.health = info.health;
                }
                Ok(Err(BackendError::NotFound(_))) => {
                    m.running = false;
                    m.health = HealthState::None;
                }
                Ok(Err(e)) => {
                    tracing::warn!(group = %self.spec.name, container = %m.spec.name, "inspect failed, keeping last known state: {e}");
                }
                Err(_) => {
                    tracing::warn!(group = %self.spec.name, container = %m.spec.name, "inspect timed out, keeping last known state");
                }
            }
        }
    }

    /// Derive the group state from what the members do, unless a transition is in flight.
    fn settle(&mut self, source: &str) {
        if self.transition.is_some() {
            return;
        }
        match (self.state, self.all_running()) {
            (StateKind::Running, false) => {
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::External, format!("{source}: not all containers running; group is asleep"));
            }
            (StateKind::Stopped | StateKind::Starting | StateKind::Stopping, true) => {
                // `last_error` is kept: a failed stop stays visible while the group runs on.
                self.failed_at_mono_ms = None;
                self.partial_since_mono_ms = None;
                self.activity.mark();
                self.set_state(StateKind::Running);
                self.emit(EventKind::External, format!("{source}: containers are running; group is awake"));
            }
            // "Running" is not "healthy": a group whose start failed stays Failed until a new
            // start attempt has probed it, however its containers look meanwhile.
            (StateKind::Failed, true) => {}
            // A transition ended without a verdict (e.g. task failure): settle on Stopped.
            (StateKind::Starting | StateKind::Stopping, false) => self.set_state(StateKind::Stopped),
            (StateKind::Running, true) | (StateKind::Stopped | StateKind::Failed, false) => {}
        }
    }

    fn all_running(&self) -> bool {
        !self.members.is_empty() && self.members.iter().all(|m| m.running)
    }

    fn any_running(&self) -> bool {
        self.members.iter().any(|m| m.running)
    }

    fn set_state(&mut self, state: StateKind) {
        if self.state != state {
            self.state = state;
            self.state_since_ms = epoch_ms();
            self.state_since_mono_ms = mono_ms();
        }
    }

    fn emit(&self, kind: EventKind, message: impl Into<String>) {
        self.bus.emit(SdsEvent::new(self.spec.name.clone(), kind, message));
    }

    fn snapshot(&self) -> GroupSnapshot {
        GroupSnapshot {
            name: self.spec.name.clone(),
            state: self.state,
            held: self.held,
            state_since_ms: self.state_since_ms,
            state_since_mono_ms: self.state_since_mono_ms,
            failed_at_mono_ms: self.failed_at_mono_ms,
            last_activity_ms: self.activity.last_mono_ms().map(super::time::mono_to_epoch_ms),
            inflight: self.activity.inflight(),
            requests_total: self.activity.requests_total(),
            wakes_total: self.wakes,
            sleeps_total: self.sleeps,
            members: self
                .members
                .iter()
                .map(|m| MemberSnapshot {
                    name: m.spec.name.clone(),
                    id: m.spec.id.chars().take(12).collect(),
                    running: m.running,
                    health: m.health,
                })
                .collect(),
            last_error: self.last_error.clone(),
            warnings: self.spec.warnings.clone(),
            source: self.spec.source,
            settings: self.spec.settings.clone(),
        }
    }

    fn publish(&self) {
        let snapshot = self.snapshot();
        self.state_tx.send_if_modified(|current| {
            if *current == snapshot {
                false
            } else {
                *current = snapshot;
                true
            }
        });
    }
}

/// Resolve a running transition, or pend forever when there is none (the `select!` arm is
/// disabled in that case anyway).
async fn join_transition(
    transition: &mut Option<JoinHandle<Transition>>,
) -> Result<Transition, tokio::task::JoinError> {
    match transition.as_mut() {
        Some(t) => t.await,
        None => std::future::pending().await,
    }
}

async fn start_group(
    backend: Arc<dyn Backend>,
    members: Vec<MemberSpec>,
    settings: GroupSettings,
    http: reqwest::Client,
) -> Transition {
    for m in &members {
        if let Err(e) = backend.start(&m.id).await {
            return Transition::StartFailed(format!("docker start `{}`: {e}", m.name));
        }
    }
    let started = Instant::now();
    let mut last_reason;
    loop {
        match probe(&backend, &members, &settings, started, &http).await {
            Ok((Probe::Ready, _)) => return Transition::Started(members.into_iter().map(|m| m.id).collect()),
            Ok((Probe::Crashed(reason), _)) => return Transition::StartFailed(reason),
            Ok((Probe::NotYet(reason), _)) => last_reason = reason,
            Err(e) => last_reason = e,
        }
        if started.elapsed() >= settings.start_timeout {
            return Transition::StartFailed(format!(
                "not ready after {}: {last_reason}",
                humantime::format_duration(settings.start_timeout)
            ));
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

async fn stop_group(backend: Arc<dyn Backend>, members: Vec<MemberSpec>, timeout: Duration) -> Transition {
    let results = futures_util::future::join_all(members.iter().map(|m| {
        let backend = Arc::clone(&backend);
        async move { backend.stop(&m.id, timeout).await.map_err(|e| format!("docker stop `{}`: {e}", m.name)) }
    }))
    .await;
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    if errors.is_empty() {
        Transition::Stopped(members.into_iter().map(|m| m.id).collect())
    } else {
        Transition::StopFailed(errors.join("; "))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures_util::StreamExt;

    use super::*;
    use crate::controller::WakeOutcome;
    use crate::docker::{ContainerInfo, GroupSource};

    /// In-memory backend: containers flip state on start/stop; optional failure injection.
    #[derive(Default)]
    pub struct MockBackend {
        pub containers: Mutex<HashMap<String, ContainerInfo>>,
        pub fail_start: Mutex<Option<String>>,
        /// When set, `inspect` fails with `Unreachable` (simulates a daemon hiccup).
        pub fail_inspect: Mutex<Option<String>>,
        pub fail_stop: Mutex<Option<String>>,
        pub start_calls: Mutex<Vec<String>>,
        pub stop_calls: Mutex<Vec<String>>,
        pub events: Mutex<Option<broadcast::Sender<ContainerEvent>>>,
    }

    impl MockBackend {
        pub fn with(names: &[&str], running: bool) -> Arc<Self> {
            let backend = Self::default();
            for n in names {
                backend.add(n, running);
            }
            Arc::new(backend)
        }

        pub fn add(&self, name: &str, running: bool) {
            self.containers.lock().unwrap().insert(
                format!("id-{name}"),
                ContainerInfo {
                    id: format!("id-{name}"),
                    name: name.to_owned(),
                    labels: HashMap::new(),
                    running,
                    status: if running { "running".into() } else { "exited".into() },
                    health: HealthState::None,
                    has_healthcheck: false,
                },
            );
        }

        pub fn set_running(&self, name: &str, running: bool) {
            let mut c = self.containers.lock().unwrap();
            if let Some(info) = c.get_mut(&format!("id-{name}")) {
                info.running = running;
                info.status = if running { "running".into() } else { "exited".into() };
            }
        }
    }

    #[async_trait]
    impl Backend for MockBackend {
        fn describe(&self) -> String {
            "mock".into()
        }
        async fn ping(&self) -> Result<String, BackendError> {
            Ok("mock".into())
        }
        async fn list(&self) -> Result<Vec<ContainerInfo>, BackendError> {
            Ok(self.containers.lock().unwrap().values().cloned().collect())
        }
        async fn inspect(&self, id: &str) -> Result<ContainerInfo, BackendError> {
            if let Some(msg) = self.fail_inspect.lock().unwrap().clone() {
                return Err(BackendError::Unreachable(msg));
            }
            self.containers.lock().unwrap().get(id).cloned().ok_or_else(|| BackendError::NotFound(id.into()))
        }
        async fn start(&self, id: &str) -> Result<(), BackendError> {
            self.start_calls.lock().unwrap().push(id.to_owned());
            if let Some(msg) = self.fail_start.lock().unwrap().clone() {
                return Err(BackendError::Other(msg));
            }
            let mut c = self.containers.lock().unwrap();
            let info = c.get_mut(id).ok_or_else(|| BackendError::NotFound(id.into()))?;
            info.running = true;
            info.status = "running".into();
            Ok(())
        }
        async fn stop(&self, id: &str, _timeout: Duration) -> Result<(), BackendError> {
            self.stop_calls.lock().unwrap().push(id.to_owned());
            if let Some(msg) = self.fail_stop.lock().unwrap().clone() {
                return Err(BackendError::Other(msg));
            }
            let mut c = self.containers.lock().unwrap();
            let info = c.get_mut(id).ok_or_else(|| BackendError::NotFound(id.into()))?;
            info.running = false;
            info.status = "exited".into();
            Ok(())
        }
        fn events(&self) -> futures_util::stream::BoxStream<'static, Result<ContainerEvent, BackendError>> {
            let (tx, rx) = broadcast::channel(16);
            *self.events.lock().unwrap() = Some(tx);
            tokio_stream_from(rx).boxed()
        }
    }

    fn tokio_stream_from(
        rx: broadcast::Receiver<ContainerEvent>,
    ) -> impl futures_util::Stream<Item = Result<ContainerEvent, BackendError>> + Send {
        futures_util::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => return Some((Ok(ev), rx)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
    }

    pub fn spec(names: &[&str], settings: GroupSettings) -> GroupSpec {
        GroupSpec {
            name: "g".into(),
            members: names
                .iter()
                .map(|n| MemberSpec { id: format!("id-{n}"), name: (*n).to_owned(), compose_service: None })
                .collect(),
            settings,
            source: GroupSource::Labels,
            warnings: Vec::new(),
        }
    }

    pub fn fast_settings() -> GroupSettings {
        GroupSettings {
            idle_timeout: Duration::from_millis(1500),
            start_timeout: Duration::from_secs(5),
            stop_timeout: Duration::from_secs(1),
            start_grace: Duration::from_millis(50),
            hold_timeout: Duration::from_secs(5),
            ..GroupSettings::default()
        }
    }

    const FAST_TIMING: Timing = Timing { tick: Duration::from_millis(100), reconcile_every_ticks: 3 };

    async fn wait_state(handle: &GroupHandle, state: StateKind) -> GroupSnapshot {
        let mut rx = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(10), rx.wait_for(|s| s.state == state))
            .await
            .expect("timed out waiting for state")
            .expect("actor gone")
            .clone()
    }

    fn setup(
        backend: Arc<MockBackend>,
        spec: GroupSpec,
    ) -> (ActorLink, Arc<EventBus>, broadcast::Sender<ContainerEvent>) {
        let bus = Arc::new(EventBus::new(50));
        let (ev_tx, ev_rx) = broadcast::channel(16);
        let backend: Arc<dyn Backend> = backend;
        let link = spawn(spec, backend, Arc::clone(&bus), ev_rx, reqwest::Client::new(), FAST_TIMING, false);
        (link, bus, ev_tx)
    }

    #[tokio::test]
    async fn wake_starts_all_members_and_becomes_running() {
        let backend = MockBackend::with(&["app", "db"], false);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app", "db"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        assert_eq!(handle.ensure_ready().await, WakeOutcome::Ready);
        let snap = handle.snapshot();
        assert_eq!(snap.state, StateKind::Running);
        assert_eq!(snap.wakes_total, 1);
        assert_eq!(backend.start_calls.lock().unwrap().len(), 2);
        assert!(snap.members.iter().all(|m| m.running));
    }

    #[tokio::test]
    async fn idle_group_is_stopped_and_can_wake_again() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        let snap = wait_state(handle, StateKind::Stopped).await;
        assert_eq!(snap.sleeps_total, 1);
        assert_eq!(backend.stop_calls.lock().unwrap().as_slice(), ["id-app"]);
        assert_eq!(handle.ensure_ready().await, WakeOutcome::Ready);
    }

    #[tokio::test]
    async fn open_connections_keep_the_group_awake() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        let guard = handle.begin_inflight();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(handle.snapshot().state, StateKind::Running);
        drop(guard);
        wait_state(handle, StateKind::Stopped).await;
    }

    #[tokio::test]
    async fn start_failure_is_reported_and_rate_limited_in_the_actor() {
        let backend = MockBackend::with(&["app"], false);
        *backend.fail_start.lock().unwrap() = Some("boom".into());
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        match handle.ensure_ready().await {
            WakeOutcome::Failed(reason) => assert!(reason.contains("boom"), "{reason}"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(handle.snapshot().state, StateKind::Failed);
        let calls = backend.start_calls.lock().unwrap().len();
        // Traffic from any path (handle fast path or a raw wake) does not hit Docker again…
        assert!(matches!(handle.ensure_ready().await, WakeOutcome::Failed(_)));
        handle.request_wake();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(backend.start_calls.lock().unwrap().len(), calls);
        // …but an operator wake does.
        *backend.fail_start.lock().unwrap() = None;
        assert_eq!(handle.wake_and_wait().await, WakeOutcome::Ready);
        assert_eq!(backend.start_calls.lock().unwrap().len(), calls + 1);
    }

    #[tokio::test]
    async fn external_stop_and_start_are_tracked_via_events() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        backend.set_running("app", false);
        ev.send(ContainerEvent { id: "id-app".into(), name: Some("app".into()), action: EventAction::Die }).unwrap();
        wait_state(handle, StateKind::Stopped).await;
        backend.set_running("app", true);
        ev.send(ContainerEvent { id: "id-app".into(), name: Some("app".into()), action: EventAction::Start }).unwrap();
        wait_state(handle, StateKind::Running).await;
    }

    #[tokio::test]
    async fn wake_during_stop_restarts_afterwards() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        handle.request_sleep();
        // Stopping is near-instant with the mock, so send the wake right behind the sleep.
        handle.request_wake();
        let mut rx = handle.subscribe();
        let snap = tokio::time::timeout(
            Duration::from_secs(10),
            rx.wait_for(|s| s.state == StateKind::Running && s.sleeps_total == 1),
        )
        .await
        .expect("timed out")
        .expect("actor gone")
        .clone();
        assert_eq!(snap.wakes_total, 1);
        assert_eq!(backend.stop_calls.lock().unwrap().len(), 1);
        assert_eq!(backend.start_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn hold_stops_the_group_and_blocks_every_wake_until_released() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        handle.hold();
        let snap = wait_state(handle, StateKind::Stopped).await;
        assert!(snap.held);
        assert_eq!(backend.stop_calls.lock().unwrap().len(), 1);
        // Neither traffic nor an operator wake starts a held group.
        assert_eq!(handle.ensure_ready().await, WakeOutcome::Held);
        assert_eq!(handle.wake_and_wait().await, WakeOutcome::Held);
        handle.request_wake();
        handle.force_wake();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(backend.start_calls.lock().unwrap().is_empty());
        // Release does not start anything by itself; the next request does.
        handle.release();
        let mut rx = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| !s.held)).await.unwrap().unwrap();
        assert_eq!(handle.snapshot().state, StateKind::Stopped);
        assert_eq!(handle.ensure_ready().await, WakeOutcome::Ready);
    }

    #[tokio::test]
    async fn hold_during_start_stops_once_the_start_has_finished() {
        let backend = MockBackend::with(&["app"], false);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        handle.force_wake();
        wait_state(handle, StateKind::Starting).await;
        handle.hold();
        let mut rx = handle.subscribe();
        let snap = tokio::time::timeout(
            Duration::from_secs(10),
            rx.wait_for(|s| s.state == StateKind::Stopped && s.sleeps_total == 1),
        )
        .await
        .expect("timed out")
        .expect("actor gone")
        .clone();
        assert!(snap.held);
        assert_eq!(backend.stop_calls.lock().unwrap().as_slice(), ["id-app"]);
    }

    #[tokio::test]
    async fn page_mode_returns_immediately() {
        let backend = MockBackend::with(&["app"], false);
        let settings = GroupSettings { wake: crate::config::WakeMode::Page, ..fast_settings() };
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], settings));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        assert_eq!(handle.ensure_ready().await, WakeOutcome::Starting);
        wait_state(handle, StateKind::Running).await;
    }

    #[tokio::test]
    async fn spec_update_replaces_members_and_is_never_lost() {
        let backend = MockBackend::with(&["app"], true);
        backend.add("app-v2", true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        // Flood the actor with wakes, then send the update: nothing may be dropped.
        for _ in 0..1000 {
            handle.request_wake();
        }
        link.spec_tx.send_replace(spec(&["app-v2"], fast_settings()));
        let mut rx = handle.subscribe();
        let snap = tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| s.members[0].name == "app-v2"))
            .await
            .expect("update not applied")
            .unwrap()
            .clone();
        assert_eq!(snap.state, StateKind::Running);
        assert!(snap.members[0].running);
    }

    #[tokio::test]
    async fn inspect_errors_keep_the_last_known_state() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        *backend.fail_inspect.lock().unwrap() = Some("daemon hiccup".into());
        let _keep_awake = handle.begin_inflight();
        // Several reconcile rounds with a failing daemon.
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(handle.snapshot().state, StateKind::Running);
        assert!(handle.snapshot().members[0].running);
    }

    #[tokio::test]
    async fn half_running_group_is_put_to_sleep_after_idle_timeout() {
        let backend = MockBackend::with(&["app"], true);
        backend.add("db", false);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app", "db"], fast_settings()));
        let handle = &link.handle;
        let mut rx = handle.subscribe();
        // First reconcile: half running, group reported asleep.
        let snap = tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| s.members.iter().any(|m| m.running)))
            .await
            .expect("reconcile did not see the running member")
            .unwrap()
            .clone();
        assert_eq!(snap.state, StateKind::Stopped);
        tokio::time::timeout(
            Duration::from_secs(5),
            rx.wait_for(|s| {
                s.sleeps_total == 1 && s.state == StateKind::Stopped && s.members.iter().all(|m| !m.running)
            }),
        )
        .await
        .expect("partial group was not cleaned up")
        .unwrap();
        assert!(backend.stop_calls.lock().unwrap().contains(&"id-app".to_string()));
        assert!(!backend.containers.lock().unwrap()["id-app"].running);
    }

    #[tokio::test]
    async fn unhealthy_start_stays_failed_although_containers_run() {
        // `docker start` works but the health check never passes: the containers run, the
        // group must still be Failed and the waiter must get Failed, not Ready.
        let backend = MockBackend::with(&["app"], false);
        let settings = GroupSettings {
            health: crate::config::HealthSpec::Tcp("127.0.0.1:1".into()),
            start_timeout: Duration::from_millis(600),
            ..fast_settings()
        };
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], settings));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        assert!(matches!(handle.ensure_ready().await, WakeOutcome::Failed(_)));
        let snap = handle.snapshot();
        assert_eq!(snap.state, StateKind::Failed);
        assert!(snap.members[0].running, "member flags are refreshed");
        assert!(snap.last_error.is_some());
        // Stays Failed across reconcile rounds and traffic wakes…
        handle.request_wake();
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(handle.snapshot().state, StateKind::Failed);
        assert_eq!(backend.start_calls.lock().unwrap().len(), 1);
        // …and the running-but-unhealthy member is cleaned up after idle_timeout.
        let mut rx = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| s.sleeps_total == 1 && !s.members[0].running))
            .await
            .expect("unhealthy member was not stopped")
            .unwrap();
    }

    #[tokio::test]
    async fn failed_stop_is_retried_with_backoff_not_every_tick() {
        let backend = MockBackend::with(&["app"], true);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Running).await;
        *backend.fail_stop.lock().unwrap() = Some("busy".into());
        let mut rx = handle.subscribe();
        // First attempt after idle fails; the group is honestly reported running again.
        let snap = tokio::time::timeout(
            Duration::from_secs(5),
            rx.wait_for(|s| s.sleeps_total == 1 && s.last_error.is_some() && s.state == StateKind::Running),
        )
        .await
        .expect("no failed stop attempt reported")
        .unwrap()
        .clone();
        assert!(snap.last_error.as_deref().is_some_and(|e| e.contains("busy")), "{:?}", snap.last_error);
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(backend.stop_calls.lock().unwrap().len(), 1, "no hot loop");
        assert_eq!(handle.snapshot().state, StateKind::Running);
        *backend.fail_stop.lock().unwrap() = None;
        tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| s.sleeps_total == 2 && !s.members[0].running))
            .await
            .expect("second attempt did not happen")
            .unwrap();
    }

    #[tokio::test]
    async fn waiter_joining_a_running_start_sees_its_failure() {
        let backend = MockBackend::with(&["app"], false);
        let settings = GroupSettings {
            health: crate::config::HealthSpec::Tcp("127.0.0.1:1".into()),
            start_timeout: Duration::from_millis(800),
            ..fast_settings()
        };
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], settings));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        handle.request_wake();
        wait_state(handle, StateKind::Starting).await;
        // Joins the attempt in progress: must get Failed when it fails, not Timeout (5 s).
        let started = Instant::now();
        assert!(matches!(handle.ensure_ready().await, WakeOutcome::Failed(_)));
        assert!(started.elapsed() < Duration::from_secs(3), "waiter was held until hold_timeout");
    }

    #[tokio::test]
    async fn empty_group_resolves_waiters_immediately() {
        let backend = MockBackend::with(&[], false);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&[], fast_settings()));
        let handle = &link.handle;
        wait_state(handle, StateKind::Stopped).await;
        let started = Instant::now();
        assert!(matches!(handle.ensure_ready().await, WakeOutcome::Failed(_)));
        assert!(started.elapsed() < Duration::from_secs(2), "waiter was held until hold_timeout");
    }

    #[tokio::test]
    async fn cancel_stops_the_actor() {
        let backend = MockBackend::with(&["app"], false);
        let (link, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        wait_state(&link.handle, StateKind::Stopped).await;
        link.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), link.task).await.expect("actor did not exit").unwrap();
    }
}
