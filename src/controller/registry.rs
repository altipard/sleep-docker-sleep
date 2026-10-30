//! Keeps one actor (and optionally one native proxy listener) per discovered group.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::actor::{self, ActorLink, Timing};
use super::events::EventBus;
use super::handle::GroupHandle;
use super::snapshot::GroupSnapshot;
use super::{EventKind, SdsEvent};
use crate::docker::{Backend, ContainerEvent, Discovery, GroupSpec};
use crate::proxy::ProxyTask;

/// How long `shutdown` waits for actors to finish a running `docker stop`.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

struct Entry {
    link: ActorLink,
    spec: GroupSpec,
    proxy: Option<ProxyTask>,
}

pub struct Registry {
    backend: Arc<dyn Backend>,
    bus: Arc<EventBus>,
    container_events: broadcast::Sender<ContainerEvent>,
    /// One HTTP client for all health probes (connection pool, DNS cache).
    http: reqwest::Client,
    entries: RwLock<BTreeMap<String, Entry>>,
    discovery_warnings: RwLock<Vec<String>>,
    /// Groups on hold. Kept here, not in the actor, so a group that vanishes and comes
    /// back (`docker compose down` / `up`) is still on hold. Lost when `sds` restarts.
    held: RwLock<BTreeSet<String>>,
    last_discovery_ms: std::sync::atomic::AtomicU64,
    /// Human-readable description of the Docker endpoint, for the UI.
    pub docker_endpoint: String,
}

impl Registry {
    pub fn new(backend: Arc<dyn Backend>, bus: Arc<EventBus>) -> Self {
        let (container_events, _) = broadcast::channel(1024);
        let docker_endpoint = backend.describe();
        Self {
            backend,
            bus,
            container_events,
            http: reqwest::Client::builder().build().unwrap_or_default(),
            entries: RwLock::new(BTreeMap::new()),
            discovery_warnings: RwLock::new(Vec::new()),
            held: RwLock::new(BTreeSet::new()),
            last_discovery_ms: std::sync::atomic::AtomicU64::new(0),
            docker_endpoint,
        }
    }

    pub fn bus(&self) -> &Arc<EventBus> {
        &self.bus
    }

    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    /// Sender used by the supervisor to fan container events out to the actors.
    pub fn container_events(&self) -> broadcast::Sender<ContainerEvent> {
        self.container_events.clone()
    }

    pub fn get(&self, name: &str) -> Option<Arc<GroupHandle>> {
        self.entries.read().ok()?.get(name).map(|e| Arc::clone(&e.link.handle))
    }

    pub fn handles(&self) -> Vec<Arc<GroupHandle>> {
        self.entries.read().map(|e| e.values().map(|e| Arc::clone(&e.link.handle)).collect()).unwrap_or_default()
    }

    pub fn snapshots(&self) -> Vec<GroupSnapshot> {
        self.handles().iter().map(|h| h.snapshot()).collect()
    }

    /// Put a known group on hold (`true`) or release it (`false`). `None` for an unknown
    /// group.
    pub fn set_hold(&self, name: &str, held: bool) -> Option<Arc<GroupHandle>> {
        let handle = self.get(name)?;
        if let Ok(mut set) = self.held.write() {
            if held {
                set.insert(name.to_owned());
            } else {
                set.remove(name);
            }
        }
        if held {
            handle.hold();
        } else {
            handle.release();
        }
        Some(handle)
    }

    fn is_held(&self, name: &str) -> bool {
        self.held.read().is_ok_and(|set| set.contains(name))
    }

    pub fn discovery_warnings(&self) -> Vec<String> {
        self.discovery_warnings.read().map(|w| w.clone()).unwrap_or_default()
    }

