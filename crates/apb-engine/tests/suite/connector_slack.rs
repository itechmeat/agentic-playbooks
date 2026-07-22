//! Render-path engine tests for the official `slack` connector (spec
//! 2026-07-22-official-connectors-wave-3, section 5.1). Slack is the first
//! and motivating consumer of the connector-level `error_when` block
//! (section 4.2): its API reports failures as HTTP 200 with `"ok": false`,
//! so without reclassification a failed call would count as node success.
//! These tests drive the real, checked-in `connectors/slack/connector.yaml`
//! through `connector_call::execute` against an ephemeral one-shot HTTP
//! server (`common::spawn_http`), asserting:
//!
//! - the Authorization header carries `Bearer <token>` from the resolved
//!   secret;
//! - a 200 response with `ok: false` maps to a `service` error whose
//!   message carries the body's `error` string (the manifest's proof of
//!   `error_when`);
//! - an `ok: true` response passes through and projects the body-carried
//!   pagination cursor (`response_metadata.next_cursor`) via
//!   `response_pick`;
//! - `send_message` posts a JSON body with channel and text;
//! - `list_channels` drops absent optional query pairs.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_SLACK_TEST_TOKEN";
const SECRET_VALUE: &str = "xoxb-test-0123456789";
const NODE: &str = "n";
const CONNECTOR: &str = "slack";

/// The real, checked-in connector manifest. Loading it (rather than an inline
/// copy) means a drift between this test and the shipped `connector.yaml` is
/// impossible.
fn slack_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/slack/connector.yaml")
        .canonicalize()
        .expect("repository connectors/slack/connector.yaml must exist");
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
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), slack_yaml()).unwrap();
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

/// `auth_test` (healthcheck) must send `Authorization: Bearer <token>` with
/// the resolved secret, as a POST per Slack API convention while staying
/// `read_only: true` in the manifest.
#[test]
fn auth_test_sends_bearer_header_and_posts() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"ok":true,"url":"https://acme.slack.com/","team":"Acme","user":"bot","team_id":"T1","user_id":"U1"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "auth_test");

    let (value, ok) = call(run.path(), root.path(), "auth_test", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("POST /auth.test HTTP/1.1\r\n"),
        "auth.test is a POST by Slack API convention: {req}"
    );
    let expected_value = format!("Bearer {SECRET_VALUE}");
    assert!(
        req.contains(&expected_value),
        "expected Bearer auth value in request, got:\n{req}"
    );
}

/// The acceptance criterion of spec section 10 against the real manifest: a
/// Slack-style 200 response with `ok: false` produces a `service` error
/// carrying the body's `error` message, so retries and fallbacks trigger on
/// it instead of treating the call as a success.
#[test]
fn ok_false_reclassifies_to_service_error_with_message() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"ok":false,"error":"missing_scope"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "send_message");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_message",
        serde_json::json!({ "channel": "C123", "text": "hi" }),
    );
    assert!(!ok, "an ok:false body must not count as success: {value}");
    assert_eq!(value["error"]["code"], serde_json::json!("service"));
    let msg = value["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("missing_scope"),
        "the service error must carry Slack's error string verbatim: {msg}"
    );
}

/// An `ok: true` response passes through `error_when` untouched and projects
/// through `response_pick`, including the body-carried pagination cursor
/// (`response_metadata.next_cursor`, the asana pattern per spec section 5).
#[test]
fn list_channels_projects_cursor_through_response_pick() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"ok":true,"channels":[{"id":"C1","name":"general","is_private":false,"topic":{"value":"t"}}],"response_metadata":{"next_cursor":"dGVhbTpDMDI="}}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "list_channels");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_channels",
        serde_json::json!({}),
    );
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"]["response_metadata"]["next_cursor"],
        serde_json::json!("dGVhbTpDMDI="),
        "the pagination cursor must survive the projection: {value}"
    );
    assert_eq!(
        value["body"]["channels"],
        serde_json::json!([{ "id": "C1", "name": "general", "is_private": false }]),
        "channels must project to exactly the picked fields: {value}"
    );
}

/// `send_message` posts a JSON body carrying channel and text; Slack accepts
/// JSON bodies with Bearer auth on `chat.postMessage`.
#[test]
fn send_message_posts_json_body() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"ok":true,"channel":"C123","ts":"1721745600.000100"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "send_message");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_message",
        serde_json::json!({ "channel": "C123", "text": "release is green" }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("POST /chat.postMessage HTTP/1.1\r\n"),
        "request line: {req}"
    );
    let body_start = req.find("\r\n\r\n").expect("header/body separator") + 4;
    let body: serde_json::Value = serde_json::from_str(&req[body_start..])
        .unwrap_or_else(|e| panic!("body must be JSON: {e}: {req}"));
    assert_eq!(
        body,
        serde_json::json!({ "channel": "C123", "text": "release is green" })
    );
}

/// `reply_in_thread` posts to the same endpoint with `thread_ts` in the body,
/// so a grant can allow thread replies without allowing top-level posts.
#[test]
fn reply_in_thread_posts_thread_ts_in_body() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"ok":true,"channel":"C123","ts":"1721745601.000200"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "reply_in_thread");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "reply_in_thread",
        serde_json::json!({
            "channel": "C123",
            "thread_ts": "1721745600.000100",
            "text": "ack",
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body_start = req.find("\r\n\r\n").expect("header/body separator") + 4;
    let body: serde_json::Value = serde_json::from_str(&req[body_start..])
        .unwrap_or_else(|e| panic!("body must be JSON: {e}: {req}"));
    assert_eq!(
        body,
        serde_json::json!({
            "channel": "C123",
            "text": "ack",
            "thread_ts": "1721745600.000100",
        })
    );
}

/// `get_messages` declares optional `cursor` and `limit` query pairs, each a
/// single `{{args.*}}` placeholder, so absent args drop the pair entirely and
/// only the required `channel` renders.
#[test]
fn get_messages_drops_absent_optional_query_pairs() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"ok":true,"messages":[],"has_more":false}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "get_messages");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "get_messages",
        serde_json::json!({ "channel": "C123" }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /conversations.history?channel=C123 HTTP/1.1\r\n"),
        "absent cursor/limit must not render query pairs: {req}"
    );
}
