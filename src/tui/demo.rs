//! Simulated groups for `sds top --demo`: no Docker, no daemon, no network.
//!
//! The simulation advances in fixed ticks of [`TICK_MS`] on a virtual clock that starts at a
//! fixed date, and request traffic comes from a hash of (group, tick). The same sequence of
//! key presses at the same virtual times therefore gives the same screens, whatever the poll
//! rate or machine speed, which keeps terminal recordings reproducible. Timeouts are
//! compressed to seconds so a short recording shows full sleep and wake cycles.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::Source;
use crate::config::GroupSettings;
use crate::controller::{EventKind, GroupSnapshot, MemberSnapshot, SdsEvent, StateKind};
use crate::docker::{GroupSource, HealthState};

/// Virtual time zero: 2026-01-01T00:00:00Z.
pub const DEMO_EPOCH_MS: u64 = 1_767_225_600_000;
const TICK_MS: u64 = 250;
/// The simulation runs this long before the first frame so counters and events look lived in.
const PREROLL_MS: u64 = 75_000;
const EVENT_KEEP: usize = 200;
/// After a failed start the next request retries only once this much time has passed.
const RETRY_PAUSE_MS: u64 = 10_000;
/// Demo only: after `sleep` the group stays untouched for this long even if its simulated
/// traffic would wake it again at once, so the key press stays visible.
const QUIET_AFTER_SLEEP_MS: u64 = 20_000;

/// Static description of one simulated project.
struct Script {
    name: &'static str,
    /// Container names; the first one is the database-like container that fails in `fails`.
    members: &'static [&'static str],
    idle_s: u64,
    start_s: u64,
    /// Every start ends `failed`.
    fails: bool,
    warning: Option<&'static str>,
    /// Requests arrive during `active_s` of every `period_s`, shifted by `offset_s`.
    period_s: u64,
    active_s: u64,
    offset_s: u64,
    /// At most this many requests per tick while traffic flows.
    intensity: u64,
    /// Starts out running (with `requests`, `wakes`, `sleeps` already counted).
    running_at_start: bool,
    held_at_start: bool,
    totals: (u64, u64, u64),
}

const SCRIPTS: &[Script] = &[
    Script {
        name: "blog",
        members: &["blog-web"],
        idle_s: 30,
        start_s: 2,
        fails: false,
        warning: None,
        period_s: 95,
        active_s: 45,
        offset_s: 0,
        intensity: 2,
        running_at_start: true,
        held_at_start: false,
        totals: (1_204, 31, 30),
    },
    Script {
        name: "shop",
        members: &["shop-postgres", "shop-redis", "shop-api"],
        idle_s: 45,
        start_s: 6,
        fails: false,
        warning: None,
        period_s: 130,
        active_s: 75,
        offset_s: 20,
        intensity: 5,
        running_at_start: true,
        held_at_start: false,
        totals: (48_730, 112, 111),
    },
    Script {
        name: "notes",
        members: &["notes-db", "notes-web"],
        idle_s: 30,
        start_s: 3,
        fails: false,
        warning: None,
        period_s: 100,
        active_s: 25,
        offset_s: 55,
        intensity: 1,
        running_at_start: false,
        held_at_start: false,
        totals: (356, 14, 14),
    },
    Script {
        name: "gallery",
        members: &["gallery-minio", "gallery-web"],
        idle_s: 40,
        start_s: 4,
        fails: false,
        warning: None,
        period_s: 160,
        active_s: 18,
        offset_s: 105,
        intensity: 2,
        running_at_start: false,
        held_at_start: false,
        totals: (92, 6, 6),
    },
    Script {
        name: "ml-lab",
        members: &["mlflow-db", "mlflow-ui", "jupyter"],
        idle_s: 60,
        start_s: 7,
        fails: false,
        warning: None,
        period_s: 300,
        active_s: 30,
        offset_s: 190,
        intensity: 1,
        running_at_start: false,
        held_at_start: false,
        totals: (41, 3, 3),
    },
    Script {
        name: "legacy-php",
        members: &["legacy-mysql", "legacy-php"],
        idle_s: 30,
        start_s: 4,
        fails: true,
        warning: Some("container `legacy-php` sets different sds.* values than `legacy-mysql`; the first one wins"),
        period_s: 140,
        active_s: 30,
        offset_s: 20,
        intensity: 1,
        running_at_start: false,
        held_at_start: false,
        totals: (27, 5, 4),
    },
    Script {
        name: "staging",
        members: &["staging-db", "staging-web"],
        idle_s: 30,
        start_s: 3,
        fails: false,
        warning: None,
        period_s: 70,
        active_s: 25,
        offset_s: 0,
        intensity: 1,
        running_at_start: false,
        held_at_start: true,
        totals: (0, 2, 2),
    },
];

