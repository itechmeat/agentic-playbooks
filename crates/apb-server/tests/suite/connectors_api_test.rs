//! /api/connectors: list, detail, healthcheck, and approve (Task 16, spec
//! 2026-07-18-connectors-design section 9), plus the dashboard run handler's
//! connector permit-map fix (Task 15 review follow-up): `POST
//! /api/playbooks/{id}/run` must compute the same connector/account trust
//! gate `apb-mcp`'s policy module runs for an MCP-started run, not start with
//! empty permit maps.
//!
//! Every test mutates process-wide env (`APB_CONFIG_DIR`, the fixture's
//! secret env var), so all of them take `common::env_lock()` to serialize
//! against other env-mutating modules in this consolidated binary.

use apb_core::connector::config;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::path::Path;
use tower::ServiceExt;

// --- env guards -------------------------------------------------------------

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

// --- http helpers ------------------------------------------------------------

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn post_json(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

// --- fixture -----------------------------------------------------------------

const CONNECTOR: &str = "mock-tracker";
const TOKEN_VAR: &str = "APB_SRV_CONN_TEST_TOKEN";

fn write_connector(cfg: &Path) {
    let dir = cfg.join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        r#"
name: mock-tracker
version: 0.1.0
healthcheck: ping
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
  - name: ping
    description: Reachability check
    mock: { status: 200, body: { ok: true } }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("PUBLIC.md"),
        "---\ndisplay_name: Mock Tracker\nsummary: A fixture connector\ntags: [test]\n---\nBody.\n",
    )
    .unwrap();
}

/// Writes the project account config with one account, `acct1`, whose secret
/// `token` field references `TOKEN_VAR` (left unset unless a test sets it).
fn write_account(root: &Path) {
    let path = config::project_config_path(root, CONNECTOR);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: https://first.example.com\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
        ),
    )
    .unwrap();
}

const PLAYBOOK_ID: &str = "conn-pb";

/// A registered playbook whose single agent node binds `mock-tracker`
/// (granting only `acct1`) and an executor profile `main`, so the run
/// handler's connector permit-map fix can be exercised end to end.
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
    profile: main
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

fn seed_profile_main(root: &Path) {
    let dir = root.join(".apb/profiles/main");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();
}

/// Common setup shared by every test below: fixture connector + account under
/// a temp `APB_CONFIG_DIR`, a project with the fixture registered. Connector
/// and account trust are left for each test to arrange. Returns the env
/// guard (kept alive by the caller) restoring `APB_CONFIG_DIR` on drop.
fn setup(cfg: &Path, root: &Path) -> EnvGuard {
    let guard = set_var("APB_CONFIG_DIR", cfg);
    apb_core::registry::init_project(root).unwrap();
    write_connector(cfg);
    write_account(root);
    guard
}

// --- tests ---------------------------------------------------------------

#[tokio::test]
async fn list_endpoint_shows_fixture_connector_unapproved() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/connectors").await;
    assert_eq!(status, StatusCode::OK);
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == CONNECTOR)
        .expect("fixture connector listed");
    assert_eq!(entry["trust"], "unapproved");
    assert_eq!(entry["display_name"], "Mock Tracker");
    assert_eq!(entry["accounts_total"], serde_json::json!(1));
    // The token env var is not set: the one account is not ready.
    assert_eq!(entry["accounts_ready"], serde_json::json!(0));
}

#[tokio::test]
async fn approve_endpoint_flips_connector_trust() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, approved) = post_json(
        app,
        "/api/connectors/approve",
        serde_json::json!({ "name": CONNECTOR, "account": null }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve failed: {approved}");
    assert_eq!(approved["ok"], serde_json::json!(true));

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (_status, json) = get_json(app, "/api/connectors").await;
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == CONNECTOR)
        .expect("fixture connector listed");
    assert_eq!(entry["trust"], "approved", "trust must flip: {entry}");
}

#[tokio::test]
async fn detail_endpoint_carries_missing_env() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // Deliberately do NOT set TOKEN_VAR: the account's secret must not resolve.

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["name"], CONNECTOR);
    let accounts = json["accounts"].as_array().expect("accounts array");
    assert_eq!(accounts.len(), 1);
    let acct1 = &accounts[0];
    assert_eq!(acct1["name"], "acct1");
    assert_eq!(acct1["trust"], "unapproved");
    let missing = acct1["missing_env"].as_array().expect("missing_env array");
    assert!(
        missing.iter().any(|v| v == TOKEN_VAR),
        "missing_env must name the unresolved var: {acct1}"
    );
    // Never the secret value or the raw `{{env.VAR}}` reference under `token`.
    assert!(
        acct1["fields"].get("token").is_none(),
        "a secret field must never appear in `fields`: {acct1}"
    );
    assert_eq!(acct1["fields"]["base_url"], "https://first.example.com");
}

