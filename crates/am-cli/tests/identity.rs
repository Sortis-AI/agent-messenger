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
fn generate_default_identity() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("npub"));
}

#[test]
fn generate_named_identity() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "alice"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice"));
}

#[test]
fn show_identity_after_generate() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "test"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["identity", "show", "--name", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("npub"));
}

#[test]
fn show_secret_key() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "test"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["identity", "show", "--name", "test", "--secret"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nsec"));
}

#[test]
fn list_identities() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "alice"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "bob"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["identity", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice").and(predicate::str::contains("bob")));
}

#[test]
fn import_identity() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "source"])
        .assert()
        .success();

    let show_output = am_cmd(&data, &config)
        .args(["identity", "show", "--name", "source", "--secret"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&show_output.stdout).unwrap();
    let nsec = json["nsec"].as_str().unwrap();
    let original_npub = json["npub"].as_str().unwrap();

    // Import into a fresh environment
    let data2 = TempDir::new().unwrap();
    let config2 = TempDir::new().unwrap();

    am_cmd(&data2, &config2)
        .args(["identity", "import", nsec, "--name", "imported"])
        .assert()
        .success();

    am_cmd(&data2, &config2)
        .args(["identity", "show", "--name", "imported"])
        .assert()
        .success()
        .stdout(predicate::str::contains(original_npub));
}

#[test]
fn show_nonexistent_identity_fails() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "show", "--name", "ghost"])
        .assert()
        .failure();
}

#[test]
fn duplicate_identity_fails() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "dup"])
        .assert()
        .success();

    am_cmd(&data, &config)
        .args(["identity", "generate", "--name", "dup"])
        .assert()
        .failure();
}

#[test]
fn import_invalid_nsec_exits_args() {
    let data = TempDir::new().unwrap();
    let config = TempDir::new().unwrap();

    am_cmd(&data, &config)
        .args(["identity", "import", "not-an-nsec"])
        .assert()
        .code(2);
}
