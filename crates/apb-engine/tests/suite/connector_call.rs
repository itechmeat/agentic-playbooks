//! Task 13: the engine connector-call executor (`connector_call::execute`,
//! spec 2026-07-18-connectors-design section 6 step 4 + 6.1/6.2). These tests
//! drive `execute` against an ephemeral one-shot HTTP server
//! (`common::spawn_http`) and hand-built run snapshots (manifest + copied
//! `connector.yaml`). Secrets are resolved from a project `.apb/secrets.env`
//! under the temp root, so no process-env mutation is needed - but every test
//! still takes `common::env_lock()` because `secrets::resolve_var` reads the
//! process environment, which must not race another module's `set_var`.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_CC_TEST_TOKEN";
const SECRET_VALUE: &str = "super-secret-xyz";
const NODE: &str = "n";
const CONNECTOR: &str = "mock-tracker";

/// The snapshotted connector definition. Header auth uses the secret token;
/// functions cover a read (`list_items`), an echo (`echo`), a schema-guarded
/// write (`create_item`), and a mock (`ping`).
const CONNECTOR_YAML: &str = r#"
name: mock-tracker
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
  - name: echo
    description: Echo whatever the service returns
    method: GET
    url: "{{account.base_url}}/echo"
  - name: create_item
    description: Create an item
    method: POST
    url: "{{account.base_url}}/items"
    body: "{{args}}"
    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }
  - name: with_headers
    description: sends custom headers
    read_only: true
    method: GET
    url: "{{account.base_url}}/h"
    headers:
      X-Api-Version: "2024-01"
      Accept: application/vnd.test+json
  - name: ua_override
    description: overrides the default user agent
    read_only: true
    method: GET
    url: "{{account.base_url}}/u"
    headers:
      User-Agent: custom-agent/9
  - name: list_pick
    description: list with a projection
    read_only: true
    method: GET
    url: "{{account.base_url}}/pick"
    response_pick: [number, user.login, labels.name]
  - name: ping
    description: A canned mock, no network
    mock: { status: 200, body: { ok: true } }
"#;

/// One granted account named `acct1`, `default`, whose `base_url` is `base_url`
/// and whose secret `token` field is backed by `SECRET_VAR`.
fn account(base_url: &str) -> ManifestAccount {
    ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("base_url".to_string(), base_url.to_string()),
            ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
        ]),
        env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct".to_string(),
    }
}

/// Writes the run manifest (one connector, one grant for `NODE`) and the copied
/// connector snapshot into `run_dir`.
fn seed_run(
    run_dir: &Path,
    accounts: Vec<ManifestAccount>,
    granted_accounts: &[&str],
    functions: &[&str],
    max_calls: Option<u32>,
) {
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts,
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: granted_accounts.iter().map(|s| s.to_string()).collect(),
            functions: functions.iter().map(|s| s.to_string()).collect(),
            max_calls,
        }],
    );
    manifest::write(run_dir, &m).unwrap();

    let cdir = run_dir.join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), CONNECTOR_YAML).unwrap();
}

/// Writes the project secret so `resolve_var` finds it without touching the
/// process environment.
fn seed_secret(root: &Path) {
    let path = root.join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{SECRET_VAR}={SECRET_VALUE}\n")).unwrap();
}

/// A connector whose `token` secret is sourced from a command, echoed back by
/// the `echo` function so redaction can be asserted end to end. The command
/// itself is bound per-test through the manifest account's `cmd` map, so the
/// definition carries no path.
#[cfg(unix)]
const CMD_CONNECTOR_YAML: &str = r#"
name: cmd-conn
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
  - name: echo
    description: Echo whatever the service returns
    method: GET
    url: "{{account.base_url}}/echo"
"#;

#[cfg(unix)]
fn write_cmd_connector(run_dir: &Path) {
    let cdir = run_dir.join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("cmd-conn.yaml"), CMD_CONNECTOR_YAML).unwrap();
}

