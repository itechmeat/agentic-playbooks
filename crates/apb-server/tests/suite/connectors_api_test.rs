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

use apb_core::config::ServerConfig;
use apb_core::connector::config;
use apb_core::server_auth;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_server::auth::{AuthState, CSRF_HEADER, CSRF_VALUE, SESSION_COOKIE};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::path::Path;
use std::sync::Arc;
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

/// A `Some(cookie)` request carries the signed-in session's credentials: the
/// `Cookie` pair and the CSRF marker, what every write of the dashboard
/// sends (design D4.13 item 1, test item 3). `None` keeps the old anonymous
/// shape, what the keyless tests and the rest of the file use.
fn auth_headers(cookie: Option<&str>) -> Vec<(&'static str, String)> {
    match cookie {
        None => Vec::new(),
        Some(cookie) => vec![
            ("cookie", cookie.to_string()),
            (CSRF_HEADER, CSRF_VALUE.into()),
        ],
    }
}

async fn get_json(
    app: axum::Router,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::get(uri);
    for (name, value) in auth_headers(cookie) {
        builder = builder.header(name, value);
    }
    let res = app
        .oneshot(builder.body(Body::empty()).unwrap())
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
    cookie: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in auth_headers(cookie) {
        builder = builder.header(name, value);
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

// --- keyed-state helpers (design D4.13 item 1, test item 3) -------------------

/// A keyed state over the same root: a key issued into a temp
/// `server-auth.yaml`, an `AuthState` built from it attached through
/// `with_auth`, exactly as `auth_endpoints_test.rs`'s `authed_with` builds
/// one. A keyless `AppState` cannot sign in (`login_handler` answers
/// `auth_disabled`), so the approval tests that need a signed-in session
/// build a keyed one. Returns the keyed state and the key.
fn keyed(state: AppState) -> (AppState, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server-auth.yaml");
    let (key, _record) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(AuthState::new(None, file.keys, &ServerConfig::default()).unwrap());
    (state.with_auth(auth), key.expose_secret().to_string())
}

/// Signs in with the key and returns the `<SESSION_COOKIE>=<value>` pair of
/// its `Set-Cookie`. The session lives in the auth state the clones share,
/// so every later request through `build_router(state.clone())` of the same
/// state authenticates with this pair.
async fn sign_in(state: &AppState, key: &str) -> String {
    let res = build_router(state.clone())
        .oneshot(
            Request::post("/api/auth/login")
                .header("content-type", "application/json")
                .header(CSRF_HEADER, CSRF_VALUE)
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "key": key })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "sign in failed");
    let set_cookie = res
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .expect("a session cookie is set");
    assert!(
        set_cookie.starts_with(&format!("{SESSION_COOKIE}=")),
        "{set_cookie}"
    );
    set_cookie.split(';').next().unwrap().to_string()
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
    let (status, json) = get_json(app, "/api/connectors", None).await;
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
    let (status, json) = get_json(app, "/api/connectors", None).await;
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

/// Finding 2 (final-fix-wave): a BROKEN installed OFFICIAL connector must not be
/// masked by the detail endpoint. `store::load` failing with a non-`NotFound`
/// error (here an escaping symlink -> `ContentError::Escape` -> `Invalid`, the
/// same mechanism `list_endpoint_marks_broken_connector_invalid` uses) once let
/// the detail endpoint fall back to the embedded manifest and report
/// `installed: false` / `trust: "not_installed"` with a Connect button, while
/// the list endpoint reported `trust: "invalid"` for the same connector - the
/// two views disagreed. The detail endpoint must now report the same invalid
/// state (installed, `invalid`), so both views agree.
#[cfg(unix)]
#[tokio::test]
async fn detail_endpoint_surfaces_a_broken_installed_official_connector_as_invalid() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    // Install the real embedded official connector, then break it on disk: an
    // absolute-target symlink inside its tree makes `store::load` fail (digest
    // escape) while `store::list` still enumerates it (its connector.yaml
    // parses). This is precisely the "installed but broken official connector"
    // the masking bug hid.
    let (status, _json) = post_no_body(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}/install"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let dir = cfg.path().join("connectors").join(EMBEDDED);
    std::os::unix::fs::symlink("/nonexistent-absolute-target", dir.join("broken-link")).unwrap();

    // Detail: it must be reported installed + invalid, NOT not_installed.
    let (status, detail) = get_json(
        router(root.path()),
        &format!("/api/connectors/{EMBEDDED}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["name"], EMBEDDED);
    assert_eq!(
        detail["trust"], "invalid",
        "a broken installed connector must report invalid, not not_installed: {detail}"
    );
    assert_eq!(
        detail["installed"],
        serde_json::json!(true),
        "the connector IS installed (bytes on disk), just broken: {detail}"
    );

    // List: the same connector is invalid there too.
    let (status, list) = get_json(router(root.path()), "/api/connectors", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == EMBEDDED)
        .unwrap_or_else(|| panic!("the broken connector must still be listed: {list}"));

    // The two views must agree on the trust state.
    assert_eq!(
        listed["trust"], detail["trust"],
        "list and detail must agree on the broken connector's trust: list={listed} detail={detail}"
    );
    assert_eq!(listed["trust"], "invalid", "list view: {listed}");
}

/// The approval is a person's act, so the test signs in on a keyed server:
/// every request below (the approval and the listing) carries the session
/// cookie and the CSRF marker (design D4.13 item 1, test item 3).
#[tokio::test]
async fn approve_endpoint_flips_connector_trust() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let (state, key) = keyed(AppState::new(root.path().to_path_buf()));
    let cookie = sign_in(&state, &key).await;

    let app = build_router(state.clone());
    let (status, approved) = post_json(
        app,
        "/api/connectors/approve",
        serde_json::json!({ "name": CONNECTOR, "account": null }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approve failed: {approved}");
    assert_eq!(approved["ok"], serde_json::json!(true));

    let app = build_router(state.clone());
    let (_status, json) = get_json(app, "/api/connectors", Some(&cookie)).await;
    let entry = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == CONNECTOR)
        .expect("fixture connector listed");
    assert_eq!(entry["trust"], "approved", "trust must flip: {entry}");
}

/// Posts an approval with exactly these extra headers.
async fn post_approve(
    state: &AppState,
    body: serde_json::Value,
    headers: &[(&str, String)],
) -> (StatusCode, serde_json::Value) {
    let mut builder =
        Request::post("/api/connectors/approve").header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// The trust the detail page shows for the fixture connector and its
/// account `acct1`.
async fn trust_of(state: &AppState, cookie: Option<&str>) -> (String, String) {
    let (_, json) = get_json(
        build_router(state.clone()),
        &format!("/api/connectors/{CONNECTOR}"),
        cookie,
    )
    .await;
    let account = json["accounts"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == "acct1"))
        .map(|a| a["trust"].as_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    (
        json["trust"].as_str().unwrap_or_default().to_string(),
        account,
    )
}

/// The two approval bodies: the connector, and one of its accounts.
fn approve_bodies() -> [serde_json::Value; 2] {
    [
        serde_json::json!({ "name": CONNECTOR, "account": null }),
        serde_json::json!({ "name": CONNECTOR, "account": "acct1" }),
    ]
}

/// A keyless dashboard is reachable by anything on this machine, so it must
/// not take an approval (design D4.13 item 1), whatever shape the request
/// has: a local process posting without an `Origin` (what a run step's
/// script sends), the keyless page's own request with the CSRF marker, and
/// a same-origin browser request, each for the connector and for an
/// account. Each is refused before anything is approved, and the server's
/// own environment has no run id either: the credential decides, never the
/// environment.
#[tokio::test]
async fn approve_endpoint_refuses_a_keyless_request_from_a_local_process() {
    let _guard = crate::common::env_lock().await;
    let _no_run = unset_var("APB_RUN_ID");
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());

    let state = AppState::new(root.path().to_path_buf());
    let shapes: [Vec<(&str, String)>; 3] = [
        Vec::new(),
        vec![(CSRF_HEADER, CSRF_VALUE.to_string())],
        vec![
            ("origin", "http://127.0.0.1:8770".to_string()),
            ("sec-fetch-site", "same-origin".to_string()),
        ],
    ];
    for headers in &shapes {
        for body in approve_bodies() {
            let (status, json) = post_approve(&state, body, headers).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "keyless approve must be refused for {headers:?}: {json}"
            );
            assert_eq!(
                json["error"],
                serde_json::json!("keyless_dashboard"),
                "{json}"
            );
            assert!(
                json["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("apb connector approve"),
                "the refusal names the CLI command: {json}"
            );
            assert_eq!(
                trust_of(&state, None).await,
                ("unapproved".to_string(), "unapproved".to_string()),
                "nothing approved after {headers:?}"
            );
        }
    }
}

/// A bearer key is not a person (a script may hold it), so the route
/// refuses an approval posted with one, for the connector and for an
/// account, and nothing is approved (design D4.13 item 1).
#[tokio::test]
async fn approve_endpoint_refuses_a_bearer_key() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let (state, key) = keyed(AppState::new(root.path().to_path_buf()));
    let cookie = sign_in(&state, &key).await;

    for body in approve_bodies() {
        let (status, json) =
            post_approve(&state, body, &[("authorization", format!("Bearer {key}"))]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
        assert_eq!(json["error"], serde_json::json!("bearer_key"), "{json}");
    }
    assert_eq!(
        trust_of(&state, Some(&cookie)).await,
        ("unapproved".to_string(), "unapproved".to_string()),
        "nothing approved"
    );
}

/// The credential decides, never the environment: a signed-in session
/// approves even while the server's own environment names a run (a
/// dashboard started from inside a run step's shell).
#[tokio::test]
async fn a_signed_in_session_approves_while_the_server_has_a_run_id() {
    let _guard = crate::common::env_lock().await;
    let _run = set_var("APB_RUN_ID", "run-of-the-server");
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let (state, key) = keyed(AppState::new(root.path().to_path_buf()));
    let cookie = sign_in(&state, &key).await;

    for body in approve_bodies() {
        let (status, json) = post_json(
            build_router(state.clone()),
            "/api/connectors/approve",
            body,
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
    }
    assert_eq!(
        trust_of(&state, Some(&cookie)).await,
        ("approved".to_string(), "approved".to_string())
    );
}

#[tokio::test]
async fn detail_endpoint_carries_missing_env() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    // Deliberately do NOT set TOKEN_VAR: the account's secret must not resolve.

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}"), None).await;
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

/// The account detail the dashboard approves from names every secret that is
/// sourced from a command, with the command line: approving the account
/// authorizes running it.
#[tokio::test]
async fn detail_endpoint_shows_a_command_sourced_secret() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let global = config::global_config_path(CONNECTOR).unwrap();
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(
        &global,
        "accounts:\n  - name: mine\n    base_url: https://mine.example.com\n    token: \"{{cmd:pass show tracker}}\"\n",
    )
    .unwrap();

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let mine = json["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "mine")
        .cloned()
        .unwrap_or_else(|| panic!("account `mine` listed: {json}"));
    assert_eq!(mine["cmd"]["token"], "pass show tracker", "{mine}");
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
        None,
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
        None,
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
        None,
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
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an approved connector-binding playbook must start: {json}"
    );
    assert!(json["run_id"].is_string(), "expected a run_id: {json}");
}

// --- run: connector permits for a `type: playbook` child (issue #102.1) ----
//
// The dashboard handler used to walk only the TOP-LEVEL nodes for connector
// bindings and never set `expected_children`, so a parent that delegates to a
// connector-binding child spawned that child with empty permit maps and the
// child died fail-closed with "connector bindings present but no connector
// permit". The handler now computes the child pins in the SAME gate pass and
// threads them onto `opts.expected_children`.

const PARENT_ID: &str = "conn-parent";

/// A parent whose sub-playbook node `c` runs the connector-binding child
/// `conn-pb`. The parent itself binds no connectors.
fn parent_yaml() -> &'static str {
    r#"schema: 2
id: conn-parent
name: conn-parent
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: c, type: playbook, playbook: conn-pb }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: c }
  - { from: c, to: f }
