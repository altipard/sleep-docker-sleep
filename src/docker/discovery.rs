//! Turn the container list plus `sds.toml` into group specifications.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::backend::ContainerInfo;
use crate::config::{Config, GroupSettings, parse_labels};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupSource {
    Labels,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSpec {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_service: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub name: String,
    pub members: Vec<MemberSpec>,
    pub settings: GroupSettings,
    pub source: GroupSource,
    /// Non-fatal problems the admin should see (`sds check`, web overview).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub groups: Vec<GroupSpec>,
    /// Problems not attributable to one group (e.g. a container with broken labels).
    pub warnings: Vec<String>,
}

impl Discovery {
    pub fn group(&self, name: &str) -> Option<&GroupSpec> {
        self.groups.iter().find(|g| g.name == name)
    }
}

/// Build the group list. Deterministic: groups and members are sorted by name so repeated
/// discoveries compare equal when nothing changed.
pub fn discover(containers: &[ContainerInfo], config: &Config) -> Discovery {
    let mut warnings = Vec::new();
    let mut groups: BTreeMap<String, GroupSpec> = BTreeMap::new();

    // 1. Static groups from the file: they exist even when no container is found, so the
    //    admin sees the problem instead of a silently missing group.
    for (name, static_group) in &config.groups {
        let mut members = Vec::new();
        let mut group_warnings = Vec::new();
        for wanted in &static_group.containers {
            match containers.iter().find(|c| &c.name == wanted) {
                Some(c) if parse_labels(&c.labels).is_ok_and(|p| p.keep_awake) => {
                    group_warnings.push(format!("container `{wanted}` has sds.keep_awake=true and is not managed"));
                }
                Some(c) => members.push(MemberSpec {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    compose_service: c.labels.get(crate::config::COMPOSE_SERVICE).cloned(),
                }),
                None => group_warnings.push(format!("container `{wanted}` not found")),
            }
        }
        groups.insert(
            name.clone(),
            GroupSpec {
                name: name.clone(),
                members,
                settings: static_group.settings(&config.defaults),
                source: GroupSource::Config,
                warnings: group_warnings,
            },
        );
    }

    // 2. Labelled containers, in name order so "first label wins" is deterministic.
    let mut labelled: Vec<&ContainerInfo> =
        containers.iter().filter(|c| crate::config::ParsedLabels::mentions_sds(&c.labels)).collect();
    labelled.sort_by(|a, b| a.name.cmp(&b.name));
    // Name of the first labelled member per group: its labels define the group settings.
    let mut first_labelled: BTreeMap<String, String> = BTreeMap::new();

    for c in labelled {
        let parsed = match parse_labels(&c.labels) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("container `{}` ignored: {e}", c.name));
                continue;
            }
        };
        if parsed.keep_awake {
            continue;
        }
        if !parsed.enabled() {
            if parsed.enable.is_none() {
                warnings.push(format!("container `{}` has sds.* labels but no sds.enable=true; ignored", c.name));
            }
            continue;
        }
        let group_name = parsed.group_name(&c.name);
        let entry = groups.entry(group_name.clone()).or_insert_with(|| GroupSpec {
            name: group_name.clone(),
            members: Vec::new(),
            settings: config.defaults.clone(),
            source: GroupSource::Labels,
            warnings: Vec::new(),
        });
        match first_labelled.get(&group_name) {
            None => {
                // First labelled member: its labels win over [groups.*] and [defaults].
                parsed.apply_to(&mut entry.settings);
                first_labelled.insert(group_name.clone(), c.name.clone());
            }
            Some(first) => {
                let mut proposed = entry.settings.clone();
                parsed.apply_to(&mut proposed);
                if proposed != entry.settings {
                    entry.warnings.push(format!(
                        "container `{}` sets different sds.* values than `{first}`; the first one wins",
                        c.name
                    ));
                }
            }
        }
        if !entry.members.iter().any(|m| m.id == c.id) {
            entry.members.push(MemberSpec {
                id: c.id.clone(),
                name: c.name.clone(),
                compose_service: parsed.compose_service.clone(),
            });
        }
    }

    let mut groups: Vec<GroupSpec> = groups.into_values().collect();
    // A container in two groups would be fought over by two actors: it stays with the
    // first group (by name) and the others are told.
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for g in &mut groups {
        let name = g.name.clone();
        g.members.retain(|m| match owner.get(&m.id) {
            Some(first) => {
                warnings.push(format!(
                    "container `{}` is listed in groups `{first}` and `{name}`; it belongs to `{first}`",
                    m.name
                ));
                false
            }
            None => {
                owner.insert(m.id.clone(), name.clone());
                true
            }
        });
    }
    for g in &mut groups {
        g.members.sort_by(|a, b| a.name.cmp(&b.name));
        if g.members.is_empty() {
            g.warnings.push("no containers found for this group".to_owned());
        }
        if g.settings.proxy.is_some()
            && g.settings.wake == crate::config::WakeMode::Page
            && g.settings.proxy.as_ref().is_some_and(|p| p.mode == crate::config::ProxyMode::Tcp)
        {
            g.warnings.push("wake=page has no effect with a tcp proxy; connections are held".to_owned());
        }
    }
    Discovery { groups, warnings }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::config::{COMPOSE_PROJECT, HealthSpec};

    fn container(name: &str, labels: &[(&str, &str)]) -> ContainerInfo {
        ContainerInfo {
            id: format!("id-{name}"),
            name: name.to_owned(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>(),
            running: true,
            status: "running".into(),
            health: Default::default(),
            has_healthcheck: false,
        }
    }

    #[test]
    fn groups_labelled_containers_by_compose_project() {
        let containers = vec![
            container("shop-app-1", &[("sds.enable", "true"), (COMPOSE_PROJECT, "shop"), ("sds.idle_timeout", "30m")]),
            container("shop-db-1", &[("sds.enable", "true"), (COMPOSE_PROJECT, "shop")]),
            container("shop-caddy-1", &[("sds.keep_awake", "true"), (COMPOSE_PROJECT, "shop")]),
            container("unrelated", &[]),
        ];
        let d = discover(&containers, &Config::default());
        assert_eq!(d.groups.len(), 1);
        let g = &d.groups[0];
        assert_eq!(g.name, "shop");
        assert_eq!(g.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["shop-app-1", "shop-db-1"]);
        assert_eq!(g.settings.idle_timeout, Duration::from_secs(1800));
        assert_eq!(g.source, GroupSource::Labels);
        assert!(g.warnings.is_empty());
    }

    #[test]
    fn warns_on_conflicting_member_labels_and_broken_labels() {
        let containers = vec![
            container("a", &[("sds.enable", "true"), ("sds.group", "g"), ("sds.idle_timeout", "1m")]),
            container("b", &[("sds.enable", "true"), ("sds.group", "g"), ("sds.idle_timeout", "2m")]),
            container("c", &[("sds.enable", "nope")]),
        ];
        let d = discover(&containers, &Config::default());
        assert_eq!(d.groups[0].settings.idle_timeout, Duration::from_secs(60));
        assert_eq!(d.groups[0].warnings.len(), 1);
        assert_eq!(d.warnings.len(), 1);
        assert!(d.warnings[0].contains("`c`"));
    }

    #[test]
    fn a_container_belongs_to_one_group_only() {
        let cfg: Config = toml::from_str(
            r#"
            [groups.legacy]
            containers = ["shared"]
            "#,
        )
        .unwrap();
        let d = discover(&[container("shared", &[("sds.enable", "true"), ("sds.group", "new")])], &cfg);
        assert_eq!(d.group("legacy").unwrap().members.len(), 1);
        assert!(d.group("new").unwrap().members.is_empty());
        assert!(d.warnings.iter().any(|w| w.contains("`legacy`") && w.contains("`new`")), "{:?}", d.warnings);
    }

    #[test]
    fn keep_awake_protects_static_members_and_missing_enable_warns() {
        let cfg: Config = toml::from_str("[groups.g]\ncontainers = [\"proxy\", \"app\"]").unwrap();
        let d = discover(
            &[container("proxy", &[("sds.keep_awake", "true")]), container("app", &[("sds.idle_timeout", "1m")])],
            &cfg,
        );
        let g = d.group("g").unwrap();
        assert_eq!(g.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["app"]);
        assert!(g.warnings.iter().any(|w| w.contains("keep_awake")));
        assert!(d.warnings.iter().any(|w| w.contains("`app`") && w.contains("sds.enable")), "{:?}", d.warnings);
    }

    #[test]
    fn static_groups_resolve_names_and_report_missing() {
        let cfg: Config = toml::from_str(
            r#"
            [groups.legacy]
            containers = ["legacy-web", "legacy-gone"]
            health = "tcp://127.0.0.1:5432"
            "#,
        )
        .unwrap();
        let d = discover(&[container("legacy-web", &[])], &cfg);
        let g = d.group("legacy").unwrap();
        assert_eq!(g.source, GroupSource::Config);
        assert_eq!(g.members.len(), 1);
        assert_eq!(g.settings.health, HealthSpec::Tcp("127.0.0.1:5432".into()));
        assert_eq!(g.warnings, vec!["container `legacy-gone` not found".to_string()]);
    }

    #[test]
    fn labels_override_static_group_settings_for_the_same_container() {
        let cfg: Config = toml::from_str(
            r#"
            [groups.smoke]
            containers = ["smoke-1"]
            idle_timeout = "1h"
            [groups.smoke.proxy]
            listen = "127.0.0.1:18090"
            upstream = "127.0.0.1:18081"
            "#,
        )
        .unwrap();
        let d = discover(
            &[container("smoke-1", &[("sds.enable", "true"), ("sds.group", "smoke"), ("sds.idle_timeout", "20s")])],
            &cfg,
        );
        let g = d.group("smoke").unwrap();
        assert_eq!(g.members.len(), 1);
        assert_eq!(g.settings.idle_timeout, Duration::from_secs(20));
        assert!(g.settings.proxy.is_some());
        assert!(g.warnings.is_empty());
    }

    #[test]
    fn discovery_is_deterministic() {
        let a = container("a", &[("sds.enable", "true")]);
        let b = container("b", &[("sds.enable", "true")]);
        let d1 = discover(&[a.clone(), b.clone()], &Config::default());
        let d2 = discover(&[b, a], &Config::default());
        assert_eq!(d1, d2);
    }
}
