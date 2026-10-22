use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How a request that hits a sleeping group is answered while it wakes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WakeMode {
    /// Hold the request until the group is healthy (or `hold_timeout` elapses).
    #[default]
    Hold,
    /// Answer immediately with a "starting" page that refreshes itself.
    Page,
}

impl FromStr for WakeMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "hold" => Ok(Self::Hold),
            "page" => Ok(Self::Page),
            other => Err(format!("unknown wake mode `{other}` (expected `hold` or `page`)")),
        }
    }
}

impl fmt::Display for WakeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hold => f.write_str("hold"),
            Self::Page => f.write_str("page"),
        }
    }
}

/// How `sds` decides that a freshly started group is ready for traffic.
///
/// Serialised as a string (`docker`, `grace`, `http://…`, `tcp://host:port`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HealthSpec {
    /// Use the container's own `HEALTHCHECK` (falls back to `Grace` when none is defined).
    #[default]
    Docker,
    /// `GET` the URL (as reachable from the `sds` process) until it returns 2xx/3xx.
    Http(String),
    /// Open a TCP connection to `host:port` (as reachable from the `sds` process).
    Tcp(String),
    /// Wait a fixed grace period after `docker start`.
    Grace,
}

impl FromStr for HealthSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "docker" => Ok(Self::Docker),
            "grace" | "none" => Ok(Self::Grace),
            _ if s.starts_with("http://") || s.starts_with("https://") => Ok(Self::Http(s.to_owned())),
            _ if s.starts_with("tcp://") => {
                let addr = s.trim_start_matches("tcp://");
                if addr.rsplit_once(':').is_none() {
                    return Err(format!("tcp health check `{s}` needs host:port"));
                }
                Ok(Self::Tcp(addr.to_owned()))
            }
            _ => {
                Err(format!("unknown health check `{s}` (expected `docker`, `grace`, `http://…` or `tcp://host:port`)"))
            }
        }
    }
}

impl fmt::Display for HealthSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Docker => f.write_str("docker"),
            Self::Grace => f.write_str("grace"),
            Self::Http(url) => f.write_str(url),
            Self::Tcp(addr) => write!(f, "tcp://{addr}"),
        }
    }
}

impl Serialize for HealthSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for HealthSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Protocol handled by the native proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// Layer 7: understands requests, can serve the waiting page, passes upgrades through.
    #[default]
    Http,
    /// Layer 4: opaque byte stream, waits for the group before connecting upstream.
    Tcp,
}

impl FromStr for ProxyMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "tcp" => Ok(Self::Tcp),
            other => Err(format!("unknown proxy mode `{other}` (expected `http` or `tcp`)")),
        }
    }
}

impl fmt::Display for ProxyMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http => f.write_str("http"),
            Self::Tcp => f.write_str("tcp"),
        }
    }
}

/// Native proxy in front of a group (only for setups without a reverse proxy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySettings {
    /// Address `sds` listens on (the public port).
    pub listen: SocketAddr,
    /// Where the group's service is reachable from the `sds` process.
    pub upstream: String,
    #[serde(default)]
    pub mode: ProxyMode,
}

/// Effective settings of one group after merging defaults, file and labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GroupSettings {
    /// Stop the group after this much time without a request (and no open connection).
    #[serde(with = "humantime_serde")]
    pub idle_timeout: Duration,
    /// Give up a wake-up after this long without becoming healthy.
    #[serde(with = "humantime_serde")]
    pub start_timeout: Duration,
    /// Passed to `docker stop` as the grace period before SIGKILL.
    #[serde(with = "humantime_serde")]
    pub stop_timeout: Duration,
    /// Grace period used by [`HealthSpec::Grace`] and when a container has no `HEALTHCHECK`.
    #[serde(with = "humantime_serde")]
    pub start_grace: Duration,
    pub health: HealthSpec,
    pub wake: WakeMode,
    /// How long a held request waits for the group before it is answered with 503.
    #[serde(with = "humantime_serde")]
    pub hold_timeout: Duration,
    /// Upper bound of simultaneously held requests per group; beyond that: 503 immediately.
    pub max_waiters: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxySettings>,
}

impl Default for GroupSettings {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            start_timeout: Duration::from_secs(90),
            stop_timeout: Duration::from_secs(20),
            start_grace: Duration::from_secs(5),
            health: HealthSpec::Docker,
            wake: WakeMode::Hold,
            hold_timeout: Duration::from_secs(60),
            max_waiters: 200,
            proxy: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_health_specs() {
        assert_eq!("docker".parse::<HealthSpec>(), Ok(HealthSpec::Docker));
        assert_eq!("none".parse::<HealthSpec>(), Ok(HealthSpec::Grace));
        assert_eq!(
            "http://127.0.0.1:8080/healthz".parse::<HealthSpec>(),
            Ok(HealthSpec::Http("http://127.0.0.1:8080/healthz".into()))
        );
        assert_eq!("tcp://db:5432".parse::<HealthSpec>(), Ok(HealthSpec::Tcp("db:5432".into())));
        assert!("tcp://db".parse::<HealthSpec>().is_err());
        assert!("ftp://x".parse::<HealthSpec>().is_err());
    }

    #[test]
    fn health_spec_round_trips_through_serde() {
        let json = serde_json::to_string(&HealthSpec::Tcp("db:5432".into())).unwrap();
        assert_eq!(json, "\"tcp://db:5432\"");
        let back: HealthSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, HealthSpec::Tcp("db:5432".into()));
        assert!(serde_json::from_str::<HealthSpec>("\"ftp://x\"").is_err());
    }

    #[test]
    fn parses_modes() {
        assert_eq!("Hold".parse::<WakeMode>(), Ok(WakeMode::Hold));
        assert!("wait".parse::<WakeMode>().is_err());
        assert_eq!("TCP".parse::<ProxyMode>(), Ok(ProxyMode::Tcp));
    }
}
