//! Parsing of `sds.*` container labels.
//!
//! | Label                | Value                                   |
//! |----------------------|-----------------------------------------|
//! | `sds.enable`         | `true` / `false`                        |
//! | `sds.group`          | group name (default: compose project)   |
//! | `sds.keep_awake`     | `true`: never stopped by `sds`          |
//! | `sds.idle_timeout`   | duration, e.g. `15m`                    |
//! | `sds.start_timeout`  | duration                                |
//! | `sds.stop_timeout`   | duration                                |
//! | `sds.start_grace`    | duration                                |
//! | `sds.health`         | `docker`, `grace`, `http://…`, `tcp://…`|
//! | `sds.wake`           | `hold` / `page`                         |
//! | `sds.hold_timeout`   | duration                                |
//! | `sds.max_waiters`    | integer                                 |
//! | `sds.proxy.listen`   | socket address, e.g. `0.0.0.0:8080`     |
//! | `sds.proxy.upstream` | `host:port`                             |
//! | `sds.proxy.mode`     | `http` / `tcp`                          |

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

use super::settings::{GroupSettings, HealthSpec, ProxyMode, ProxySettings, SettingsOverlay, WakeMode};

pub const COMPOSE_PROJECT: &str = "com.docker.compose.project";
pub const COMPOSE_SERVICE: &str = "com.docker.compose.service";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LabelError {
    #[error("label `{label}`: {reason}")]
    Invalid { label: String, reason: String },
    #[error("label `sds.proxy.listen` requires `sds.proxy.upstream` (and vice versa)")]
    IncompleteProxy,
}

/// Everything `sds` reads from one container's labels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedLabels {
    /// `sds.enable` as written; `None` when the label is absent.
    pub enable: Option<bool>,
    pub keep_awake: bool,
    pub group: Option<String>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    pub overrides: SettingsOverlay,
}

impl ParsedLabels {
    /// `true` when the container carries any `sds.*` label at all.
    pub fn mentions_sds(labels: &HashMap<String, String>) -> bool {
        labels.keys().any(|k| k.starts_with(crate::LABEL_PREFIX))
    }

    pub fn enabled(&self) -> bool {
        self.enable == Some(true)
    }

    /// The group this container belongs to: explicit `sds.group`, else the compose
    /// project, else the container's own name.
    pub fn group_name(&self, container_name: &str) -> String {
        self.group.clone().or_else(|| self.compose_project.clone()).unwrap_or_else(|| container_name.to_owned())
    }

    /// Overlay the label values onto `base` (labels win).
    pub fn apply_to(&self, base: &mut GroupSettings) {
        self.overrides.apply_to(base);
    }
}

fn invalid(label: &str, reason: impl Into<String>) -> LabelError {
    LabelError::Invalid { label: label.to_owned(), reason: reason.into() }
}

fn parse_bool(labels: &HashMap<String, String>, key: &str) -> Result<Option<bool>, LabelError> {
    match labels.get(key).map(|v| v.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(v) if matches!(v.as_str(), "true" | "1" | "yes" | "on") => Ok(Some(true)),
        Some(v) if matches!(v.as_str(), "false" | "0" | "no" | "off") => Ok(Some(false)),
        Some(v) => Err(invalid(key, format!("expected true/false, got `{v}`"))),
    }
}

fn parse_duration(labels: &HashMap<String, String>, key: &str) -> Result<Option<Duration>, LabelError> {
    labels.get(key).map(|v| humantime::parse_duration(v.trim()).map_err(|e| invalid(key, e.to_string()))).transpose()
}

fn parse_with<T: std::str::FromStr<Err = String>>(
    labels: &HashMap<String, String>,
    key: &str,
) -> Result<Option<T>, LabelError> {
    labels.get(key).map(|v| v.parse::<T>().map_err(|e| invalid(key, e))).transpose()
}

