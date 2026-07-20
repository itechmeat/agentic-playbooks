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

fn unset_var(var: &str) -> EnvGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::remove_var(var);
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

/// A connector directory that `store::list` still enumerates (its
/// `connector.yaml` parses fine, since `list` only reads and parses that one
/// file) but whose `store::load` fails: an absolute-target symlink inside the
/// tree makes `content::tree_digest` refuse it (`ContentError::Escape`), and
/// `load` folds that into `ConnectorError::Invalid`. This is the "connector
/// installed but broken" case spec 9's fourth trust state exists for.
#[cfg(unix)]
#[tokio::test]
async fn list_endpoint_marks_broken_connector_invalid() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let dir = cfg.path().join("connectors").join(CONNECTOR);
    std::os::unix::fs::symlink("/nonexistent-absolute-target", dir.join("broken-link")).unwrap();

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/connectors").await;
    assert_eq!(status, StatusCode::OK);
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == CONNECTOR)
        .expect("fixture connector listed");
    assert_eq!(
        entry["trust"], "invalid",
        "a connector whose store::load fails must report invalid, not unapproved: {entry}"
    );
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

// --- machine-wide (workspace-less) reads on the global server --------------

/// Registers `root` in the machine-wide project registry the global server
/// enumerates, and returns its `workspace_id` plus the env guards that keep
/// auto-registration enabled (`CI` / `APB_NO_REGISTRY` disable it, and the
/// consolidated test binary may well run under `CI`). The guards must be kept
/// alive by the caller.
fn register_workspace(root: &Path) -> (String, Vec<EnvGuard>) {
    let guards = vec![unset_var("CI"), unset_var("APB_NO_REGISTRY")];
    apb_core::projects::touch(root);
    let id = std::fs::read_to_string(root.join(".apb/workspace.local"))
        .expect("workspace.local written by registration")
        .trim()
        .to_string();
    (id, guards)
}

/// The connectors page loads without pinning a project: connectors are
/// installed machine-wide, so `GET /api/connectors` with no `workspace` must
/// answer 200 with the array rather than the old 400, and must list each
/// installed connector exactly once no matter how many projects are reachable.
#[tokio::test]
async fn list_endpoint_without_workspace_lists_each_connector_once() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let root2 = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // A SECOND reachable project also configuring the same connector: the
    // aggregate must still emit one card per connector, not one per project.
    let _g2 = setup(cfg.path(), root2.path());
    let (_id, _reg) = register_workspace(root.path());
    let (_id2, _reg2) = register_workspace(root2.path());

    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, "/api/connectors").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a workspace-less connector list must not 400: {json}"
    );
    let arr = json.as_array().expect("connectors array");
    let hits = arr.iter().filter(|c| c["name"] == CONNECTOR).count();
    assert_eq!(
        hits, 1,
        "the fixture connector must appear exactly once across projects: {json}"
    );
    let entry = arr.iter().find(|c| c["name"] == CONNECTOR).unwrap();
    assert_eq!(entry["display_name"], "Mock Tracker");
    assert_eq!(entry["trust"], "unapproved");
}

/// The single-project contract is unchanged: `?workspace=<id>` still answers
/// with exactly that project's account numbers.
#[tokio::test]
async fn list_endpoint_with_workspace_keeps_single_project_behavior() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let (id, _reg) = register_workspace(root.path());

    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, &format!("/api/connectors?workspace={id}")).await;
    assert_eq!(status, StatusCode::OK, "scoped connector list: {json}");
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == CONNECTOR)
        .expect("fixture connector listed");
    assert_eq!(entry["trust"], "unapproved");
    assert_eq!(entry["accounts_total"], serde_json::json!(1));
    // TOKEN_VAR is unset, so the one account is configured but not ready.
    assert_eq!(entry["accounts_ready"], serde_json::json!(0));

    // An unknown workspace stays strict.
    let app = build_router(AppState::new_global());
    let (status, _) = get_json(app, "/api/connectors?workspace=no-such-workspace").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The list page links to a connector without a workspace, so the detail
/// endpoint must answer there too: identity, manifest and trust are
/// root-independent, and the account rows are the union across reachable
/// projects.
#[tokio::test]
async fn detail_endpoint_without_workspace_returns_connector() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let (_id, _reg) = register_workspace(root.path());

    let app = build_router(AppState::new_global());
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a workspace-less connector detail must not 400: {json}"
    );
    assert_eq!(json["name"], CONNECTOR);
    assert_eq!(json["trust"], "unapproved");
    assert!(
        json["functions"]
            .as_array()
            .expect("functions array")
            .iter()
            .any(|f| f["name"] == "ping"),
        "manifest functions are root-independent: {json}"
    );
    let accounts = json["accounts"].as_array().expect("accounts array");
    assert_eq!(
        accounts.iter().filter(|a| a["name"] == "acct1").count(),
        1,
        "each account is listed once in the machine-wide view: {json}"
    );
}

