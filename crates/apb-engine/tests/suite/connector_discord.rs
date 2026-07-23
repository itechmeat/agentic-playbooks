//! Render-path engine tests for the official `discord` connector.
//! Drives the real, checked-in `connectors/discord/connector.yaml` through
//! `connector_call::execute` against an ephemeral one-shot HTTP server
//! (`common::spawn_http`), so a change to the shipped manifest or to the
//! renderer that breaks Discord's Bot-prefix auth, before/limit drop, or
//! nested `message_reference` body fails here.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector_call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_DISCORD_TEST_TOKEN";
const SECRET_VALUE: &str = "discord-bot-token-xyz";
const NODE: &str = "n";
const CONNECTOR: &str = "discord";

/// The real, checked-in connector manifest - loading it (rather than an
/// inline copy) means a drift between this test and the shipped
/// `connector.yaml` is impossible.
fn discord_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/discord/connector.yaml")
        .canonicalize()
        .expect("repository connectors/discord/connector.yaml must exist");
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
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), discord_yaml()).unwrap();
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

/// The Authorization header on the wire is exactly `Bot <resolved-secret>`
/// (the Bot prefix lives inside the value_template, not a separate scheme).
#[test]
fn auth_header_is_bot_prefixed_secret() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"id":"1","username":"bot","bot":true}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "get_me");

    let (value, ok) = call(run.path(), root.path(), "get_me", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.contains(&format!("authorization: Bot {SECRET_VALUE}")),
        "Authorization must be exactly `Bot <token>`, got:\n{req}"
    );
    // Not Bearer and not the bare token alone as the full header value.
    assert!(
        !req.to_ascii_lowercase()
            .contains(&format!("authorization: bearer {SECRET_VALUE}")),
        "must not use Bearer scheme: {req}"
    );
}

/// `get_messages` with only `channel_id` drops the optional `before` and
/// `limit` query pairs entirely (single-placeholder drop rule).
#[test]
fn get_messages_drops_absent_before_and_limit() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "get_messages");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "get_messages",
        serde_json::json!({"channel_id": "111"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let request_line = req.lines().next().unwrap_or("");
    assert!(
        request_line.starts_with("GET /channels/111/messages"),
        "request line: {request_line}"
    );
    assert!(
        !request_line.contains("before="),
        "absent before must drop: {request_line}"
    );
    assert!(
        !request_line.contains("limit="),
        "absent limit must drop: {request_line}"
    );
}

/// `get_messages` with both pagination args renders them as query pairs.
#[test]
fn get_messages_propagates_before_and_limit() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(200, "OK", &[], r#"[]"#.to_string());
    seed_run(run.path(), &server.base_url, "get_messages");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "get_messages",
        serde_json::json!({
            "channel_id": "111",
            "limit": 25,
            "before": "999888777"
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let request_line = req.lines().next().unwrap_or("");
    assert!(
        request_line.starts_with("GET /channels/111/messages?"),
        "request line: {request_line}"
    );
    assert!(
        request_line.contains("before=999888777"),
        "before missing: {request_line}"
    );
    assert!(
        request_line.contains("limit=25"),
        "limit missing: {request_line}"
    );
}

/// `reply_to_message` renders a nested `message_reference` object with the
/// resolved `message_id` inside, plus the required `content` field.
#[test]
fn reply_to_message_renders_nested_message_reference() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"id":"2","content":"ack","timestamp":"t"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "reply_to_message");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "reply_to_message",
        serde_json::json!({
            "channel_id": "111",
            "message_id": "222333",
            "content": "ack"
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let request_line = req.lines().next().unwrap_or("");
    assert!(
        request_line.starts_with("POST /channels/111/messages"),
        "request line: {request_line}"
    );
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({
            "content": "ack",
            "message_reference": {
                "message_id": "222333"
            }
        }),
        "nested message_reference must render literally: {body}"
    );
}

/// `send_message` posts `{content}` only (no message_reference).
#[test]
fn send_message_body_is_content_only() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"id":"3","content":"hi","timestamp":"t"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "send_message");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "send_message",
        serde_json::json!({
            "channel_id": "111",
            "content": "hi"
        }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let body = captured_body_json(&req);
    assert_eq!(
        body,
        serde_json::json!({ "content": "hi" }),
        "send_message body should be content only: {body}"
    );
}

/// `get_me` is read_only with a tight response_pick; extra fields drop.
#[test]
fn get_me_response_pick_projects_identity() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let raw = r#"{"id":"99","username":"apb-bot","bot":true,"discriminator":"0000","flags":0}"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(run.path(), &server.base_url, "get_me");

    let (value, ok) = call(run.path(), root.path(), "get_me", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!({
            "id": "99",
            "username": "apb-bot",
            "bot": true
        })
    );
}

/// `list_channels` path-substitutes guild_id and projects channel fields.
#[test]
fn list_channels_path_and_response_pick() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let raw =
        r#"[{"id":"c1","name":"general","type":0,"parent_id":null,"position":1,"nsfw":false}]"#;
    let server = common::spawn_http(200, "OK", &[], raw.to_string());
    seed_run(run.path(), &server.base_url, "list_channels");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "list_channels",
        serde_json::json!({"guild_id": "g1"}),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    let request_line = req.lines().next().unwrap_or("");
    assert!(
        request_line.starts_with("GET /guilds/g1/channels"),
        "request line: {request_line}"
    );
    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!([{
            "id": "c1",
            "name": "general",
            "type": 0,
            "parent_id": null,
            "position": 1
        }])
    );
}
