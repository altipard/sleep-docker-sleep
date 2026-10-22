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
//! so the actor keeps reacting to commands and container events meanwhile.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use super::activity::Activity;
use super::events::{EventBus, EventKind, SdsEvent};
use super::handle::GroupHandle;
use super::health::{Probe, probe};
use super::snapshot::{GroupSnapshot, MemberSnapshot, StateKind};
use super::time::epoch_ms;
use crate::config::GroupSettings;
use crate::docker::{Backend, ContainerEvent, EventAction, GroupSpec, HealthState, MemberSpec};

const TICK: Duration = Duration::from_secs(1);
const RECONCILE_EVERY_TICKS: u32 = 10;
const PROBE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum Command {
    Wake,
    Sleep,
    Update(Box<GroupSpec>),
    Shutdown,
}

#[derive(Debug)]
enum Transition {
    Started,
    StartFailed(String),
    Stopped,
    StopFailed(String),
}

#[derive(Debug, Clone)]
struct MemberStatus {
    spec: MemberSpec,
    running: bool,
    health: HealthState,
}

pub(super) struct GroupActor {
    spec: GroupSpec,
    backend: Arc<dyn Backend>,
    activity: Arc<Activity>,
    bus: Arc<EventBus>,
    state_tx: watch::Sender<GroupSnapshot>,
    cmd_rx: mpsc::Receiver<Command>,
    container_events: broadcast::Receiver<ContainerEvent>,
    http: reqwest::Client,
    state: StateKind,
    state_since_ms: u64,
    members: Vec<MemberStatus>,
    wakes: u64,
    sleeps: u64,
    last_error: Option<String>,
    pending_wake: bool,
    transition: Option<JoinHandle<Transition>>,
}

