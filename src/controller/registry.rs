//! Keeps one actor (and optionally one native proxy listener) per discovered group.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::actor::{self, Command};
use super::events::EventBus;
use super::handle::GroupHandle;
use super::snapshot::GroupSnapshot;
use super::{EventKind, SdsEvent};
use crate::docker::{Backend, ContainerEvent, Discovery, GroupSpec};
use crate::proxy::ProxyTask;

struct Entry {
    handle: Arc<GroupHandle>,
    cmd: mpsc::Sender<Command>,
    task: JoinHandle<()>,
    spec: GroupSpec,
    proxy: Option<ProxyTask>,
}

pub struct Registry {
    backend: Arc<dyn Backend>,
    bus: Arc<EventBus>,
    container_events: broadcast::Sender<ContainerEvent>,
    entries: RwLock<BTreeMap<String, Entry>>,
    discovery_warnings: RwLock<Vec<String>>,
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
            entries: RwLock::new(BTreeMap::new()),
            discovery_warnings: RwLock::new(Vec::new()),
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
        self.entries.read().ok()?.get(name).map(|e| Arc::clone(&e.handle))
    }

    pub fn handles(&self) -> Vec<Arc<GroupHandle>> {
        self.entries.read().map(|e| e.values().map(|e| Arc::clone(&e.handle)).collect()).unwrap_or_default()
    }

    pub fn snapshots(&self) -> Vec<GroupSnapshot> {
        self.handles().iter().map(|h| h.snapshot()).collect()
    }

    pub fn discovery_warnings(&self) -> Vec<String> {
        self.discovery_warnings.read().map(|w| w.clone()).unwrap_or_default()
    }

    pub fn last_discovery_ms(&self) -> u64 {
        self.last_discovery_ms.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Bring the actors in line with a fresh discovery: spawn new groups, update changed
    /// ones, shut down vanished ones.
    pub fn apply(&self, discovery: Discovery) {
        self.last_discovery_ms.store(super::epoch_ms(), std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut w) = self.discovery_warnings.write() {
            if *w != discovery.warnings {
                for warning in discovery.warnings.iter().filter(|x| !w.contains(x)) {
                    self.bus.emit(SdsEvent::new("-", EventKind::Warning, warning.clone()));
                }
                *w = discovery.warnings;
            }
        }
        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        let wanted: BTreeMap<String, GroupSpec> = discovery.groups.into_iter().map(|g| (g.name.clone(), g)).collect();

        let gone: Vec<String> = entries.keys().filter(|k| !wanted.contains_key(*k)).cloned().collect();
        for name in gone {
            if let Some(entry) = entries.remove(&name) {
                let _ = entry.cmd.try_send(Command::Shutdown);
                if let Some(p) = entry.proxy {
                    p.abort();
                }
                self.bus.emit(SdsEvent::new(&name, EventKind::Removed, "group removed (no containers left)"));
                drop(entry.task);
            }
        }

        for (name, spec) in wanted {
            match entries.get_mut(&name) {
                Some(entry) => {
                    if entry.spec != spec {
                        if entry.spec.members != spec.members {
                            self.bus.emit(SdsEvent::new(
                                &name,
                                EventKind::Discovered,
                                format!("members changed: {}", member_list(&spec)),
                            ));
                        }
                        let _ = entry.cmd.try_send(Command::Update(Box::new(spec.clone())));
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
                    let (handle, cmd, task) = actor::spawn(
                        spec.clone(),
                        Arc::clone(&self.backend),
                        Arc::clone(&self.bus),
                        self.container_events.subscribe(),
                    );
                    let mut entry = Entry { handle, cmd, task, spec: spec.clone(), proxy: None };
                    self.sync_proxy(&mut entry, &spec);
                    entries.insert(name, entry);
                }
            }
        }
    }

    fn sync_proxy(&self, entry: &mut Entry, spec: &GroupSpec) {
        let wanted = spec.settings.proxy.clone();
        let current = entry.proxy.as_ref().map(|p| p.settings().clone());
        if wanted == current {
            return;
        }
        if let Some(p) = entry.proxy.take() {
            p.abort();
        }
        if let Some(settings) = wanted {
            match ProxyTask::spawn(Arc::clone(&entry.handle), settings.clone(), Arc::clone(&self.bus)) {
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

    /// Stop all actors (containers are left as they are).
    pub fn shutdown(&self) {
        if let Ok(mut entries) = self.entries.write() {
            for entry in entries.values() {
                let _ = entry.cmd.try_send(Command::Shutdown);
                if let Some(p) = &entry.proxy {
                    p.abort();
                }
            }
            entries.clear();
        }
    }
}

fn member_list(spec: &GroupSpec) -> String {
    if spec.members.is_empty() {
        return "no containers".into();
    }
    spec.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")
}
