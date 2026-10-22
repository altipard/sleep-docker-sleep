use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use owo_colors::OwoColorize;

use crate::service::{ServiceKind, UnitInput, render};

#[derive(Debug, Args)]
pub struct ServiceArgs {
    #[command(subcommand)]
    pub command: ServiceCommand,
}

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Print the unit file (systemd on Linux, launchd on macOS)
    Show(UnitArgs),
    /// Write the unit file to its system location (needs root) and print the enable command
    Install(UnitArgs),
    /// Print the commands that remove the unit
    Uninstall,
}

#[derive(Debug, Args)]
pub struct UnitArgs {
    /// Path of the sds binary (default: the running executable)
    #[arg(long)]
    pub binary: Option<PathBuf>,
    /// Unix user the daemon runs as (must have Docker socket access)
    #[arg(long, default_value = "root")]
    pub user: String,
}

fn unit(explicit: Option<&Path>, args: &UnitArgs) -> Result<(ServiceKind, String)> {
    let kind = ServiceKind::for_host();
    let binary = match &args.binary {
        Some(b) => b.clone(),
        None => std::env::current_exe().context("cannot determine sds binary path")?,
    };
    let config = explicit.map(Path::to_path_buf).or_else(|| crate::config::locate(None));
    let text = render(kind, &UnitInput { binary: &binary, config: config.as_deref(), user: &args.user });
    Ok((kind, text))
}

pub fn service(explicit: Option<&Path>, args: ServiceArgs) -> Result<()> {
    match args.command {
        ServiceCommand::Show(a) => {
            let (kind, text) = unit(explicit, &a)?;
            println!("# {}", kind.unit_path().display());
            print!("{text}");
            Ok(())
        }
        ServiceCommand::Install(a) => {
            let (kind, text) = unit(explicit, &a)?;
            let path = kind.unit_path();
            std::fs::write(&path, text).with_context(|| format!("write {} (run with sudo?)", path.display()))?;
            println!("{} wrote {}", "ok".green().bold(), path.display());
            println!("next:\n{}", kind.install_hint());
            Ok(())
        }
        ServiceCommand::Uninstall => {
            let kind = ServiceKind::for_host();
            println!("{}", kind.uninstall_hint());
            Ok(())
        }
    }
}