/// Approves the fixture connector's tree digest and one account's digest -
/// the healthcheck probe is trust-gated (fix round, spec 9: it resolves live
/// secrets against the live config, so an unapproved connector/account must
/// never be probeable), so a test exercising a successful probe must approve
/// both first, mirroring what the dashboard's approve endpoint would do.
fn approve_connector_and_account(root: &Path, account: &str) {
    let loaded = apb_core::connector::store::load(CONNECTOR).unwrap();
    let mut trust = TrustStore::load();
    trust
        .approve_kind(
            &loaded.digest,
            CONNECTOR,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let accounts = config::load_merged(root, CONNECTOR).unwrap();
    let acct = accounts.iter().find(|a| a.name == account).unwrap();
    trust
        .approve_kind(
            &config::account_digest(acct),
            &account_trust_id(CONNECTOR, account),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();
}

#[tokio::test]
async fn healthcheck_endpoint_refuses_unapproved_connector() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // Neither the connector nor the account is approved.

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/healthcheck/acct1"),
        serde_json::json!({}),
    )
    .await;
    // The endpoint always returns 200 with the executor's JSON verbatim
    // (mirrors the CLI: the caller reads `ok`, the HTTP status is not the
    // signal); the refusal is carried in the body.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(false), "healthcheck: {json}");
    assert_eq!(json["error"]["code"], serde_json::json!("permission"));
}

#[tokio::test]
async fn healthcheck_endpoint_runs_mock_function_once_approved() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    approve_connector_and_account(root.path(), "acct1");

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/healthcheck/acct1"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(true), "healthcheck: {json}");
    assert_eq!(json["status"], serde_json::json!(200));
    assert_eq!(json["body"], serde_json::json!({ "ok": true }));
}

#[tokio::test]
async fn run_handler_refuses_unapproved_connector_binding_playbook() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // The secret resolves (so the env-presence check passes and the gate
    // actually reaches the trust step); connector/account trust is
    // deliberately left unapproved.
    let _g_tok = set_var(TOKEN_VAR, "secret-value");
    seed_profile_main(root.path());
    write_pb(root.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, refusal) = post_json(
        app,
        &format!("/api/playbooks/{PLAYBOOK_ID}/run"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an unapproved connector-binding playbook must refuse: {refusal}"
    );
    assert_eq!(
        refusal["policy"],
        serde_json::json!("untrusted_connector_requires_approve"),
        "refusal: {refusal}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_handler_starts_once_connector_and_account_are_approved() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = setup(cfg.path(), root.path());
    let _g_tok = set_var(TOKEN_VAR, "secret-value");
    seed_profile_main(root.path());
    write_pb(root.path());

    // Approve the connector digest and the merged account digest, exactly as
    // the dashboard approve endpoint (and `apb connector approve`) would.
    let loaded = apb_core::connector::store::load(CONNECTOR).unwrap();
    let mut trust = TrustStore::load();
    trust
        .approve_kind(
            &loaded.digest,
            CONNECTOR,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let accounts = config::load_merged(root.path(), CONNECTOR).unwrap();
    let acct1 = accounts.iter().find(|a| a.name == "acct1").unwrap();
    trust
        .approve_kind(
            &config::account_digest(acct1),
            &account_trust_id(CONNECTOR, "acct1"),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();

    // A stub agent binary so the background run has something harmless to
    // spawn: `run_background` returns synchronously once `prepare` (profile +
    // connector snapshot checks) succeeds, well before the spawned thread's
    // agent process actually finishes - we only assert on that immediate
    // response, not on run completion.
    let agent_path = root.path().join("ok-agent.sh");
    std::fs::write(&agent_path, "#!/bin/sh\necho ok\n").unwrap();
    let mut perms = std::fs::metadata(&agent_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&agent_path, perms).unwrap();
    let _g_agent = set_var("APB_AGENT_CMD", &agent_path);

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/playbooks/{PLAYBOOK_ID}/run"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an approved connector-binding playbook must start: {json}"
    );
    assert!(json["run_id"].is_string(), "expected a run_id: {json}");
}
