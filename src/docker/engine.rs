//! [`Backend`] implementation on top of the Docker Engine API via `bollard`.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use bollard::Docker;
use bollard::models::{
    ContainerInspectResponse, ContainerSummary, EventMessage, EventMessageTypeEnum, HealthStatusEnum,
};
use bollard::query_parameters::{EventsOptionsBuilder, ListContainersOptionsBuilder, StopContainerOptionsBuilder};
use futures_util::stream::{BoxStream, StreamExt};

use super::backend::{Backend, BackendError, ContainerEvent, ContainerInfo, EventAction, HealthState};

#[derive(Clone)]
pub struct DockerEngine {
    docker: Docker,
    description: String,
}

impl DockerEngine {
    /// Connect to `host` (`unix://…`, `tcp://…`, `http://…`) or, when `None`, to
    /// `$DOCKER_HOST` / the platform default socket.
    pub fn connect(host: Option<&str>) -> Result<Self, BackendError> {
        let (docker, description) = match host {
            Some(h) => (Docker::connect_with_host(h), h.to_owned()),
            None => {
                let h = std::env::var("DOCKER_HOST").unwrap_or_else(|_| "unix:///var/run/docker.sock".to_owned());
                (Docker::connect_with_defaults(), h)
            }
        };
        let docker = docker.map_err(|e| BackendError::Unreachable(e.to_string()))?;
        Ok(Self { docker, description })
    }
}

fn map_err(id: &str, err: bollard::errors::Error) -> BackendError {
    match err {
        bollard::errors::Error::DockerResponseServerError { status_code: 404, .. } => {
            BackendError::NotFound(id.to_owned())
        }
        bollard::errors::Error::DockerResponseServerError { status_code, message } => {
            BackendError::Api { status: status_code, message }
        }
        bollard::errors::Error::HyperResponseError { .. }
        | bollard::errors::Error::IOError { .. }
        | bollard::errors::Error::RequestTimeoutError
        | bollard::errors::Error::SocketNotFoundError(_) => BackendError::Unreachable(err.to_string()),
        other => BackendError::Other(other.to_string()),
    }
}

fn strip_slash(name: &str) -> String {
    name.trim_start_matches('/').to_owned()
}

fn from_summary(c: ContainerSummary) -> ContainerInfo {
    let name = c.names.as_ref().and_then(|n| n.first()).map(|n| strip_slash(n)).unwrap_or_default();
    let status = c.state.map(|s| s.to_string()).unwrap_or_default();
    ContainerInfo {
        id: c.id.unwrap_or_default(),
        name,
        labels: c.labels.unwrap_or_default(),
        running: status == "running",
        status,
        health: HealthState::None,
        has_healthcheck: false,
    }
}

fn health_from(status: Option<HealthStatusEnum>) -> HealthState {
    match status {
        Some(HealthStatusEnum::STARTING) => HealthState::Starting,
        Some(HealthStatusEnum::HEALTHY) => HealthState::Healthy,
        Some(HealthStatusEnum::UNHEALTHY) => HealthState::Unhealthy,
        Some(HealthStatusEnum::NONE | HealthStatusEnum::EMPTY) | None => HealthState::None,
    }
}

fn from_inspect(c: ContainerInspectResponse) -> ContainerInfo {
    let state = c.state.unwrap_or_default();
    let config = c.config.unwrap_or_default();
    // A healthcheck of `["NONE"]` explicitly disables the image's check.
    let has_healthcheck = config
        .healthcheck
        .as_ref()
        .and_then(|h| h.test.as_ref())
        .is_some_and(|t| !t.is_empty() && t.first().map(String::as_str) != Some("NONE"));
    ContainerInfo {
        id: c.id.unwrap_or_default(),
        name: c.name.as_deref().map(strip_slash).unwrap_or_default(),
        labels: config.labels.unwrap_or_default(),
        running: state.running.unwrap_or(false),
        status: state.status.map(|s| s.to_string()).unwrap_or_default(),
        health: health_from(state.health.and_then(|h| h.status)),
        has_healthcheck,
    }
}