// --- usage stats (task 17.5, spec 9's dropped "usage stats" bullet) --------

/// Appends one `ConnectorCall` event to `run_dir`'s event log, creating the
/// run directory and log if needed - the only event `GET
/// /api/connectors/{name}/stats` reads from.
fn write_connector_call_event(
    run_dir: &Path,
    connector: &str,
    function: &str,
    account: &str,
    outcome: &str,
    duration_ms: u64,
) {
    let mut log = apb_engine::event::EventLog::create(run_dir).unwrap();
    log.append(apb_engine::event::EventPayload::ConnectorCall {
        node_id: "a".into(),
        connector: connector.into(),
        function: function.into(),
        account: account.into(),
        url: String::new(),
        outcome: outcome.into(),
        http_status: None,
        duration_ms,
        smtp_subject: None,
        smtp_recipients: None,
    })
    .unwrap();
}

#[tokio::test]
async fn stats_endpoint_aggregates_connector_calls_across_recent_runs() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let runs_dir = root.path().join(".apb/runs");
    let run1 = runs_dir.join("run-1");
    let run2 = runs_dir.join("run-2");

    write_connector_call_event(&run1, CONNECTOR, "list_items", "acct1", "ok", 100);
    write_connector_call_event(&run1, CONNECTOR, "list_items", "acct1", "auth", 50);
    write_connector_call_event(&run2, CONNECTOR, "ping", "acct1", "ok", 20);
    // A call for a DIFFERENT connector, and one for a different function/
    // account pair, must not bleed into the requested connector's aggregate.
    write_connector_call_event(&run2, "other-connector", "ping", "acct1", "ok", 5);

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}/stats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["connector"], CONNECTOR);
    assert_eq!(json["runs_scanned"], serde_json::json!(2), "stats: {json}");
    assert_eq!(json["calls"], serde_json::json!(3), "stats: {json}");
    assert_eq!(
        json["by_outcome"]["ok"],
        serde_json::json!(2),
        "stats: {json}"
    );
    assert_eq!(
        json["by_outcome"]["auth"],
        serde_json::json!(1),
        "stats: {json}"
    );

    let by_function = json["by_function"].as_array().expect("by_function array");
    let list_items = by_function
        .iter()
        .find(|f| f["function"] == "list_items" && f["account"] == "acct1")
        .unwrap_or_else(|| panic!("list_items/acct1 aggregate present: {json}"));
    assert_eq!(list_items["calls"], serde_json::json!(2));
    assert_eq!(list_items["errors"], serde_json::json!(1));
    assert_eq!(list_items["avg_duration_ms"], serde_json::json!(75.0));

    let ping = by_function
        .iter()
        .find(|f| f["function"] == "ping" && f["account"] == "acct1")
        .unwrap_or_else(|| panic!("ping/acct1 aggregate present: {json}"));
    assert_eq!(ping["calls"], serde_json::json!(1));
    assert_eq!(ping["errors"], serde_json::json!(0));
    assert_eq!(ping["avg_duration_ms"], serde_json::json!(20.0));
}

#[tokio::test]
async fn stats_endpoint_empty_when_no_calls_recorded() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}/stats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["connector"], CONNECTOR);
    assert_eq!(json["runs_scanned"], serde_json::json!(0));
    assert_eq!(json["calls"], serde_json::json!(0));
    assert_eq!(json["by_function"], serde_json::json!([]));
    assert_eq!(json["by_outcome"], serde_json::json!({}));
}

// --- args_schema exposure (slice 6, spec section 7) ------------------------

#[tokio::test]
async fn detail_endpoint_exposes_function_args_schema() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}")).await;
    assert_eq!(status, StatusCode::OK);
    let functions = json["functions"].as_array().unwrap();
    let list_items = functions
        .iter()
        .find(|f| f["name"] == "list_items")
        .expect("list_items present");
    assert_eq!(
        list_items["args_schema"]["properties"]["q"]["type"],
        serde_json::json!("string"),
        "args_schema must be surfaced verbatim: {list_items}"
    );
    let ping = functions.iter().find(|f| f["name"] == "ping").unwrap();
    assert_eq!(
        ping["args_schema"],
        serde_json::json!(null),
        "a function with no args_schema serializes null, not omitted: {ping}"
    );
}

