//! Slice 6: the dashboard playground's live call
//! (`connector_call::play_call`, spec 2026-07-19-official-connectors-design
//! section 7). Generalizes the `healthcheck` probe pipeline (live connector
//! definition, live merged account config, no run context) to an arbitrary
//! function, args, an optional dry-run, and an explicit `full` flag. Mirrors
//! `connector_healthcheck.rs`'s structure and fixtures.
//!
//! Trust gating: a real call is gated exactly like the healthcheck probe. A
//! dry-run resolves no secrets and is therefore NOT gated.
//!
//! `full`: unlike the healthcheck probe (always `full: true`, a raw
//! reachability check), the playground's default is `false`, so a call whose
//! function declares `response_pick` gets the same projection - and can mark
//! `picked: true` - as a normal agent call.
//!
//! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` and the fixture's
//! secret env var are process-wide state shared with every other module in
//! this consolidated test binary.

use std::path::Path;

use apb_core::connector::config;
use apb_core::connector::store;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::connector_call::play_call;
use serde_json::json;

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

const CONNECTOR: &str = "play-conn";
const TOKEN_VAR: &str = "APB_PLAY_TEST_TOKEN";

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

const HTTP_YAML: &str = r#"
name: play-conn
version: 0.1.0
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
  - name: list_items
    description: List items
    read_only: true
    method: GET
    url: "{{account.base_url}}/items"
    args_schema: { type: object, properties: { q: { type: string } } }
  - name: list_pick
    description: list with a projection
    read_only: true
    method: GET
    url: "{{account.base_url}}/pick"
    response_pick: [items]
  - name: ping
    description: Reachability check
    mock: { status: 200, body: { ok: true } }
"#;

#[test]
fn dry_run_renders_without_secrets_or_trust_approval() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), HTTP_YAML);
    write_account(
        root.path(),
        &format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: https://unused.example\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
        ),
    );
    // Neither the connector nor the account is approved; TOKEN_VAR is unset.

    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        Some("acct1"),
        "list_items",
        &json!({}),
        true,
        false,
    );
    assert!(
        ok,
        "a dry-run must succeed with no approval and no secret: {value}"
    );
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["dry_run"], json!(true));
    assert_eq!(value["method"], json!("GET"));
    assert_eq!(value["url"], json!("https://unused.example/items"));
}

#[test]
fn real_call_on_unapproved_connector_is_permission_denied() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), HTTP_YAML);
    write_account(
        root.path(),
        &format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: https://unused.example\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
        ),
    );
    let _g_tok = set_var(TOKEN_VAR, "secret-value");

    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        Some("acct1"),
        "list_items",
        &json!({}),
        false,
        false,
    );
    assert!(
        !ok,
        "a real call on an unapproved connector must refuse: {value}"
    );
    assert_eq!(value["error"]["code"], json!("permission"));
}

#[test]
fn approved_real_call_reaches_the_url_with_auth() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _g_tok = set_var(TOKEN_VAR, "play-secret-value");
    write_connector(cfg.path(), HTTP_YAML);

    let server = common::spawn_http(200, "OK", &[], r#"{"items":[]}"#.to_string());
    write_account(
        root.path(),
        &format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server.base_url
        ),
    );
    approve_connector();
    approve_account(root.path(), "acct1");

    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        Some("acct1"),
        "list_items",
        &json!({}),
        false,
        false,
    );
    assert!(ok, "approved real call should succeed: {value}");
    assert_eq!(value["status"], json!(200));
    assert_eq!(value["body"], json!({"items": []}));

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains("Authorization: Bearer play-secret-value"),
        "auth header missing/wrong in request:\n{req}"
    );
}

#[test]
fn unknown_function_name_is_config_error() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), HTTP_YAML);
    write_account(
        root.path(),
        "accounts:\n  - name: acct1\n    default: true\n    base_url: https://unused.example\n    token: \"{{env.NOPE}}\"\n",
    );

    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        Some("acct1"),
        "no_such_function",
        &json!({}),
        true,
        false,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], json!("config"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(msg.contains("no_such_function"), "message: {msg}");
}

#[test]
fn single_configured_account_is_auto_selected() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), HTTP_YAML);
    // Only one account, not flagged default: still auto-selected.
    write_account(
        root.path(),
        "accounts:\n  - name: only-one\n    base_url: https://solo.example\n    token: \"{{env.NOPE}}\"\n",
    );

    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        None,
        "list_items",
        &json!({}),
        true,
        false,
    );
    assert!(ok, "the single account should be auto-selected: {value}");
    assert_eq!(value["url"], json!("https://solo.example/items"));
}

#[test]
fn ambiguous_accounts_without_default_is_config_error() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    write_connector(cfg.path(), HTTP_YAML);
    write_account(
        root.path(),
        "accounts:\n  - name: a\n    base_url: https://a.example\n    token: \"{{env.NOPE}}\"\n  - name: b\n    base_url: https://b.example\n    token: \"{{env.NOPE}}\"\n",
    );

    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        None,
        "list_items",
        &json!({}),
        true,
        false,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], json!("config"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains('a') && msg.contains('b'),
        "message should list choices: {msg}"
    );
}

#[test]
fn full_false_applies_response_pick_and_full_true_bypasses_it() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _g_tok = set_var(TOKEN_VAR, "play-secret-value");
    write_connector(cfg.path(), HTTP_YAML);

    let raw = r#"{"items":["a","b"],"extra":"drop"}"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    write_account(
        root.path(),
        &format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server.base_url
        ),
    );
    approve_connector();
    approve_account(root.path(), "acct1");

    // Default (full: false): the projection applies, like a normal agent call.
    let (value, ok) = play_call(
        root.path(),
        CONNECTOR,
        Some("acct1"),
        "list_pick",
        &json!({}),
        false,
        false,
    );
    assert!(ok, "projected call should succeed: {value}");
    assert_eq!(value["picked"], json!(true));
    assert_eq!(value["body"], json!({ "items": ["a", "b"] }));

    // full: true bypasses the projection (spec 7 debugging escape). A fresh
    // one-shot server: the first server already consumed its single response.
    // A changed `base_url` changes the account digest, so it must be
    // re-approved (trust is keyed by content digest, spec 9).
    let server2 = common::spawn_http(200, "OK", &[], raw.to_string());
    write_account(
        root.path(),
        &format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server2.base_url
        ),
    );
    approve_account(root.path(), "acct1");
    let (full, ok2) = play_call(
        root.path(),
        CONNECTOR,
        Some("acct1"),
        "list_pick",
        &json!({}),
        false,
        true,
    );
    assert!(ok2, "full call should succeed: {full}");
    assert!(
        full.get("picked").is_none(),
        "full must not mark picked: {full}"
    );
    assert_eq!(
        full["body"],
        json!({ "items": ["a", "b"], "extra": "drop" })
    );
}
