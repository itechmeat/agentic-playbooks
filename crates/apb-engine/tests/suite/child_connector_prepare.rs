//! Finding 2 of issue #42 (engine side): a gated sub-playbook child that binds
//! a connector must be preparable. The parent policy gate pins the child's
//! verified connector permit map on the `ChildExpectation`, and the child spawn
//! threads it verbatim into `RunOptions::expected_connectors` /
//! `expected_connector_accounts`. This module drives the ENGINE half: it feeds
//! those exact maps (the same ones a `ChildExpectation` carries) into the
//! child's preparation via `prepare_supervised_background` and asserts:
//!   * with the pinned map -> prepare accepts, the manifest snapshots the
//!     connector, and the run config carries the expected map;
//!   * with an empty map -> prepare refuses ("no connector permit"), the
//!     regression the fix removes for gated children;
//!   * with a tampered pinned digest (bindings drifted after the gate) -> the
//!     run-start verification refuses.
//!
//! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` and the secret var
//! are process-wide state that must not race another module.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use apb_core::connector::config::account_digest;
use apb_core::connector::resolve::resolve_playbook;
use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_engine::RunOptions;
use apb_engine::run_config::read_run_config;

use crate::common;

const CONNECTOR: &str = "mock-tracker";
const CHILD_ID: &str = "conn-child";
const SECRET_VAR: &str = "APB_CHILD_PREP_TOKEN";

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
fn set_var(var: &'static str, value: impl AsRef<std::ffi::OsStr>) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    VarGuard { var, prior }
}

/// Writes the `mock-tracker` fixture connector into `cfg`'s connectors dir.
fn write_fixture_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join(CONNECTOR);
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
"#,
    )
    .unwrap();
}

fn write_project_account(root: &Path) {
    let path = apb_core::connector::config::project_config_path(root, CONNECTOR);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "accounts:\n  - name: acct1\n    base_url: https://example.com\n    token: \"{{{{env.{SECRET_VAR}}}}}\"\n"
        ),
    )
    .unwrap();
}

/// The child playbook: a single agent node binds `mock-tracker` (the
/// `read_only` shorthand). It stands in for the connector-binding sub-playbook.
/// `defaults.profile` satisfies V18 (an agent node needs an executor binding);
/// the profile is seeded and left untrusted, since these tests exercise the
/// connector permit path, not profile-bundle trust (`expected_profile_bundles`
/// stays `None`).
fn child_yaml() -> &'static str {
    r#"schema: 2
id: conn-child
name: conn-child
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    connectors: [{ name: mock-tracker, functions: read_only }]
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: f }
"#
}

fn register_child(root: &Path) {
    let vdir = root.join(".apb/playbooks").join(CHILD_ID).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), child_yaml()).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(CHILD_ID).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// The two permit maps a correct policy gate (and thus a `ChildExpectation`)
/// would carry for the child, built from a live resolution.
fn pinned_maps(root: &Path) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let pb = Playbook::from_yaml(child_yaml()).unwrap();
    let out = resolve_playbook(root, &pb).unwrap();
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

fn base_setup(cfg: &Path, root: &Path) {
    init_project(root).unwrap();
    write_fixture_connector(cfg);
    write_project_account(root);
    common::seed_main(root);
    register_child(root);
}

#[test]
fn child_prepare_accepts_pinned_connector_permit() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _token = set_var(SECRET_VAR, "shh");
    base_setup(cfg.path(), root.path());

    let (connectors, accounts) = pinned_maps(root.path());
    let opts = RunOptions {
        expected_connectors: connectors.clone(),
        expected_connector_accounts: accounts,
        ..Default::default()
    };
    // Prepare only (no supervisor spawn, no drive): validate + snapshot the
    // connector into the run manifest, then stop.
    let prepared = apb_engine::prepare_supervised_background(root.path(), CHILD_ID, None, opts)
        .expect("child prepare must accept the pinned connector permit (finding 2 of issue #42)");
    let run_id = prepared.run_id().to_string();
    let run_dir = root.path().join(".apb/runs").join(&run_id);

    // The manifest snapshotted the bound connector.
    let manifest = apb_engine::manifest::read(&run_dir)
        .expect("manifest read")
        .expect("connector-binding run has a manifest");
    assert!(
        manifest.connectors.iter().any(|c| c.name == CONNECTOR),
        "manifest snapshots the pinned connector"
    );

    // The run config persists the expected connector map for audit.
    let cfg_read = read_run_config(&run_dir).expect("run config");
    assert_eq!(
        cfg_read.expected_connectors, connectors,
        "run config carries the pinned connector map verbatim"
    );
}

#[test]
fn child_prepare_refuses_without_connector_permit() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _token = set_var(SECRET_VAR, "shh");
    base_setup(cfg.path(), root.path());

    // An empty permit map is the ungated shape the fix replaces for gated
    // children: a connector-binding run with no permit must fail closed.
    let opts = RunOptions::default();
    // `PreparedRun` is not `Debug`, so match instead of `expect_err`.
    let msg = match apb_engine::prepare_supervised_background(root.path(), CHILD_ID, None, opts) {
        Ok(_) => panic!("a connector-binding child with no permit must be refused"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("no connector permit"),
        "refusal names the missing permit: {msg}"
    );
}

#[test]
fn child_prepare_refuses_on_connector_drift() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _token = set_var(SECRET_VAR, "shh");
    base_setup(cfg.path(), root.path());

    // The pin's connector digest no longer matches the live folder: the child's
    // bindings drifted after the gate. Run-start verification must refuse.
    let (mut connectors, accounts) = pinned_maps(root.path());
    connectors.insert(CONNECTOR.to_string(), "sha256:deadbeef".to_string());
    let opts = RunOptions {
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..Default::default()
    };
    let msg = match apb_engine::prepare_supervised_background(root.path(), CHILD_ID, None, opts) {
        Ok(_) => panic!("connector drift must be refused"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains(CONNECTOR) && msg.contains("changed since the trust check"),
        "refusal names the drifted connector: {msg}"
    );
}