/// Spawn the actor for `spec`. Returns the handle for request handlers plus the command
/// sender and task handle for the registry.
pub(super) fn spawn(
    spec: GroupSpec,
    backend: Arc<dyn Backend>,
    bus: Arc<EventBus>,
    container_events: broadcast::Receiver<ContainerEvent>,
) -> (Arc<GroupHandle>, mpsc::Sender<Command>, JoinHandle<()>) {
    let activity = Arc::new(Activity::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let members: Vec<MemberStatus> = spec
        .members
        .iter()
        .cloned()
        .map(|spec| MemberStatus { spec, running: false, health: HealthState::None })
        .collect();
    let http = reqwest::Client::builder().build().unwrap_or_default();
    let actor = GroupActor {
        spec,
        backend,
        activity: Arc::clone(&activity),
        bus,
        state_tx: watch::channel(GroupSnapshot {
            name: String::new(),
            state: StateKind::Stopped,
            state_since_ms: 0,
            last_activity_ms: None,
            inflight: 0,
            requests_total: 0,
            wakes_total: 0,
            sleeps_total: 0,
            members: Vec::new(),
            last_error: None,
            warnings: Vec::new(),
            source: crate::docker::GroupSource::Labels,
            settings: GroupSettings::default(),
        })
        .0,
        cmd_rx,
        container_events,
        http,
        state: StateKind::Stopped,
        state_since_ms: epoch_ms(),
        members,
        wakes: 0,
        sleeps: 0,
        last_error: None,
        pending_wake: false,
        transition: None,
    };
    let state_rx = actor.state_tx.subscribe();
    actor.publish();
    let handle = Arc::new(GroupHandle::new(actor.spec.name.clone(), activity, state_rx, cmd_tx.clone()));
    let task = tokio::spawn(async move { actor.run().await });
    (handle, cmd_tx, task)
}

impl GroupActor {
    async fn run(mut self) {
        self.reconcile().await;
        self.publish();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ticks: u32 = 0;
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => match cmd {
                    Some(Command::Wake) => self.on_wake(),
                    Some(Command::Sleep) => self.on_sleep(true),
                    Some(Command::Update(spec)) => self.on_update(*spec).await,
                    Some(Command::Shutdown) | None => break,
                },
                ev = self.container_events.recv() => match ev {
                    Ok(ev) => self.on_container_event(ev),
                    Err(broadcast::error::RecvError::Lagged(_)) => self.reconcile().await,
                    Err(broadcast::error::RecvError::Closed) => {
                        // No more events: fall back to periodic reconciliation only.
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                },
                result = async { match self.transition.as_mut() { Some(t) => t.await, None => std::future::pending().await } }, if self.transition.is_some() => {
                    self.transition = None;
                    match result {
                        Ok(t) => self.on_transition(t),
                        Err(e) => self.on_transition(Transition::StartFailed(format!("internal task failed: {e}"))),
                    }
                }
                _ = tick.tick() => {
                    ticks = ticks.wrapping_add(1);
                    self.check_idle();
                    if ticks % RECONCILE_EVERY_TICKS == 0 && self.transition.is_none() {
                        self.reconcile().await;
                    }
                }
            }
            self.publish();
        }
        if let Some(t) = self.transition.take() {
            t.abort();
        }
    }

    // ----- commands ---------------------------------------------------------------------

    fn on_wake(&mut self) {
        match self.state {
            StateKind::Stopped | StateKind::Failed => self.begin_start(),
            StateKind::Stopping => self.pending_wake = true,
            StateKind::Starting | StateKind::Running => {}
        }
    }

    fn on_sleep(&mut self, manual: bool) {
        match self.state {
            StateKind::Running => self.begin_stop(manual),
            StateKind::Starting => {
                self.emit(EventKind::Warning, "sleep requested while starting; ignored");
            }
            StateKind::Stopped | StateKind::Stopping | StateKind::Failed => {}
        }
    }

    async fn on_update(&mut self, spec: GroupSpec) {
        let members_changed = spec.members != self.spec.members;
        self.spec = spec;
        if members_changed {
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
                        running: known.is_some_and(|k| k.running),
                        health: known.map(|k| k.health).unwrap_or_default(),
                    }
                })
                .collect();
            if self.transition.is_none() {
                self.reconcile().await;
            }
        }
    }

    // ----- transitions ------------------------------------------------------------------

    fn begin_start(&mut self) {
        if self.members.is_empty() {
            self.set_state(StateKind::Failed);
            self.last_error = Some("no containers in this group".into());
            self.emit(EventKind::StartFailed, "cannot wake: no containers in this group");
            return;
        }
        self.wakes += 1;
        self.pending_wake = false;
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
        self.set_state(StateKind::Stopping);
        let why = if manual {
            "sleep requested".to_owned()
        } else {
            format!("idle for {}", humantime::format_duration(self.spec.settings.idle_timeout))
        };
        self.emit(EventKind::Sleeping, format!("stopping {} container(s): {why}", self.members.len()));
        let backend = Arc::clone(&self.backend);
        let members: Vec<MemberSpec> = self.members.iter().map(|m| m.spec.clone()).collect();
        let timeout = self.spec.settings.stop_timeout;
        self.transition = Some(tokio::spawn(stop_group(backend, members, timeout)));
    }

    fn on_transition(&mut self, t: Transition) {
        match t {
            Transition::Started => {
                for m in &mut self.members {
                    m.running = true;
                }
                self.last_error = None;
                self.activity.mark();
                self.set_state(StateKind::Running);
                self.emit(EventKind::Running, "ready, forwarding traffic");
            }
            Transition::StartFailed(reason) => {
                self.last_error = Some(reason.clone());
                self.set_state(StateKind::Failed);
                self.emit(EventKind::StartFailed, format!("wake-up failed: {reason}"));
            }
            Transition::Stopped => {
                for m in &mut self.members {
                    m.running = false;
                    m.health = HealthState::None;
                }
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::Stopped, "asleep");
                if self.pending_wake {
                    self.emit(EventKind::Waking, "request arrived while stopping; waking again");
                    self.begin_start();
                }
            }
            Transition::StopFailed(reason) => {
                self.last_error = Some(reason.clone());
                // Containers are in an unknown mix; reconcile on the next tick decides.
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::StopFailed, format!("stop failed: {reason}"));
                if self.pending_wake {
                    self.begin_start();
                }
            }
        }
    }

    fn check_idle(&mut self) {
        if self.state != StateKind::Running || self.transition.is_some() {
            return;
        }
        if self.activity.inflight() > 0 {
            return;
        }
        let idle = self.activity.idle_ms().unwrap_or(0);
        let limit = u64::try_from(self.spec.settings.idle_timeout.as_millis()).unwrap_or(u64::MAX);
        if idle >= limit {
            self.begin_stop(false);
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
        if self.transition.is_some() {
            return;
        }
        match (self.state, self.all_running(), self.none_running()) {
            (StateKind::Running, false, _) => {
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::External, format!("container `{name}` stopped outside sds; group is asleep"));
            }
            (StateKind::Stopped | StateKind::Failed, true, _) => {
                self.last_error = None;
                self.activity.mark();
                self.set_state(StateKind::Running);
                self.emit(EventKind::External, format!("container `{name}` started outside sds; group is running"));
            }
            _ => {}
        }
    }

    /// Inspect every member and align the state with reality (missed events, restarts).
    async fn reconcile(&mut self) {
        for m in &mut self.members {
            match self.backend.inspect(&m.spec.id).await {
                Ok(info) => {
                    m.running = info.running;
                    m.health = info.health;
                }
                Err(e) => {
                    tracing::debug!(group = %self.spec.name, container = %m.spec.name, "inspect failed: {e}");
                    m.running = false;
                }
            }
        }
        if self.transition.is_some() {
            return;
        }
        match (self.state, self.all_running()) {
            (StateKind::Running, false) => {
                self.set_state(StateKind::Stopped);
                self.emit(EventKind::External, "containers are not all running; group is asleep");
            }
            (StateKind::Stopped, true) => {
                self.activity.mark();
                self.set_state(StateKind::Running);
                self.emit(EventKind::External, "containers are running; group is awake");
            }
            _ => {}
        }
    }

    fn all_running(&self) -> bool {
        !self.members.is_empty() && self.members.iter().all(|m| m.running)
    }

    fn none_running(&self) -> bool {
        self.members.iter().all(|m| !m.running)
    }

    fn set_state(&mut self, state: StateKind) {
        if self.state != state {
            self.state = state;
            self.state_since_ms = epoch_ms();
        }
    }

    fn emit(&self, kind: EventKind, message: impl Into<String>) {
        self.bus.emit(SdsEvent::new(self.spec.name.clone(), kind, message));
    }

    fn publish(&self) {
        let snapshot = GroupSnapshot {
            name: self.spec.name.clone(),
            state: self.state,
            state_since_ms: self.state_since_ms,
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
        };
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
            Ok((Probe::Ready, _)) => return Transition::Started,
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
    if errors.is_empty() { Transition::Stopped } else { Transition::StopFailed(errors.join("; ")) }
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures_util::StreamExt;

    use super::*;
    use crate::docker::{BackendError, ContainerInfo, GroupSource};

    /// In-memory backend: containers flip state on start/stop; optional failure injection.
    #[derive(Default)]
    pub struct MockBackend {
        pub containers: Mutex<HashMap<String, ContainerInfo>>,
        pub fail_start: Mutex<Option<String>>,
        pub start_calls: Mutex<Vec<String>>,
        pub stop_calls: Mutex<Vec<String>>,
        pub events: Mutex<Option<broadcast::Sender<ContainerEvent>>>,
    }

    impl MockBackend {
        pub fn with(names: &[&str], running: bool) -> Arc<Self> {
            let backend = Self::default();
            for n in names {
                backend.containers.lock().unwrap().insert(
                    format!("id-{n}"),
                    ContainerInfo {
                        id: format!("id-{n}"),
                        name: (*n).to_owned(),
                        labels: HashMap::new(),
                        running,
                        status: if running { "running".into() } else { "exited".into() },
                        health: HealthState::None,
                        has_healthcheck: false,
                    },
                );
            }
            Arc::new(backend)
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
    ) -> (Arc<GroupHandle>, mpsc::Sender<Command>, Arc<EventBus>, broadcast::Sender<ContainerEvent>) {
        let bus = Arc::new(EventBus::new(50));
        let (ev_tx, ev_rx) = broadcast::channel(16);
        let backend: Arc<dyn Backend> = backend;
        let (handle, cmd, _task) = spawn(spec, backend, Arc::clone(&bus), ev_rx);
        (handle, cmd, bus, ev_tx)
    }

    #[tokio::test]
    async fn wake_starts_all_members_and_becomes_running() {
        let backend = MockBackend::with(&["app", "db"], false);
        let (handle, _cmd, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app", "db"], fast_settings()));
        wait_state(&handle, StateKind::Stopped).await;
        assert_eq!(handle.ensure_ready().await, super::super::handle::WakeOutcome::Ready);
        let snap = handle.snapshot();
        assert_eq!(snap.state, StateKind::Running);
        assert_eq!(snap.wakes_total, 1);
        assert_eq!(backend.start_calls.lock().unwrap().len(), 2);
        assert!(snap.members.iter().all(|m| m.running));
    }

    #[tokio::test]
    async fn idle_group_is_stopped_and_can_wake_again() {
        let backend = MockBackend::with(&["app"], true);
        let (handle, _cmd, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        wait_state(&handle, StateKind::Running).await;
        let snap = wait_state(&handle, StateKind::Stopped).await;
        assert_eq!(snap.sleeps_total, 1);
        assert_eq!(backend.stop_calls.lock().unwrap().as_slice(), ["id-app"]);
        assert_eq!(handle.ensure_ready().await, super::super::handle::WakeOutcome::Ready);
    }

    #[tokio::test]
    async fn open_connections_keep_the_group_awake() {
        let backend = MockBackend::with(&["app"], true);
        let (handle, _cmd, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        wait_state(&handle, StateKind::Running).await;
        let guard = handle.begin_inflight();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(handle.snapshot().state, StateKind::Running);
        drop(guard);
        wait_state(&handle, StateKind::Stopped).await;
    }

    #[tokio::test]
    async fn start_failure_is_reported_and_rate_limited() {
        let backend = MockBackend::with(&["app"], false);
        *backend.fail_start.lock().unwrap() = Some("boom".into());
        let (handle, _cmd, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        wait_state(&handle, StateKind::Stopped).await;
        match handle.ensure_ready().await {
            super::super::handle::WakeOutcome::Failed(reason) => assert!(reason.contains("boom"), "{reason}"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(handle.snapshot().state, StateKind::Failed);
        // A second request within the retry interval does not hit Docker again.
        let calls = backend.start_calls.lock().unwrap().len();
        assert!(matches!(handle.ensure_ready().await, super::super::handle::WakeOutcome::Failed(_)));
        assert_eq!(backend.start_calls.lock().unwrap().len(), calls);
    }

    #[tokio::test]
    async fn external_stop_and_start_are_tracked_via_events() {
        let backend = MockBackend::with(&["app"], true);
        let (handle, _cmd, _bus, ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        wait_state(&handle, StateKind::Running).await;
        backend.set_running("app", false);
        ev.send(ContainerEvent { id: "id-app".into(), name: Some("app".into()), action: EventAction::Die }).unwrap();
        wait_state(&handle, StateKind::Stopped).await;
        backend.set_running("app", true);
        ev.send(ContainerEvent { id: "id-app".into(), name: Some("app".into()), action: EventAction::Start }).unwrap();
        wait_state(&handle, StateKind::Running).await;
    }

    #[tokio::test]
    async fn wake_during_stop_restarts_afterwards() {
        let backend = MockBackend::with(&["app"], true);
        let (handle, cmd, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], fast_settings()));
        wait_state(&handle, StateKind::Running).await;
        cmd.send(Command::Sleep).await.unwrap();
        // Stopping is near-instant with the mock, so send the wake right behind the sleep.
        cmd.send(Command::Wake).await.unwrap();
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
    async fn page_mode_returns_immediately() {
        let backend = MockBackend::with(&["app"], false);
        let settings = GroupSettings { wake: crate::config::WakeMode::Page, ..fast_settings() };
        let (handle, _cmd, _bus, _ev) = setup(Arc::clone(&backend), spec(&["app"], settings));
        wait_state(&handle, StateKind::Stopped).await;
        assert_eq!(handle.ensure_ready().await, super::super::handle::WakeOutcome::Starting);
        wait_state(&handle, StateKind::Running).await;
    }
}
