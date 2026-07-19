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

#[test]
fn env_write_creates_secrets_file_and_gitignores_it() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    // The token is unset, so `--write` templates it into a fresh secrets.env.
    let out = apb_ok(dir.path(), &["connector", "env", "widget", "--write"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("WIDGET_TOKEN="),
        "write output should name the appended key: {stdout}"
    );

    let secrets = dir.path().join(".apb/secrets.env");
    let body = fs::read_to_string(&secrets).expect("secrets.env should exist");
    assert!(
        body.lines().any(|l| l == "WIDGET_TOKEN="),
        "secrets.env missing the templated key: {body}"
    );

    // The project .gitignore now covers the secrets file.
    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap_or_default();
    assert!(
        gitignore.lines().any(|l| l.trim() == ".apb/secrets.env"),
        ".gitignore should cover .apb/secrets.env: {gitignore}"
    );

    // The dotenv is created private (0600) so a token value is not world- or
    // group-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&secrets).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets.env must be mode 0600, was {mode:o}");
    }
}

#[test]
fn env_write_is_idempotent_and_preserves_existing_lines() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    // A pre-existing, unrelated line must survive the append untouched.
    let secrets = dir.path().join(".apb/secrets.env");
    fs::create_dir_all(secrets.parent().unwrap()).unwrap();
    fs::write(&secrets, "OTHER=keepme\n").unwrap();

    apb_ok(dir.path(), &["connector", "env", "widget", "--write"]);
    let after_first = fs::read_to_string(&secrets).unwrap();
    assert!(
        after_first.contains("OTHER=keepme"),
        "the unrelated line was dropped: {after_first}"
    );
    assert!(
        after_first.lines().any(|l| l == "WIDGET_TOKEN="),
        "the missing key was not appended: {after_first}"
    );

    // A second `--write` appends nothing: WIDGET_TOKEN is still unresolved but
    // already sits in the file (empty value), so it is not duplicated.
    apb_ok(dir.path(), &["connector", "env", "widget", "--write"]);
    let after_second = fs::read_to_string(&secrets).unwrap();
    assert_eq!(
        after_first, after_second,
        "a second --write must not change the file"
    );
    assert_eq!(
        after_second
            .lines()
            .filter(|l| l.starts_with("WIDGET_TOKEN="))
            .count(),
        1,
        "WIDGET_TOKEN must appear exactly once: {after_second}"
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

// --- approve --------------------------------------------------------------

#[test]
fn approve_marks_connector_then_account_as_trusted() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    write_widget_account(dir.path(), "WIDGET_TOKEN");

    // Before approval the connector lists as unapproved.
    let before = apb_ok(dir.path(), &["connector", "list"]);
    let before_out = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_out.contains("unapproved"),
        "connector should start unapproved: {before_out}"
    );

    // Approve the connector tree digest.
    let ca = apb_ok(dir.path(), &["connector", "approve", "widget"]);
    assert!(
        String::from_utf8_lossy(&ca.stdout).contains("approved connector `widget`"),
        "approve should confirm the connector"
    );
    let after = apb_ok(dir.path(), &["connector", "list"]);
    let after_out = String::from_utf8_lossy(&after.stdout);
    assert!(
        after_out.contains("approved") && !after_out.contains("unapproved"),
        "connector should list as approved after approval: {after_out}"
    );

    // Approve one account: the CLI prints the non-secret fields approved and
    // never the secret value.
    let aa = apb_ok(
        dir.path(),
        &["connector", "approve", "widget", "--account", "default"],
    );
    let aa_out = String::from_utf8_lossy(&aa.stdout);
    let v: serde_json::Value = serde_json::from_str(aa_out.trim())
        .unwrap_or_else(|e| panic!("account approve not JSON ({e}): {aa_out}"));
    assert_eq!(v["approved"], serde_json::json!("widget/default"));
    assert_eq!(
        v["fields"]["base_url"],
        serde_json::json!("https://example.com")
    );
    // The token field keeps its raw env ref, never a resolved secret value.
    assert_eq!(
        v["fields"]["token"],
        serde_json::json!("{{env.WIDGET_TOKEN}}")
    );

    // An unknown account is a clean error, not a panic.
    let bad = playbook(
        dir.path(),
        &["connector", "approve", "widget", "--account", "nope"],
    );
    assert!(!bad.status.success(), "unknown account must fail");
}

// --- run: connector permit gate (final-review fix) -------------------------
//
// Task 19 review follow-up: `apb run`'s two `RunOptions` sites (foreground and
// the `__drive-supervised` child) used to pass empty `expected_connectors`/
// `expected_connector_accounts` unconditionally, so ANY connector-binding
// playbook was refused by the engine with the opaque "connector bindings
// present but no connector permit" message - even via the CLI, which is
// supposed to behave exactly like the MCP and dashboard run paths (both of
// which already ran the gate). These tests exercise the now-fixed foreground
// `apb run` path end to end: an unapproved connector-binding playbook must
// refuse with an actionable message; once approved (via `apb connector
// approve`, exactly as a user would), the run must get past the connector
// gate and actually succeed.