"#
}

fn write_parent_pb(root: &Path) {
    let vdir = root.join(".apb/playbooks").join(PARENT_ID).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), parent_yaml()).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(PARENT_ID).join("current"),
        "1.0.0",
    )
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn run_handler_pins_the_connector_permit_of_a_playbook_child() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g_cfg = setup(cfg.path(), root.path());
    let _g_tok = set_var(TOKEN_VAR, "secret-value");
    seed_profile_main(root.path());
    write_pb(root.path());
    write_parent_pb(root.path());
    approve_connector_and_account(root.path(), "acct1");

    let agent_path = root.path().join("ok-agent.sh");
    std::fs::write(&agent_path, "#!/bin/sh\necho ok\n").unwrap();
    let mut perms = std::fs::metadata(&agent_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&agent_path, perms).unwrap();
    let _g_agent = set_var("APB_AGENT_CMD", &agent_path);

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = post_json(
        app,
        &format!("/api/playbooks/{PARENT_ID}/run"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a parent delegating to an approved connector-binding child must start: {json}"
    );
    let run_id = json["run_id"].as_str().expect("run_id");

    // The pin is what the child spawn reads for its own permit maps. Without
    // it the child would be spawned with empty maps and refused at prepare.
    let cfg_yaml =
        std::fs::read_to_string(root.path().join(".apb/runs").join(run_id).join("run.yaml"))
            .expect("run.yaml written at prepare");
    let run_cfg: apb_engine::run_config::RunConfig = yaml_serde::from_str(&cfg_yaml).unwrap();
    let children = run_cfg
        .expected_children
        .expect("the run permit must carry the child pins");
    let child = children
        .get("c")
        .expect("pin for the sub-playbook node `c`");
    let loaded = apb_core::connector::store::load(CONNECTOR).unwrap();
    assert_eq!(
        child.connectors.get(CONNECTOR),
        Some(&loaded.digest),
        "the child pin carries the child's verified connector map: {children:?}"
    );
    assert!(
        child
            .connector_accounts
            .contains_key(&account_trust_id(CONNECTOR, "acct1")),
        "the child pin carries the child's verified account map: {children:?}"
    );
}