/// Splitmix64 finaliser: deterministic, well mixed and independent of any RNG crate.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

struct Group {
    script: &'static Script,
    index: u64,
    state: StateKind,
    held: bool,
    since: u64,
    last_activity: Option<u64>,
    requests: u64,
    wakes: u64,
    sleeps: u64,
    last_error: Option<String>,
    /// When the running `Starting`/`Stopping` transition completes.
    done_at: u64,
    failed_at: Option<u64>,
    quiet_until: u64,
}

struct Sim {
    t: u64,
    groups: Vec<Group>,
    events: VecDeque<SdsEvent>,
}

impl Sim {
    fn new() -> Self {
        let groups = SCRIPTS
            .iter()
            .zip(0u64..)
            .map(|(script, index)| {
                let running = script.running_at_start;
                Group {
                    script,
                    index,
                    state: if running { StateKind::Running } else { StateKind::Stopped },
                    held: script.held_at_start,
                    since: DEMO_EPOCH_MS.saturating_sub(10 * 60_000 + index * 37_000),
                    last_activity: running.then(|| DEMO_EPOCH_MS.saturating_sub(index * 4_000)),
                    requests: script.totals.0,
                    wakes: script.totals.1,
                    sleeps: script.totals.2,
                    last_error: None,
                    done_at: 0,
                    failed_at: None,
                    quiet_until: 0,
                }
            })
            .collect();
        let mut sim = Self { t: DEMO_EPOCH_MS, groups, events: VecDeque::new() };
        sim.advance_to(DEMO_EPOCH_MS + PREROLL_MS);
        sim
    }

    fn emit(&mut self, group: usize, kind: EventKind, message: impl Into<String>) {
        let name = self.groups[group].script.name;
        if self.events.len() == EVENT_KEEP {
            self.events.pop_front();
        }
        self.events.push_back(SdsEvent { ts_ms: self.t, group: name.into(), kind, message: message.into() });
    }

    /// Run whole ticks up to `to`; a partial tick waits for the next call.
    fn advance_to(&mut self, to: u64) {
        while self.t + TICK_MS <= to {
            self.t += TICK_MS;
            for i in 0..self.groups.len() {
                self.step(i);
            }
        }
    }

    fn traffic_now(&self, i: usize) -> u64 {
        let g = &self.groups[i];
        let s = g.script;
        let phase = ((self.t - DEMO_EPOCH_MS) / 1000 + s.offset_s) % s.period_s;
        if phase >= s.active_s {
            return 0;
        }
        mix(g.index.wrapping_mul(1_000_003) ^ ((self.t - DEMO_EPOCH_MS) / TICK_MS)) % (s.intensity + 1)
    }

    fn step(&mut self, i: usize) {
        let t = self.t;
        let requests = self.traffic_now(i);
        let (state, held) = (self.groups[i].state, self.groups[i].held);
        match state {
            StateKind::Stopped | StateKind::Failed => {
                let retry_ok = self.groups[i].failed_at.is_none_or(|at| t >= at + RETRY_PAUSE_MS);
                if requests > 0 && !held && t >= self.groups[i].quiet_until && retry_ok {
                    self.groups[i].requests += requests;
                    self.groups[i].last_activity = Some(t);
                    self.begin_start(i);
                }
            }
            StateKind::Starting => {
                if t >= self.groups[i].done_at {
                    self.finish_start(i);
                }
            }
            StateKind::Running => {
                if requests > 0 {
                    self.groups[i].requests += requests;
                    self.groups[i].last_activity = Some(t);
                }
                let g = &self.groups[i];
                let idle = t.saturating_sub(g.last_activity.unwrap_or(g.since));
                if idle >= g.script.idle_s * 1000 {
                    let why = format!("idle for {}", humantime::format_duration(Duration::from_secs(g.script.idle_s)));
                    self.begin_stop(i, &why);
                }
            }
            StateKind::Stopping => {
                if t >= self.groups[i].done_at {
                    let g = &mut self.groups[i];
                    g.state = StateKind::Stopped;
                    g.since = t;
                    self.emit(i, EventKind::Stopped, "asleep");
                }
            }
        }
    }

