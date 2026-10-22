//! Readiness probing after `docker start`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{GroupSettings, HealthSpec};
use crate::docker::{Backend, ContainerInfo, HealthState, MemberSpec};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Ready,
    NotYet(String),
    /// A member exited after being started: waiting longer will not help.
    Crashed(String),
}

/// Inspect all members, then apply the configured health check.
pub async fn probe(
    backend: &Arc<dyn Backend>,
    members: &[MemberSpec],
    settings: &GroupSettings,
    started_at: Instant,
    http: &reqwest::Client,
) -> Result<(Probe, Vec<ContainerInfo>), String> {
    let mut infos = Vec::with_capacity(members.len());
    for m in members {
        let info = backend.inspect(&m.id).await.map_err(|e| e.to_string())?;
        if !info.running {
            return Ok((Probe::Crashed(format!("container `{}` is {}", info.name, info.status)), infos));
        }
        infos.push(info);
    }
    let grace_over = started_at.elapsed() >= settings.start_grace;
    let probe = match &settings.health {
        HealthSpec::Docker => {
            let checked: Vec<&ContainerInfo> = infos.iter().filter(|i| i.has_healthcheck).collect();
            if checked.is_empty() {
                if grace_over { Probe::Ready } else { Probe::NotYet("no HEALTHCHECK, waiting for start_grace".into()) }
            } else if let Some(bad) = checked.iter().find(|i| i.health == HealthState::Unhealthy) {
                Probe::NotYet(format!("container `{}` is unhealthy", bad.name))
            } else if let Some(pending) = checked.iter().find(|i| i.health != HealthState::Healthy) {
                Probe::NotYet(format!("container `{}` health is {:?}", pending.name, pending.health))
            } else {
                Probe::Ready
            }
        }
        HealthSpec::Grace => {
            if grace_over {
                Probe::Ready
            } else {
                Probe::NotYet("waiting for start_grace".into())
            }
        }
        HealthSpec::Http(url) => match http.get(url).timeout(PROBE_TIMEOUT).send().await {
            Ok(resp) if resp.status().is_success() || resp.status().is_redirection() => Probe::Ready,
            Ok(resp) => Probe::NotYet(format!("{url} answered {}", resp.status())),
            Err(e) => Probe::NotYet(format!("{url}: {e}")),
        },
        HealthSpec::Tcp(addr) => {
            match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
                Ok(Ok(_)) => Probe::Ready,
                Ok(Err(e)) => Probe::NotYet(format!("tcp {addr}: {e}")),
                Err(_) => Probe::NotYet(format!("tcp {addr}: connect timed out")),
            }
        }
    };
    Ok((probe, infos))
}
