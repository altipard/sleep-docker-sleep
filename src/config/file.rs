//! The `sds.toml` file.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::settings::{GroupSettings, HealthSpec, ProxySettings, WakeMode};

pub const FILE_NAME: &str = "sds.toml";
pub const ENV_VAR: &str = "SDS_CONFIG";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub admin: AdminConfig,
    pub docker: DockerConfig,
    pub discovery: DiscoveryConfig,
    pub log: LogConfig,
    /// Defaults for every group; labels and `[groups.<name>]` override them.
    pub defaults: GroupSettings,
    /// Static groups for containers that cannot carry labels.
    pub groups: BTreeMap<String, StaticGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdminConfig {
    /// Where the admin API, forward-auth endpoint and web overview listen.
    pub bind: SocketAddr,
    /// Optional bearer token required for `/api/*` and `/_sds/check/*`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Public base URL of the waiting page as seen by browsers; only needed when the
    /// reverse proxy cannot serve `/_sds/wait/*` from the same origin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self { bind: "127.0.0.1:9900".parse().unwrap_or_else(|_| unreachable!()), token: None, public_url: None }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerConfig {
    /// `unix:///var/run/docker.sock`, `tcp://host:2375`, … Defaults to `DOCKER_HOST` or the local socket.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// How often all containers are re-listed (events trigger an immediate refresh anyway).
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self { interval: Duration::from_secs(30) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// `tracing` filter, e.g. `info` or `sds=debug,hyper=warn`.
    pub filter: String,
    /// Emit JSON lines instead of human-readable logs (for journald / log shippers).
    pub json: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self { filter: "info".to_owned(), json: false }
    }
}

/// A group defined in the file instead of via labels. Every field except `containers`
/// overrides `[defaults]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StaticGroup {
    /// Container names (as shown by `docker ps`).
    pub containers: Vec<String>,
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub idle_timeout: Option<Duration>,
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub start_timeout: Option<Duration>,
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub stop_timeout: Option<Duration>,
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub start_grace: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeMode>,
    #[serde(with = "humantime_serde", skip_serializing_if = "Option::is_none")]
    pub hold_timeout: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_waiters: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxySettings>,
}

impl StaticGroup {
    pub fn settings(&self, defaults: &GroupSettings) -> GroupSettings {
        let mut s = defaults.clone();
        if let Some(v) = self.idle_timeout {
            s.idle_timeout = v;
        }
        if let Some(v) = self.start_timeout {
            s.start_timeout = v;
        }
        if let Some(v) = self.stop_timeout {
            s.stop_timeout = v;
        }
        if let Some(v) = self.start_grace {
            s.start_grace = v;
        }
        if let Some(v) = &self.health {
            s.health = v.clone();
        }
        if let Some(v) = self.wake {
            s.wake = v;
        }
        if let Some(v) = self.hold_timeout {
            s.hold_timeout = v;
        }
        if let Some(v) = self.max_waiters {
            s.max_waiters = v;
        }
        if let Some(v) = &self.proxy {
            s.proxy = Some(v.clone());
        }
        s
    }
}

/// Where a configuration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    File(PathBuf),
    BuiltIn,
}

impl std::fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(p) => write!(f, "{}", p.display()),
            Self::BuiltIn => f.write_str("built-in defaults (no sds.toml found)"),
        }
    }
}

/// Candidate paths in precedence order: explicit `--config`, `$SDS_CONFIG`, `./sds.toml`,
/// `~/.config/sds/sds.toml`, `/etc/sds/sds.toml`.
pub fn candidates(explicit: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = explicit {
        out.push(p.to_path_buf());
        return out;
    }
    if let Ok(p) = std::env::var(ENV_VAR) {
        if !p.is_empty() {
            out.push(PathBuf::from(p));
        }
    }
    out.push(PathBuf::from(FILE_NAME));
    if let Some(dirs) = directories::ProjectDirs::from("", "", "sds") {
        out.push(dirs.config_dir().join(FILE_NAME));
    }
    out.push(PathBuf::from("/etc/sds").join(FILE_NAME));
    out
}

