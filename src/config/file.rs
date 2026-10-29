//! The `sds.toml` file.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::settings::{GroupSettings, SettingsOverlay};

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
    /// What `/_sds/check/{group}` answers for a group `sds` does not know.
    pub unknown_group: UnknownGroupPolicy,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 9900)),
            token: None,
            public_url: None,
            unknown_group: UnknownGroupPolicy::Allow,
        }
    }
}

/// Forward-auth behaviour for unknown groups. `sds` gates wake-ups, it is not an
/// authorization layer; `allow` keeps a typo in the proxy config from taking a site down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UnknownGroupPolicy {
    /// Answer 200 with `X-SDS-State: unknown-group` and log a warning.
    #[default]
    Allow,
    /// Answer with the `deny` status and a "unknown service" page.
    Deny,
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
/// overrides `[defaults]` (see [`SettingsOverlay`]).
#[derive(Debug, Clone, Default, Serialize)]
pub struct StaticGroup {
    /// Container names (as shown by `docker ps`).
    pub containers: Vec<String>,
    #[serde(flatten)]
    pub overrides: SettingsOverlay,
}

/// Hand-written because serde cannot combine `flatten` with `deny_unknown_fields`: the
/// table is split into `containers` and the rest, and the rest must be a valid overlay, so
/// a typo such as `idle_timeoutt` is still an error.
impl<'de> Deserialize<'de> for StaticGroup {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut table = toml::Table::deserialize(deserializer)?;
        let containers = match table.remove("containers") {
            Some(v) => v.try_into::<Vec<String>>().map_err(|e| serde::de::Error::custom(format!("containers: {e}")))?,
            None => Vec::new(),
        };
        let overrides: SettingsOverlay = toml::Value::Table(table).try_into().map_err(serde::de::Error::custom)?;
        Ok(Self { containers, overrides })
    }
}

impl StaticGroup {
    pub fn settings(&self, defaults: &GroupSettings) -> GroupSettings {
        self.overrides.applied_to(defaults)
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

/// Group names appear in URL paths (`/_sds/check/{group}`) and log lines.
pub fn validate_group_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("group name must not be empty".into());
    }
    if name.contains('/') || name.contains(char::is_whitespace) {
        return Err(format!("group `{name}`: name must not contain `/` or whitespace"));
    }
    Ok(())
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.discovery.interval.is_zero(), "discovery.interval must be > 0");
        if let Some(token) = &self.admin.token {
            anyhow::ensure!(!token.trim().is_empty(), "admin.token must not be empty (remove it to disable auth)");
        }
        self.defaults.validate().map_err(|e| anyhow::anyhow!("defaults: {e}"))?;
        for (name, group) in &self.groups {
            validate_group_name(name).map_err(anyhow::Error::msg)?;
            anyhow::ensure!(!group.containers.is_empty(), "group `{name}`: `containers` must not be empty");
            group.settings(&self.defaults).validate().map_err(|e| anyhow::anyhow!("group `{name}`: {e}"))?;
        }
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
# What /_sds/check answers for a group sds does not know: allow (default) or deny.
# unknown_group = "allow"

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
        assert_eq!(s.health, super::super::HealthSpec::Tcp("127.0.0.1:5432".into()));
        assert_eq!(s.proxy.unwrap().mode, super::super::ProxyMode::Http);
    }

    #[test]
    fn rejects_unknown_keys_and_empty_groups() {
        assert!(toml::from_str::<Config>("[admin]\nbindd = \"x\"").is_err());
        // Unknown keys inside a static group are caught through the flattened overlay.
        assert!(toml::from_str::<Config>("[groups.g]\ncontainers = [\"a\"]\nidle_timeoutt = \"1m\"").is_err());
        let cfg: Config = toml::from_str("[groups.empty]\ncontainers = []").unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_values_that_break_at_runtime() {
        let zero_interval: Config = toml::from_str("[discovery]\ninterval = \"0s\"").unwrap();
        assert!(zero_interval.validate().unwrap_err().to_string().contains("discovery.interval"));
        let zero_idle: Config = toml::from_str("[groups.g]\ncontainers = [\"a\"]\nidle_timeout = \"0s\"").unwrap();
        assert!(zero_idle.validate().unwrap_err().to_string().contains("idle_timeout"));
        let bad_name: Config = toml::from_str("[groups.\"a/b\"]\ncontainers = [\"a\"]").unwrap();
        assert!(bad_name.validate().is_err());
        let empty_token: Config = toml::from_str("[admin]\ntoken = \"\"").unwrap();
        assert!(empty_token.validate().is_err());
    }

    #[test]
    fn explicit_missing_file_is_an_error() {
        assert!(load(Some(Path::new("/nonexistent/sds.toml"))).is_err());
    }

    #[test]
    fn serialized_config_redacts_the_admin_token() {
        let cfg: Config = toml::from_str("[admin]\ntoken = \"supersecret\"").unwrap();
        let output = toml::to_string_pretty(&cfg).unwrap();
        assert!(!output.contains("supersecret"));
        assert!(output.contains("token = \"<redacted>\""));
    }
}
