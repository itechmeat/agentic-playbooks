//! Engine run-start connector snapshot (spec 2026-07-18-connectors-design
//! section 6): `snapshot_connectors` re-resolves the playbook's connectors,
//! verifies the permit maps verbatim, checks every required env var resolves,
//! copies each used `connector.yaml` into `runs/<id>/connectors/`, and returns
//! the manifest pieces. These tests drive that function directly against a
//! fixture connector installed under a temp `APB_CONFIG_DIR`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::connector::config::account_digest;
use apb_core::connector::resolve::resolve_playbook;
use apb_core::schema::Playbook;
use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, ConnectorEnvPolicy};
use apb_engine::connector_run::snapshot_connectors;
use apb_engine::invocation::builtin;

use crate::common;

/// Restores one process-wide env var to its prior value on drop. Used under
/// the shared `common::env_lock()` so a mutation never races another module.
struct VarGuard {
    var: &'static str,
    prior: Option<OsString>,
}
impl Drop for VarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }
}

fn set_var(var: &'static str, value: &Path) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    VarGuard { var, prior }
}

fn set_str(var: &'static str, value: &str) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    VarGuard { var, prior }
}

fn clear_var(var: &'static str) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::remove_var(var);
    }
    VarGuard { var, prior }
}

/// Writes the `mock-tracker` fixture connector into `cfg`'s connectors dir:
/// one read-only function, one write function, and a secret `token` account
/// field. Mirrors the fixture in `apb-core`'s resolve tests.
fn write_fixture_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join("mock-tracker");
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

fn write_project_account(root: &Path, yaml: &str) {
    let path = apb_core::connector::config::project_config_path(root, "mock-tracker");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, yaml).unwrap();
}

const ACCOUNT_YAML: &str = r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.MOCK_TOKEN}}"
"#;

/// A playbook whose single agent node binds `mock-tracker` with the
/// `read_only` shorthand.
fn bound_playbook() -> Playbook {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    profile: x
    connectors: [{ name: mock-tracker, functions: read_only }]
edges: []
"#;
    Playbook::from_yaml(yaml).unwrap()
}

/// Builds the two permit maps (connector digest, account digests) from a live
/// resolution - what a correct policy gate would hand the engine verbatim.
fn expected_maps(
    root: &Path,
    pb: &Playbook,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let out = resolve_playbook(root, pb).unwrap();
    let mut connectors = BTreeMap::new();
    let mut accounts = BTreeMap::new();
    for (name, resolved) in &out.connectors {
        connectors.insert(name.clone(), resolved.loaded.digest.clone());
        for a in &resolved.accounts {
            accounts.insert(format!("{name}/{}", a.name), account_digest(a));
        }
    }
    (connectors, accounts)
}

/// Writes an executable stub agent that dumps its environment to stdout, so a
/// test can inspect exactly what the spawned agent inherited.
fn env_dump_agent(dir: &Path) -> String {
    let path = dir.join("env-dump.sh");
    common::write_sync(&path, "#!/bin/sh\nenv\n");
    let mut perm = std::fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn adapter_scrubs_connector_env_and_injects_run_context() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    // A connector token present in the parent process environment: without
    // scrubbing the spawned agent would inherit it.
    let _token = set_str("MOCK_SCRUB_TOKEN", "super-secret");

    let ad = ClaudeAdapter {
        program: env_dump_agent(dir.path()),
        spec: builtin("claude").unwrap(),
    };
    let policy = ConnectorEnvPolicy {
        scrub: vec!["MOCK_SCRUB_TOKEN".to_string()],
        run_dir: Some(dir.path().to_path_buf()),
        node_id: Some("node-a".to_string()),
    };
    let report = ad
        .run(&AgentTask {
            prompt: "hi",
            model: "haiku",
            workdir: dir.path(),
            timeout: None,
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            connector_policy: &policy,
            interactive: false,
            node: "test",
        })
        .unwrap();

    // The scrubbed connector token never reaches the agent's environment.
    assert!(
        !report.raw.contains("MOCK_SCRUB_TOKEN"),
        "scrubbed connector var leaked into the agent env: {}",
        report.raw
    );
    // The run-context env the connector-call child reads is present.
    assert!(
        report.raw.contains("APB_RUN_DIR="),
        "APB_RUN_DIR missing from the agent env: {}",
        report.raw
    );
    assert!(
        report.raw.contains("APB_NODE_ID=node-a"),
        "APB_NODE_ID missing or wrong in the agent env: {}",
        report.raw
    );
}

