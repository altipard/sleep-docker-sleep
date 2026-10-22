use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use owo_colors::OwoColorize;

use crate::config::Config;

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Where to write the file
    #[arg(long, default_value = "sds.toml", value_name = "PATH")]
    pub path: PathBuf,
    /// Overwrite an existing file
    #[arg(long)]
    pub force: bool,
    /// Print to stdout instead of writing a file
    #[arg(long)]
    pub stdout: bool,
}

pub fn init(args: InitArgs) -> Result<()> {
    let text = Config::example();
    if args.stdout {
        print!("{text}");
        return Ok(());
    }
    if args.path.exists() && !args.force {
        bail!("{} exists; use --force to overwrite", args.path.display());
    }
    if let Some(parent) = args.path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&args.path, text).with_context(|| format!("write {}", args.path.display()))?;
    println!("{} wrote {}", "ok".green().bold(), args.path.display());
    println!("next: label your containers (sds.enable=true) and run `sds check`");
    Ok(())
}
