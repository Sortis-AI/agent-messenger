// Messaging integration tests require a running relay.
// These are placeholders for Phase 4/5 when send/listen are fully wired.

#[test]
#[ignore = "requires relay connection"]
fn send_and_receive_message() {
    // TODO: Phase 5 — test A→B send, B listen, verify content
}

#[test]
fn nip04_rejects_multiple_recipients() {
    let data = tempfile::TempDir::new().unwrap();
    let config = tempfile::TempDir::new().unwrap();
    let am = |args: &[&str]| {
        let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("am");
        cmd.env_remove("AM_HOME")
            .env("XDG_DATA_HOME", data.path())
            .env("XDG_CONFIG_HOME", config.path())
            .args(args);
        cmd
    };
    am(&["identity", "generate"]).assert().success();
    am(&["relay", "add", "--no-publish", "wss://relay.damus.io"])
        .assert()
        .success();

    let a = "npub1quyp6jcvvs7ql363udpwaseagux333v09x704zrwzqyszhkvjwtsd07uh0";
    let b = "npub16zedugfn6kmp3u295qap24p6d4g3wx562wd5up7396ffcde87w4swmvzme";
    am(&[
        "send",
        "--to",
        a,
        "--to",
        b,
        "--reply-protocol",
        "nip04",
        "hi",
    ])
    .assert()
    .code(2);
}

const A: &str = "npub1quyp6jcvvs7ql363udpwaseagux333v09x704zrwzqyszhkvjwtsd07uh0";

/// An `am` command isolated in a throwaway AM_HOME (no identity, no relays):
/// everything below must be rejected before keys or the network are touched.
fn am_isolated(home: &tempfile::TempDir) -> assert_cmd::Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("am");
    cmd.env("AM_HOME", home.path())
        .env_remove("AM_PASSPHRASE")
        .env_remove("NO_DNA");
    cmd
}

fn file_args() -> Vec<&'static str> {
    vec![
        "--file-url",
        "https://files.example/abc",
        "--file-type",
        "image/png",
        "--decryption-key",
        "k",
        "--decryption-nonce",
        "n",
        "--file-hash",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ]
}

#[test]
fn file_url_requires_the_other_file_fields() {
    let home = tempfile::TempDir::new().unwrap();
    am_isolated(&home)
        .args([
            "send",
            "--to",
            A,
            "--file-url",
            "https://f",
            "--file-type",
            "image/png",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--decryption-key"));
}

#[test]
fn file_field_without_file_url_is_refused() {
    let home = tempfile::TempDir::new().unwrap();
    am_isolated(&home)
        .args(["send", "--to", A, "--file-type", "image/png", "hi"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--file-url"));
}

#[test]
fn file_and_text_message_are_mutually_exclusive() {
    let home = tempfile::TempDir::new().unwrap();
    let mut args = vec!["send", "--to", A];
    args.extend(file_args());
    args.push("some text");
    am_isolated(&home)
        .args(&args)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("cannot be used with"));
}

#[test]
fn nip04_with_file_is_refused() {
    let home = tempfile::TempDir::new().unwrap();
    let mut args = vec!["send", "--to", A, "--reply-protocol", "nip04"];
    args.extend(file_args());
    am_isolated(&home)
        .args(&args)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("NIP-17 only"));
}

#[test]
fn bad_file_hash_is_refused() {
    let home = tempfile::TempDir::new().unwrap();
    let mut args = vec!["send", "--to", A];
    args.extend(file_args());
    let last = args.len() - 1;
    args[last] = "not-a-sha256";
    am_isolated(&home)
        .args(&args)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--file-hash"));
}

#[test]
fn invalid_reply_to_is_refused() {
    let home = tempfile::TempDir::new().unwrap();
    am_isolated(&home)
        .args(["send", "--to", A, "--reply-to", "zzz", "hi"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--reply-to"));
}

#[test]
fn complete_file_args_pass_validation() {
    // Valid file args get past argument checks and fail later for the
    // missing identity (exit 1/4/5, not 2).
    let home = tempfile::TempDir::new().unwrap();
    let mut args = vec!["send", "--to", A];
    args.extend(file_args());
    let out = am_isolated(&home).args(&args).output().unwrap();
    assert!(!out.status.success());
    assert_ne!(out.status.code(), Some(2), "{out:?}");
}
