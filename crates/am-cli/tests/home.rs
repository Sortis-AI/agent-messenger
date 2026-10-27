use assert_cmd::Command;
use tempfile::TempDir;

/// An `am` command with XDG dirs pointed at a decoy, so any write that ignores
/// the home override lands somewhere we can detect.
fn am_cmd(decoy: &TempDir) -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("am");
    cmd.env("XDG_DATA_HOME", decoy.path().join("data"));
    cmd.env("XDG_CONFIG_HOME", decoy.path().join("config"));
    cmd.env_remove("AM_HOME");
    cmd
}

fn assert_decoy_untouched(decoy: &TempDir) {
    assert!(
        !decoy.path().join("data").exists(),
        "wrote to XDG_DATA_HOME"
    );
    assert!(
        !decoy.path().join("config").exists(),
        "wrote to XDG_CONFIG_HOME"
    );
}

#[test]
fn am_home_env_isolates_state() {
    let home = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();

    am_cmd(&decoy)
        .env("AM_HOME", home.path())
        .args(["identity", "generate", "--name", "alice"])
        .assert()
        .success();
    assert!(home.path().join("identities").join("alice.nsec").is_file());

    am_cmd(&decoy)
        .env("AM_HOME", home.path())
        .args(["relay", "add", "wss://relay.damus.io"])
        .assert()
        .success();
    let config = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(config.contains("relay.damus.io"));

    am_cmd(&decoy)
        .env("AM_HOME", home.path())
        .args(["identity", "show", "--name", "alice"])
        .assert()
        .success();

    assert_decoy_untouched(&decoy);
}

#[test]
fn home_flag_isolates_state() {
    let home = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();
    let home_arg = home.path().to_str().unwrap();

    am_cmd(&decoy)
        .args(["--home", home_arg, "identity", "generate"])
        .assert()
        .success();
    assert!(
        home.path()
            .join("identities")
            .join("default.nsec")
            .is_file()
    );

    // Global flag also works after the subcommand.
    am_cmd(&decoy)
        .args(["relay", "add", "wss://nos.lol", "--home", home_arg])
        .assert()
        .success();
    assert!(home.path().join("config.toml").is_file());

    assert_decoy_untouched(&decoy);
}

#[test]
fn home_flag_overrides_am_home_env() {
    let flag_home = TempDir::new().unwrap();
    let env_home = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();

    am_cmd(&decoy)
        .env("AM_HOME", env_home.path())
        .args(["--home", flag_home.path().to_str().unwrap()])
        .args(["identity", "generate"])
        .assert()
        .success();

    assert!(
        flag_home
            .path()
            .join("identities")
            .join("default.nsec")
            .is_file()
    );
    assert!(!env_home.path().join("identities").exists());
}