#[cfg(unix)]
fn cmd_account(base_url: &str, stub_path: &str) -> ManifestAccount {
    ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("base_url".to_string(), base_url.to_string()),
            ("token".to_string(), format!("{{{{cmd:{stub_path}}}}}")),
        ]),
        env: BTreeMap::new(),
        cmd: BTreeMap::from([("token".to_string(), stub_path.to_string())]),
        digest: "sha256:acct".to_string(),
    }
}

#[cfg(unix)]
fn write_stub(dir: &Path, name: &str, script: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn seed_cmd_run(run_dir: &Path, account: ManifestAccount) {
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: "cmd-conn".to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: "cmd-conn".to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["echo".to_string()],
            max_calls: None,
        }],
    );
    manifest::write(run_dir, &m).unwrap();
}

fn call<'a>(
    run_dir: &'a Path,
    root: &'a Path,
    function: &'a str,
    account: Option<&'a str>,
    args: serde_json::Value,
    dry_run: bool,
) -> (serde_json::Value, bool) {
    execute(CallRequest {
        run_dir,
        root,
        node_id: NODE,
        connector: CONNECTOR,
        function,
        account,
        args,
        dry_run,
        full: false,
    })
}

fn call_full<'a>(
    run_dir: &'a Path,
    root: &'a Path,
    function: &'a str,
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
        full: true,
    })
}

#[test]
fn ok_json_roundtrip_injects_auth_header() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"items":[1,2]}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok, "expected ok result: {value}");
    assert_eq!(value["ok"], serde_json::json!(true));
    assert_eq!(value["status"], serde_json::json!(200));
    assert_eq!(value["body"], serde_json::json!({"items": [1, 2]}));
    assert_eq!(value["truncated"], serde_json::json!(false));

    // The auth header was injected with the resolved secret value.
    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains(&format!("authorization: Bearer {SECRET_VALUE}")),
        "auth header missing/wrong in request:\n{req}"
    );

    // An `ok` ConnectorCall event was appended, with the pre-auth URL.
    let events = read_all(run.path()).unwrap();
    let call_events: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ConnectorCall {
                outcome,
                url,
                http_status,
                account,
                ..
            } => Some((outcome.clone(), url.clone(), *http_status, account.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(call_events.len(), 1);
    assert_eq!(call_events[0].0, "ok");
    assert!(
        call_events[0].1.ends_with("/items"),
        "event url should be the pre-auth URL: {}",
        call_events[0].1
    );
    assert_eq!(call_events[0].2, Some(200));
    assert_eq!(call_events[0].3, "acct1");
}

#[test]
fn unauthorized_maps_to_auth_error() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(401, "Unauthorized", &[], r#"{"error":"nope"}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(!ok);
    assert_eq!(value["ok"], serde_json::json!(false));
    assert_eq!(value["error"]["code"], serde_json::json!("auth"));
    assert_eq!(value["error"]["http_status"], serde_json::json!(401));
}

#[test]
fn rate_limited_carries_retry_after() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        429,
        "Too Many Requests",
        &[("Retry-After", "30")],
        r#"{"error":"slow down"}"#.to_string(),
    );
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("rate_limited"));
    assert_eq!(value["error"]["http_status"], serde_json::json!(429));
    assert_eq!(value["error"]["retry_after_sec"], serde_json::json!(30));
}

#[test]
fn redirect_maps_to_service_with_message() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        302,
        "Found",
        &[("Location", "https://evil.example/steal")],
        String::new(),
    );
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("service"));
    assert_eq!(value["error"]["http_status"], serde_json::json!(302));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("redirect"),
        "service message should mention redirects: {msg}"
    );
}

#[test]
fn oversized_body_is_truncated() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    // A body just over the 1 MiB cap, plain text so it round-trips as a string.
    let big = "a".repeat(1024 * 1024 + 100);
    let server = common::spawn_http(200, "OK", &[("Content-Type", "text/plain")], big);
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["truncated"], serde_json::json!(true));
    assert_eq!(value["body"].as_str().unwrap().len(), 1024 * 1024);
}