    fn begin_start(&mut self, i: usize) {
        let t = self.t;
        let g = &mut self.groups[i];
        g.state = StateKind::Starting;
        g.since = t;
        g.wakes += 1;
        g.done_at = t + g.script.start_s * 1000;
        let n = g.script.members.len();
        self.emit(i, EventKind::Waking, format!("starting {n} container(s)"));
    }

    fn finish_start(&mut self, i: usize) {
        let t = self.t;
        if self.groups[i].held {
            // As in the daemon: a start in progress is stopped once it has finished.
            self.begin_stop(i, "put on hold");
            return;
        }
        let g = &mut self.groups[i];
        if g.script.fails {
            let first = g.script.members[0];
            let reason = format!("container `{first}` is unhealthy: health check failed 3 times");
            g.state = StateKind::Failed;
            g.since = t;
            g.failed_at = Some(t);
            g.last_error = Some(reason.clone());
            self.emit(i, EventKind::StartFailed, format!("wake-up failed: {reason}"));
        } else {
            g.state = StateKind::Running;
            g.since = t;
            g.failed_at = None;
            g.last_error = None;
            g.last_activity = Some(t);
            self.emit(i, EventKind::Running, "ready, forwarding traffic");
        }
    }

    fn begin_stop(&mut self, i: usize, why: &str) {
        let t = self.t;
        let g = &mut self.groups[i];
        g.state = StateKind::Stopping;
        g.since = t;
        g.sleeps += 1;
        g.done_at = t + 1_500;
        let n = g.script.members.len();
        self.emit(i, EventKind::Sleeping, format!("stopping {n} container(s): {why}"));
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.script.name == name)
    }

    fn wake(&mut self, name: &str) {
        let Some(i) = self.find(name) else { return };
        let g = &self.groups[i];
        if g.held || !matches!(g.state, StateKind::Stopped | StateKind::Failed) {
            return;
        }
        self.groups[i].failed_at = None;
        self.groups[i].last_activity = Some(self.t);
        self.begin_start(i);
    }

    fn sleep(&mut self, name: &str) {
        let Some(i) = self.find(name) else { return };
        match self.groups[i].state {
            StateKind::Running => {
                self.groups[i].quiet_until = self.t + QUIET_AFTER_SLEEP_MS;
                self.begin_stop(i, "sleep requested");
            }
            StateKind::Starting => self.emit(i, EventKind::Warning, "sleep requested while starting; ignored"),
            StateKind::Stopped | StateKind::Stopping | StateKind::Failed => {}
        }
    }

    fn set_hold(&mut self, name: &str, held: bool) {
        let Some(i) = self.find(name) else { return };
        if self.groups[i].held == held {
            return;
        }
        self.groups[i].held = held;
        if !held {
            self.emit(i, EventKind::Released, "released: the next request wakes the group");
            return;
        }
        self.emit(i, EventKind::Held, "on hold: requests no longer wake the group");
        if self.groups[i].state == StateKind::Running {
            self.begin_stop(i, "put on hold");
        }
    }

    fn members(&self, g: &Group) -> Vec<MemberSnapshot> {
        let n = g.script.members.len() as u64;
        g.script
            .members
            .iter()
            .zip(0u64..)
            .map(|(name, i)| {
                let has_healthcheck = !name.ends_with("-web") && !name.ends_with("-ui") && *name != "jupyter";
                let running = match g.state {
                    StateKind::Running | StateKind::Stopping => true,
                    StateKind::Stopped => false,
                    // Containers come up one after the other over the start time.
                    StateKind::Starting => {
                        let elapsed = self.t.saturating_sub(g.since);
                        elapsed * (n + 1) > i * g.script.start_s * 1000 + g.script.start_s * 1000 / 2
                    }
                    StateKind::Failed => i == 0,
                };
                let health = match (running, has_healthcheck, g.state) {
                    (false, ..) | (true, false, _) => HealthState::None,
                    (true, true, StateKind::Failed) => HealthState::Unhealthy,
                    (true, true, StateKind::Starting) => HealthState::Starting,
                    (true, true, _) => HealthState::Healthy,
                };
                let id = format!("{:012x}", mix(g.index << 8 | i) & 0xffff_ffff_ffff);
                MemberSnapshot { name: (*name).into(), id, running, health }
            })
            .collect()
    }

    fn snapshots(&self) -> Vec<GroupSnapshot> {
        self.groups
            .iter()
            .map(|g| GroupSnapshot {
                name: g.script.name.into(),
                state: g.state,
                held: g.held,
                state_since_ms: g.since,
                state_since_mono_ms: 0,
                failed_at_mono_ms: None,
                last_activity_ms: g.last_activity,
                inflight: 0,
                requests_total: g.requests,
                wakes_total: g.wakes,
                sleeps_total: g.sleeps,
                members: self.members(g),
                last_error: g.last_error.clone(),
                warnings: g.script.warning.map(str::to_owned).into_iter().collect(),
                source: GroupSource::Labels,
                settings: GroupSettings {
                    idle_timeout: Duration::from_secs(g.script.idle_s),
                    ..GroupSettings::default()
                },
            })
            .collect()
    }
}