// --- POST /api/connectors/{name}/call (slice 6, spec section 7) -----------

#[tokio::test]
async fn call_endpoint_refuses_unapproved_connector_for_real_call() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // Neither the connector nor the account is approved.

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "list_items", "account": "acct1", "args": {}, "dry_run": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(false), "call: {json}");
    assert_eq!(json["error"]["code"], serde_json::json!("permission"));
}

#[tokio::test]
async fn call_endpoint_dry_run_works_without_approval_or_secrets() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // Neither the connector nor the account is approved; TOKEN_VAR is unset.

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "list_items", "account": "acct1", "args": {}, "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(true), "dry-run call: {json}");
    assert_eq!(json["dry_run"], serde_json::json!(true));
    assert_eq!(json["method"], serde_json::json!("GET"));
    assert_eq!(
        json["url"],
        serde_json::json!("https://first.example.com/items")
    );
}

#[tokio::test]
async fn call_endpoint_real_call_reaches_a_live_mock_http_server() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = setup(cfg.path(), root.path());
    let _g_tok = set_var(TOKEN_VAR, "secret-value");

    // Point acct1's base_url at a spawned one-shot mock server instead of
    // the fixture's default unreachable https://first.example.com.
    let server = crate::common::spawn_http(200, "OK", &[], r#"{"items":["a","b"]}"#.to_string());
    let path = config::project_config_path(root.path(), CONNECTOR);
    std::fs::write(
        &path,
        format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    approve_connector_and_account(root.path(), "acct1");

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "list_items", "account": "acct1", "args": {}, "dry_run": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(true), "real call: {json}");
    assert_eq!(json["status"], serde_json::json!(200));
    assert_eq!(json["body"], serde_json::json!({"items": ["a", "b"]}));
    // The fixture declares no response_pick, so `picked` must never read true.
    assert_ne!(
        json["picked"],
        serde_json::json!(true),
        "unexpected pick: {json}"
    );

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains("Authorization: Bearer secret-value"),
        "auth header missing/wrong:\n{req}"
    );
}

/// The `full` flag (spec 2026-07-19-official-connectors-design section 7
/// post-review fix): omitted or `false` applies the function's
/// `response_pick` projection like a normal agent call (and marks
/// `picked: true`); `full: true` bypasses it and returns the raw body with
/// no `picked` marker, mirroring the CLI's `--full` debugging escape.
#[tokio::test]
async fn call_endpoint_full_flag_controls_the_response_pick_projection() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = setup(cfg.path(), root.path());
    let _g_tok = set_var(TOKEN_VAR, "secret-value");

    let raw_body = r#"{"items":["a","b"],"extra":"drop"}"#;

    // Default (full omitted): the projection applies, picked: true.
    let server = crate::common::spawn_http(200, "OK", &[], raw_body.to_string());
    let path = config::project_config_path(root.path(), CONNECTOR);
    std::fs::write(
        &path,
        format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    approve_connector_and_account(root.path(), "acct1");

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "list_pick", "account": "acct1", "args": {}, "dry_run": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["ok"],
        serde_json::json!(true),
        "projected call: {json}"
    );
    assert_eq!(json["picked"], serde_json::json!(true));
    assert_eq!(json["body"], serde_json::json!({"items": ["a", "b"]}));

    // full: true bypasses the projection. A changed base_url changes the
    // account digest, so it needs a fresh one-shot server plus re-approval.
    let server2 = crate::common::spawn_http(200, "OK", &[], raw_body.to_string());
    std::fs::write(
        &path,
        format!(
            "accounts:\n  - name: acct1\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server2.base_url
        ),
    )
    .unwrap();
    approve_connector_and_account(root.path(), "acct1");

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "list_pick", "account": "acct1", "args": {}, "dry_run": false, "full": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(true), "full call: {json}");
    assert!(
        json.get("picked").is_none(),
        "full must not mark picked: {json}"
    );
    assert_eq!(
        json["body"],
        serde_json::json!({"items": ["a", "b"], "extra": "drop"})
    );
}

#[tokio::test]
async fn call_endpoint_unknown_function_is_config_error() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "no_such_fn", "account": "acct1", "args": {}, "dry_run": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(false));
    assert_eq!(json["error"]["code"], serde_json::json!("config"));
}

