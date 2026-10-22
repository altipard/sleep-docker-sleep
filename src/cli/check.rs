use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use clap::Args;
use owo_colors::OwoColorize;

use crate::config;
use crate::docker::{Backend, DockerEngine, discover};

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Machine-readable output
    #[arg(long)]
    pub json: bool,
}

/// Exit code 1 when something needs attention, so it can gate deployments.
pub async fn check(explicit: Option<&Path>, args: CheckArgs) -> Result<()> {
    let mut problems = 0usize;
    let (cfg, source) = config::load(explicit)?;
    if !args.json {
        println!("{} config: {source}", "ok".green().bold());
        if cfg.admin.token.is_none() && !cfg.admin.bind.ip().is_loopback() {
            println!("{} admin.bind is not loopback but admin.token is unset", "warn".yellow().bold());
            problems += 1;
        }
    }

    let backend: Arc<dyn Backend> = match DockerEngine::connect(cfg.docker.host.as_deref()) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            if args.json {
                println!("{}", serde_json::json!({ "ok": false, "error": e.to_string() }));
            } else {
                println!("{} docker: {e}", "fail".red().bold());
            }
            std::process::exit(1);
        }
    };
    let version = match backend.ping().await {
        Ok(v) => v,
        Err(e) => {
            if args.json {
                println!("{}", serde_json::json!({ "ok": false, "error": e.to_string() }));
            } else {
                println!("{} docker ({}): {e}", "fail".red().bold(), backend.describe());
            }
            std::process::exit(1);
        }
    };
    if !args.json {
        println!("{} docker: {version} via {}", "ok".green().bold(), backend.describe());
    }

    let containers = backend.list().await?;
    let discovery = discover(&containers, &cfg);
    problems += discovery.warnings.len() + discovery.groups.iter().map(|g| g.warnings.len()).sum::<usize>();

    if args.json {
        let groups: Vec<serde_json::Value> = discovery
            .groups
            .iter()
            .map(|g| {
                serde_json::json!({
                    "name": g.name, "source": g.source, "members": g.members, "settings": g.settings, "warnings": g.warnings,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": problems == 0, "config": source.to_string(), "docker": version, "containers": containers.len(),
                "warnings": discovery.warnings, "groups": groups,
            }))?
        );
    } else {
        println!("{} {} container(s) listed", "ok".green().bold(), containers.len());
        for w in &discovery.warnings {
            println!("{} {w}", "warn".yellow().bold());
        }
        if discovery.groups.is_empty() {
            println!(
                "{} no groups: label containers with sds.enable=true or add [groups.*] to sds.toml",
                "warn".yellow().bold()
            );
            problems += 1;
        }
        for g in &discovery.groups {
            let members = if g.members.is_empty() {
                "(none)".to_owned()
            } else {
                g.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ")
            };
            println!(
                "{} group {} [{:?}]: {members} · idle {} · health {} · wake {}{}",
                if g.warnings.is_empty() {
                    "ok".green().bold().to_string()
                } else {
                    "warn".yellow().bold().to_string()
                },
                g.name.bold(),
                g.source,
                humantime::format_duration(g.settings.idle_timeout),
                g.settings.health,
                g.settings.wake,
                g.settings
                    .proxy
                    .as_ref()
                    .map(|p| format!(" · proxy {} {} -> {}", p.mode, p.listen, p.upstream))
                    .unwrap_or_default()
            );
            for w in &g.warnings {
                println!("       {} {w}", "↳".yellow());
            }
        }
        if problems > 0 {
            println!("\n{} {problems} issue(s) found", "warn".yellow().bold());
        } else {
            println!("\n{} everything looks good", "ok".green().bold());
        }
    }
    if problems > 0 {
        std::process::exit(1);
    }
    Ok(())
}
