use std::path::Path;

use anyhow::Result;
use clap::Args;

use crate::config;
use crate::integrate::{ProxyKind, SnippetInput, render};

#[derive(Debug, Args)]
pub struct IntegrateArgs {
    /// nginx, caddy or traefik
    pub proxy: ProxyKind,
    /// Group name
    pub group: String,
    /// Public hostname for the server block / router rule
    #[arg(long, default_value = "app.example.com")]
    pub host: String,
    /// Application upstream as the proxy sees it
    #[arg(long, default_value = "http://127.0.0.1:18080")]
    pub upstream: String,
    /// How the proxy reaches sds (default: derived from admin.bind; use host.docker.internal from containers)
    #[arg(long)]
    pub sds_url: Option<String>,
    /// Hold timeout in seconds the proxy must tolerate (default: the group default from sds.toml)
    #[arg(long)]
    pub hold_timeout: Option<u64>,
}

pub fn integrate(explicit: Option<&Path>, args: IntegrateArgs) -> Result<()> {
    let cfg = config::load(explicit).map(|(c, _)| c).unwrap_or_default();
    let sds_url = args.sds_url.unwrap_or_else(|| {
        let bind = cfg.admin.bind;
        let host = if bind.ip().is_unspecified() { "127.0.0.1".to_owned() } else { bind.ip().to_string() };
        format!("http://{host}:{}", bind.port())
    });
    let hold = args.hold_timeout.unwrap_or_else(|| {
        cfg.groups
            .get(&args.group)
            .and_then(|g| g.overrides.hold_timeout)
            .unwrap_or(cfg.defaults.hold_timeout)
            .as_secs()
    });
    let input = SnippetInput {
        group: args.group,
        sds_url,
        upstream: args.upstream,
        host: args.host,
        token: cfg.admin.token.clone(),
        hold_timeout_secs: hold,
    };
    print!("{}", render(args.proxy, &input));
    Ok(())
}