// --- install / uninstall / available -----------------------------------------
//
// These exercise the connector lifecycle endpoints against the REAL embedded
// official set (not the `mock-tracker` fixture above), because that is exactly
// what the dashboard's connect button installs.

/// An embedded official connector guaranteed to be present in the binary.
const EMBEDDED: &str = "github";

async fn post_no_body(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn router(root: &Path) -> axum::Router {
    build_router(AppState::new(root.to_path_buf()))
}

/// `GET /api/connectors/available` needs no `?workspace=`: the embedded set
/// comes out of the binary and the store is machine-wide.
#[tokio::test]
async fn available_endpoint_lists_embedded_and_omits_installed() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, json) = get_json(router(root.path()), "/api/connectors/available").await;
    assert_eq!(status, StatusCode::OK);
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == EMBEDDED)
        .unwrap_or_else(|| panic!("embedded `{EMBEDDED}` must be offered: {json}"));
    assert!(entry["version"].as_str().is_some_and(|v| !v.is_empty()));
    assert!(
        entry["display_name"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "display_name comes from the embedded PUBLIC.md: {entry}"
    );
    assert!(entry.get("summary").is_some());
    assert!(entry["tags"].is_array());

    let (status, _) = post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/install"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_status, json) = get_json(router(root.path()), "/api/connectors/available").await;
    assert!(
        !json
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == EMBEDDED),
        "an installed connector must drop out of the available list: {json}"
    );
}

#[tokio::test]
async fn install_endpoint_then_list_shows_the_connector() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, json) = post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/install"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "install failed: {json}");
    assert_eq!(json["ok"], serde_json::json!(true));
    assert_eq!(json["name"], serde_json::json!(EMBEDDED));
    assert_eq!(json["no_op"], serde_json::json!(false));
    assert_eq!(json["trust_recorded"], serde_json::json!(true));
    assert!(json["digest"].as_str().unwrap().starts_with("sha256:"));

    let (status, json) = get_json(router(root.path()), "/api/connectors").await;
    assert_eq!(status, StatusCode::OK);
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == EMBEDDED)
        .unwrap_or_else(|| panic!("installed connector must be listed: {json}"));
    // Installed via the bundled-origin path, so it is trusted straight away.
    assert_eq!(entry["trust"], "approved");
}

#[tokio::test]
async fn install_endpoint_twice_is_a_no_op_not_an_error() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let uri = format!("/api/connectors/{EMBEDDED}/install");
    let (first_status, first) = post_no_body(router(root.path()), &uri).await;
    let (second_status, second) = post_no_body(router(root.path()), &uri).await;

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK, "reinstall failed: {second}");
    assert_eq!(first["no_op"], serde_json::json!(false));
    assert_eq!(second["no_op"], serde_json::json!(true));
    assert_eq!(first["digest"], second["digest"]);
}

/// A target that exists and DIFFERS needs `?force=true`; without it the answer
/// is 409 so the dashboard can ask before clobbering a local edit.
#[tokio::test]
async fn install_endpoint_conflicts_on_a_differing_target_until_forced() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let uri = format!("/api/connectors/{EMBEDDED}/install");
    post_no_body(router(root.path()), &uri).await;
    std::fs::write(
        cfg.path()
            .join("connectors")
            .join(EMBEDDED)
            .join("EXTRA.md"),
        "local edit",
    )
    .unwrap();

    let (status, json) = post_no_body(router(root.path()), &uri).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert_eq!(json["error"], serde_json::json!("needs_force"));
    assert!(json["detail"].as_str().is_some_and(|d| !d.is_empty()));

    let (status, json) = post_no_body(router(root.path()), &format!("{uri}?force=true")).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["no_op"], serde_json::json!(false));
}

/// The core promise behind "disconnect but keep the configuration": uninstall
/// drops the connector out of the list but the account config file written
/// before it is still on disk afterwards.
#[tokio::test]
async fn uninstall_endpoint_removes_connector_but_keeps_account_config() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/install"),
    )
    .await;

    // An account the user configured for this connector, in both scopes.
    let global = config::global_config_path(EMBEDDED).unwrap();
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(&global, "accounts:\n  - name: work\n    default: true\n").unwrap();
    let scoped = config::project_config_path(root.path(), EMBEDDED);
    std::fs::create_dir_all(scoped.parent().unwrap()).unwrap();
    std::fs::write(&scoped, "accounts:\n  - name: side\n").unwrap();

    let (status, json) = post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/uninstall"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "uninstall failed: {json}");
    assert_eq!(json["ok"], serde_json::json!(true));
    assert_eq!(json["no_op"], serde_json::json!(false));

    let (_status, json) = get_json(router(root.path()), "/api/connectors").await;
    assert!(
        !json
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == EMBEDDED),
        "uninstalled connector must leave the list: {json}"
    );

    assert!(
        global.is_file(),
        "global account config must survive an uninstall"
    );
    assert!(
        scoped.is_file(),
        "project account config must survive an uninstall"
    );
    assert_eq!(
        std::fs::read_to_string(&scoped).unwrap(),
        "accounts:\n  - name: side\n"
    );

    // Reconnecting picks the previous accounts back up with no re-entry.
    post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/install"),
    )
    .await;
    let accounts = config::load_merged(root.path(), EMBEDDED).unwrap();
    let names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["work", "side"]);
}