#[test]
fn snapshot_writes_connector_yaml_and_returns_expanded_grants() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _token = set_str("MOCK_TOKEN", "shh");

    write_fixture_connector(cfg.path());
    write_project_account(root.path(), ACCOUNT_YAML);

    let pb = bound_playbook();
    let (expected_connectors, expected_accounts) = expected_maps(root.path(), &pb);

    let (connectors, grants) = snapshot_connectors(
        root.path(),
        run_dir.path(),
        &pb,
        &expected_connectors,
        &expected_accounts,
    )
    .unwrap();

    // The raw connector.yaml is copied verbatim into runs/<id>/connectors/.
    let copied = run_dir.path().join("connectors").join("mock-tracker.yaml");
    assert!(
        copied.is_file(),
        "connector.yaml was not copied to {copied:?}"
    );
    let raw = std::fs::read_to_string(&copied).unwrap();
    assert!(raw.contains("name: mock-tracker"));
    assert!(raw.contains("list_items"));

    // One connector in the manifest, digest matches the permit.
    assert_eq!(connectors.len(), 1);
    assert_eq!(connectors[0].name, "mock-tracker");
    assert_eq!(connectors[0].digest, expected_connectors["mock-tracker"]);
    assert_eq!(connectors[0].accounts.len(), 1);
    let acct = &connectors[0].accounts[0];
    assert_eq!(acct.name, "acct1");
    // The secret field keeps its raw {{env.VAR}} ref; env maps field -> var name.
    assert_eq!(acct.fields["token"], "{{env.MOCK_TOKEN}}");
    assert_eq!(acct.env["token"], "MOCK_TOKEN");

    // The grant for node `a` has the read_only shorthand expanded to the one
    // read-only function, and the account listed by name.
    let node_grants = grants.get("a").expect("node `a` should have grants");
    assert_eq!(node_grants.len(), 1);
    assert_eq!(node_grants[0].connector, "mock-tracker");
    assert_eq!(node_grants[0].functions, vec!["list_items".to_string()]);
    assert_eq!(node_grants[0].accounts, vec!["acct1".to_string()]);
}

#[test]
fn tampered_connector_digest_fails_naming_the_connector() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _token = set_str("MOCK_TOKEN", "shh");

    write_fixture_connector(cfg.path());
    write_project_account(root.path(), ACCOUNT_YAML);

    let pb = bound_playbook();
    let (mut expected_connectors, expected_accounts) = expected_maps(root.path(), &pb);
    // Tamper the pinned digest: the live folder no longer matches the permit.
    expected_connectors.insert("mock-tracker".to_string(), "sha256:deadbeef".to_string());

    let err = snapshot_connectors(
        root.path(),
        run_dir.path(),
        &pb,
        &expected_connectors,
        &expected_accounts,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("mock-tracker"),
        "message should name the connector: {msg}"
    );
}

#[test]
fn missing_env_var_fails_naming_var_and_account() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run_dir = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    // The secret var is not set anywhere (process env, project/global dotenv).
    let _token = clear_var("MOCK_TOKEN");

    write_fixture_connector(cfg.path());
    write_project_account(root.path(), ACCOUNT_YAML);

    let pb = bound_playbook();
    let (expected_connectors, expected_accounts) = expected_maps(root.path(), &pb);

    let err = snapshot_connectors(
        root.path(),
        run_dir.path(),
        &pb,
        &expected_connectors,
        &expected_accounts,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("MOCK_TOKEN"),
        "message should name the missing var: {msg}"
    );
    assert!(
        msg.contains("acct1"),
        "message should name the account: {msg}"
    );
}