#[test]
fn mock_function_needs_no_server() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    // No secret needed and no server started.
    seed_run(
        run.path(),
        vec![account("https://unused.example")],
        &["acct1"],
        &["ping"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "ping",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok, "mock should succeed: {value}");
    assert_eq!(value["status"], serde_json::json!(200));
    assert_eq!(value["body"], serde_json::json!({"ok": true}));

    // A mock records an event with an empty pre-auth URL.
    let events = read_all(run.path()).unwrap();
    let url = events.iter().find_map(|e| match &e.payload {
        EventPayload::ConnectorCall { url, outcome, .. } if outcome == "ok" => Some(url.clone()),
        _ => None,
    });
    assert_eq!(url, Some(String::new()));
}

#[test]
fn dry_run_resolves_without_secrets_and_records_no_event() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    // Deliberately do NOT seed the secret: dry-run must not need it.
    seed_run(
        run.path(),
        vec![account("https://api.example")],
        &["acct1"],
        &["create_item"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "create_item",
        None,
        serde_json::json!({"title": "hi"}),
        true,
    );
    assert!(ok, "dry-run should succeed without secrets: {value}");
    assert_eq!(value["dry_run"], serde_json::json!(true));
    assert_eq!(value["method"], serde_json::json!("POST"));
    assert_eq!(value["url"], serde_json::json!("https://api.example/items"));
    assert_eq!(value["body"], serde_json::json!({"title": "hi"}));

    // A dry-run executes nothing, so no event is appended.
    let events = read_all(run.path()).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ConnectorCall { .. })),
        "dry-run must not append a ConnectorCall event"
    );
}

#[test]
fn invalid_args_fail_schema_validation() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_run(
        run.path(),
        vec![account("https://api.example")],
        &["acct1"],
        &["create_item"],
        None,
    );

    // `title` is required by the schema; omit it.
    let (value, ok) = call(
        run.path(),
        root.path(),
        "create_item",
        None,
        serde_json::json!({}),
        true,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("invalid_args"));
}

#[test]
fn unknown_function_is_permission_denied() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_run(
        run.path(),
        vec![account("https://api.example")],
        &["acct1"],
        &["list_items"],
        None,
    );

    // `create_item` is a real function but is not in the node's grant.
    let (value, ok) = call(
        run.path(),
        root.path(),
        "create_item",
        None,
        serde_json::json!({"title": "x"}),
        false,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("permission"));
}

#[test]
fn wrong_account_is_permission_denied() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_run(
        run.path(),
        vec![account("https://api.example")],
        &["acct1"],
        &["list_items"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        Some("acct2"),
        serde_json::json!({}),
        false,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("permission"));
}

#[test]
fn max_calls_budget_rejects_second_call() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"items":[]}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        Some(1),
    );

    // First call executes (consumes the one-shot server, appends one event).
    let (first, ok1) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok1, "first call should succeed: {first}");

    // Second call is rejected by the budget before any HTTP happens.
    let (second, ok2) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(!ok2);
    assert_eq!(second["error"]["code"], serde_json::json!("permission"));
    let msg = second["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("max_calls"),
        "message should name the budget: {msg}"
    );

    // Only the first (executed) call left an event; the rejection did not.
    let count = read_all(run.path())
        .unwrap()
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::ConnectorCall { .. }))
        .count();
    assert_eq!(count, 1, "the rejected call must not append an event");
}

#[test]
fn echoed_secret_is_redacted_in_result() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    // The service echoes the token back in its body (the common real leak).
    let body = format!(r#"{{"echo":"{SECRET_VALUE}"}}"#);
    let server = common::spawn_http(200, "OK", &[], body);
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["echo"],
        None,
    );

    let (value, ok) = call(
        run.path(),
        root.path(),
        "echo",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok, "expected ok: {value}");
    assert_eq!(
        value["body"]["echo"],
        serde_json::json!(format!("[redacted:{SECRET_VAR}]"))
    );
    assert!(
        !value.to_string().contains(SECRET_VALUE),
        "the raw secret leaked into the printed result: {value}"
    );
}