/// A [`Source`] that plays a fixed script of developer side projects.
pub struct DemoSource {
    sim: Mutex<Sim>,
    /// Virtual milliseconds since start (real time in production, manual in tests).
    clock: Box<dyn Fn() -> u64 + Send>,
}

impl DemoSource {
    pub fn new() -> Self {
        let start = Instant::now();
        Self::with_clock(Box::new(move || u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)))
    }

    /// `clock` returns the milliseconds elapsed since the dashboard opened.
    pub fn with_clock(clock: Box<dyn Fn() -> u64 + Send>) -> Self {
        Self { sim: Mutex::new(Sim::new()), clock }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Sim) -> R) -> Option<R> {
        let mut sim = self.sim.lock().ok()?;
        sim.advance_to(DEMO_EPOCH_MS + PREROLL_MS + (self.clock)());
        Some(f(&mut sim))
    }
}

impl Default for DemoSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for DemoSource {
    fn snapshots(&self) -> Vec<GroupSnapshot> {
        self.with(|s| s.snapshots()).unwrap_or_default()
    }
    fn logs(&self) -> Vec<String> {
        self.events().iter().map(super::format_event).collect()
    }
    fn events(&self) -> Vec<SdsEvent> {
        self.with(|s| s.events.iter().cloned().collect()).unwrap_or_default()
    }
    fn now_ms(&self) -> u64 {
        self.with(|s| s.t).unwrap_or(DEMO_EPOCH_MS)
    }
    fn wake(&self, group: &str) {
        self.with(|s| s.wake(group));
    }
    fn sleep(&self, group: &str) {
        self.with(|s| s.sleep(group));
    }
    fn set_hold(&self, group: &str, held: bool) {
        self.with(|s| s.set_hold(group, held));
    }
    fn docker_endpoint(&self) -> String {
        "simulated, no Docker".into()
    }
    fn location(&self) -> String {
        "demo: simulated data".into()
    }
    fn is_demo(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn source() -> (DemoSource, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&now);
        (DemoSource::with_clock(Box::new(move || clock.load(Ordering::Relaxed))), now)
    }

    fn state(src: &DemoSource, name: &str) -> GroupSnapshot {
        src.snapshots().into_iter().find(|g| g.name == name).unwrap()
    }

    #[test]
    fn starts_with_a_mix_of_states_that_reads_like_a_dev_machine() {
        let (src, _) = source();
        let groups = src.snapshots();
        assert_eq!(groups.len(), SCRIPTS.len());
        let count = |s: StateKind| groups.iter().filter(|g| g.state == s).count();
        assert!(count(StateKind::Running) >= 2, "{groups:#?}");
        assert!(count(StateKind::Stopped) >= 2);
        assert!(groups.iter().any(|g| g.held));
        assert!(groups.iter().all(|g| !g.members.is_empty()));
        assert!(groups.iter().any(|g| !g.warnings.is_empty()));
        assert!(src.is_demo());
        assert!(src.problem().is_none());
        assert!(!src.events().is_empty(), "the pre-roll leaves a history behind");
        assert_eq!(src.logs().len(), src.events().len());
        assert!(src.now_ms() >= DEMO_EPOCH_MS + PREROLL_MS);
    }

    #[test]
    fn is_deterministic_regardless_of_poll_rate() {
        let (coarse, clock_a) = source();
        let (fine, clock_b) = source();
        for step in 1..=240u64 {
            clock_b.store(step * 1_000, Ordering::Relaxed);
            fine.snapshots();
        }
        clock_a.store(240_000, Ordering::Relaxed);
        assert_eq!(coarse.snapshots(), fine.snapshots());
        assert_eq!(coarse.events(), fine.events());
        // And a fresh source replays the same history.
        let (again, clock_c) = source();
        clock_c.store(240_000, Ordering::Relaxed);
        assert_eq!(coarse.snapshots(), again.snapshots());
    }

