//! `apb connector` CLI tests (Task 14). Spawns the real `apb` binary against a
//! per-test temp `APB_CONFIG_DIR`/`HOME`, mirroring `profile_cli_test.rs`'s
//! pattern: the child process needs no lock since each test gets its own
//! directories.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Runs `apb <args>` with a fresh per-test config dir and no inherited run
/// context (`APB_RUN_DIR`/`APB_NODE_ID` explicitly removed so a developer's
/// shell state can never leak into the "call without run context" case).
fn playbook(dir: &Path, args: &[&str]) -> std::process::Output {
    playbook_env(dir, args, &[])
}

/// Like `playbook`, but with extra environment variables set on the child
/// (e.g. the scaffold's secret env var).
fn playbook_env(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_apb"));
    cmd.args(args)
        .current_dir(dir)
        .env("APB_CONFIG_DIR", dir.join("cfg"))
        .env("HOME", dir.join("home"))
        .env("APB_NO_REGISTRY", "1")
        .env_remove("APB_RUN_DIR")
        .env_remove("APB_NODE_ID");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// Like `playbook`, but requires a zero exit code; stderr is shown on
/// failure so a failing setup step isn't masked as empty stdout.
fn apb_ok(dir: &Path, args: &[&str]) -> std::process::Output {
    let out = playbook(dir, args);
    assert!(
        out.status.success(),
        "`apb {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn setup(dir: &Path) {
    fs::create_dir_all(dir.join("cfg")).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    apb_ok(dir, &["init"]);
}

fn write_widget_account(dir: &Path, var: &str) {
    let path = dir.join(".apb/connector-config/widget.yaml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "accounts:\n  - name: default\n    base_url: https://example.com\n    token: \"{{{{env.{var}}}}}\"\n"
        ),
    )
    .unwrap();
}

// --- init ---------------------------------------------------------------

#[test]
fn init_scaffolds_a_connector_that_loads_and_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    apb_ok(dir.path(), &["connector", "init", "widget"]);
    let cfg_dir = dir.path().join("cfg/connectors/widget");
    assert!(cfg_dir.join("connector.yaml").is_file());
    assert!(cfg_dir.join("PUBLIC.md").is_file());

    // A second init on the same name must refuse rather than overwrite.
    let out = playbook(dir.path(), &["connector", "init", "widget"]);
    assert!(
        !out.status.success(),
        "second init on an existing connector must fail"
    );
}

// --- init + doctor --------------------------------------------------------

#[test]
fn init_then_doctor_reports_no_errors_when_token_is_set() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    let out = playbook_env(
        dir.path(),
        &["connector", "doctor"],
        &[("WIDGET_TOKEN", "shh-secret-value")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "doctor must report no errors once the token is set: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !stdout.contains("[fail]"),
        "doctor output should have no failing checks: {stdout}"
    );
    assert!(
        stdout.contains("widget"),
        "doctor output missing widget: {stdout}"
    );
}

// --- call without run context --------------------------------------------

#[test]
fn call_without_run_context_prints_config_error_json_and_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);

    let out = playbook(dir.path(), &["connector", "call", "widget", "ping"]);
    assert!(
        !out.status.success(),
        "call without run context must exit non-zero"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}"));
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["error"]["code"], serde_json::json!("config"));
    let msg = v["error"]["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("APB_RUN_DIR"),
        "message missing APB_RUN_DIR: {msg}"
    );
    assert!(
        msg.contains("APB_NODE_ID"),
        "message missing APB_NODE_ID: {msg}"
    );
}

// --- env ------------------------------------------------------------------

#[test]
fn env_lists_the_scaffold_token_var_when_unset() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    let out = apb_ok(dir.path(), &["connector", "env", "widget"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l == "WIDGET_TOKEN="),
        "env output missing WIDGET_TOKEN=: {stdout}"
    );
}

#[test]
fn env_omits_a_var_once_it_resolves() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    let out = playbook_env(
        dir.path(),
        &["connector", "env", "widget"],
        &[("WIDGET_TOKEN", "value")],
    );
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("WIDGET_TOKEN"),
        "a resolved var must not be listed: {stdout}"
    );
}

// --- list -------------------------------------------------------------

#[test]
fn list_shows_scaffold_as_unapproved_with_zero_accounts() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);

    let out = apb_ok(dir.path(), &["connector", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("widget"), "list missing widget: {stdout}");
    assert!(
        stdout.contains("unapproved"),
        "list missing trust state: {stdout}"
    );
}

// --- show ---------------------------------------------------------------

#[test]
fn show_reports_functions_and_account_fields() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    let out = apb_ok(dir.path(), &["connector", "show", "widget"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["name"], serde_json::json!("widget"));
    let functions = v["functions"].as_array().unwrap();
    assert!(functions.iter().any(|f| f["name"] == "get_item"));
    assert!(functions.iter().any(|f| f["name"] == "ping"));
    let account_fields = v["account_fields"].as_array().unwrap();
    assert!(account_fields.iter().any(|f| f["name"] == "base_url"));
    assert!(
        account_fields
            .iter()
            .any(|f| f["name"] == "token" && f["secret"] == true)
    );
    let accounts = v["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    let env = accounts[0]["env"].as_array().unwrap();
    assert!(
        env.iter()
            .any(|e| e["field"] == "token" && e["var"] == "WIDGET_TOKEN"),
        "accounts[0].env missing token/WIDGET_TOKEN: {accounts:?}"
    );
    // Never the secret value itself.
    assert!(!stdout.contains("shh-secret-value"));
}
