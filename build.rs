//! Sets `SDS_VERSION` for `crate::VERSION`: the release version from CI (`SDS_VERSION`),
//! else `git describe` of the checkout (`0.3.0-3-g34a5faa` between releases), else the
//! placeholder from `Cargo.toml`.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SDS_VERSION");
    // A new commit or tag changes the description. Only watch paths that exist: cargo
    // reruns on every build for a missing one.
    for path in [".git/HEAD", ".git/refs", ".git/packed-refs"] {
        if Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let version = std::env::var("SDS_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=SDS_VERSION={}", version.trim());
}

fn git_describe() -> Option<String> {
    let out = Command::new("git").args(["describe", "--tags", "--always"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8(out.stdout).ok()?;
    (!v.trim().is_empty()).then_some(v)
}
