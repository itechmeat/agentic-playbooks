//! MCP policy-gate coverage for connector trust (spec
//! 2026-07-18-connectors-design sections 6 step 1 and 7). Drives the public
//! `check_run` gate against a fixture connector installed under a temp
//! `APB_CONFIG_DIR` and a registered playbook that binds it, asserting the
//! connector/account refusals and the two permit maps.
//!
//! Env isolation mirrors the other suite modules: every test mutates
//! process-wide env (`APB_CONFIG_DIR`, the fixture's secret vars) under the
//! shared `common::env_lock`, with `VarGuard`s that restore the prior value on
//! drop so a mutation never races or leaks into another module.

use std::path::Path;

use apb_core::connector::config::{self, account_digest};
use apb_core::connector::resolve::resolve_playbook;
use apb_core::connector::store;
use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_mcp::policy::check_run;

use crate::common::env_lock as lock;

// --- env guards -----------------------------------------------------------

struct VarGuard {
    var: String,
    prior: Option<std::ffi::OsString>,
}
impl Drop for VarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(&self.var, v),
                None => std::env::remove_var(&self.var),
            }
        }
    }
}

fn set_var(var: &str, value: &str) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    VarGuard {
        var: var.to_string(),
        prior,
    }
}

fn clear_var(var: &str) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::remove_var(var);
    }
    VarGuard {
        var: var.to_string(),
        prior,
    }
}

// --- fixtures -------------------------------------------------------------

const CONNECTOR_NAME: &str = "mock-tracker";
const TOKEN_A: &str = "APB_TEST_CONN_TOKEN_A";
const TOKEN_B: &str = "APB_TEST_CONN_TOKEN_B";

/// Writes the fixture connector into `cfg/connectors/mock-tracker`.
fn write_fixture_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join(CONNECTOR_NAME);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        r#"
name: mock-tracker
version: 0.1.0
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: list_items
    description: List items
    read_only: true
    method: GET
    url: "{{account.base_url}}/items"
  - name: create_item
    description: Create an item
    method: POST
    url: "{{account.base_url}}/items"
    body: "{{args}}"
"#,
    )
    .unwrap();
}

/// Writes the project account config with two accounts, each referencing a
/// distinct secret env var. `acct1` is the one a node grants; `acct2` is a
/// merged-but-ungranted account that the permit map must still cover.
fn write_accounts(root: &Path, base_url_a: &str) {
    let path = config::project_config_path(root, CONNECTOR_NAME);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            r#"
accounts:
  - name: acct1
    base_url: {base_url_a}
    token: "{{{{env.{TOKEN_A}}}}}"
  - name: acct2
    base_url: https://second.example.com
    token: "{{{{env.{TOKEN_B}}}}}"
"#
        ),
    )
    .unwrap();
}

/// A registered playbook whose single agent node binds `mock-tracker` and
/// grants only `acct1`. The node carries no profile, so the profile-bundle
/// gate is a no-op and the connector gate is what the test exercises.
const PLAYBOOK_ID: &str = "conn-pb";
fn playbook_yaml() -> &'static str {
    r#"schema: 2
id: conn-pb
name: conn-pb
version: 1.0.0
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    connectors: [{ name: mock-tracker, accounts: [acct1] }]
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#
}

fn write_pb(root: &Path) {
    let vdir = root.join(".apb/playbooks").join(PLAYBOOK_ID).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), playbook_yaml()).unwrap();
    std::fs::write(
        root.join(".apb/playbooks")
            .join(PLAYBOOK_ID)
            .join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn approve_playbook() {
    let mut store = TrustStore::load();
    store
        .approve(
            &digest_str(playbook_yaml()),
            PLAYBOOK_ID,
            OriginKind::LocallyApproved,
        )
        .unwrap();
}

/// Approves the connector tree digest.
fn approve_connector() {
    let loaded = store::load(CONNECTOR_NAME).unwrap();
    let mut store = TrustStore::load();
    store
        .approve_kind(
            &loaded.digest,
            CONNECTOR_NAME,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
}

/// Approves every merged account's digest against the live config under `root`.
fn approve_accounts(root: &Path, pb: &Playbook) {
    let out = resolve_playbook(root, pb).unwrap();
    let mut store = TrustStore::load();
    for (name, resolved) in &out.connectors {
        for account in &resolved.accounts {
            store
                .approve_kind(
                    &account_digest(account),
                    &account_trust_id(name, &account.name),
                    Kind::ConnectorAccount,
                    OriginKind::LocallyApproved,
                )
                .unwrap();
        }
    }
}

fn wref() -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: PLAYBOOK_ID.into(),
        version: None,
    }
}

/// Full setup: fixture connector, two-account config, registered+approved
/// playbook, and both secret vars set. Returns the env guards (kept alive by
/// the caller) so they restore on drop. The connector/account trust is left
/// for each test to arrange.
fn setup(cfg: &Path, root: &Path, base_url_a: &str) -> Vec<VarGuard> {
    let guards = vec![
        set_var("APB_CONFIG_DIR", &cfg.to_string_lossy()),
        set_var(TOKEN_A, "secret-a"),
        set_var(TOKEN_B, "secret-b"),
    ];
    init_project(root).unwrap();
    write_fixture_connector(cfg);
    write_accounts(root, base_url_a);
    write_pb(root);
    approve_playbook();
    guards
}

fn parsed_playbook() -> Playbook {
    Playbook::from_yaml(playbook_yaml()).unwrap()
}

// --- tests ----------------------------------------------------------------