fn from_event(msg: EventMessage) -> Option<ContainerEvent> {
    if msg.typ != Some(EventMessageTypeEnum::CONTAINER) {
        return None;
    }
    let actor = msg.actor?;
    let id = actor.id?;
    let attrs = actor.attributes.unwrap_or_default();
    let name = attrs.get("name").cloned();
    let action = match msg.action.as_deref().unwrap_or("") {
        "create" => EventAction::Create,
        "start" => EventAction::Start,
        "die" => EventAction::Die,
        "stop" => EventAction::Stop,
        "destroy" => EventAction::Destroy,
        "rename" => EventAction::Rename,
        a if a.starts_with("health_status") => {
            let state = match a.rsplit_once(": ").map(|(_, s)| s) {
                Some("healthy") => HealthState::Healthy,
                Some("unhealthy") => HealthState::Unhealthy,
                Some("starting") => HealthState::Starting,
                _ => HealthState::None,
            };
            EventAction::Health(state)
        }
        other => EventAction::Other(other.to_owned()),
    };
    Some(ContainerEvent { id, name, action })
}

#[async_trait]
impl Backend for DockerEngine {
    fn describe(&self) -> String {
        self.description.clone()
    }

    async fn ping(&self) -> Result<String, BackendError> {
        let v = self.docker.version().await.map_err(|e| map_err("", e))?;
        Ok(format!(
            "Docker {} (API {})",
            v.version.unwrap_or_else(|| "?".into()),
            v.api_version.unwrap_or_else(|| "?".into())
        ))
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>, BackendError> {
        let opts = ListContainersOptionsBuilder::new().all(true).build();
        let containers = self.docker.list_containers(Some(opts)).await.map_err(|e| map_err("", e))?;
        Ok(containers.into_iter().map(from_summary).collect())
    }

    async fn inspect(&self, id: &str) -> Result<ContainerInfo, BackendError> {
        let c = self.docker.inspect_container(id, None).await.map_err(|e| map_err(id, e))?;
        Ok(from_inspect(c))
    }

    async fn start(&self, id: &str) -> Result<(), BackendError> {
        match self.docker.start_container(id, None).await {
            Ok(()) => Ok(()),
            // 304: already started.
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 304, .. }) => Ok(()),
            Err(e) => Err(map_err(id, e)),
        }
    }

    async fn stop(&self, id: &str, timeout: Duration) -> Result<(), BackendError> {
        let secs = i32::try_from(timeout.as_secs()).unwrap_or(i32::MAX);
        let opts = StopContainerOptionsBuilder::new().t(secs).build();
        match self.docker.stop_container(id, Some(opts)).await {
            Ok(()) => Ok(()),
            // 304: already stopped.
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 304, .. }) => Ok(()),
            Err(e) => Err(map_err(id, e)),
        }
    }

    fn events(&self) -> BoxStream<'static, Result<ContainerEvent, BackendError>> {
        let filters: HashMap<&str, Vec<&str>> = HashMap::from([("type", vec!["container"])]);
        let opts = EventsOptionsBuilder::new().filters(&filters).build();
        let docker = self.docker.clone();
        let stream = async_stream_events(docker, opts);
        stream.boxed()
    }
}

fn async_stream_events(
    docker: Docker,
    opts: bollard::query_parameters::EventsOptions,
) -> impl futures_util::Stream<Item = Result<ContainerEvent, BackendError>> + Send + 'static {
    docker.events(Some(opts)).filter_map(|item| async move {
        match item {
            Ok(msg) => from_event(msg).map(Ok),
            Err(e) => Some(Err(map_err("", e))),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::EventActor;

    #[test]
    fn maps_health_events() {
        let msg = EventMessage {
            typ: Some(EventMessageTypeEnum::CONTAINER),
            action: Some("health_status: healthy".into()),
            actor: Some(EventActor {
                id: Some("abc".into()),
                attributes: Some(HashMap::from([("name".to_string(), "web".to_string())])),
            }),
            ..Default::default()
        };
        let ev = from_event(msg).unwrap();
        assert_eq!(ev.action, EventAction::Health(HealthState::Healthy));
        assert_eq!(ev.name.as_deref(), Some("web"));
    }

    #[test]
    fn ignores_non_container_events() {
        let msg = EventMessage { typ: Some(EventMessageTypeEnum::IMAGE), ..Default::default() };
        assert!(from_event(msg).is_none());
    }
}
