//! Engine tests for the official `web-search` connector (openspec change
//! `google-connectors`, design D6). They drive the real, checked-in
//! `connectors/web-search/connector.yaml` through
//! `connector_call::execute` against an ephemeral one-shot HTTP server
//! (`common::spawn_http`), asserting:
//!
//! - the `X-Subscription-Token` header carries the resolved API key (the
//!   header never appears in the offline contract render, which only sees
//!   function headers plus the default User-Agent, so the live path is the
//!   one place this can be proven);
//! - a search result is projected to the result list: the picked fields
//!   survive (`query.more_results_available`, `web.results.*`), and nothing
//!   else of the response body does;
//! - the key echoed back in a response body is redacted to
//!   `[redacted:<ENV_VAR>]` (the resolved secret's redaction, labelled with
//!   the env var name) before the projection, so no raw key reaches the
//!   result, picked or not;
//! - the free plan's 429 maps to a `rate_limited` error carrying the
//!   service's `Retry-After` as `retry_after_sec`.

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector::call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const SECRET_VAR: &str = "APB_WEB_SEARCH_TEST_KEY";
const SECRET_VALUE: &str = "BSA-test-key-0123456789abcdef";
const NODE: &str = "n";
const CONNECTOR: &str = "web-search";

/// The real, checked-in connector manifest. Loading it (rather than an inline
/// copy) means a drift between this test and the shipped `connector.yaml` is
/// impossible.
fn web_search_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/web-search/connector.yaml")
        .canonicalize()
        .expect("repository connectors/web-search/connector.yaml must exist");
    std::fs::read_to_string(path).unwrap()
}

/// One granted account named `acct1`, `default`, whose `api_base` is
/// `base_url` and whose secret `api_key` field is backed by `SECRET_VAR`.
fn account(base_url: &str) -> ManifestAccount {
    ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("api_base".to_string(), base_url.to_string()),
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
            on_unknown: None,
        }],
    );
    manifest::write(run_dir, &m).unwrap();

    let cdir = run_dir.join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), web_search_yaml()).unwrap();
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

/// `verify` (the healthcheck) sends the fixed minimal query with the
/// `X-Subscription-Token` header carrying the resolved key, and asks for
/// JSON with the declared Accept header.
#[test]
fn verify_sends_subscription_token_header() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"query":{"original":"apb"},"web":{"type":"search","results":[{"title":"apb","url":"https://example.com/apb","description":"d"}]}}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "verify");

    let (value, ok) = call(run.path(), root.path(), "verify", serde_json::json!({}));
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /web/search?count=1&q=apb HTTP/1.1\r\n"),
        "verify must send the fixed minimal query: {req}"
    );
    // Header NAMES are lowercased on the wire by the HTTP client, so the
    // header check compares case-insensitively (the value keeps its case).
    let req_lower = req.to_ascii_lowercase();
    let expected_header = format!(
        "x-subscription-token: {}",
        SECRET_VALUE.to_ascii_lowercase()
    );
    assert!(
        req_lower.contains(&expected_header),
        "the subscription key must travel in the X-Subscription-Token header, got:\n{req}"
    );
    assert!(
        req_lower.contains("accept: application/json"),
        "the declared Accept header must reach the wire: {req}"
    );
    assert!(
        !req.contains("api_key="),
        "the key must not leak into the URL as a query pair: {req}"
    );
}

/// The spec's fresh-search scenario against the real manifest: the request
/// carries `freshness` and `count`, and the result is projected to the
/// result list, keeping the picked fields and dropping everything else
/// (here: the per-result `profile` block and `query.original`).
#[test]
fn search_projects_the_result_list() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        200,
        "OK",
        &[],
        r#"{"query":{"original":"release notes","more_results_available":true},"web":{"type":"search","results":[{"title":"Release notes 1.4","url":"https://example.com/releases/1.4","description":"What changed in 1.4.","age":"2 days ago","profile":{"name":"Example"}},{"title":"Changelog","url":"https://example.com/changelog","description":"The full changelog.","age":"5 days ago"}]}}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "search");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "search",
        serde_json::json!({ "q": "release notes", "freshness": "pw", "count": 5 }),
    );
    assert!(ok, "expected ok: {value}");

    let req = server.captured_request().expect("server saw a request");
    assert!(
        req.starts_with("GET /web/search?count=5&freshness=pw&q=release%20notes HTTP/1.1\r\n"),
        "freshness and count must reach the request, q percent-encoded: {req}"
    );

    assert_eq!(value["picked"], serde_json::json!(true));
    assert_eq!(
        value["body"],
        serde_json::json!({
            "query": { "more_results_available": true },
            "web": {
                "results": [
                    {
                        "title": "Release notes 1.4",
                        "url": "https://example.com/releases/1.4",
                        "description": "What changed in 1.4.",
                        "age": "2 days ago",
                    },
                    {
                        "title": "Changelog",
                        "url": "https://example.com/changelog",
                        "description": "The full changelog.",
                        "age": "5 days ago",
                    },
                ],
            },
        }),
        "the result must be exactly the projected list: {value}"
    );
    assert!(
        value.get("query").is_none() && value["body"].get("type").is_none(),
        "unpicked fields must not survive the projection: {value}"
    );
}

/// A response body that echoes the key (a misbehaving or hostile endpoint)
/// has every occurrence replaced with `[redacted:<ENV_VAR>]` before the
/// projection, so no raw key survives in the result, picked field or not.
#[test]
fn secret_echoed_in_body_is_redacted() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    // The key is echoed inside a picked field (the snippet) and in an
    // unpicked one (`query.original`), so both redaction and projection are
    // exercised in one call.
    let body = format!(
        r#"{{"query":{{"original":"leaked {SECRET_VALUE}","more_results_available":false}},"web":{{"results":[{{"title":"t","url":"https://example.com/x","description":"snippet echoes {SECRET_VALUE} verbatim","age":"1 day ago"}}]}}}}"#
    );
    let server = common::spawn_http(200, "OK", &[], body);
    seed_run(run.path(), &server.base_url, "search");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "search",
        serde_json::json!({ "q": "leak check" }),
    );
    assert!(ok, "expected ok: {value}");

    let text = value.to_string();
    assert!(
        !text.contains(SECRET_VALUE),
        "the raw key must not appear anywhere in the result: {text}"
    );
    let expected_redacted = format!("[redacted:{SECRET_VAR}]");
    assert_eq!(
        value["body"]["web"]["results"][0]["description"],
        serde_json::json!(format!("snippet echoes {expected_redacted} verbatim")),
        "the echoed key in a picked field must carry the env-var label: {value}"
    );
}

/// The free plan's rate ceiling: a 429 maps to `rate_limited` and carries
/// the service's `Retry-After` as `retry_after_sec`, so a run can back off.
#[test]
fn rate_limit_maps_to_rate_limited_with_retry_after() {
    let _lock = common::env_lock();
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    seed_secret(root.path());

    let server = common::spawn_http(
        429,
        "Too Many Requests",
        &[("Retry-After", "7")],
        r#"{"error":"Requests too fast"}"#.to_string(),
    );
    seed_run(run.path(), &server.base_url, "search");

    let (value, ok) = call(
        run.path(),
        root.path(),
        "search",
        serde_json::json!({ "q": "brave" }),
    );
    assert!(!ok, "a 429 must not count as success: {value}");
    assert_eq!(value["error"]["code"], serde_json::json!("rate_limited"));
    assert_eq!(
        value["error"]["retry_after_sec"],
        serde_json::json!(7),
        "the service Retry-After must reach the error: {value}"
    );
}