/// First existing candidate, if any.
pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    candidates(explicit).into_iter().find(|p| p.is_file())
}

/// Load the configuration. An explicitly named file must exist; otherwise a missing file
/// yields the built-in defaults.
pub fn load(explicit: Option<&Path>) -> Result<(Config, ConfigSource)> {
    if let Some(p) = explicit {
        let cfg = parse_file(p)?;
        return Ok((cfg, ConfigSource::File(p.to_path_buf())));
    }
    match locate(None) {
        Some(p) => {
            let cfg = parse_file(&p)?;
            Ok((cfg, ConfigSource::File(p)))
        }
        None => Ok((Config::default(), ConfigSource::BuiltIn)),
    }
}

fn parse_file(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let cfg: Config = toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    cfg.validate()?;
    Ok(cfg)
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        for (name, group) in &self.groups {
            anyhow::ensure!(!name.is_empty(), "group name must not be empty");
            anyhow::ensure!(
                !name.contains('/') && !name.contains(char::is_whitespace),
                "group `{name}`: name must not contain `/` or whitespace"
            );
            anyhow::ensure!(!group.containers.is_empty(), "group `{name}`: `containers` must not be empty");
        }
        anyhow::ensure!(self.defaults.max_waiters > 0, "defaults.max_waiters must be > 0");
        Ok(())
    }

    /// A commented example file, used by `sds init`.
    pub fn example() -> String {
        EXAMPLE.to_owned()
    }
}

const EXAMPLE: &str = r#"# sleep-docker-sleep (sds) configuration
# All durations use humantime syntax: 30s, 15m, 2h.

[admin]
# Admin API, forward-auth endpoint and web overview. Keep it on loopback unless the
# reverse proxy runs on another host; then set a token too.
bind = "127.0.0.1:9900"
# token = "change-me"
# public_url = "https://sds.example.internal"

[docker]
# host = "unix:///var/run/docker.sock"

[discovery]
interval = "30s"

[log]
filter = "info"
json = false

[defaults]
idle_timeout = "15m"
start_timeout = "90s"
stop_timeout = "20s"
start_grace = "5s"
health = "docker"      # docker | grace | "http://…" | "tcp://host:port"
wake = "hold"          # hold | page
hold_timeout = "60s"
max_waiters = 200

# Static groups for containers that cannot be labelled. Labelled containers
# (sds.enable=true) are discovered automatically and do not need an entry here.
#
# [groups.legacy]
# containers = ["legacy-web-1", "legacy-db-1"]
# idle_timeout = "30m"
# health = "http://127.0.0.1:18080/healthz"
#
# [groups.legacy.proxy]      # only without a reverse proxy in front
# listen = "0.0.0.0:8080"
# upstream = "127.0.0.1:18080"
# mode = "http"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_and_matches_defaults() {
        let cfg: Config = toml::from_str(&Config::example()).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.defaults, GroupSettings::default());
        assert_eq!(cfg.admin.bind, AdminConfig::default().bind);
    }

    #[test]
    fn static_group_overrides_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [defaults]
            idle_timeout = "10m"
            [groups.shop]
            containers = ["shop-app-1"]
            idle_timeout = "1h"
            health = "tcp://127.0.0.1:5432"
            [groups.shop.proxy]
            listen = "127.0.0.1:8080"
            upstream = "127.0.0.1:18080"
            "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let s = cfg.groups["shop"].settings(&cfg.defaults);
        assert_eq!(s.idle_timeout, Duration::from_secs(3600));
        assert_eq!(s.health, HealthSpec::Tcp("127.0.0.1:5432".into()));
        assert_eq!(s.proxy.unwrap().mode, super::super::ProxyMode::Http);
    }

    #[test]
    fn rejects_unknown_keys_and_empty_groups() {
        assert!(toml::from_str::<Config>("[admin]\nbindd = \"x\"").is_err());
        let cfg: Config = toml::from_str("[groups.empty]\ncontainers = []").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn explicit_missing_file_is_an_error() {
        assert!(load(Some(Path::new("/nonexistent/sds.toml"))).is_err());
    }
}