const RUN_PLAYBOOK_ID: &str = "conn-pb";

/// A registered playbook whose single agent node binds the `widget` connector
/// (scaffolded by `connector init`) and an executor profile `main`.
fn run_playbook_yaml() -> String {
    format!(
        r#"schema: 2
id: {RUN_PLAYBOOK_ID}
name: {RUN_PLAYBOOK_ID}
version: 1.0.0
nodes:
  - {{ id: s, type: start }}
  - id: a
    type: agent_task
    prompt: hi
    profile: main
    connectors: [{{ name: widget, accounts: [default] }}]
  - {{ id: f, type: finish, outcome: success }}
edges:
  - {{ from: s, to: a }}
  - {{ from: a, to: f }}
"#
    )
}

fn write_run_playbook(dir: &Path) {
    let vdir = dir
        .join(".apb/playbooks")
        .join(RUN_PLAYBOOK_ID)
        .join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), run_playbook_yaml()).unwrap();
    fs::write(
        dir.join(".apb/playbooks")
            .join(RUN_PLAYBOOK_ID)
            .join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Seeds a minimal project-scope profile `main` (agent `claude`, so
/// `APB_AGENT_CMD` overrides it to the test's stub script regardless of the
/// configured agent name - see `adapter_for`).
fn seed_profile_main(dir: &Path) {
    let pdir = dir.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "test soul").unwrap();
}

/// A stub agent binary: prints a single line and exits 0, enough for the
/// engine to consider the node complete (mirrors
/// `stdio_profile_e2e_test.rs`'s `make_stub`).
fn make_stub_agent(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("stub-agent.sh");
    fs::write(&path, "#!/bin/sh\necho done\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// Common setup for the run-gate tests: project + scaffolded `widget`
/// connector with its `default` account referencing `WIDGET_TOKEN`, plus the
/// connector-binding playbook and its executor profile. Connector/account
/// trust is left for each test to arrange.
fn setup_run_fixture(dir: &Path) {
    setup(dir);
    apb_ok(dir, &["connector", "init", "widget"]);
    write_widget_account(dir, "WIDGET_TOKEN");
    write_run_playbook(dir);
    seed_profile_main(dir);
}

#[test]
fn run_refuses_unapproved_connector_binding_playbook_with_actionable_message() {
    let dir = tempfile::tempdir().unwrap();
    setup_run_fixture(dir.path());
    // The secret resolves (so the env-presence check passes and the gate
    // actually reaches the trust step); connector/account trust is
    // deliberately left unapproved.
    let out = playbook_env(
        dir.path(),
        &["run", RUN_PLAYBOOK_ID],
        &[("WIDGET_TOKEN", "shh-secret-value")],
    );
    assert!(
        !out.status.success(),
        "an unapproved connector-binding playbook must refuse to run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("untrusted_connector_requires_approve"),
        "stderr should name the refusal policy code: {stderr}"
    );
    assert!(
        stderr.contains("apb connector approve"),
        "stderr should point at `apb connector approve`: {stderr}"
    );
    assert!(
        !stderr.contains("connector bindings present but no connector permit"),
        "the fix must replace the opaque engine message with an actionable one: {stderr}"
    );
}

#[test]
fn run_proceeds_past_the_connector_gate_once_approved() {
    let dir = tempfile::tempdir().unwrap();
    setup_run_fixture(dir.path());
    let stub = make_stub_agent(dir.path());

    // Approve the connector tree digest and the `default` account's digest,
    // exactly as a user would via `apb connector approve`.
    apb_ok(dir.path(), &["connector", "approve", "widget"]);
    apb_ok(
        dir.path(),
        &["connector", "approve", "widget", "--account", "default"],
    );

    let out = playbook_env(
        dir.path(),
        &["run", RUN_PLAYBOOK_ID],
        &[
            ("WIDGET_TOKEN", "shh-secret-value"),
            ("APB_AGENT_CMD", stub.to_str().unwrap()),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("connector bindings present but no connector permit"),
        "the connector-permit refusal must be gone once approved: stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stderr.contains("untrusted_connector_requires_approve"),
        "the connector trust refusal must be gone once approved: stderr={stderr}"
    );
    assert!(
        out.status.success() && stdout.contains("succeeded"),
        "an approved connector-binding playbook should run its node to completion: \
         stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn call_accepts_the_full_flag() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "init", "widget"]);
    // No run context, so this still exits config-error, but --full must parse.
    let out = playbook(
        dir.path(),
        &["connector", "call", "widget", "ping", "--full"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["error"]["code"], serde_json::json!("config"));
}

// --- install --------------------------------------------------------------

#[test]
fn install_embedded_example_records_trust_and_lists_approved() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    let out = apb_ok(dir.path(), &["connector", "install", "example"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("installed connector `example`"),
        "install should confirm: {stdout}"
    );
    let cfg = dir.path().join("cfg/connectors/example");
    assert!(cfg.join("connector.yaml").is_file());
    assert!(cfg.join("tests.yaml").is_file());

    // Embedded install seeds trust: the connector lists as approved.
    let list = apb_ok(dir.path(), &["connector", "list"]);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("example") && list_out.contains("approved"),
        "installed connector should list as approved: {list_out}"
    );
}

#[test]
fn install_same_digest_is_a_noop_and_differing_refuses_without_force() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());
    apb_ok(dir.path(), &["connector", "install", "example"]);

    // Re-install: same digest, a no-op that still succeeds.
    let again = apb_ok(dir.path(), &["connector", "install", "example"]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("already installed"));

    // Mutate the installed folder so it differs from the embedded version.
    let manifest = dir.path().join("cfg/connectors/example/connector.yaml");
    let mut body = fs::read_to_string(&manifest).unwrap();
    body.push_str("# local edit\n");
    fs::write(&manifest, body).unwrap();

    let refused = playbook(dir.path(), &["connector", "install", "example"]);
    assert!(!refused.status.success(), "differing target must refuse");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--force"),
        "refusal should point at --force"
    );

    // --force overwrites and restores the embedded content.
    apb_ok(dir.path(), &["connector", "install", "example", "--force"]);
    let restored = fs::read_to_string(&manifest).unwrap();
    assert!(!restored.contains("# local edit"), "force should overwrite");
}

#[test]
fn install_from_dir_installs_without_recording_trust() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    // A local connector folder named `widget` (basename is the connector name).
    let src = dir.path().join("src/widget");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("connector.yaml"),
        "name: widget\nversion: 0.1.0\nfunctions:\n  - name: ping\n    description: d\n    mock: { status: 200, body: { ok: true } }\n",
    )
    .unwrap();

    let out = apb_ok(
        dir.path(),
        &["connector", "install", "--from-dir", src.to_str().unwrap()],
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("installed connector `widget`"));
    assert!(
        dir.path()
            .join("cfg/connectors/widget/connector.yaml")
            .is_file()
    );

    // No trust recorded: the connector lists as unapproved.
    let list = apb_ok(dir.path(), &["connector", "list"]);
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("unapproved"),
        "--from-dir must not seed trust"
    );
}

