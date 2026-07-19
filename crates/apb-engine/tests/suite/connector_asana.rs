//! Task 6: render-path engine tests for the official `asana` connector,
//! mirroring the `connector_call.rs` mock-HTTP harness (spec Task 3 rendering
//! rules): a body string leaf or query value that is exactly one
//! `{{args.x}}` placeholder renders the typed JSON value and is dropped
//! (body field, or query pair) when the arg is absent or null. These tests
//! drive the real, checked-in `connectors/asana/connector.yaml` through
//! `connector_call::execute` against an ephemeral one-shot HTTP server
//! (`common::spawn_http`), so a change to the shipped manifest or to the Task
//! 3 renderer that breaks asana's typed/optional bodies fails here.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_ASANA_TEST_TOKEN";
const SECRET_VALUE: &str = "asana-secret-xyz";
const NODE: &str = "n";
const CONNECTOR: &str = "asana";

/// The real, checked-in connector manifest - loading it (rather than an
/// inline copy) means a drift between this test and the shipped
/// `connector.yaml` is impossible.
fn asana_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/asana/connector.yaml")
        .canonicalize()
        .expect("repository connectors/asana/connector.yaml must exist");
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
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), asana_yaml()).unwrap();
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

/// `create_task`'s body is wrapped under `data` and every optional field
/// (`notes`, `assignee`, `due_on`) that has no arg is absent entirely - not
/// present as JSON `null` - while the required `projects` field renders as a
/// one-element array from the single `{{args.project}}` placeholder.
#[test]
fn create_task_body_is_data_wrapped_and_typed() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(201, "Created", &[], r#"{"data":{"gid":"1"}}"#.to_string());
    seed_run(run.path(), &server.base_url, "create_task");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "create_task",
        serde_json::json!({"project": "1200", "name": "Ship it"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({
            "data": {
                "name": "Ship it",
                "projects": ["1200"],
            }
        }),
        "no absent optional key, projects is a single-element array: {body}"
    );
}

/// `update_task` with only `task` and `completed` set renders a body that
/// carries exactly `data.completed` - typed as a JSON boolean, not the
/// string `"true"` - and every other optional field is dropped.
#[test]
fn update_task_partial_body_drops_absent_fields() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"data":{"gid":"1"}}"#.to_string());
    seed_run(run.path(), &server.base_url, "update_task");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "update_task",
        serde_json::json!({"task": "1", "completed": true}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({ "data": { "completed": true } }),
        "only data.completed should render, typed as a bool: {body}"
    );
}

/// `search_tasks` renders the literal `resource_type=task` query pair (not a
/// placeholder) alongside the templated `query` argument, proving a plain
/// literal query value survives untouched next to a typed single-placeholder
/// one.
#[test]
fn typeahead_renders_literal_resource_type_and_query() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"data":[]}"#.to_string());
    seed_run(run.path(), &server.base_url, "search_tasks");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "search_tasks",
        serde_json::json!({"workspace": "1100", "query": "release"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /workspaces/1100/typeahead?"),
        "request line: {req}"
    );
    assert!(
        req.contains("resource_type=task"),
        "literal resource_type missing: {req}"
    );
    assert!(req.contains("query=release"), "query arg missing: {req}");
}

/// `list_workspaces` is read_only with `response_pick: [data.gid, data.name,
/// next_page.offset]`; a mock server answering an Asana-shaped page (a
/// `next_page` object alongside `data`) must survive the projection with
/// `next_page.offset` intact, since that offset is what a caller passes back
/// as the next `offset` argument (spec 5 pagination note).
#[test]
fn next_page_offset_survives_projection() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let raw = r#"{"data":[{"gid":"1","name":"W1","extra":"drop"}],"next_page":{"offset":"eyJ0eXAi","path":"/workspaces?offset=eyJ0eXAi","uri":"https://app.asana.com/x"}}"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(run.path(), &server.base_url, "list_workspaces");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_workspaces",
        serde_json::json!({"limit": 50}),
    );
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!({
            "data": [{ "gid": "1", "name": "W1" }],
            "next_page": { "offset": "eyJ0eXAi" }
        })
    );
}
