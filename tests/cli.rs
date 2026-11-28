#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;

fn sds() -> Command {
    Command::cargo_bin("sds").expect("binary builds")
}

#[test]
fn help_lists_commands() {
    sds()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("integrate"))
        .stdout(predicate::str::contains("service"));
}

#[test]
fn init_stdout_is_valid_config() {
    let out = sds().args(["init", "--stdout"]).assert().success().get_output().stdout.clone();
    let text = String::from_utf8(out).unwrap();
    let cfg: sds::config::Config = toml::from_str(&text).unwrap();
    cfg.validate().unwrap();
}

#[test]
fn init_refuses_to_overwrite_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sds.toml");
    sds().args(["init", "--path"]).arg(&path).assert().success();
    sds().args(["init", "--path"]).arg(&path).assert().failure().stderr(predicate::str::contains("--force"));
    sds().args(["init", "--force", "--path"]).arg(&path).assert().success();
}

#[test]
fn integrate_renders_snippets_for_all_proxies() {
    for proxy in ["nginx", "caddy", "traefik"] {
        sds()
            .args(["integrate", proxy, "shop", "--host", "shop.example.com"])
            .assert()
            .success()
            .stdout(predicate::str::contains("/_sds/check/shop"))
            .stdout(predicate::str::contains("shop.example.com"));
    }
    sds().args(["integrate", "haproxy", "shop"]).assert().failure();
}

#[test]
fn config_reports_missing_explicit_file() {
    sds()
        .args(["--config", "/nonexistent/sds.toml", "config"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nonexistent"));
}

#[test]
fn config_rejects_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sds.toml");
    std::fs::write(&path, "[admin]\nbindd = \"127.0.0.1:1\"\n").unwrap();
    sds().args(["--config"]).arg(&path).arg("config").assert().failure().stderr(predicate::str::contains("bindd"));
}

#[test]
fn service_show_prints_unit() {
    sds()
        .args(["service", "show", "--binary", "/usr/local/bin/sds"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--headless"));
}

#[test]
fn status_without_daemon_fails_clearly() {
    sds()
        .args(["status", "--url", "http://127.0.0.1:1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot reach sds"));
}

#[test]
fn top_demo_needs_a_terminal_and_no_daemon() {
    // Fails on the missing terminal, not on a missing daemon or config.
    sds()
        .args(["top", "--demo", "--config", "/nonexistent/sds.toml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a terminal"));
    // A daemon address from the environment (SDS_URL, SDS_TOKEN) does not get in the way.
    sds()
        .args(["top", "--demo"])
        .env("SDS_URL", "http://127.0.0.1:1")
        .env("SDS_TOKEN", "x")
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a terminal"));
}

fn integrate_cmd() -> Command {
    let mut cmd = sds();
    cmd.args(["integrate", "nginx", "shop", "--host", "shop.example.com"]);
    cmd
}

#[test]
fn integrate_fails_on_missing_explicit_config() {
    let mut cmd = sds();
    cmd.args(["-c", "/nonexistent/sds.toml", "integrate", "nginx", "shop"]);
    cmd.assert().failure().stdout("").stderr(predicate::str::contains("/nonexistent/sds.toml"));
}

#[test]
fn integrate_fails_on_invalid_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sds.toml");
    std::fs::write(&path, "this is = not [valid toml").unwrap();
    let mut cmd = sds();
    cmd.arg("-c").arg(&path).args(["integrate", "nginx", "shop"]);
    cmd.assert().failure().stdout("").stderr(predicate::str::contains("sds.toml"));
}

#[cfg(unix)]
#[test]
fn integrate_fails_on_unreadable_config() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sds.toml");
    std::fs::write(&path, "[admin]\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_to_string(&path).is_ok() {
        // running as root: mode 0 does not block reads
        return;
    }
    let mut cmd = sds();
    cmd.arg("-c").arg(&path).args(["integrate", "nginx", "shop"]);
    cmd.assert().failure().stdout("").stderr(predicate::str::contains("sds.toml"));
}

#[test]
fn integrate_emits_token_from_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sds.toml");
    std::fs::write(&path, "[admin]\nbind = \"127.0.0.1:9911\"\ntoken = \"s3cret-token\"\n").unwrap();
    let mut cmd = sds();
    cmd.arg("-c").arg(&path).args(["integrate", "nginx", "shop"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("X-SDS-Token"))
        .stdout(predicate::str::contains("s3cret-token"))
        .stdout(predicate::str::contains("127.0.0.1:9911"))
        .stderr("");
}

#[test]
fn integrate_without_any_config_notes_defaults_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let out = integrate_cmd()
        .current_dir(cwd.path())
        .env_remove("SDS_CONFIG")
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("xdg"))
        .assert()
        .success()
        .stderr(predicate::str::contains("built-in defaults"))
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("/_sds/check/shop"));
    assert!(!stdout.contains("note:"));
}