// --- list available section ------------------------------------------------

#[test]
fn list_shows_embedded_available_section_before_install_and_hides_after() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    // Before install: `example` appears under the available section.
    let before = apb_ok(dir.path(), &["connector", "list"]);
    let before_out = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_out.contains("AVAILABLE") && before_out.contains("example"),
        "available section should list the embedded example: {before_out}"
    );

    apb_ok(dir.path(), &["connector", "install", "example"]);

    // After install: `example` is an installed row, no longer under available.
    let after = apb_ok(dir.path(), &["connector", "list"]);
    let after_out = String::from_utf8_lossy(&after.stdout);
    let available_block = after_out.split("AVAILABLE").nth(1).unwrap_or("");
    assert!(
        !available_block.contains("example"),
        "installed connector must not appear under available: {after_out}"
    );
}

// --- test -----------------------------------------------------------------

#[test]
fn test_runs_embedded_example_cases_and_passes() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    // Embedded connector, not installed: `test` resolves it from the binary.
    let out = apb_ok(dir.path(), &["connector", "test", "example"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[pass] ping"),
        "ping case should pass: {stdout}"
    );
    assert!(
        stdout.contains("[pass] get_item"),
        "get_item case should pass: {stdout}"
    );
    assert!(
        stdout.contains("[pass] create_item"),
        "create_item should pass: {stdout}"
    );
}

#[test]
fn test_dir_with_a_failing_case_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path());

    let src = dir.path().join("src/widget");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("connector.yaml"),
        "name: widget\nversion: 0.1.0\naccount_fields:\n  - name: api_base\n    required: true\nfunctions:\n  - name: get_item\n    description: d\n    read_only: true\n    method: GET\n    url: \"{{account.api_base}}/items/{{args.id}}\"\n    args_schema: { type: object, properties: { id: { type: string } }, required: [id] }\n",
    )
    .unwrap();
    fs::write(
        src.join("tests.yaml"),
        "cases:\n  - function: get_item\n    account: { api_base: https://api.example.com }\n    args: { id: \"42\" }\n    expect: { method: GET, url: https://api.example.com/items/WRONG }\n",
    )
    .unwrap();

    let out = playbook(
        dir.path(),
        &["connector", "test", "--dir", src.to_str().unwrap()],
    );
    assert!(!out.status.success(), "a failing case must exit non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[fail] get_item"),
        "should report the failing case: {stdout}"
    );
}
