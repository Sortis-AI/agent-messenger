use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn am_cmd(data_dir: &TempDir, config_dir: &TempDir) -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("am");
    cmd.env("XDG_DATA_HOME", data_dir.path());
    cmd.env("XDG_CONFIG_HOME", config_dir.path());
    cmd.env_remove("AM_HOME");
    cmd
}

#[test]
fn add_and_list_relay() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["relay", "add", "wss://relay.damus.io"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["relay", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("relay.damus.io"));
}

#[test]
fn add_remove_relay() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["relay", "add", "wss://relay.damus.io"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["relay", "remove", "wss://relay.damus.io"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["relay", "list"])
        .assert()
        .success()
        .stdout(predicate::str::is_match("\\[\\s*\\]").unwrap());
}

#[test]
fn add_duplicate_relay_fails() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["relay", "add", "wss://relay.damus.io"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["relay", "add", "wss://relay.damus.io"])
        .assert()
        .failure();
}

#[test]
fn remove_nonexistent_relay_fails() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["relay", "remove", "wss://ghost.relay"])
        .assert()
        .failure();
}

#[test]
fn add_relay_without_identity_reports_publish_error() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    // The relay is still added; the kind-10050 publish failure is non-fatal.
    am_cmd(&data, &config)
        .args(["relay", "add", "wss://relay.damus.io"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"added\""))
        .stdout(predicate::str::contains("relay_list_error"));
}

#[test]
fn add_relay_no_publish_skips_relay_list() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["relay", "add", "--no-publish", "wss://relay.damus.io"])
        .assert()
        .success()
        .stdout(predicate::str::contains("relay_list").not());
}

#[test]
fn publish_without_relays_fails() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["relay", "publish"])
        .assert()
        .code(3);
}