    #[test]
    fn cycles_through_every_state_and_counters_never_decrease() {
        let (src, clock) = source();
        let mut seen = std::collections::HashSet::new();
        let mut last: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for step in 0..1_200u64 {
            clock.store(step * 500, Ordering::Relaxed);
            for g in src.snapshots() {
                seen.insert(g.state.to_string());
                let prev = last.insert(g.name.clone(), g.requests_total).unwrap_or(0);
                assert!(g.requests_total >= prev, "{} went backwards", g.name);
                if let Some(timeout) = g.sleeps_in_ms(src.now_ms()) {
                    assert!(timeout <= u64::try_from(g.settings.idle_timeout.as_millis()).unwrap());
                }
            }
        }
        for want in ["running", "stopped", "starting", "stopping", "failed"] {
            assert!(seen.contains(want), "state `{want}` never shown: {seen:?}");
        }
    }

    #[test]
    fn wake_sleep_and_hold_keys_act_on_the_simulation() {
        let (src, _) = source();
        let stopped = src.snapshots().into_iter().find(|g| g.state == StateKind::Stopped && !g.held).unwrap().name;
        src.wake(&stopped);
        assert_eq!(state(&src, &stopped).state, StateKind::Starting);

        let running = src.snapshots().into_iter().find(|g| g.state == StateKind::Running).unwrap().name;
        let sleeps = state(&src, &running).sleeps_total;
        src.sleep(&running);
        let after = state(&src, &running);
        assert_eq!(after.state, StateKind::Stopping);
        assert_eq!(after.sleeps_total, sleeps + 1);

        let held = src.snapshots().into_iter().find(|g| g.held).unwrap().name;
        src.wake(&held);
        assert_eq!(state(&src, &held).state, StateKind::Stopped, "a held group cannot be woken");
        src.set_hold(&held, false);
        assert!(!state(&src, &held).held);
        src.wake(&held);
        assert_eq!(state(&src, &held).state, StateKind::Starting);
        src.set_hold(&held, true);
        assert!(state(&src, &held).held);

        // Hold stops a running group; unknown names are ignored.
        let other = src.snapshots().into_iter().find(|g| g.state == StateKind::Running && g.name != running);
        if let Some(other) = other {
            src.set_hold(&other.name, true);
            assert_eq!(state(&src, &other.name).state, StateKind::Stopping);
        }
        src.wake("missing");
        src.sleep("missing");
        src.set_hold("missing", true);
        let events = src.events();
        assert!(events.iter().any(|e| e.kind == EventKind::Held));
        assert!(events.iter().any(|e| e.kind == EventKind::Released));
    }

    #[test]
    fn slept_groups_stay_asleep_a_while_and_hold_during_start_stops_after() {
        let (src, clock) = source();
        let running = src.snapshots().into_iter().find(|g| g.state == StateKind::Running).unwrap().name;
        src.sleep(&running);
        clock.store(5_000, Ordering::Relaxed);
        assert_eq!(state(&src, &running).state, StateKind::Stopped);

        let stopped = src.snapshots().into_iter().find(|g| g.state == StateKind::Stopped && !g.held).unwrap().name;
        src.wake(&stopped);
        src.sleep(&stopped);
        assert!(src.events().iter().any(|e| e.kind == EventKind::Warning));
        src.set_hold(&stopped, true);
        clock.store(30_000, Ordering::Relaxed);
        let g = state(&src, &stopped);
        assert!(g.held);
        assert!(matches!(g.state, StateKind::Stopped | StateKind::Stopping));
    }

    #[test]
    fn failing_group_shows_error_and_unhealthy_member() {
        let (src, clock) = source();
        let mut failed = None;
        for step in 0..2_000u64 {
            clock.store(step * 500, Ordering::Relaxed);
            if let Some(g) = src.snapshots().into_iter().find(|g| g.state == StateKind::Failed) {
                failed = Some(g);
                break;
            }
        }
        let g = failed.expect("legacy-php fails within a few minutes");
        assert!(g.last_error.as_deref().unwrap().contains("unhealthy"));
        assert!(g.members.iter().any(|m| m.health == HealthState::Unhealthy));
        // An operator wake retries at once.
        src.wake(&g.name);
        assert_eq!(state(&src, &g.name).state, StateKind::Starting);
    }

    #[test]
    fn event_history_is_bounded_and_default_works() {
        let (src, clock) = source();
        clock.store(20 * 60 * 1000, Ordering::Relaxed);
        assert!(src.events().len() <= EVENT_KEEP);
        let _ = DemoSource::default().snapshots();
    }
}
