//! `apb server key issue|list|revoke` against a temp global config dir,
//! driving the real binary the way the other CLI suites do. The config dir is
//! passed per spawn with `Command::env`, never by mutating this process's env
//! (other suites in this binary spawn concurrently).

use std::path::Path;
use std::process::Command;

fn apb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apb")
}

fn run(cfg: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(apb_bin())
        .args(["server"])
        .args(args)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .expect("run apb server");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn issue_list_revoke_cycle() {
    let cfg = tempfile::tempdir().unwrap();

    // Nothing issued yet: list says so without failing.
    let (stdout, _, ok) = run(cfg.path(), &["key", "list"]);
    assert!(ok, "an empty list is not an error");
    assert!(
        stdout.contains("no server keys"),
        "empty list must explain the state: {stdout}"
    );

    // Issue prints the key exactly once, plus its id.
    let (stdout, _, ok) = run(cfg.path(), &["key", "issue"]);
    assert!(ok, "issue must succeed: {stdout}");
    let key = stdout
        .lines()
        .find(|l| l.starts_with("apb_"))
        .expect("the key is printed on its own line")
        .to_string();
    assert_eq!(
        stdout.matches(&key).count(),
        1,
        "the key is printed once and only once: {stdout}"
    );
    assert!(
        stdout.contains("shown once"),
        "issue warns that the key is not recoverable: {stdout}"
    );
    assert!(!stdout.contains('!'), "no exclamation marks: {stdout}");

    // The list shows an id and a timestamp, never the key.
    let (stdout, _, ok) = run(cfg.path(), &["key", "list"]);
    assert!(ok);
    assert!(
        !stdout.contains(&key),
        "list must not echo the key: {stdout}"
    );
    let id = key_id(cfg.path());
    assert!(stdout.contains(&id), "list shows the id: {stdout}");

    // A second key is fine, a third is refused.
    assert!(
        run(cfg.path(), &["key", "issue"]).2,
        "a second key is allowed"
    );
    let (_, stderr, ok) = run(cfg.path(), &["key", "issue"]);
    assert!(!ok, "a third key must fail");
    assert!(
        stderr.contains("revoke"),
        "the refusal names the remedy: {stderr}"
    );

    // Revoke by id frees a slot again.
    assert!(
        run(cfg.path(), &["key", "revoke", &id]).2,
        "revoke succeeds"
    );
    assert!(run(cfg.path(), &["key", "issue"]).2, "a slot is free again");

    let (_, stderr, ok) = run(cfg.path(), &["key", "revoke", "deadbeef"]);
    assert!(!ok, "an unknown id must fail");
    assert!(stderr.contains("deadbeef"), "{stderr}");
}

/// The id of the first key in the store, read straight from the file.
fn key_id(cfg: &Path) -> String {
    let raw = std::fs::read_to_string(cfg.join("server-auth.yaml")).unwrap();
    let file: apb_core::server_auth::AuthFile = serde_yaml_ng::from_str(&raw).unwrap();
    file.keys[0].id.clone()
}

#[test]
fn json_listing_carries_ids_and_timestamps_only() {
    let cfg = tempfile::tempdir().unwrap();
    run(cfg.path(), &["key", "issue"]);
    let (stdout, _, ok) = run(cfg.path(), &["key", "list", "--json"]);
    assert!(ok, "{stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let keys = v["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1);
    assert!(keys[0]["id"].is_string());
    assert!(keys[0]["created_at"].is_string());
    assert!(
        keys[0].get("sha256").is_none(),
        "the stored hash is not part of the listing: {stdout}"
    );
}