/// Parse all `sds.*` labels of one container. Unknown `sds.*` keys are reported as errors so
/// typos do not silently disable a feature.
pub fn parse_labels(labels: &HashMap<String, String>) -> Result<ParsedLabels, LabelError> {
    const KNOWN: &[&str] = &[
        "sds.enable",
        "sds.keep_awake",
        "sds.group",
        "sds.idle_timeout",
        "sds.start_timeout",
        "sds.stop_timeout",
        "sds.start_grace",
        "sds.health",
        "sds.wake",
        "sds.hold_timeout",
        "sds.max_waiters",
        "sds.proxy.listen",
        "sds.proxy.upstream",
        "sds.proxy.mode",
    ];
    if let Some(unknown) = labels.keys().find(|k| k.starts_with(crate::LABEL_PREFIX) && !KNOWN.contains(&k.as_str())) {
        return Err(invalid(unknown, "unknown sds label"));
    }

    let listen = labels
        .get("sds.proxy.listen")
        .map(|v| v.trim().parse::<SocketAddr>().map_err(|e| invalid("sds.proxy.listen", e.to_string())))
        .transpose()?;
    let upstream = labels.get("sds.proxy.upstream").map(|v| v.trim().to_owned());
    let proxy = match (listen, upstream) {
        (None, None) => None,
        (Some(listen), Some(upstream)) => Some(ProxySettings {
            listen,
            upstream,
            mode: parse_with::<ProxyMode>(labels, "sds.proxy.mode")?.unwrap_or_default(),
        }),
        _ => return Err(LabelError::IncompleteProxy),
    };

    let max_waiters = labels
        .get("sds.max_waiters")
        .map(|v| v.trim().parse::<usize>().map_err(|e| invalid("sds.max_waiters", e.to_string())))
        .transpose()?;

    let group = labels.get("sds.group").map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    if let Some(g) = &group {
        super::file::validate_group_name(g).map_err(|e| invalid("sds.group", e))?;
    }

    let overrides = SettingsOverlay {
        idle_timeout: parse_duration(labels, "sds.idle_timeout")?,
        start_timeout: parse_duration(labels, "sds.start_timeout")?,
        stop_timeout: parse_duration(labels, "sds.stop_timeout")?,
        start_grace: parse_duration(labels, "sds.start_grace")?,
        health: parse_with::<HealthSpec>(labels, "sds.health")?,
        wake: parse_with::<WakeMode>(labels, "sds.wake")?,
        hold_timeout: parse_duration(labels, "sds.hold_timeout")?,
        max_waiters,
        proxy,
    };
    // Values that break at runtime (zero timeouts, max_waiters = 0) are label errors, so the
    // container is reported instead of behaving strangely.
    overrides
        .applied_to(&GroupSettings::default())
        .validate()
        .map_err(|e| LabelError::Invalid { label: format!("sds.{}", e.split(' ').next().unwrap_or("?")), reason: e })?;

    Ok(ParsedLabels {
        enable: parse_bool(labels, "sds.enable")?,
        keep_awake: parse_bool(labels, "sds.keep_awake")?.unwrap_or(false),
        group,
        compose_project: labels.get(COMPOSE_PROJECT).cloned(),
        compose_service: labels.get(COMPOSE_SERVICE).cloned(),
        overrides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn parses_full_label_set() {
        let parsed = parse_labels(&labels(&[
            ("sds.enable", "true"),
            ("sds.group", "shop"),
            ("sds.idle_timeout", "30m"),
            ("sds.health", "http://127.0.0.1:18080/healthz"),
            ("sds.wake", "page"),
            ("sds.proxy.listen", "0.0.0.0:8080"),
            ("sds.proxy.upstream", "127.0.0.1:18080"),
            ("sds.proxy.mode", "tcp"),
            (COMPOSE_PROJECT, "shop-prod"),
        ]))
        .unwrap();
        assert!(parsed.enabled());
        assert_eq!(parsed.group.as_deref(), Some("shop"));
        assert_eq!(parsed.overrides.idle_timeout, Some(Duration::from_secs(1800)));
        assert_eq!(parsed.overrides.wake, Some(WakeMode::Page));
        let proxy = parsed.overrides.proxy.unwrap();
        assert_eq!(proxy.mode, ProxyMode::Tcp);
        assert_eq!(proxy.upstream, "127.0.0.1:18080");
    }

    #[test]
    fn group_name_falls_back_to_compose_project_then_container_name() {
        let p = parse_labels(&labels(&[("sds.enable", "1"), (COMPOSE_PROJECT, "shop")])).unwrap();
        assert_eq!(p.group_name("shop-app-1"), "shop");
        let p = parse_labels(&labels(&[("sds.enable", "1")])).unwrap();
        assert_eq!(p.group_name("lonely"), "lonely");
    }

    #[test]
    fn rejects_unknown_and_invalid_labels() {
        assert_eq!(
            parse_labels(&labels(&[("sds.idle_timout", "5m")])),
            Err(LabelError::Invalid { label: "sds.idle_timout".into(), reason: "unknown sds label".into() })
        );
        assert!(parse_labels(&labels(&[("sds.enable", "maybe")])).is_err());
        assert!(parse_labels(&labels(&[("sds.idle_timeout", "soon")])).is_err());
        assert_eq!(parse_labels(&labels(&[("sds.proxy.listen", "0.0.0.0:80")])), Err(LabelError::IncompleteProxy));
    }

    #[test]
    fn rejects_values_that_break_at_runtime() {
        let err = parse_labels(&labels(&[("sds.idle_timeout", "0s")])).unwrap_err();
        assert!(err.to_string().contains("idle_timeout"), "{err}");
        assert!(parse_labels(&labels(&[("sds.max_waiters", "0")])).is_err());
        let err = parse_labels(&labels(&[("sds.group", "a/b")])).unwrap_err();
        assert!(err.to_string().contains("sds.group"), "{err}");
        assert!(parse_labels(&labels(&[("sds.group", "has space")])).is_err());
    }

    #[test]
    fn enable_distinguishes_absent_from_false() {
        assert_eq!(parse_labels(&labels(&[("sds.idle_timeout", "5m")])).unwrap().enable, None);
        assert_eq!(parse_labels(&labels(&[("sds.enable", "false")])).unwrap().enable, Some(false));
    }

    #[test]
    fn labels_override_defaults_only_where_set() {
        let parsed = parse_labels(&labels(&[("sds.stop_timeout", "5s")])).unwrap();
        let mut settings = GroupSettings::default();
        parsed.apply_to(&mut settings);
        assert_eq!(settings.stop_timeout, Duration::from_secs(5));
        assert_eq!(settings.idle_timeout, GroupSettings::default().idle_timeout);
    }
}
