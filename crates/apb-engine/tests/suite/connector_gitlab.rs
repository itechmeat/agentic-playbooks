//! Render-path engine tests for the official `gitlab` connector.
//! Drives the real, checked-in `connectors/gitlab/connector.yaml` through
//! `connector_call::execute` against an ephemeral one-shot HTTP server
//! (`common::spawn_http`). Covers PRIVATE-TOKEN header injection, page/per_page
//! query pair dropping, and update_issue optional body field dropping.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_GITLAB_TEST_TOKEN";
const SECRET_VALUE: &str = "gitlab-pat-secret-xyz";
const NODE: &str = "n";
const CONNECTOR: &str = "gitlab";

/// The real, checked-in connector manifest - loading it (rather than an
/// inline copy) means a drift between this test and the shipped
/// `connector.yaml` is impossible.
fn gitlab_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/gitlab/connector.yaml")
        .canonicalize()
        .expect("repository connectors/gitlab/connector.yaml must exist");
    std::fs::read_to_string(path).unwrap()
}

/// One granted account named `acct1`, `default`, whose `api_base` is
/// `base_url` and whose secret `token` field is backed by `SECRET_VAR`.
fn account(base_url: &str) -> ManifestAccount {
    ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("api_base".to_string(), base_url.to_string()),
            ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
        ]),
        env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct".to_string(),
    }
}

/// Writes the run manifest (one connector, one grant for `NODE` covering
/// `function`) and the copied `connector.yaml` snapshot into `run_dir`.
fn seed_run(run_dir: &Path, base_url: &str, function: &str) {
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account(base_url)],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec![function.to_string()],
            max_calls: None,
        }],
    );
    manifest::write(run_dir, &m).unwrap();

    let cdir = run_dir.join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), gitlab_yaml()).unwrap();
}

/// Writes the project secret so `resolve_var` finds it without touching the
/// process environment.
fn seed_secret(root: &Path) {
    let path = root.join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{SECRET_VAR}={SECRET_VALUE}\n")).unwrap();
}

fn call(
    run_dir: &Path,
    root: &Path,
    function: &str,
    args: serde_json::Value,
) -> (serde_json::Value, bool) {
    execute(CallRequest {
        run_dir,
        root,
        node_id: NODE,
        connector: CONNECTOR,
        function,
        account: None,
        args,
        dry_run: false,
        full: false,
    })
}

/// Isolates the JSON body from a captured raw HTTP request (head + body, as
/// `common::spawn_http` records it): the body starts right after the blank
/// line terminating the headers.
fn captured_body_json(raw: &str) -> serde_json::Value {
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or_else(|| panic!("no header/body separator in captured request:\n{raw}"));
    serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("captured request body is not valid JSON ({e}):\n{body}"))
}

/// First request line (method + path + HTTP version) from a captured raw
/// request.
fn request_line(raw: &str) -> &str {
    raw.lines()
        .next()
        .unwrap_or_else(|| panic!("empty captured request:\n{raw}"))
}

/// The `PRIVATE-TOKEN` header reaches the wire with the resolved secret
/// value (not the env placeholder), proving header auth for GitLab PATs.
#[test]
fn private_token_header_reaches_wire_with_resolved_secret() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"id":1,"username":"pat-user","name":"Pat User"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "get_user");

    let (value, ok) = call(run.path(), root.path(), "get_user", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    // ureq lowercases header names on the wire.
    assert!(
        req.to_ascii_lowercase()
            .contains(&format!("private-token: {SECRET_VALUE}").to_ascii_lowercase()),
        "PRIVATE-TOKEN header missing or secret not resolved:\n{req}"
    );
    assert!(
        !req.contains(&format!("{{{{env.{SECRET_VAR}}}}}")),
        "unresolved env placeholder leaked into the request:\n{req}"
    );
}

