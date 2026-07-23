//! ST7: render-path engine tests for the official `zulip` connector (spec
//! 2026-07-22-official-connectors-wave-3, section 5.2). Zulip is the first
//! connector with HTTP Basic auth and the first real consumer of `body_form`
//! (section 4.1). These tests drive the real, checked-in
//! `connectors/zulip/connector.yaml` through `connector_call::execute` against
//! an ephemeral one-shot HTTP server (`common::spawn_http`), asserting:
//!
//! - the Authorization header is Basic with base64(email:api_key) of the
//!   resolved values;
//! - `send_stream_message` puts an `application/x-www-form-urlencoded`
//!   content-type and a correctly percent-encoded form body on the wire;
//! - `get_messages` drops absent anchor/num_before/narrow query pairs;
//! - a read-only function (`get_me`) projects through its `response_pick`.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_ZULIP_TEST_KEY";
const SECRET_VALUE: &str = "0123456789abcdef";
const EMAIL: &str = "bot@zulip.example";
/// base64("bot@zulip.example:0123456789abcdef").
const BASIC_TOKEN: &str = "Ym90QHp1bGlwLmV4YW1wbGU6MDEyMzQ1Njc4OWFiY2RlZg==";
const NODE: &str = "n";
const CONNECTOR: &str = "zulip";

/// The real, checked-in connector manifest. Loading it (rather than an inline
/// copy) means a drift between this test and the shipped `connector.yaml` is
/// impossible.
fn zulip_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/zulip/connector.yaml")
        .canonicalize()
        .expect("repository connectors/zulip/connector.yaml must exist");
    std::fs::read_to_string(path).unwrap()
}

/// One granted account named `acct1`, `default`, whose `api_base` is
/// `base_url`, whose `email` is the test address, and whose secret `api_key`
/// field is backed by `SECRET_VAR`.
fn account(base_url: &str) -> ManifestAccount {
    ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("api_base".to_string(), base_url.to_string()),
            ("email".to_string(), EMAIL.to_string()),
            ("api_key".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
        ]),
        env: BTreeMap::from([("api_key".to_string(), SECRET_VAR.to_string())]),
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
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), zulip_yaml()).unwrap();
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

/// The raw body of a captured HTTP request (the bytes after the blank line
/// terminating the headers), preserving the exact wire encoding.
fn captured_body(raw: &str) -> &str {
    raw.split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or_else(|| panic!("no header/body separator in captured request:\n{raw}"))
}

/// `get_me` (healthcheck) must authenticate with HTTP Basic auth. The engine
/// renders `Authorization: Basic base64(email:api_key)` where the username is
/// the resolved `email` account field and the password is the resolved
/// `api_key` secret. Zulip is the first connector exercising the `Basic` auth
/// variant end to end against the wire.
#[test]
fn get_me_sends_basic_auth_header_with_resolved_credentials() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"user_id":12,"full_name":"Bot","email":"bot@zulip.example","is_bot":true}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "get_me");

    let (value, ok) = call(run.path(), root.path(), "get_me", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    // The engine sends header name "Authorization", but ureq 3 lowercases
    // header names on the wire. The base64 token is case-sensitive, so match
    // the value exactly and the header name case-insensitively.
    let expected_value = format!("Basic {BASIC_TOKEN}");
    assert!(
        req.contains(&expected_value),
        "expected Basic auth value `{expected_value}` in request, got:\n{req}"
    );
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("authorization:"),
        "expected an authorization header in request, got:\n{req}"
    );
}

/// `get_me` declares `response_pick: [user_id, full_name, email, is_bot]`, so a
/// Zulip-shaped response survives the projection with exactly those fields and
/// nothing else.
#[test]
fn get_me_projects_through_response_pick() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"user_id":12,"full_name":"Bot","email":"bot@zulip.example","is_bot":true,"extra":"drop","timezone":""}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "get_me");

    let (value, ok) = call(run.path(), root.path(), "get_me", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!({
            "user_id": 12,
            "full_name": "Bot",
            "email": "bot@zulip.example",
            "is_bot": true,
        })
    );
}

/// `send_stream_message` posts via `body_form` (spec section 4.1): the wire
/// body is `application/x-www-form-urlencoded` with keys percent-encoded in
/// BTreeMap (alphabetical) order. A `content` value containing a space and an
/// ampersand must be correctly percent-encoded so the assertion pins the
/// encoded wire form. This is the manifest's proof of the `body_form` feature.
#[test]
fn send_stream_message_posts_form_urlencoded_body_with_special_chars() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"id":9001,"msg":""}"#.to_string());
    seed_run(run.path(), &server.base_url, "send_stream_message");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_stream_message",
        serde_json::json!({
            "to": "general",
            "topic": "release",
            "content": "hello world & café=bar",
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("POST /messages HTTP/1.1\r\n"),
        "request line: {req}"
    );
    assert!(
        req.to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "expected urlencoded content-type header, got:\n{req}"
    );
    // BTreeMap order: content, to, topic, type. Space -> %20, & -> %26,
    // = -> %3D, non-ASCII (e) -> %C3%A9.
    let expected_body =
        "content=hello%20world%20%26%20caf%C3%A9%3Dbar&to=general&topic=release&type=stream";
    assert_eq!(
        captured_body(&req),
        expected_body,
        "raw wire body must be correctly percent-encoded"
    );
}

/// `send_direct_message` also uses `body_form`, with `type=direct` and no
/// `topic` pair. A simple content value verifies the literal `direct` type and
/// the pair ordering without special characters.
#[test]
fn send_direct_message_posts_form_urlencoded_body() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"id":9002,"msg":""}"#.to_string());
    seed_run(run.path(), &server.base_url, "send_direct_message");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_direct_message",
        serde_json::json!({ "to": "12", "content": "ping" }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "expected urlencoded content-type header, got:\n{req}"
    );
    // BTreeMap order: content, to, type. No topic pair for direct messages.
    assert_eq!(
        captured_body(&req),
        "content=ping&to=12&type=direct",
        "direct-message form body must omit topic and carry type=direct"
    );
}

/// `get_messages` declares three optional query pairs (`anchor`, `num_before`,
/// `narrow`), each a single `{{args.*}}` placeholder. When all three args are
/// absent, the wave-2 single-placeholder rule drops every pair, so the URL
/// carries no query string at all.
#[test]
fn get_messages_drops_absent_query_pairs() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"messages":[]}"#.to_string());
    seed_run(run.path(), &server.base_url, "get_messages");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "get_messages",
        serde_json::json!({}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /messages HTTP/1.1\r\n"),
        "no query pairs should render when all args are absent: {req}"
    );
    assert!(
        !req.contains("?"),
        "absent anchor/num_before/narrow must not produce a query string: {req}"
    );
}

/// `get_messages` with all three args present renders them as percent-encoded
/// query pairs, proving the same optional pairs that drop when absent render
/// typed and encoded when provided.
#[test]
fn get_messages_renders_present_query_pairs() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"{"messages":[]}"#.to_string());
    seed_run(run.path(), &server.base_url, "get_messages");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "get_messages",
        serde_json::json!({
            "anchor": "100",
            "num_before": 10,
            "narrow": "[{\"operator\": \"channel\", \"operand\": \"general\"}]",
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    // BTreeMap key order: anchor, narrow, num_before.
    assert!(
        req.contains(
            "GET /messages?anchor=100&narrow=%5B%7B%22operator%22%3A%20%22channel%22%2C%20%22operand%22%3A%20%22general%22%7D%5D&num_before=10"
        ),
        "present query pairs must render percent-encoded in BTreeMap order: {req}"
    );
}
