//! Render-path engine tests for the official `youtrack` connector: drives
//! the real, checked-in `connectors/youtrack/connector.yaml` through
//! `connector_call::execute` against an ephemeral one-shot HTTP server
//! (`common::spawn_http`). These pin the binding's render discipline:
//! the Bearer auth header reaches the wire with the resolved secret, every
//! GET carries the literal `fields=` query value, `$skip`/`$top` query keys
//! are percent-encoded (`%24`) and drop when absent, and `apply_command`
//! renders the issues array through the single typed placeholder.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_YOUTRACK_TEST_TOKEN";
const SECRET_VALUE: &str = "yt-secret-xyz";
const NODE: &str = "n";
const CONNECTOR: &str = "youtrack";

/// The real, checked-in connector manifest: loading it (rather than an inline
/// copy) means drift between this test and the shipped `connector.yaml` is
/// impossible.
fn youtrack_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/youtrack/connector.yaml")
        .canonicalize()
        .expect("repository connectors/youtrack/connector.yaml must exist");
    std::fs::read_to_string(path).unwrap()
}

/// One granted account named `acct1` (default), whose `api_base` is
/// `base_url` and whose secret `token` is backed by `SECRET_VAR`.
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
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), youtrack_yaml()).unwrap();
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

/// The Bearer auth header reaches the wire carrying the resolved secret value
/// (not the `{{env.*}}` reference), proving the header auth block resolves
/// `{{secret.token}}` through the env-backed secret store.
#[test]
fn bearer_auth_header_reaches_wire_with_resolved_secret() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"id":"me"}"#.to_string());
    seed_run(run.path(), &server.base_url, "get_me");

    let (value, ok) = call(run.path(), root.path(), "get_me", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.to_ascii_lowercase().contains(&format!(
            "authorization: bearer {secret}\r\n",
            secret = SECRET_VALUE.to_lowercase()
        )),
        "Bearer header with resolved secret missing: {req}"
    );
}

/// Every GET carries the literal `fields=` query value matching its
/// `response_pick`. `search_issues` with no pagination args renders exactly
/// `fields=...&query=...` with no `$skip`/`$top` pairs, proving both the
/// fields discipline and the absent-pair drop rule.
#[test]
fn fields_query_present_on_gets_and_skip_top_drop_when_absent() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "search_issues");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "search_issues",
        serde_json::json!({"query": "release"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /issues?"),
        "request line targets /issues with a query: {req}"
    );
    assert!(
        req.contains("fields=idReadable,summary,resolved,project(shortName),reporter(login)"),
        "literal fields= value missing: {req}"
    );
    assert!(req.contains("query=release"), "query arg missing: {req}");
    assert!(
        !req.contains("%24skip") && !req.contains("%24top"),
        "$skip/$top must be absent when no pagination args: {req}"
    );
}

/// `search_issues` with pagination args renders `$skip`/`$top` as
/// percent-encoded keys (`%24skip`, `%24top`) alongside the fields and query
/// pairs, pinning the exact on-the-wire form YouTrack accepts.
#[test]
fn skip_top_render_as_percent_encoded_keys_when_present() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "search_issues");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "search_issues",
        serde_json::json!({"query": "state: Fixed", "skip": 10, "top": 5}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains("%24skip=10"),
        "$skip must render as %24skip=10: {req}"
    );
    assert!(
        req.contains("%24top=5"),
        "$top must render as %24top=5: {req}"
    );
    assert!(
        req.contains("query=state%3A%20Fixed"),
        "query value must be percent-encoded: {req}"
    );
}

/// `list_projects` proves the fields discipline on a second GET function with
/// a different projection (`id,name,shortName`), so the fields= value is
/// asserted on more than one endpoint shape.
#[test]
fn list_projects_carries_its_fields_projection() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "list_projects");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_projects",
        serde_json::json!({}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /admin/projects?fields=id,name,shortName"),
        "list_projects must carry its literal fields value: {req}"
    );
}

/// `apply_command` renders the `issues` array through the single typed
/// placeholder, preserving object structure inside each element, rather than
/// stringifying the array or dropping nested fields.
#[test]
fn apply_command_renders_issues_array_through_typed_placeholder() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"id":"c1"}"#.to_string());
    seed_run(run.path(), &server.base_url, "apply_command");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "apply_command",
        serde_json::json!({
            "query": "state Fixed",
            "issues": [
                {"idReadable": "DEMO-1"},
                {"idReadable": "DEMO-2"}
            ]
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("POST /commands "),
        "request line targets /commands: {req}"
    );
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({
            "query": "state Fixed",
            "issues": [
                {"idReadable": "DEMO-1"},
                {"idReadable": "DEMO-2"}
            ]
        }),
        "issues array must render typed with nested objects intact: {body}"
    );
}

/// `create_issue` renders the nested `project: {id: ...}` body and drops the
/// optional `description` field when absent, proving the nested body
/// discipline carries through on the YouTrack create path.
#[test]
fn create_issue_renders_nested_project_and_drops_absent_description() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"idReadable":"DEMO-7"}"#.to_string());
    seed_run(run.path(), &server.base_url, "create_issue");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "create_issue",
        serde_json::json!({"project": "0-0", "summary": "Build fails on main"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({
            "project": {"id": "0-0"},
            "summary": "Build fails on main"
        }),
        "project.id must render nested, description dropped when absent: {body}"
    );
}

/// `get_issue` is read_only with a `response_pick` projecting
/// `idReadable, summary, resolved, project.shortName, reporter.login`; a mock
/// server answering a YouTrack-shaped object with extra fields must survive
/// the projection with only the picked fields intact.
#[test]
fn get_issue_response_pick_projects_tight_fields() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let raw = r#"{"idReadable":"DEMO-42","summary":"Bug","resolved":null,"number":99,"project":{"shortName":"DEMO","name":"Demo"},"reporter":{"login":"alice","name":"Alice"},"extra":"drop"}"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(run.path(), &server.base_url, "get_issue");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "get_issue",
        serde_json::json!({"id": "DEMO-42"}),
    );
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!({
            "idReadable": "DEMO-42",
            "summary": "Bug",
            "resolved": null,
            "project": {"shortName": "DEMO"},
            "reporter": {"login": "alice"}
        })
    );
}