#[test]
fn transport_error_message_redacts_query_auth_secret() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    // Query-kind auth places the secret in the URL query string. A ureq
    // transport error's Display includes that URL, so the resolved secret must
    // be scrubbed from `error.message` before it is printed or logged.
    const Q_YAML: &str = r#"
name: q-conn
version: 0.1.0
auth:
  kind: query
  param: api_key
  value_template: "{{secret.token}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: list_items
    description: List items
    method: GET
    url: "{{account.base_url}}/items"
"#;
    let acct = ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        // Port 1 needs root to bind, so nothing listens: connect is refused
        // immediately and deterministically.
        fields: BTreeMap::from([
            ("base_url".to_string(), "http://127.0.0.1:1".to_string()),
            ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
        ]),
        env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct".to_string(),
    };
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: "q-conn".to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![acct],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: "q-conn".to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["list_items".to_string()],
            max_calls: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("q-conn.yaml"), Q_YAML).unwrap();

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "q-conn",
        function: "list_items",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("network"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(
        !msg.contains(SECRET_VALUE),
        "the resolved secret leaked into the error message: {msg}"
    );
    // If the request URL (with its query-auth param) reached the message at
    // all, it must have been redacted rather than dropped.
    if msg.contains("api_key") {
        assert!(
            msg.contains(&format!("[redacted:{SECRET_VAR}]")),
            "url present in message but the secret was not redacted: {msg}"
        );
    }
}

#[test]
fn max_calls_budget_is_per_connector() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();

    // Two connectors granted to one node, each with a budget of 1 and each
    // exposing a mock (no server, no secret). Exhausting one connector's budget
    // must not consume the other's.
    const OTHER_YAML: &str = r#"
name: other
version: 0.1.0
functions:
  - name: ping
    description: A canned mock, no network
    mock: { status: 200, body: { ok: true } }
"#;
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account("https://unused.example")],
    });
    m.connectors.push(ManifestConnector {
        name: "other".to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![ManifestAccount {
            name: "a".to_string(),
            default: true,
            fields: BTreeMap::new(),
            env: BTreeMap::new(),
            cmd: BTreeMap::new(),
            digest: "sha256:a".to_string(),
        }],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![
            ManifestConnectorGrant {
                connector: CONNECTOR.to_string(),
                accounts: vec!["acct1".to_string()],
                functions: vec!["ping".to_string()],
                max_calls: Some(1),
            },
            ManifestConnectorGrant {
                connector: "other".to_string(),
                accounts: vec!["a".to_string()],
                functions: vec!["ping".to_string()],
                max_calls: Some(1),
            },
        ],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), CONNECTOR_YAML).unwrap();
    std::fs::write(cdir.join("other.yaml"), OTHER_YAML).unwrap();

    // Exhaust mock-tracker's budget: first ping ok, second rejected.
    let (_v1, ok1) = call(
        run.path(),
        root.path(),
        "ping",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok1);
    let (v2, ok2) = call(
        run.path(),
        root.path(),
        "ping",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(!ok2, "second mock-tracker call should hit the budget: {v2}");
    assert_eq!(v2["error"]["code"], serde_json::json!("permission"));

    // The `other` connector's budget is independent and still allows a call.
    let (v3, ok3) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "other",
        function: "ping",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(
        ok3,
        "the other connector's budget must be independent: {v3}"
    );
    assert_eq!(v3["status"], serde_json::json!(200));
}

#[test]
fn path_auth_renders_token_into_path_segment_keeping_colon() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    // A token with a colon and url-reserved-adjacent bytes.
    let path = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("{SECRET_VAR}=111:AAA_bbb-CCC\n")).unwrap();

    let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
    const TELE_YAML: &str = r#"
