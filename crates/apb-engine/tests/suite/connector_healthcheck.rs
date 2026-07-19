//! Task 16: the live healthcheck probe seam
//! (`connector_call::healthcheck`, spec 2026-07-18-connectors-design section
//! 9). Unlike `connector_call::execute`, this path has no run context: it
//! loads the LIVE connector definition from `APB_CONFIG_DIR` and the LIVE
//! merged account config from a project root, resolves secrets from the
//! process env, and executes ONLY the connector's declared `healthcheck`
//! function through the same render/dispatch pipeline a real call uses.
//!
//! Fix round (spec 9, updated): the probe resolves LIVE secrets and sends
//! them to the LIVE config's base_url, so it is trust-gated the same way a
//! real run is - an unapproved connector digest or an unapproved/changed
//! account digest must refuse before any secret is touched.
//!
//! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` and the fixture's
//! secret env var are process-wide state that must not race another module's
//! `set_var`.

use std::path::Path;

use apb_core::connector::config;
use apb_core::connector::store;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::connector_call::healthcheck;

use crate::common;

struct EnvGuard {
    var: String,
    prior: Option<std::ffi::OsString>,
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(&self.var, v),
                None => std::env::remove_var(&self.var),
            }
        }
    }
}
fn set_var(var: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    EnvGuard {
        var: var.to_string(),
        prior,
    }
}

const CONNECTOR: &str = "hc-conn";

fn write_connector(cfg: &Path, yaml: &str) {
    let dir = cfg.join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
}

fn write_account(root: &Path, yaml: &str) {
    let path = root
        .join(".apb/connector-config")
        .join(format!("{CONNECTOR}.yaml"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, yaml).unwrap();
}

/// Approves the connector's current tree digest.
fn approve_connector() {
    let loaded = store::load(CONNECTOR).unwrap();
    let mut trust = TrustStore::load();
    trust
        .approve_kind(
            &loaded.digest,
            CONNECTOR,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
}

/// Approves one account's current non-secret-field digest against the live
/// config under `root`.
fn approve_account(root: &Path, account: &str) {
    let accounts = config::load_merged(root, CONNECTOR).unwrap();
    let acct = accounts.iter().find(|a| a.name == account).unwrap();
    let digest = config::account_digest(acct);
    let mut trust = TrustStore::load();
    trust
        .approve_kind(
            &digest,
            &account_trust_id(CONNECTOR, account),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();
}

const MOCK_YAML: &str = r#"
name: hc-conn
version: 0.1.0
healthcheck: ping
account_fields:
  - name: base_url
    required: true
functions:
  - name: ping
    description: Reachability check
    mock: { status: 200, body: { ok: true } }
"#;

#[test]
fn unapproved_connector_is_permission_denied() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), MOCK_YAML);
    write_account(
        root.path(),
        "accounts:\n  - name: acct1\n    base_url: https://unused.example\n",
    );
    // Neither the connector nor the account is approved.

    let (value, ok) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(!ok, "must refuse an unapproved connector: {value}");
    assert_eq!(value["error"]["code"], serde_json::json!("permission"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("connector") && msg.contains("not approved"),
        "message should name the unapproved connector: {msg}"
    );
}

#[test]
fn approved_connector_but_unapproved_account_is_permission_denied() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), MOCK_YAML);
    write_account(
        root.path(),
        "accounts:\n  - name: acct1\n    base_url: https://unused.example\n",
    );
    approve_connector();
    // The account itself is left unapproved.

    let (value, ok) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(!ok, "must refuse an unapproved account: {value}");
    assert_eq!(value["error"]["code"], serde_json::json!("permission"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("acct1") && msg.contains("not approved"),
        "message should name the unapproved account: {msg}"
    );
}

#[test]
fn mock_healthcheck_needs_no_network() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), MOCK_YAML);
    write_account(
        root.path(),
        "accounts:\n  - name: acct1\n    base_url: https://unused.example\n",
    );
    approve_connector();
    approve_account(root.path(), "acct1");

    let (value, ok) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(ok, "mock healthcheck should succeed: {value}");
    assert_eq!(value["status"], serde_json::json!(200));
    assert_eq!(value["body"], serde_json::json!({"ok": true}));
}

const HTTP_YAML: &str = r#"
name: hc-conn
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: ping
    description: Reachability check
    method: GET
    url: "{{account.base_url}}/health"
"#;

#[test]
fn http_healthcheck_reaches_the_real_url() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = set_var("APB_CONFIG_DIR", cfg.path());
    const TOKEN_VAR: &str = "APB_HC_TEST_TOKEN";
    let _g_tok = set_var(TOKEN_VAR, "hc-secret-value");
    write_connector(cfg.path(), HTTP_YAML);

    let server = common::spawn_http(200, "OK", &[], r#"{"status":"up"}"#.to_string());
    write_account(
        root.path(),
        &format!(
            "accounts:\n  - name: acct1\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server.base_url
        ),
    );
    approve_connector();
    approve_account(root.path(), "acct1");

    let (value, ok) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(ok, "http healthcheck should succeed: {value}");
    assert_eq!(value["status"], serde_json::json!(200));
    assert_eq!(value["body"], serde_json::json!({"status": "up"}));

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains("Authorization: Bearer hc-secret-value"),
        "auth header missing/wrong in request:\n{req}"
    );
}

#[test]
fn missing_healthcheck_declaration_is_config_error() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(
        cfg.path(),
        "name: hc-conn\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    mock: { status: 200, body: {} }\n",
    );
    write_account(
        root.path(),
        "accounts:\n  - name: acct1\n    base_url: https://unused.example\n",
    );

    let (value, ok) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("config"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(msg.contains("no healthcheck"), "message: {msg}");
}

#[test]
fn unknown_account_is_config_error() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), MOCK_YAML);
    write_account(
        root.path(),
        "accounts:\n  - name: acct1\n    base_url: https://unused.example\n",
    );

    let (value, ok) = healthcheck(root.path(), CONNECTOR, "no-such-account");
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("config"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(msg.contains("no account"), "message: {msg}");
}