#[test]
fn unapproved_connector_refused_even_with_acknowledge() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path(), "https://first.example.com");
    // The connector digest is NOT approved. acknowledge_untrusted must NOT
    // bypass connector trust (it guards secret egress).

    let refusal = check_run(root.path(), &wref(), true, false).unwrap_err();
    assert_eq!(refusal["policy"], "untrusted_connector_requires_approve");
    let connectors = refusal["connectors"].as_array().expect("connectors list");
    assert!(
        connectors.iter().any(|c| c == CONNECTOR_NAME),
        "names the untrusted connector: {refusal}"
    );
    assert!(
        refusal["detail"]
            .as_str()
            .is_some_and(|d| d.contains("acknowledge_untrusted")),
        "detail explains acknowledge does not bypass: {refusal}"
    );
}

#[test]
fn approving_connector_digest_passes_connector_check() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path(), "https://first.example.com");
    approve_connector();
    // Accounts are still unapproved: the gate moves PAST the connector check
    // and refuses on the account instead, proving the connector check passed.

    let refusal = check_run(root.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "unapproved_connector_account");
    let accounts = refusal["accounts"].as_array().expect("accounts list");
    assert!(
        accounts.iter().any(|a| a == "mock-tracker/acct1"),
        "names the unapproved account: {refusal}"
    );
    // The display fields let the user see what they approve (raw env ref kept).
    let fields = &refusal["fields"]["mock-tracker/acct1"];
    assert_eq!(fields["fields"]["base_url"], "https://first.example.com");
}

#[test]
fn changed_account_field_refuses() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path(), "https://first.example.com");
    approve_connector();
    approve_accounts(root.path(), &parsed_playbook());
    // Redirect acct1 to another host AFTER approval: the account digest drifts
    // and trust no longer applies (spec 7: a shared-config edit must re-approve).
    write_accounts(root.path(), "https://evil.example.com");

    let refusal = check_run(root.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "unapproved_connector_account");
    let accounts = refusal["accounts"].as_array().expect("accounts list");
    assert!(
        accounts.iter().any(|a| a == "mock-tracker/acct1"),
        "names the changed account: {refusal}"
    );
    assert_eq!(
        refusal["fields"]["mock-tracker/acct1"]["fields"]["base_url"], "https://evil.example.com",
        "shows the new field value: {refusal}"
    );
}

#[test]
fn fully_approved_yields_permit_with_matching_maps() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path(), "https://first.example.com");
    approve_connector();
    approve_accounts(root.path(), &parsed_playbook());

    let permit = check_run(root.path(), &wref(), false, false).expect("permit");

    // Connector map: name -> live tree digest.
    let loaded = store::load(CONNECTOR_NAME).unwrap();
    assert_eq!(
        permit.connectors.get(CONNECTOR_NAME),
        Some(&loaded.digest),
        "connector map matches the store digest"
    );

    // Account map covers EVERY merged account (acct1 granted, acct2 not), each
    // keyed `connector/account` with the account_digest of the merged account.
    let out = resolve_playbook(root.path(), &parsed_playbook()).unwrap();
    let resolved = out.connectors.get(CONNECTOR_NAME).unwrap();
    assert_eq!(resolved.accounts.len(), 2, "two merged accounts");
    for account in &resolved.accounts {
        let key = account_trust_id(CONNECTOR_NAME, &account.name);
        assert_eq!(
            permit.connector_accounts.get(&key),
            Some(&account_digest(account)),
            "account map covers `{key}` with its digest"
        );
    }
    assert!(
        permit.connector_accounts.contains_key("mock-tracker/acct2"),
        "covers the merged-but-ungranted account"
    );
}

#[test]
fn missing_env_var_refuses() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut guards = setup(cfg.path(), root.path(), "https://first.example.com");
    approve_connector();
    approve_accounts(root.path(), &parsed_playbook());
    // Drop the token vars set by `setup` so no account's secret var resolves
    // (process env, project dotenv, global dotenv all miss).
    guards.push(clear_var(TOKEN_A));
    guards.push(clear_var(TOKEN_B));

    let refusal = check_run(root.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "connector_env_missing");
    let missing = refusal["missing"].as_array().expect("missing list");
    assert!(
        missing.iter().any(|m| m == TOKEN_A) && missing.iter().any(|m| m == TOKEN_B),
        "lists the unresolved env vars: {refusal}"
    );
}

#[cfg(unix)]
#[test]
fn cmd_secret_passes_env_gate_without_executing_command() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path(), "https://first.example.com");

    // A stub that touches a sentinel iff it ever runs.
    use std::os::unix::fs::PermissionsExt;
    let sentinel = stubs.path().join("ran.marker");
    let stub = stubs.path().join("cmd-token");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\ntouch '{}'\nprintf 'x\\n'\n", sentinel.display()),
    )
    .unwrap();
    let mut p = std::fs::metadata(&stub).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&stub, p).unwrap();

    // Rewrite acct1 to source its token from the command (no env var).
    let path = config::project_config_path(root.path(), CONNECTOR_NAME);
    std::fs::write(
        &path,
        format!(
            "accounts:\n  - name: acct1\n    base_url: https://first.example.com\n    token: \"{{{{cmd:{}}}}}\"\n  - name: acct2\n    base_url: https://second.example.com\n    token: \"{{{{env.{TOKEN_B}}}}}\"\n",
            stub.to_string_lossy()
        ),
    )
    .unwrap();

    approve_connector();
    approve_accounts(root.path(), &parsed_playbook());

    // The gate must not report a missing env var for the cmd-sourced acct1,
    // and must not execute the command. It permits (acct2's env var is set by
    // `setup`, and both accounts are approved after the rewrite).
    let result = check_run(root.path(), &wref(), false, false);
    assert!(result.is_ok(), "gate should permit: {result:?}");
    assert!(
        !sentinel.exists(),
        "the policy gate must never execute a secret command"
    );
}
