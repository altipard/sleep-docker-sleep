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
