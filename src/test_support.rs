use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use crate::config::GroupSettings;
use crate::controller::{EventBus, GroupSnapshot, MemberSnapshot, Registry, StateKind};
use crate::docker::{
    Backend, BackendError, ContainerEvent, ContainerInfo, Discovery, GroupSource, GroupSpec, HealthState, MemberSpec,
};

/// Small deterministic Docker double shared by HTTP, registry, health and proxy tests.
pub(crate) struct TestBackend {
    containers: Mutex<HashMap<String, ContainerInfo>>,
    pub start_calls: Mutex<Vec<String>>,
    pub stop_calls: Mutex<Vec<String>>,
}

impl TestBackend {
    pub fn with(names: &[&str], running: bool) -> Arc<Self> {
        let backend = Self {
            containers: Mutex::new(HashMap::new()),
            start_calls: Mutex::new(Vec::new()),
            stop_calls: Mutex::new(Vec::new()),
        };
        for name in names {
            backend.add(name, running);
        }
        Arc::new(backend)
    }

    pub fn add(&self, name: &str, running: bool) {
        self.containers.lock().unwrap().insert(
            format!("id-{name}"),
            ContainerInfo {
                id: format!("id-{name}"),
                name: name.to_owned(),
                labels: HashMap::from([("sds.enable".into(), "true".into())]),
                running,
                status: if running { "running".into() } else { "exited".into() },
                health: HealthState::None,
                has_healthcheck: false,
            },
        );
    }

    pub fn set_health(&self, name: &str, health: HealthState) {
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(&format!("id-{name}")).unwrap();
        container.health = health;
        container.has_healthcheck = true;
    }
}

#[async_trait]
impl Backend for TestBackend {
    fn describe(&self) -> String {
        "mock://docker".into()
    }

    async fn ping(&self) -> Result<String, BackendError> {
        Ok("mock Docker 1.0".into())
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>, BackendError> {
        Ok(self.containers.lock().unwrap().values().cloned().collect())
    }

    async fn inspect(&self, id: &str) -> Result<ContainerInfo, BackendError> {
        self.containers.lock().unwrap().get(id).cloned().ok_or_else(|| BackendError::NotFound(id.into()))
    }

    async fn start(&self, id: &str) -> Result<(), BackendError> {
        self.start_calls.lock().unwrap().push(id.into());
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(id).ok_or_else(|| BackendError::NotFound(id.into()))?;
        container.running = true;
        container.status = "running".into();
        Ok(())
    }

    async fn stop(&self, id: &str, _timeout: Duration) -> Result<(), BackendError> {
        self.stop_calls.lock().unwrap().push(id.into());
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(id).ok_or_else(|| BackendError::NotFound(id.into()))?;
        container.running = false;
        container.status = "exited".into();
        Ok(())
    }

    fn events(&self) -> BoxStream<'static, Result<ContainerEvent, BackendError>> {
        stream::pending().boxed()
    }
}

pub(crate) fn spec(name: &str, members: &[&str], settings: GroupSettings) -> GroupSpec {
    GroupSpec {
        name: name.into(),
        members: members
            .iter()
            .map(|member| MemberSpec { id: format!("id-{member}"), name: (*member).into(), compose_service: None })
            .collect(),
        settings,
        source: GroupSource::Labels,
        warnings: Vec::new(),
    }
}

pub(crate) fn snapshot(name: &str, state: StateKind) -> GroupSnapshot {
    let now = crate::controller::epoch_ms();
    GroupSnapshot {
        name: name.into(),
        state,
        held: false,
        state_since_ms: now,
        state_since_mono_ms: crate::controller::mono_ms(),
        failed_at_mono_ms: None,
        last_activity_ms: Some(now),
        inflight: 0,
        requests_total: 3,
        wakes_total: 1,
        sleeps_total: 0,
        members: vec![MemberSnapshot {
            name: "app".into(),
            id: "0123456789ab".into(),
            running: state == StateKind::Running,
            health: HealthState::None,
        }],
        last_error: None,
        warnings: Vec::new(),
        source: GroupSource::Labels,
        settings: GroupSettings::default(),
    }
}

pub(crate) fn registry(
    name: &str,
    members: &[&str],
    running: bool,
    settings: GroupSettings,
) -> (Arc<Registry>, Arc<TestBackend>) {
    let backend = TestBackend::with(members, running);
    let registry = Arc::new(Registry::new(backend.clone(), Arc::new(EventBus::new(50))));
    registry.apply(Discovery { groups: vec![spec(name, members, settings)], warnings: Vec::new() });
    (registry, backend)
}
