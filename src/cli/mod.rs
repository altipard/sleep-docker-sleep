//! Command-line interface.

mod check;
mod client;
mod init;
mod integrate;
mod run;
mod service;
mod status;
mod update;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

pub use client::ApiClient;
pub use status::fmt_age;

/// sleep-docker-sleep: stop idle Docker containers and wake them on the first request.
#[derive(Debug, Parser)]
#[command(name = "sds", version = crate::VERSION, about, long_about = None, propagate_version = true)]
pub struct Cli {
    /// Path to sds.toml (default: $SDS_CONFIG, ./sds.toml, ~/.config/sds/sds.toml, /etc/sds/sds.toml)
    #[arg(short, long, global = true, env = "SDS_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon: discovery, controller, admin API, optional native proxies
    Run(run::RunArgs),
    /// Show all groups (talks to a running daemon)
    Status(status::StatusArgs),
    /// Start a group now
    Wake(status::WakeArgs),
    /// Stop a group now
    Sleep(status::SleepArgs),
    /// Stop a group and keep it stopped until `sds release`
    Hold(status::HoldArgs),
    /// Lift a hold; the next request wakes the group again
    Release(status::HoldArgs),
    /// Attach the terminal dashboard to a running daemon (alias: attach)
    #[command(alias = "attach")]
    Top(status::TopArgs),
    /// Validate configuration, Docker access and discovered groups without starting anything
    Check(check::CheckArgs),
    /// Write a commented sds.toml
    Init(init::InitArgs),
    /// Print a reverse-proxy snippet (nginx, caddy, traefik) for a group
    Integrate(integrate::IntegrateArgs),
    /// Generate or install a systemd / launchd unit
    Service(service::ServiceArgs),
    /// Show the effective configuration
    Config(ConfigArgs),
    /// Replace this binary with the latest release from GitHub
    Update(update::UpdateArgs),
}

/// Connection settings for commands that talk to a running daemon.
#[derive(Debug, Args, Clone)]
pub struct ClientArgs {
    /// Base URL of the daemon (default: derived from admin.bind in sds.toml)
    #[arg(long, env = "SDS_URL", global = true, value_name = "URL")]
    pub url: Option<String>,
    /// Token when admin.token is set (default: taken from sds.toml if readable)
    #[arg(long, env = "SDS_TOKEN", global = true, hide_env_values = true)]
    pub token: Option<String>,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Print only the path that would be used
    #[arg(long)]
    pub path: bool,
}

pub async fn main(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Run(args) => run::run(cli.config.as_deref(), args).await,
        Command::Status(args) => status::status(cli.config.as_deref(), args).await,
        Command::Wake(args) => status::wake(cli.config.as_deref(), args).await,
        Command::Sleep(args) => status::sleep(cli.config.as_deref(), args).await,
        Command::Hold(args) => status::set_hold(cli.config.as_deref(), args, true).await,
        Command::Release(args) => status::set_hold(cli.config.as_deref(), args, false).await,
        Command::Top(args) => status::top(cli.config.as_deref(), args).await,
        Command::Check(args) => check::check(cli.config.as_deref(), args).await,
        Command::Init(args) => init::init(args),
        Command::Integrate(args) => integrate::integrate(cli.config.as_deref(), args),
        Command::Service(args) => service::service(cli.config.as_deref(), args),
        Command::Config(args) => show_config(cli.config.as_deref(), args),
        Command::Update(args) => update::update(args).await,
    }
}

fn show_config(explicit: Option<&std::path::Path>, args: ConfigArgs) -> Result<()> {
    if args.path {
        match crate::config::locate(explicit) {
            Some(p) => println!("{}", p.display()),
            None => println!("(none; built-in defaults)"),
        }
        return Ok(());
    }
    let (cfg, source) = crate::config::load(explicit)?;
    println!("# source: {source}");
    print!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// clap only checks argument definitions when a subcommand is parsed; catch clashes
    /// (e.g. a `--version` option next to the global flag) in the test run instead.
    #[test]
    fn cli_definition_is_consistent() {
        super::Cli::command().debug_assert();
    }
}