name: tele
version: 0.1.0
auth:
  kind: path
  value_template: "bot{{secret.token}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: get_me
    description: probe
    read_only: true
    method: GET
    url: "{{account.base_url}}/{{auth}}/getMe"
"#;
    let acct = ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("base_url".to_string(), server.base_url.clone()),
            ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
        ]),
        env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct".to_string(),
    };
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: "tele".to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![acct],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: "tele".to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["get_me".to_string()],
            max_calls: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("tele.yaml"), TELE_YAML).unwrap();

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "tele",
        function: "get_me",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /bot111:AAA_bbb-CCC/getMe"),
        "path segment should keep ':' literal: {req}"
    );
    assert!(
        !req.contains("%3A"),
        "colon must not be percent-encoded: {req}"
    );

    // The event log keeps the literal {{auth}}, never the token.
    let url = read_all(run.path())
        .unwrap()
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ConnectorCall { url, .. } => Some(url.clone()),
            _ => None,
        })
        .unwrap();
    assert!(url.ends_with("/{{auth}}/getMe"), "pre-auth url: {url}");
    assert!(
        !url.contains("111:AAA"),
        "token leaked into event url: {url}"
    );
}

#[test]
fn path_auth_dry_run_keeps_auth_placeholder_and_needs_no_secret() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    const TELE_YAML: &str = r#"
name: tele
version: 0.1.0
auth:
  kind: path
  value_template: "bot{{secret.token}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: get_me
    description: probe
    read_only: true
    method: GET
    url: "{{account.base_url}}/{{auth}}/getMe"
"#;
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: "tele".to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![ManifestAccount {
            name: "acct1".to_string(),
            default: true,
            fields: BTreeMap::from([
                (
                    "base_url".to_string(),
                    "https://api.telegram.org".to_string(),
                ),
                ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
            ]),
            env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: "sha256:a".to_string(),
        }],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: "tele".to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["get_me".to_string()],
            max_calls: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("tele.yaml"), TELE_YAML).unwrap();

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "tele",
        function: "get_me",
        account: None,
        args: serde_json::json!({}),
        dry_run: true,
        full: false,
    });
    assert!(ok, "dry-run should succeed without a secret: {value}");
    assert_eq!(
        value["url"],
        serde_json::json!("https://api.telegram.org/{{auth}}/getMe")
    );
}

#[test]
fn sends_function_headers_and_default_user_agent() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());
    let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["with_headers"],
        None,
    );
    let (_v, ok) = call(
        run.path(),
        root.path(),
        "with_headers",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok);
    let req = server.captured_request().unwrap();
    assert!(
        req.contains("x-api-version: 2024-01"),
        "custom header missing: {req}"
    );
    assert!(
        req.contains("accept: application/vnd.test+json"),
        "accept missing: {req}"
    );
    assert!(
        req.contains("user-agent: apb/"),
        "default UA missing: {req}"
    );
}

#[test]
fn function_user_agent_overrides_the_default() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());
    let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["ua_override"],
        None,
    );
    let (_v, ok) = call(
        run.path(),
        root.path(),
        "ua_override",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok);
    let req = server.captured_request().unwrap();
    assert!(
        req.contains("user-agent: custom-agent/9"),
        "override missing: {req}"
    );
    assert!(
        !req.contains("user-agent: apb/"),
        "default UA must not also be sent: {req}"
    );
}

#[test]
fn response_link_header_surfaces_in_result() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());
    let link = r#"<https://api.example/next>; rel="next""#;
    let server = common::spawn_http(200, "OK", &[("Link", link)], r#"{"items":[]}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );
    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok, "{value}");
    assert_eq!(value["link"], serde_json::json!(link));
}

#[test]
fn absent_link_header_omits_the_field() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());
    let server = common::spawn_http(200, "OK", &[], r#"{"items":[]}"#.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_items"],
        None,
    );
    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok);
    assert!(value.get("link").is_none(), "link must be absent: {value}");
}