/// `list_issues` with only `project` set drops `page` and `per_page` (and
/// every other optional filter) from the query string entirely.
#[test]
fn list_issues_pagination_pairs_drop_when_absent() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "list_issues");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_issues",
        serde_json::json!({"project": "42"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let line = request_line(&req);
    assert!(
        line.starts_with("GET /projects/42/issues"),
        "unexpected request line: {line}"
    );
    assert!(
        !line.contains("page=") && !line.contains("per_page="),
        "absent pagination pairs must drop: {line}"
    );
    assert!(
        !line.contains("state=") && !line.contains("labels="),
        "absent filter pairs must drop: {line}"
    );
    // No query string at all when every optional pair is dropped.
    assert!(
        !line.contains('?'),
        "expected no query string when only project is set: {line}"
    );
}

/// `list_issues` with `page` and `per_page` set renders both pairs on the
/// wire as typed query values.
#[test]
fn list_issues_pagination_pairs_render_when_present() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "list_issues");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_issues",
        serde_json::json!({"project": "42", "page": 2, "per_page": 10}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let line = request_line(&req);
    assert!(
        line.starts_with("GET /projects/42/issues?"),
        "unexpected request line: {line}"
    );
    assert!(line.contains("page=2"), "page missing: {line}");
    assert!(line.contains("per_page=10"), "per_page missing: {line}");
}

/// `update_issue` with only `state_event` set renders a body that carries
/// exactly that field - every other optional label/assignee/title field is
/// dropped, not present as JSON null.
#[test]
fn update_issue_optional_label_fields_drop_when_absent() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"iid":7,"title":"t","state":"closed"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "update_issue");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "update_issue",
        serde_json::json!({"project": "42", "iid": 7, "state_event": "close"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({ "state_event": "close" }),
        "only state_event should render; labels/add_labels/remove_labels/assignee_ids must drop: {body}"
    );
}

/// A `group/project` path form for `project` is percent-encoded into the
/// URL path as `group%2Fproject` by the engine (do not pre-encode the slash).
#[test]
fn project_path_form_is_percent_encoded_in_url() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "list_issues");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_issues",
        serde_json::json!({"project": "group/project"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let line = request_line(&req);
    assert!(
        line.starts_with("GET /projects/group%2Fproject/issues"),
        "group/project must render as group%2Fproject in the path: {line}"
    );
}

/// `list_issues` response_pick projects the agent-facing fields from a
/// GitLab-shaped issue object and drops everything else.
#[test]
fn list_issues_response_pick_projects_tight_fields() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let raw = r#"[{
        "id": 99,
        "iid": 7,
        "title": "Broken build",
        "state": "opened",
        "labels": ["bug", "ci"],
        "web_url": "https://gitlab.com/acme/site/-/issues/7",
        "author": {"id": 1, "username": "alice", "name": "Alice"},
        "description": "drop me",
        "confidential": false
    }]"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(run.path(), &server.base_url, "list_issues");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_issues",
        serde_json::json!({"project": "42"}),
    );
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!([{
            "iid": 7,
            "title": "Broken build",
            "state": "opened",
            "labels": ["bug", "ci"],
            "web_url": "https://gitlab.com/acme/site/-/issues/7",
            "author": { "username": "alice" }
        }])
    );
}

/// `trigger_pipeline` with variables renders the GitLab-shaped array of
/// `{key, value}` objects as a typed JSON body field.
#[test]
fn trigger_pipeline_body_carries_typed_variables_array() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        201,
        "Created",
        &[],
        r#"{"id":1,"status":"pending","ref":"main"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "trigger_pipeline");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "trigger_pipeline",
        serde_json::json!({
            "project": "42",
            "ref": "main",
            "variables": [{"key": "DEPLOY_ENV", "value": "staging"}]
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({
            "ref": "main",
            "variables": [{"key": "DEPLOY_ENV", "value": "staging"}]
        }),
        "variables must pass through as a typed array: {body}"
    );
}
