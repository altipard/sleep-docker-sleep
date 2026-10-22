use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("container `{0}` not found")]
    NotFound(String),
    #[error("docker daemon unreachable: {0}")]
    Unreachable(String),
    #[error("docker API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("docker error: {0}")]
    Other(String),
}

/// Health as reported by the container's own `HEALTHCHECK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    /// No `HEALTHCHECK` defined, or not inspected yet.
    #[default]
    None,
    Starting,
    Healthy,
    Unhealthy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    /// Full container id.
    pub id: String,
    /// Name without the leading slash.
    pub name: String,
    pub labels: HashMap<String, String>,
    pub running: bool,
    /// Docker's status word: `running`, `exited`, `created`, …
    pub status: String,
    pub health: HealthState,
    /// Whether the image/container defines a `HEALTHCHECK` (only known after `inspect`).
    pub has_healthcheck: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAction {
    Create,
    Start,
    Die,
    Stop,
    Destroy,
    Rename,
    Health(HealthState),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerEvent {
    pub id: String,
    pub name: Option<String>,
    pub action: EventAction,
}

/// The subset of the Docker Engine API that `sds` needs.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Human-readable description of the endpoint (for logs and `sds check`).
    fn describe(&self) -> String;
    /// Round trip to the daemon; returns its version string.
    async fn ping(&self) -> Result<String, BackendError>;
    /// All containers, including stopped ones. `health` and `has_healthcheck` are not
    /// populated here; use [`Backend::inspect`] for those.
    async fn list(&self) -> Result<Vec<ContainerInfo>, BackendError>;
    async fn inspect(&self, id: &str) -> Result<ContainerInfo, BackendError>;
    async fn start(&self, id: &str) -> Result<(), BackendError>;
    /// `docker stop` with the given grace period before SIGKILL.
    async fn stop(&self, id: &str, timeout: Duration) -> Result<(), BackendError>;
    /// Live container events. The stream ends when the daemon connection drops; the caller
    /// reconnects.
    fn events(&self) -> BoxStream<'static, Result<ContainerEvent, BackendError>>;
}