#[tokio::test]
async fn uninstall_endpoint_of_an_absent_connector_is_a_no_op() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, json) = post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/uninstall"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["ok"], serde_json::json!(true));
    assert_eq!(json["no_op"], serde_json::json!(true));
}

#[tokio::test]
async fn install_endpoint_unknown_is_404_and_invalid_name_is_400() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, json) = post_no_body(
        router(root.path()),
        "/api/connectors/definitely-not-a-connector/install",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
    assert_eq!(json["error"], serde_json::json!("not_found"));
    assert!(json["detail"].as_str().is_some_and(|d| !d.is_empty()));

    let (status, json) =
        post_no_body(router(root.path()), "/api/connectors/Bad_Name/install").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"], serde_json::json!("invalid_name"));

    // Uninstall shares the name gate but has no 404: an absent connector is a
    // successful no-op, only a malformed name is refused.
    let (status, json) =
        post_no_body(router(root.path()), "/api/connectors/Bad_Name/uninstall").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"], serde_json::json!("invalid_name"));
}

/// `GET /api/connectors/{name}` for an embedded official connector that is NOT
/// installed: not being installed is a state, not a 404. The manifest half of
/// the response comes out of the binary and must be fully populated so the
/// dashboard can show what a connector does before the user connects it, with
/// `installed: false` and a `not_installed` trust that is honest about there
/// being no bytes on disk to have a trust decision about.
#[tokio::test]
async fn detail_endpoint_serves_embedded_connector_that_is_not_installed() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // `setup` installs only the fixture connector, so `github` is embedded but
    // definitely not installed under this temp config dir.

    let (status, json) = get_json(router(root.path()), "/api/connectors/github").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["name"], "github");
    assert_eq!(json["installed"], serde_json::json!(false));
    assert_eq!(json["trust"], "not_installed");
    assert!(
        json["version"].as_str().is_some_and(|v| !v.is_empty()),
        "version comes from the embedded manifest: {json}"
    );
    assert!(
        json["body_md"].as_str().is_some_and(|b| !b.is_empty()),
        "the About body comes from the embedded PUBLIC.md: {json}"
    );
    assert!(
        json["meta"]["display_name"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "storefront meta comes from the embedded PUBLIC.md: {json}"
    );
    assert!(
        !json["functions"]
            .as_array()
            .expect("functions array")
            .is_empty(),
        "functions come from the embedded manifest: {json}"
    );
    assert!(json["accounts"].is_array(), "{json}");
}

/// The installed answer carries the same field, set the other way, so the
/// dashboard branches on `installed` instead of guessing from a 404.
#[tokio::test]
async fn detail_endpoint_marks_installed_connector_installed() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, json) =
        get_json(router(root.path()), &format!("/api/connectors/{CONNECTOR}")).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["installed"], serde_json::json!(true));
    assert_eq!(json["trust"], "unapproved");
}

/// 404 is reserved for a name that is neither installed nor embedded, so the
/// dashboard can still tell "no such connector" apart from "not connected".
#[tokio::test]
async fn detail_endpoint_404s_only_for_a_name_that_is_neither_installed_nor_embedded() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, _json) = get_json(
        router(root.path()),
        "/api/connectors/definitely-not-a-connector",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `GET /api/connectors/{name}/stats` without `?workspace=`: the connector page
/// is machine-wide and pins no project, so a missing workspace aggregates
/// instead of erroring. No recorded calls is an empty 200, not a failure.
#[tokio::test]
async fn stats_endpoint_aggregates_without_a_workspace() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let (status, json) = get_json(router(root.path()), "/api/connectors/github/stats").await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["connector"], "github");
    assert_eq!(json["calls"], serde_json::json!(0));
    assert!(json["by_function"].as_array().is_some_and(|a| a.is_empty()));
}