#[tokio::test]
async fn run_handler_refuses_a_parent_whose_child_connector_is_untrusted() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _g_tok = set_var(TOKEN_VAR, "secret-value");
    seed_profile_main(root.path());
    write_pb(root.path());
    write_parent_pb(root.path());
    // The child's connector and account are deliberately left unapproved: the
    // child gets the same connector trust gate the parent would get.

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, refusal) = post_json(
        app,
        &format!("/api/playbooks/{PARENT_ID}/run"),
        serde_json::json!({}),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an untrusted child connector must refuse the parent run: {refusal}"
    );
    assert_eq!(
        refusal["policy"],
        serde_json::json!("untrusted_connector_requires_approve"),
        "refusal: {refusal}"
    );
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
    let (status, json) = get_json(app, "/api/connectors", None).await;
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
    let (status, json) = get_json(app, &format!("/api/connectors?workspace={id}"), None).await;
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
    let (status, _) = get_json(app, "/api/connectors?workspace=no-such-workspace", None).await;
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
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}"), None).await;
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

/// The machine-wide connector page offers Approve, Probe and the playground
/// for the account rows it lists without a workspace, so those three call-path
/// endpoints must find the account the same way the listing did: in the
/// reachable project that configures it. They used to demand `?workspace=`
/// and answered every click from the global view with a 400. The approval is
/// a person's act, so the test signs in on a keyed server and every request
/// carries the session cookie and the CSRF marker (design D4.13 item 1,
/// test item 3).
#[tokio::test]
async fn account_actions_without_workspace_use_the_listed_account() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _token = set_var(TOKEN_VAR, "secret-value");
    let (_id, _reg) = register_workspace(root.path());
    let (state, key) = keyed(AppState::new_global());
    let cookie = sign_in(&state, &key).await;

    for body in [
        serde_json::json!({ "name": CONNECTOR }),
        serde_json::json!({ "name": CONNECTOR, "account": "acct1" }),
    ] {
        let app = build_router(state.clone());
        let (status, json) = post_json(app, "/api/connectors/approve", body, Some(&cookie)).await;
        assert_eq!(status, StatusCode::OK, "workspace-less approve: {json}");
    }
    let app = build_router(state.clone());
    let (_, detail) = get_json(app, &format!("/api/connectors/{CONNECTOR}"), Some(&cookie)).await;
    let acct = detail["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "acct1")
        .cloned()
        .expect("acct1 listed");
    assert_eq!(
        acct["trust"], "approved",
        "the listed account is approved: {detail}"
    );

    let app = build_router(state.clone());
    let (status, probe) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/healthcheck/acct1"),
        serde_json::json!({}),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "workspace-less probe: {probe}");
    assert_eq!(probe["ok"], serde_json::json!(true), "probe: {probe}");

    let app = build_router(state.clone());
    let (status, call) = post_json(
        app,
        &format!("/api/connectors/{CONNECTOR}/call"),
        serde_json::json!({ "function": "list_items", "args": {}, "dry_run": true }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "workspace-less dry run: {call}");
    assert_eq!(
        call["url"],
        serde_json::json!("https://first.example.com/items"),
        "the default account of the listing renders: {call}"
    );

    // An account no reachable project configures is a 404, not a 400.
    let app = build_router(state.clone());
    let (status, _) = post_json(
        app,
        "/api/connectors/approve",
        serde_json::json!({ "name": CONNECTOR, "account": "nobody" }),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
        item: None,
        response_complete: None,
        pages: None,
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
        attempt: None,
        attempt_seq: None,
        intent: None,
        origin: None,
        tool_digest: None,
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
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}/stats"), None).await;
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
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}/stats"), None).await;
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
    let (status, json) = get_json(app, &format!("/api/connectors/{CONNECTOR}"), None).await;
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
        None,
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
        None,
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
        None,
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
        req.contains("authorization: Bearer secret-value"),
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
        None,
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
        None,
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
        None,
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

    let (status, json) = get_json(router(root.path()), "/api/connectors/available", None).await;
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

    let (_status, json) = get_json(router(root.path()), "/api/connectors/available", None).await;
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

    let (status, json) = get_json(router(root.path()), "/api/connectors", None).await;
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

    let (_status, json) = get_json(router(root.path()), "/api/connectors", None).await;
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

    let (status, json) = get_json(router(root.path()), "/api/connectors/github", None).await;
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

    let (status, json) = get_json(
        router(root.path()),
        &format!("/api/connectors/{CONNECTOR}"),
        None,
    )
    .await;
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
        None,
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

    let (status, json) = get_json(router(root.path()), "/api/connectors/github/stats", None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["connector"], "github");
    assert_eq!(json["calls"], serde_json::json!(0));
    assert!(json["by_function"].as_array().is_some_and(|a| a.is_empty()));
}