#[test]
fn response_pick_projects_by_default_and_full_bypasses_it() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());
    let raw = r#"[{"number":1,"title":"x","user":{"login":"octo","id":9},"labels":[{"name":"bug","color":"red"}]}]"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(
        run.path(),
        vec![account(&server.base_url)],
        &["acct1"],
        &["list_pick"],
        None,
    );

    // Default: projected body, picked: true.
    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_pick",
        None,
        serde_json::json!({}),
        false,
    );
    assert!(ok, "{value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!([
            { "number": 1, "user": { "login": "octo" }, "labels": [ { "name": "bug" } ] }
        ])
    );

    // --full: raw body, no picked marker. Needs a fresh one-shot server and a
    // fresh run dir (the run manifest is write-once).
    let run2 = tempfile::tempdir().unwrap();
    let server2 = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(
        run2.path(),
        vec![account(&server2.base_url)],
        &["acct1"],
        &["list_pick"],
        None,
    );
    let (full, ok2) = call_full(run2.path(), root.path(), "list_pick", serde_json::json!({}));
    assert!(ok2, "{full}");
    assert!(
        full.get("picked").is_none(),
        "full must not mark picked: {full}"
    );
    assert_eq!(full["body"][0]["title"], serde_json::json!("x"));
    assert_eq!(full["body"][0]["user"]["id"], serde_json::json!(9));
}

// --- command-sourced secrets (spec 4.1) --------------------------------

#[cfg(unix)]
#[test]
fn cmd_secret_is_resolved_and_injected_as_auth_header() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let stub = write_stub(
        stubs.path(),
        "ok-token",
        "#!/bin/sh\nprintf 'cmd-secret-42\\n'\n",
    );

    let server = common::spawn_http(200, "OK", &[], r#"{"ok":true}"#.to_string());
    write_cmd_connector(run.path());
    seed_cmd_run(run.path(), cmd_account(&server.base_url, &stub));

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "cmd-conn",
        function: "echo",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(ok, "expected ok: {value}");
    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains("authorization: Bearer cmd-secret-42"),
        "auth header wrong:\n{req}"
    );
}

#[cfg(unix)]
#[test]
fn cmd_secret_is_redacted_in_result_body() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let stub = write_stub(
        stubs.path(),
        "ok-token",
        "#!/bin/sh\nprintf 'cmd-secret-42\\n'\n",
    );

    // The service echoes the token back in its body.
    let server = common::spawn_http(200, "OK", &[], r#"{"echo":"cmd-secret-42"}"#.to_string());
    write_cmd_connector(run.path());
    seed_cmd_run(run.path(), cmd_account(&server.base_url, &stub));

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "cmd-conn",
        function: "echo",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(ok, "expected ok: {value}");
    assert_eq!(
        value["body"]["echo"],
        serde_json::json!("[redacted:cmd:token]")
    );
    assert!(
        !value.to_string().contains("cmd-secret-42"),
        "secret leaked: {value}"
    );
}

#[cfg(unix)]
#[test]
fn cmd_secret_nonzero_exit_is_config_error_naming_account_and_field() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let stub = write_stub(
        stubs.path(),
        "fail-token",
        "#!/bin/sh\necho 'gh: not logged in' >&2\nexit 4\n",
    );

    write_cmd_connector(run.path());
    seed_cmd_run(run.path(), cmd_account("https://unused.example", &stub));

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "cmd-conn",
        function: "echo",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("config"));
    let msg = value["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("acct1") && msg.contains("token"),
        "names account and field: {msg}"
    );
    assert!(
        msg.contains("not logged in"),
        "includes stderr excerpt: {msg}"
    );
}

#[cfg(unix)]
#[test]
fn cmd_secret_empty_output_is_config_error() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let stubs = tempfile::tempdir().unwrap();
    let stub = write_stub(stubs.path(), "empty-token", "#!/bin/sh\nexit 0\n");

    write_cmd_connector(run.path());
    seed_cmd_run(run.path(), cmd_account("https://unused.example", &stub));

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "cmd-conn",
        function: "echo",
        account: None,
        args: serde_json::json!({}),
        dry_run: false,
        full: false,
    });
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("config"));
}