    pub fn last_discovery_ms(&self) -> u64 {
        self.last_discovery_ms.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Bring the actors in line with a fresh discovery: spawn new groups, update changed
    /// ones, shut down vanished ones.
    ///
    /// Updates go through a `watch` channel, so a changed spec can never be dropped: the
    /// actor always converges on the latest discovery.
    pub fn apply(&self, discovery: Discovery) {
        self.last_discovery_ms.store(super::epoch_ms(), std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut w) = self.discovery_warnings.write()
            && *w != discovery.warnings
        {
            for warning in discovery.warnings.iter().filter(|x| !w.contains(x)) {
                self.bus.emit(SdsEvent::new("-", EventKind::Warning, warning.clone()));
            }
            *w = discovery.warnings;
        }
        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        let wanted: BTreeMap<String, GroupSpec> = discovery.groups.into_iter().map(|g| (g.name.clone(), g)).collect();

        let gone: Vec<String> = entries.keys().filter(|k| !wanted.contains_key(*k)).cloned().collect();
        for name in gone {
            if let Some(entry) = entries.remove(&name) {
                entry.link.cancel.cancel();
                if let Some(p) = entry.proxy {
                    p.stop();
                }
                self.bus.emit(SdsEvent::new(&name, EventKind::Removed, "group removed (no containers left)"));
            }
        }

        for (name, spec) in wanted {
            match entries.get_mut(&name) {
                Some(entry) => {
                    // A proxy whose bind failed (port still held by a listener that was
                    // just stopped) is retried on every discovery, not only on a change.
                    let proxy_missing = spec.settings.proxy.is_some() && entry.proxy.is_none();
                    if entry.spec != spec || proxy_missing {
                        if entry.spec.members != spec.members {
                            self.bus.emit(SdsEvent::new(
                                &name,
                                EventKind::Discovered,
                                format!("members changed: {}", member_list(&spec)),
                            ));
                        }
                        entry.link.spec_tx.send_replace(spec.clone());
                        self.sync_proxy(entry, &spec);
                        entry.spec = spec;
                    }
                }
                None => {
                    self.bus.emit(SdsEvent::new(
                        &name,
                        EventKind::Discovered,
                        format!("group discovered ({:?}): {}", spec.source, member_list(&spec)),
                    ));
                    for w in &spec.warnings {
                        self.bus.emit(SdsEvent::new(&name, EventKind::Warning, w.clone()));
                    }
                    let link = actor::spawn(
                        spec.clone(),
                        Arc::clone(&self.backend),
                        Arc::clone(&self.bus),
                        self.container_events.subscribe(),
                        self.http.clone(),
                        Timing::default(),
                        self.is_held(&name),
                    );
                    let mut entry = Entry { link, spec: spec.clone(), proxy: None };
                    self.sync_proxy(&mut entry, &spec);
                    entries.insert(name, entry);
                }
            }
        }
    }

    /// Start, stop or retune the native proxy of a group. A proxy whose `listen` address is
    /// unchanged keeps its socket and only receives the new settings, so the port is never
    /// rebound (and never clashes with the listener that is about to go away).
    fn sync_proxy(&self, entry: &mut Entry, spec: &GroupSpec) {
        let wanted = spec.settings.proxy.clone();
        let current = entry.proxy.as_ref().map(|p| p.settings());
        if wanted == current {
            return;
        }
        if let (Some(proxy), Some(settings)) = (entry.proxy.as_ref(), &wanted)
            && proxy.listen() == settings.listen
        {
            proxy.update(settings.clone());
            self.bus.emit(SdsEvent::new(
                &spec.name,
                EventKind::Discovered,
                format!("native proxy on {} now {} -> {}", settings.listen, settings.mode, settings.upstream),
            ));
            return;
        }
        if let Some(p) = entry.proxy.take() {
            p.stop();
        }
        if let Some(settings) = wanted {
            match ProxyTask::spawn(Arc::clone(&entry.link.handle), settings.clone(), Arc::clone(&self.bus)) {
                Ok(task) => {
                    self.bus.emit(SdsEvent::new(
                        &spec.name,
                        EventKind::Discovered,
                        format!(
                            "native {} proxy listening on {} -> {}",
                            settings.mode, settings.listen, settings.upstream
                        ),
                    ));
                    entry.proxy = Some(task);
                }
                Err(e) => {
                    self.bus.emit(SdsEvent::new(&spec.name, EventKind::Warning, format!("proxy not started: {e}")))
                }
            }
        }
    }

    /// Stop all actors and proxies. Containers are left as they are, but a `docker stop`
    /// that is already running may finish (bounded by [`SHUTDOWN_TIMEOUT`]).
    pub async fn shutdown(&self) {
        let tasks: Vec<JoinHandle<()>> = match self.entries.write() {
            Ok(mut entries) => {
                let drained: Vec<Entry> = std::mem::take(&mut *entries).into_values().collect();
                drained
                    .into_iter()
                    .map(|entry| {
                        entry.link.cancel.cancel();
                        if let Some(p) = entry.proxy {
                            p.stop();
                        }
                        entry.link.task
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        };
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, futures_util::future::join_all(tasks)).await.is_err() {
            tracing::warn!("some groups did not finish shutting down within {SHUTDOWN_TIMEOUT:?}");
        }
    }
}

fn member_list(spec: &GroupSpec) -> String {
    if spec.members.is_empty() {
        return "no containers".into();
    }
    spec.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")
}
