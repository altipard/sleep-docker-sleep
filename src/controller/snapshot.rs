use serde::{Deserialize, Serialize};

use crate::config::GroupSettings;
use crate::docker::{GroupSource, HealthState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl std::fmt::Display for StateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSnapshot {
    pub name: String,
    /// First 12 characters of the container id.
    pub id: String,
    pub running: bool,
    pub health: HealthState,
}

/// Point-in-time view of a group, safe to serialise for the API and the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSnapshot {
    pub name: String,
    pub state: StateKind,
    /// Wall clock (epoch ms) of the last state change.
    pub state_since_ms: u64,
    /// Monotonic ms ([`crate::controller::mono_ms`]) of the last state change; in-process
    /// only, used for the retry pause so a clock jump cannot extend or skip it.
    #[serde(skip)]
    pub state_since_mono_ms: u64,
    /// Wall clock (epoch ms) of the last request, if any.
    pub last_activity_ms: Option<u64>,
    pub inflight: u64,
    pub requests_total: u64,
    pub wakes_total: u64,
    pub sleeps_total: u64,
    pub members: Vec<MemberSnapshot>,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
    pub source: GroupSource,
    pub settings: GroupSettings,
}

impl GroupSnapshot {
    /// An empty snapshot, only used to initialise a channel before the real one is built.
    pub(super) fn placeholder() -> Self {
        Self {
            name: String::new(),
            state: StateKind::Stopped,
            state_since_ms: 0,
            state_since_mono_ms: 0,
            last_activity_ms: None,
            inflight: 0,
            requests_total: 0,
            wakes_total: 0,
            sleeps_total: 0,
            members: Vec::new(),
            last_error: None,
            warnings: Vec::new(),
            source: GroupSource::Labels,
            settings: GroupSettings::default(),
        }
    }

    /// Milliseconds until the idle timeout stops the group, relative to `now_epoch_ms`.
    /// `None` unless the group is running.
    pub fn sleeps_in_ms(&self, now_epoch_ms: u64) -> Option<u64> {
        if self.state != StateKind::Running {
            return None;
        }
        let last = self.last_activity_ms.unwrap_or(self.state_since_ms);
        let deadline = last.saturating_add(u64::try_from(self.settings.idle_timeout.as_millis()).unwrap_or(u64::MAX));
        Some(deadline.saturating_sub(now_epoch_ms))
    }
}