#[cfg(unix)]
#[test]
fn dry_run_does_not_execute_cmd_secret() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let stubs = tempfile::tempdir().unwrap();
    // A stub that touches a sentinel iff it ever runs.
    let sentinel = stubs.path().join("ran.marker");
    let stub = write_stub(
        stubs.path(),
        "sentinel-token",
        &format!("#!/bin/sh\ntouch '{}'\nprintf 'x\\n'\n", sentinel.display()),
    );
    write_cmd_connector(run.path());
    seed_cmd_run(run.path(), cmd_account("https://api.example", &stub));

    let (value, ok) = execute(CallRequest {
        run_dir: run.path(),
        root: root.path(),
        node_id: NODE,
        connector: "cmd-conn",
        function: "echo",
        account: None,
        args: serde_json::json!({}),
        dry_run: true,
        full: false,
    });
    assert!(ok, "dry-run should render without secrets: {value}");
    assert!(
        !sentinel.exists(),
        "dry-run must not execute the secret command"
    );
}

// -- smtp end-to-end coverage through `execute` (slice 3, Task 7) --

/// An smtp connector snapshot named like `CONNECTOR` so the shared `call`
/// helper can drive it. Sends over the connection block (password from the
/// env-backed secret) with an individually-templated optional body.
const SMTP_YAML: &str = r#"
name: mock-tracker
version: 0.1.0
account_fields:
  - name: host
    required: true
  - name: port
    required: true
  - name: use_tls
  - name: from_email
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: send_email
    description: Send an email
    smtp:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        password: "{{secret.token}}"
      message:
        from_email: "{{account.from_email}}"
        to: "{{args.to}}"
        subject: "{{args.subject}}"
        body_text: "{{args.body_text}}"
    args_schema: { type: object, properties: { to: { type: string }, subject: { type: string } }, required: [to, subject] }
"#;

/// Writes an smtp run manifest (one connector, one grant for `NODE`) and the
/// copied smtp connector snapshot into `run_dir`.
fn seed_smtp_run(run_dir: &Path) {
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![ManifestAccount {
            name: "acct1".to_string(),
            default: true,
            fields: BTreeMap::from([
                ("host".to_string(), "smtp.example.com".to_string()),
                ("port".to_string(), "587".to_string()),
                ("use_tls".to_string(), "true".to_string()),
                ("from_email".to_string(), "a@b.c".to_string()),
                ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
            ]),
            env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: "sha256:acct".to_string(),
        }],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["send_email".to_string()],
            max_calls: None,
        }],
    );
    manifest::write(run_dir, &m).unwrap();
    let cdir = run_dir.join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), SMTP_YAML).unwrap();
}

#[test]
fn smtp_dry_run_renders_envelope_and_records_no_event() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    // No secret seeded: dry-run must not need it.
    seed_smtp_run(run.path());
    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_email",
        None,
        serde_json::json!({"to": "x@y.z", "subject": "Hi", "body_text": "T"}),
        true,
    );
    assert!(ok, "dry-run should succeed: {value}");
    assert_eq!(value["dry_run"], serde_json::json!(true));
    assert_eq!(value["envelope"]["from"], serde_json::json!("a@b.c"));
    assert_eq!(value["envelope"]["to"], serde_json::json!(["x@y.z"]));
    assert_eq!(value["envelope"]["subject"], serde_json::json!("Hi"));
    assert!(
        !value.to_string().contains("port"),
        "dry-run must not leak the connection block"
    );
    let events = read_all(run.path()).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ConnectorCall { .. }))
    );
}

#[test]
fn smtp_bad_address_is_invalid_args() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_smtp_run(run.path());
    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_email",
        None,
        serde_json::json!({"to": "not-an-email", "subject": "Hi", "body_text": "T"}),
        true,
    );
    assert!(!ok);
    assert_eq!(value["error"]["code"], serde_json::json!("invalid_args"));
}