// --- MCP connectors ------------------------------------------------------------

const MCP_CONNECTOR: &str = "tracker-mcp";
const MCP_TOKEN_VAR: &str = "APB_SRV_MCP_TEST_TOKEN";

fn mcp_tools() -> serde_json::Value {
    serde_json::json!([{ "name": "whoami", "description": "Who", "inputSchema": { "type": "object" } }])
}

/// An installed MCP connector whose `command` is the stub, its project
/// account `work`, both approved; answers the stub.
fn install_mcp(cfg: &Path, root: &Path) -> apb_mcp_client::testing::StubServer {
    use apb_mcp_client::testing::{StubEra, StubServer};
    let stub = StubServer::write(&cfg.join("stub"), StubEra::Modern).unwrap();
    stub.set_tools(&mcp_tools()).unwrap();
    let program = std::fs::canonicalize(stub.program()).unwrap();
    let dir = cfg.join("connectors").join(MCP_CONNECTOR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        format!(
            "name: tracker-mcp\nversion: 0.1.0\nhealthcheck: whoami\nmcp:\n  transport: stdio\n  command: {}\n  env:\n    TRACKER_TOKEN: \"{{{{secret.token}}}}\"\naccount_fields:\n  - {{ name: token, required: true, secret: true }}\nfunctions:\n  - name: whoami\n    description: Who\n    read_only: true\n    mcp: {{ tool: whoami }}\n",
            // Quoted: a path holding ` #` or `: ` stays one YAML string.
            serde_json::to_string(&program.to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    let path = config::project_config_path(root, MCP_CONNECTOR);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        format!("accounts:\n  - name: work\n    default: true\n    token: \"{{{{env.{MCP_TOKEN_VAR}}}}}\"\n"),
    )
    .unwrap();
    stub
}

async fn post_raw(
    app: axum::Router,
    uri: &str,
    body: serde_json::Value,
    cookie: Option<&str>,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in auth_headers(cookie) {
        builder = builder.header(name, value);
    }
    let req = builder
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn the_dashboard_refuses_to_approve_an_mcp_connector() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let stub = install_mcp(cfg.path(), root.path());
    let trust_before = std::fs::read(cfg.path().join("trust.json")).ok();
    // Even a signed-in session, the one HTTP credential that approves the
    // other kinds, never approves an MCP connector.
    let (state, key) = keyed(AppState::new(root.path().to_path_buf()));
    let cookie = sign_in(&state, &key).await;
    for account in [serde_json::Value::Null, serde_json::json!("work")] {
        let (status, text) = post_raw(
            build_router(state.clone()),
            "/api/connectors/approve",
            serde_json::json!({ "name": MCP_CONNECTOR, "account": account }),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert!(text.starts_with("mcp_approve_needs_a_person:"), "{text}");
        assert!(!text.contains("sha256:"), "{text}");
    }
    // Without a session the person-only refusal comes first.
    let (status, text) = post_raw(
        build_router(AppState::new(root.path().to_path_buf())),
        "/api/connectors/approve",
        serde_json::json!({ "name": MCP_CONNECTOR }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert!(!text.contains("mcp_approve_needs_a_person"), "{text}");
    assert!(!text.contains("sha256:"), "{text}");
    assert_eq!(
        std::fs::read(cfg.path().join("trust.json")).ok(),
        trust_before
    );
    assert!(!cfg.path().join("mcp-tools.json").exists());
    assert!(stub.starts().is_empty());
}

/// Trusts the MCP connector's tree and the account `work` of `root`'s
/// project, as a person's approval at a terminal does before any pin.
fn trust_mcp(root: &Path) {
    let tree = apb_core::connector::store::load(MCP_CONNECTOR)
        .unwrap()
        .digest;
    let accounts = config::load_merged(root, MCP_CONNECTOR).unwrap();
    let account = config::account_digest(&accounts[0]);
    let mut trust = TrustStore::load();
    trust
        .approve_kind(
            &tree,
            MCP_CONNECTOR,
            Kind::Connector,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    trust
        .approve_kind(
            &account,
            &account_trust_id(MCP_CONNECTOR, "work"),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();
}

/// Records the pin of the account `work` of `root`'s project: the stub's
/// program and its one tool `whoami`, approved at time 1.
fn pin_mcp(root: &Path, stub: &apb_mcp_client::testing::StubServer) {
    use apb_core::connector::mcp::digest::{pin_digest, tool_digest};
    use apb_core::connector::mcp::pins::{PinStore, ServerPin, ToolPin};
    let tree = apb_core::connector::store::load(MCP_CONNECTOR)
        .unwrap()
        .digest;
    let accounts = config::load_merged(root, MCP_CONNECTOR).unwrap();
    let account = config::account_digest(&accounts[0]);
    let program = std::fs::canonicalize(stub.program()).unwrap();
    let program_s = program.to_string_lossy().to_string();
    let program_digest = apb_core::content::sha256_hex(&std::fs::read(&program).unwrap());
    let digest = tool_digest(&mcp_tools()[0]);
    let tools = std::collections::BTreeMap::from([("whoami".to_string(), digest.clone())]);
    PinStore::record(
        MCP_CONNECTOR,
        "work",
        ServerPin {
            pin_digest: pin_digest(&tree, &account, &program_s, &program_digest, &tools),
            connector_digest: tree,
            account_digest: account,
            command: program_s.clone(),
            program: program_s,
            program_digest,
            tools: std::collections::BTreeMap::from([(
                "whoami".to_string(),
                ToolPin {
                    digest,
                    description: "Who".to_string(),
                    members: std::collections::BTreeMap::new(),
                },
            )]),
            approved_at_ms: 1,
        },
    )
    .unwrap();
}

/// The credential a request to the dashboard carries.
enum Caller<'a> {
    /// No credential: a dashboard without keys.
    Keyless,
    /// `Authorization: Bearer <key>`.
    Bearer(&'a str),
    /// A signed-in browser session: the cookie and the CSRF marker.
    Session(&'a str),
}

/// A dashboard over `state` with server keys on: its key and the cookie of
/// a session signed in with it.
async fn keyed_with_session(state: AppState) -> (axum::Router, String, String) {
    let (state, key) = keyed(state);
    let cookie = sign_in(&state, &key).await;
    (build_router(state), key, cookie)
}

/// A POST with no body as `caller`: the status and the body as text.
async fn post_as(app: axum::Router, uri: &str, caller: Caller<'_>) -> (StatusCode, String) {
    let mut req = Request::builder().method("POST").uri(uri);
    match caller {
        Caller::Keyless => {}
        Caller::Bearer(key) => req = req.header("authorization", format!("Bearer {key}")),
        Caller::Session(cookie) => {
            req = req
                .header("cookie", cookie)
                .header(apb_server::auth::CSRF_HEADER, apb_server::auth::CSRF_VALUE)
        }
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn json_of(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// No `sha256:` and no run of 16 or more hexadecimal characters.
fn assert_no_digest(text: &str) {
    assert!(!text.contains("sha256:"), "{text}");
    let mut run = 0usize;
    for c in text.chars() {
        run = if c.is_ascii_hexdigit() { run + 1 } else { 0 };
        assert!(run < 16, "a run of hexadecimal characters: {text}");
    }
}

#[tokio::test]
async fn the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "srv-mcp-token-881");
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    pin_mcp(root.path(), &stub);
    let (app, _key, cookie) = keyed_with_session(AppState::new(root.path().to_path_buf())).await;
    let uri = format!("/api/connectors/{MCP_CONNECTOR}/healthcheck/work");
    let (status, text) = post_as(app.clone(), &uri, Caller::Session(&cookie)).await;
    let json = json_of(&text);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(true), "healthcheck: {json}");
    assert_eq!(json["mcp"]["tools_checked"], serde_json::json!(1), "{json}");
    assert!(stub.calls().is_empty(), "no tools/call");
    assert!(stub.methods().iter().any(|m| m == "tools/list"));
    // The same tool with another description no longer matches its pin.
    stub.set_tools(&serde_json::json!([{ "name": "whoami", "description": "Who else", "inputSchema": { "type": "object" } }]))
        .unwrap();
    let (status, text) = post_as(app, &uri, Caller::Session(&cookie)).await;
    let json = json_of(&text);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], serde_json::json!(false), "healthcheck: {json}");
    assert!(json.to_string().contains("mcp_tool_changed"), "{json}");
    assert!(stub.calls().is_empty(), "still no tools/call");
}

#[tokio::test]
async fn the_mcp_healthcheck_route_refuses_a_keyless_dashboard_and_a_bearer() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "srv-mcp-token-881");
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    pin_mcp(root.path(), &stub);
    let uri = format!("/api/connectors/{MCP_CONNECTOR}/healthcheck/work");
    let keyless = build_router(AppState::new(root.path().to_path_buf()));
    let (status, text) = post_as(keyless, &uri, Caller::Keyless).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "keyless: {text}");
    assert!(text.contains("sign in to the dashboard"), "{text}");
    assert_eq!(json_of(&text)["error"], "mcp_needs_a_person", "{text}");
    let (app, key, _cookie) = keyed_with_session(AppState::new(root.path().to_path_buf())).await;
    let (status, text) = post_as(app.clone(), &uri, Caller::Bearer(&key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "bearer: {text}");
    // The healthcheck takes the same rule from the engine, with the same
    // answer.
    let health = format!("/api/connectors/{MCP_CONNECTOR}/healthcheck/work");
    let keyless = build_router(AppState::new(root.path().to_path_buf()));
    for (who, app, caller) in [
        ("keyless", keyless, Caller::Keyless),
        ("bearer", app, Caller::Bearer(&key)),
    ] {
        let (status, text) = post_as(app, &health, caller).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {text}");
        assert_eq!(
            json_of(&text)["error"],
            "mcp_needs_a_person",
            "{who}: {text}"
        );
    }
    assert!(stub.starts().is_empty(), "nothing started");
}

#[tokio::test]
async fn the_connector_detail_carries_the_mcp_block() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    pin_mcp(root.path(), &stub);
    let (status, json) = get_json(
        build_router(AppState::new(root.path().to_path_buf())),
        &format!("/api/connectors/{MCP_CONNECTOR}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let mcp = &json["mcp"];
    assert_eq!(mcp["transport"], "stdio", "{json}");
    assert_eq!(mcp["protocol"], "auto", "{json}");
    let program = std::fs::canonicalize(stub.program()).unwrap();
    assert_eq!(mcp["command"], program.to_string_lossy().as_ref(), "{json}");
    assert_eq!(mcp["env"], serde_json::json!(["TRACKER_TOKEN"]), "{json}");
    assert_eq!(
        mcp["accounts"],
        serde_json::json!([{
            "workspace": "",
            "root_label": "",
            "account": "work",
            "approved_at_ms": 1,
            "functions": [{
                "function": "whoami",
                "tool": "whoami",
                "approved": true,
                "description": "Who",
            }],
        }]),
        "{json}"
    );
    assert!(mcp.get("pins_error").is_none(), "{json}");
    assert_no_digest(&mcp.to_string());
    assert!(stub.starts().is_empty(), "the detail starts nothing");

    // A pin store that does not read: fixed words, never the reader's
    // diagnostic or the store's path.
    let pins = cfg.path().join("mcp-tools.json");
    let kept = std::fs::read(&pins).unwrap();
    std::fs::write(&pins, "{ \"servers\": 7 }").unwrap();
    let (_, broken) = get_json(
        build_router(AppState::new(root.path().to_path_buf())),
        &format!("/api/connectors/{MCP_CONNECTOR}"),
        None,
    )
    .await;
    assert_eq!(
        broken["mcp"]["pins_error"],
        apb_core::connector::mcp::pins::PIN_STORE_UNREADABLE,
        "{broken}"
    );
    std::fs::write(&pins, kept).unwrap();

    // An http connector: the url template as written and the auth kind,
    // no command and no env.
    let dir = cfg.path().join("connectors").join("crm-mcp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        "name: crm-mcp\nversion: 0.1.0\nmcp:\n  transport: http\n  url: \"https://{{account.host}}/mcp\"\n  protocol: modern\nauth:\n  kind: header\n  header: X-Api-Key\n  value_template: \"{{secret.token}}\"\naccount_fields:\n  - { name: host, required: true }\n  - { name: token, required: true, secret: true }\nfunctions:\n  - name: search\n    description: Search\n    read_only: true\n    mcp: { tool: search }\n",
    )
    .unwrap();
    let (status, json) = get_json(
        build_router(AppState::new(root.path().to_path_buf())),
        "/api/connectors/crm-mcp",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let mcp = &json["mcp"];
    assert_eq!(mcp["transport"], "http", "{json}");
    assert_eq!(mcp["protocol"], "modern", "{json}");
    assert_eq!(mcp["url"], "https://{{account.host}}/mcp", "{json}");
    assert_eq!(mcp["auth"], "header", "{json}");
    assert!(mcp["command"].is_null(), "{json}");
    assert_eq!(mcp["env"], serde_json::json!([]), "{json}");
    assert_no_digest(&mcp.to_string());
}

#[tokio::test]
async fn the_mcp_tools_route_lists_live_rows() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "srv-mcp-token-881");
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    pin_mcp(root.path(), &stub);
    let (app, _key, cookie) = keyed_with_session(AppState::new(root.path().to_path_buf())).await;
    let (status, text) = post_as(
        app,
        &format!("/api/connectors/{MCP_CONNECTOR}/mcp-tools/work"),
        Caller::Session(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = json_of(&text);
    assert_eq!(json["ok"], true, "{json}");
    let listing = &json["listing"];
    assert_eq!(listing["connector"], MCP_CONNECTOR, "{json}");
    assert_eq!(listing["account"], "work", "{json}");
    let rows = listing["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "{json}");
    assert_eq!(rows[0]["function"], "whoami", "{json}");
    assert_eq!(rows[0]["tool"], "whoami", "{json}");
    assert_eq!(rows[0]["status"], "pinned", "{json}");
    assert_no_digest(&text);
    assert_eq!(stub.starts().len(), 1, "one start for the listing");
    assert!(stub.calls().is_empty(), "no tools/call");
}

#[tokio::test]
async fn the_mcp_tools_route_writes_no_pin() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "srv-mcp-token-881");
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    pin_mcp(root.path(), &stub);
    // A tool the record does not hold: a listing that pinned would add it.
    stub.set_tools(&serde_json::json!([
        { "name": "whoami", "description": "Who else", "inputSchema": { "type": "object" } },
        { "name": "extra", "description": "More", "inputSchema": { "type": "object" } },
    ]))
    .unwrap();
    let pins = cfg.path().join("mcp-tools.json");
    let before = std::fs::read(&pins).unwrap();
    let (app, _key, cookie) = keyed_with_session(AppState::new(root.path().to_path_buf())).await;
    let (status, text) = post_as(
        app,
        &format!("/api/connectors/{MCP_CONNECTOR}/mcp-tools/work"),
        Caller::Session(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = json_of(&text);
    assert_eq!(json["ok"], true, "{json}");
    assert_eq!(json["listing"]["rows"][0]["status"], "changed", "{json}");
    assert_eq!(
        std::fs::read(&pins).unwrap(),
        before,
        "the pins are as they were"
    );
}

#[tokio::test]
async fn the_mcp_tools_route_refuses_an_unapproved_account() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "srv-mcp-token-881");
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    let (app, _key, cookie) = keyed_with_session(AppState::new(root.path().to_path_buf())).await;
    let (status, text) = post_as(
        app,
        &format!("/api/connectors/{MCP_CONNECTOR}/mcp-tools/work"),
        Caller::Session(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = json_of(&text);
    assert_eq!(json["ok"], false, "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("mcp_server_unpinned")),
        "{json}"
    );
    assert_no_digest(&text);
    assert!(stub.starts().is_empty(), "nothing started");
    assert!(!cfg.path().join("mcp-tools.json").exists());
}

#[tokio::test]
async fn the_mcp_tools_route_refuses_a_keyless_dashboard_and_a_bearer() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), root.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "srv-mcp-token-881");
    let stub = install_mcp(cfg.path(), root.path());
    trust_mcp(root.path());
    pin_mcp(root.path(), &stub);
    let uri = format!("/api/connectors/{MCP_CONNECTOR}/mcp-tools/work");
    let keyless = build_router(AppState::new(root.path().to_path_buf()));
    let (status, text) = post_as(keyless, &uri, Caller::Keyless).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "keyless: {text}");
    assert!(
        text.contains("sign in to the dashboard, or run `apb connector tools` at a terminal"),
        "{text}"
    );
    assert_eq!(json_of(&text)["error"], "mcp_needs_a_person", "{text}");
    let (app, key, _cookie) = keyed_with_session(AppState::new(root.path().to_path_buf())).await;
    let (status, text) = post_as(app.clone(), &uri, Caller::Bearer(&key)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "bearer: {text}");
    // The healthcheck takes the same rule from the engine, with the same
    // answer.
    let health = format!("/api/connectors/{MCP_CONNECTOR}/healthcheck/work");
    let keyless = build_router(AppState::new(root.path().to_path_buf()));
    for (who, app, caller) in [
        ("keyless", keyless, Caller::Keyless),
        ("bearer", app, Caller::Bearer(&key)),
    ] {
        let (status, text) = post_as(app, &health, caller).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {text}");
        assert_eq!(
            json_of(&text)["error"],
            "mcp_needs_a_person",
            "{who}: {text}"
        );
    }
    assert!(stub.starts().is_empty(), "nothing started");
}

/// Design D5 wrong implementation 9: two registered projects, each with an
/// account `work` of different fields, only the first's approved. The
/// machine-wide detail shows both rows with their project; the live listing
/// runs only in the project its `workspace` names.
#[tokio::test]
async fn two_projects_with_the_same_account_name_are_two_rows() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let alpha = tempfile::tempdir().unwrap();
    let beta = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path(), alpha.path());
    let _g2 = setup(cfg.path(), beta.path());
    let _default = set_var("APB_TEST_DEFAULT_CONFIG_DIR", cfg.path());
    let _token = set_var(MCP_TOKEN_VAR, "alpha-token-118");
    let _beta_token = set_var("APB_SRV_MCP_TEST_TOKEN_BETA", "beta-token-229");
    let stub = install_mcp(cfg.path(), alpha.path());
    // The second project's `work` reads its token from another variable,
    // so its account digest differs.
    let path = config::project_config_path(beta.path(), MCP_CONNECTOR);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        "accounts:\n  - name: work\n    default: true\n    token: \"{{env.APB_SRV_MCP_TEST_TOKEN_BETA}}\"\n",
    )
    .unwrap();
    trust_mcp(alpha.path());
    // The second account is trusted too, so only its missing approval of
    // the server stops it.
    trust_mcp(beta.path());
    pin_mcp(alpha.path(), &stub);
    let (alpha_id, _reg) = register_workspace(alpha.path());
    let (beta_id, _reg2) = register_workspace(beta.path());
    let label = |root: &Path| {
        apb_core::projects::list_reachable()
            .into_iter()
            .find(|e| {
                e.workspace_id
                    == std::fs::read_to_string(root.join(".apb/workspace.local"))
                        .unwrap()
                        .trim()
            })
            .map(|e| e.name)
            .expect("a registered project")
    };
    let (alpha_label, beta_label) = (label(alpha.path()), label(beta.path()));

    let (status, json) = get_json(
        build_router(AppState::new_global()),
        &format!("/api/connectors/{MCP_CONNECTOR}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let rows = json["mcp"]["accounts"].as_array().expect("account rows");
    assert_eq!(rows.len(), 2, "{json}");
    let row = |id: &str| {
        rows.iter()
            .find(|r| r["workspace"] == id)
            .unwrap_or_else(|| panic!("a row of {id}: {json}"))
    };
    let (a, b) = (row(&alpha_id), row(&beta_id));
    assert_eq!(a["account"], "work");
    assert_eq!(b["account"], "work");
    assert_eq!(a["root_label"], alpha_label.as_str(), "{json}");
    assert_eq!(b["root_label"], beta_label.as_str(), "{json}");
    assert_eq!(a["functions"][0]["approved"], true, "{json}");
    assert_eq!(b["functions"][0]["approved"], false, "{json}");
    assert_eq!(b["approved_at_ms"], serde_json::Value::Null, "{json}");

    let (app, _key, cookie) = keyed_with_session(AppState::new_global()).await;
    let base = format!("/api/connectors/{MCP_CONNECTOR}/mcp-tools/work");
    // No project named: 400, nothing starts.
    let (status, text) = post_as(app.clone(), &base, Caller::Session(&cookie)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("name the project of the account"), "{text}");
    assert!(stub.starts().is_empty(), "nothing started");
    // The second project: its account differs from the approved one.
    let (status, text) = post_as(
        app.clone(),
        &format!("{base}?workspace={beta_id}"),
        Caller::Session(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = json_of(&text);
    assert_eq!(json["ok"], false, "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("mcp_server_changed")),
        "{json}"
    );
    assert!(stub.starts().is_empty(), "nothing started");
    // The first project: its server starts with its own account's token.
    let (status, text) = post_as(
        app,
        &format!("{base}?workspace={alpha_id}"),
        Caller::Session(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let json = json_of(&text);
    assert_eq!(json["ok"], true, "{json}");
    assert_eq!(stub.starts().len(), 1, "{json}");
    assert_eq!(
        stub.env_value_seen("TRACKER_TOKEN").as_deref(),
        Some("alpha-token-118")
    );
}
