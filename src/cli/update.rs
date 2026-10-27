//! `sds update`: replace the running binary with the latest GitHub release.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use owo_colors::OwoColorize;
use serde::Deserialize;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Only report whether a newer release exists
    #[arg(long)]
    pub check: bool,
    /// Install this release instead of the latest (e.g. 0.3.0)
    #[arg(long, value_name = "VERSION")]
    pub tag: Option<String>,
    /// Install even if the running version is the same or newer
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub async fn update(args: UpdateArgs) -> Result<()> {
    if Path::new("/.dockerenv").exists() {
        bail!("sds runs in a container; update the image instead (docker pull ghcr.io/altipard/sleep-docker-sleep)");
    }
    let http = reqwest::Client::builder()
        .user_agent(concat!("sds/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let release = fetch_release(&http, args.tag.as_deref()).await?;
    let current = crate::VERSION;
    let latest = release.tag_name.trim_start_matches('v');

    match compare(current, latest) {
        Newer::Yes => println!("{} {current} → {latest}", "update available:".bold()),
        Newer::No if !args.force => {
            println!("{} sds {current} is up to date (latest release {latest})", "ok".green().bold());
            return Ok(());
        }
        Newer::DevBuild if !args.force => {
            println!(
                "sds {current} is a build from source based on {latest} or later; use --force to install the release"
            );
            return Ok(());
        }
        Newer::No | Newer::DevBuild => println!("{} reinstalling {latest} (was {current})", "--force:".bold()),
    }
    if args.check {
        return Ok(());
    }

    let asset_name = asset_name()?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .with_context(|| format!("release {latest} has no binary `{asset_name}` for this platform"))?;
    let target = std::env::current_exe().and_then(std::fs::canonicalize).context("locate the running sds binary")?;
    let bytes = http
        .get(&asset.browser_download_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("download {}", asset.browser_download_url))?
        .bytes()
        .await?;
    install(&target, &bytes, latest)?;
    println!("{} installed sds {latest} at {}", "ok".green().bold(), target.display());
    println!("restart the daemon to use it, e.g. `sudo systemctl restart sds` or");
    println!("`sudo launchctl kickstart -k system/com.zilicon.sds`");
    Ok(())
}

async fn fetch_release(http: &reqwest::Client, version: Option<&str>) -> Result<Release> {
    let repo = REPOSITORY.trim_start_matches("https://github.com/").trim_end_matches('/');
    let url = match version {
        Some(v) => format!("https://api.github.com/repos/{repo}/releases/tags/{v}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let resp = http.get(&url).send().await.with_context(|| format!("cannot reach {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("no release {} found", version.unwrap_or("(latest)"));
    }
    Ok(resp.error_for_status().with_context(|| format!("GET {url}"))?.json().await?)
}

/// Release asset for this machine, as named by the release workflow.
fn asset_name() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        other => bail!("no release binaries for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("no release binaries for {other}"),
    };
    Ok(format!("sds-{os}-{arch}"))
}

/// Write next to `target`, check that the new binary runs and reports `version`, then
/// rename over `target` (atomic on the same file system; the running process keeps its
/// old inode).
fn install(target: &Path, bytes: &[u8], version: &str) -> Result<()> {
    let dir = target.parent().context("binary has no parent directory")?;
    let tmp: PathBuf = dir.join(format!(".sds-update-{}", std::process::id()));
    let result = (|| -> Result<()> {
        std::fs::write(&tmp, bytes).map_err(|e| permission_hint(e, dir))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        let out = std::process::Command::new(&tmp).arg("--version").output().context("run the downloaded binary")?;
        let reported = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !reported.contains(version) {
            bail!("downloaded binary does not report version {version} (got `{}`)", reported.trim());
        }
        std::fs::rename(&tmp, target).map_err(|e| permission_hint(e, dir))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn permission_hint(e: std::io::Error, dir: &Path) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        anyhow::anyhow!("no write access to {}; run `sudo sds update`", dir.display())
    } else {
        anyhow::Error::new(e).context(format!("write to {}", dir.display()))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Newer {
    Yes,
    No,
    /// Running a `git describe` build (`0.3.0-3-gabc`) whose base is not older than the
    /// release: it may contain unreleased changes.
    DevBuild,
}

/// Is release `latest` newer than the running `current`?
fn compare(current: &str, latest: &str) -> Newer {
    let (base, dev) = match current.split_once('-') {
        Some((base, _)) => (base, true),
        None => (current, false),
    };
    match (parse(base), parse(latest)) {
        (Some(c), Some(l)) if l > c => Newer::Yes,
        (Some(_), Some(_)) if dev => Newer::DevBuild,
        (Some(_), Some(_)) => Newer::No,
        // Unparsable (e.g. a bare commit hash from `git describe --always`): offer the release.
        _ => Newer::Yes,
    }
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim_start_matches('v').split('.').map(str::parse::<u64>);
    let version = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    it.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_and_source_builds() {
        assert_eq!(compare("0.2.1", "0.3.0"), Newer::Yes);
        assert_eq!(compare("0.3.0", "0.3.0"), Newer::No);
        assert_eq!(compare("0.10.0", "0.9.0"), Newer::No);
        assert_eq!(compare("0.3.0-4-g7beac6a", "0.3.0"), Newer::DevBuild);
        assert_eq!(compare("0.2.1-9-g34a5faa", "0.3.0"), Newer::Yes);
        assert_eq!(compare("7beac6a", "0.3.0"), Newer::Yes);
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        let name = asset_name().unwrap();
        let workflow = include_str!("../../.github/workflows/release.yml");
        assert!(workflow.contains(&format!("name: {name} ")), "{name} not built by release.yml");
    }
}
