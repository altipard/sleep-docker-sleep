use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::*;
use crate::config::{Config, GroupSettings, HealthSpec};
use crate::docker::{Discovery, GroupSpec};
use crate::test_support::{self, TestBackend};

fn settings() -> GroupSettings {
    GroupSettings { start_grace: Duration::ZERO, health: HealthSpec::Grace, ..GroupSettings::default() }
}

async fn wait_for(handle: &GroupHandle, predicate: impl Fn(&GroupSnapshot) -> bool) -> GroupSnapshot {
    let mut states = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(2), states.wait_for(predicate))
        .await
        .expect("registry state deadline")
        .expect("actor stays alive")
        .clone()
}

#[tokio::test]
async fn registry_applies_updates_preserves_holds_and_removes_groups() {
    let (registry, backend) = test_support::registry("shop", &["app"], true, settings());
    let handle = registry.get("shop").unwrap();
    wait_for(&handle, |snapshot| snapshot.state == StateKind::Running).await;
    assert_eq!(registry.snapshots().len(), 1);
    assert_eq!(registry.handles().len(), 1);
    assert!(registry.last_discovery_ms() > 0);
    assert_eq!(registry.backend().describe(), "mock://docker");

    registry.set_hold("shop", true).unwrap();
    wait_for(&handle, |snapshot| snapshot.held).await;
    assert!(registry.set_hold("missing", true).is_none());
    registry.set_hold("shop", false).unwrap();
    wait_for(&handle, |snapshot| !snapshot.held).await;

    backend.add("db", true);
    let updated = test_support::spec("shop", &["app", "db"], settings());
    registry.apply(Discovery { groups: vec![updated], warnings: vec!["new warning".into()] });
    wait_for(&handle, |snapshot| snapshot.members.len() == 2).await;
    assert_eq!(registry.discovery_warnings(), vec!["new warning"]);

    registry.apply(Discovery { groups: Vec::<GroupSpec>::new(), warnings: Vec::new() });
    assert!(registry.get("shop").is_none());
    assert!(registry.snapshots().is_empty());
    registry.shutdown().await;
    assert!(!backend.stop_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn supervisor_discovers_and_shuts_down_cleanly() {
    let backend = TestBackend::with(&["app"], true);
    let bus = Arc::new(EventBus::new(10));
    let registry = Arc::new(Registry::new(backend, bus));
    let cancel = CancellationToken::new();
    let supervisor = Supervisor::new(Arc::clone(&registry), Arc::new(Config::default()), cancel.clone());
    let task = tokio::spawn(supervisor.run());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if registry.get("app").is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial discovery");
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    assert!(registry.get("app").is_none());
}
