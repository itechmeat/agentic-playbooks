//! The ingest listener: its routes, its refusals, and the structural
//! guarantee that it cannot reach the dashboard API.
//!
//! Every test takes `crate::common::env_lock().await` because the connector
//! store, the account config and the inbox all resolve through
//! `APB_CONFIG_DIR`, which is process-wide.

use apb_server::ingest::{ACCEPT_RATE_PER_MIN, IngestState, MAX_BODY_BYTES, build_ingest_router};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use tower::ServiceExt;

/// The socket peer every request in this suite is served from.
const PEER: [u8; 4] = [127, 0, 0, 1];

const CONNECTOR: &str = "echo-hooks";
const ACCOUNT: &str = "main";
const SECRET_VAR: &str = "APB_INGEST_TEST_APP_SECRET";
const SECRET: &str = "app-secret-value";
const TOKEN_VAR: &str = "APB_INGEST_TEST_VERIFY_TOKEN";
const TOKEN: &str = "verify-token-value";

const CONNECTOR_YAML: &str = r#"
name: echo-hooks
version: 0.1.0
webhook:
  challenge: meta_hub
  verify_token: "{{secret.verify_token}}"
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: id
account_fields:
  - name: verify_token
    required: true
    secret: true
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending inbound events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
"#;

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

/// Installs the fixture connector, its global account, and the two secrets,
/// under a temp config dir. Returns the guards, which must be kept alive.
fn setup(cfg: &Path) -> Vec<EnvGuard> {
    let cdir = cfg.join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("connector.yaml"), CONNECTOR_YAML).unwrap();

    let adir = cfg.join("connector-config");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join(format!("{CONNECTOR}.yaml")),
        format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{TOKEN_VAR}}}}}\"\n    app_secret: \"{{{{env.{SECRET_VAR}}}}}\"\n"
        ),
    )
    .unwrap();

    vec![
        set_var("APB_CONFIG_DIR", cfg),
        set_var(SECRET_VAR, SECRET),
        set_var(TOKEN_VAR, TOKEN),
    ]
}

fn signed(body: &[u8]) -> String {
    format!(
        "sha256={}",
        apb_core::connector::webhook::hmac_sha256_hex(SECRET.as_bytes(), body)
    )
}

/// A state built the way the binary builds it. `new` returns a Result now,
/// because a malformed `server.trusted_proxies` is a startup error rather
/// than a silently empty proxy set; the temp config dirs here carry no such
/// section, so it always succeeds.
fn fresh_state() -> IngestState {
    IngestState::new().expect("ingest state from a clean temp config")
}

/// Serves one request with a `ConnectInfo` extension attached. `oneshot`
/// does not go through `into_make_service_with_connect_info`, so without this
/// the handlers' `ConnectInfo<SocketAddr>` extractor would fail and every
/// test would see a 500 instead of the status it asserts.
async fn send_from(app: axum::Router, mut req: Request<Body>, peer: [u8; 4]) -> Response<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 40000))));
    app.oneshot(req).await.unwrap()
}

async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let res = send_from(app, req, PEER).await;
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn post(body: &[u8], signature: Option<&str>) -> Request<Body> {
    let mut builder = Request::post(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
        .header("content-type", "application/json");
    if let Some(sig) = signature {
        builder = builder.header("X-Hub-Signature-256", sig);
    }
    builder.body(Body::from(body.to_vec())).unwrap()
}

fn inbox_events(cfg: &Path) -> Vec<apb_core::connector::inbox::InboxEvent> {
    apb_core::connector::inbox::Inbox::at(&cfg.join("connector-inbox"), CONNECTOR, ACCOUNT)
        .unwrap()
        .read_events()
        .unwrap()
}

#[tokio::test]
async fn a_signed_delivery_is_accepted_and_stored() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let app = build_ingest_router(fresh_state());

    let body = br#"{"id":"evt-1","text":"hello"}"#;
    let (status, text) = send(app, post(body, Some(&signed(body)))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.is_empty(), "the response body is empty: {text}");

    let events = inbox_events(cfg.path());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1);
    assert_eq!(
        events[0].provider_id, "evt-1",
        "the declared dedupe path is used"
    );
    assert_eq!(events[0].body["text"], "hello");
}

#[tokio::test]
async fn a_redelivery_is_acknowledged_but_stored_once() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let body = br#"{"id":"evt-1","text":"hello"}"#;
    let sig = signed(body);

    for _ in 0..3 {
        let app = build_ingest_router(fresh_state());
        let (status, _) = send(app, post(body, Some(&sig))).await;
        assert_eq!(status, StatusCode::OK, "a retry must not be refused");
    }
    assert_eq!(inbox_events(cfg.path()).len(), 1);
}

#[tokio::test]
async fn an_unsigned_or_wrongly_signed_delivery_is_a_flat_401() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let body = br#"{"id":"evt-1"}"#;

    for signature in [
        None,
        Some("sha256=0000000000000000000000000000000000000000000000000000000000000000"),
        Some("deadbeef"),
        Some(&*apb_core::connector::webhook::hmac_sha256_hex(
            b"other", body,
        )),
    ] {
        let app = build_ingest_router(fresh_state());
        let (status, text) = send(app, post(body, signature)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "signature {signature:?}");
        assert!(text.is_empty(), "the refusal carries no detail: {text}");
    }
    assert!(
        inbox_events(cfg.path()).is_empty(),
        "nothing is stored for a refused delivery"
    );

    // A signature over different bytes than were sent must not verify.
    let app = build_ingest_router(fresh_state());
    let (status, _) = send(app, post(br#"{"id":"evt-2"}"#, Some(&signed(body)))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_unknown_connector_or_account_is_a_flat_404() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let body = br#"{"id":"evt-1"}"#;
    let sig = signed(body);

    for path in [
        "/hooks/nope/main",
        "/hooks/echo-hooks/nope",
        "/hooks/..%2F..%2Fetc/main",
        "/hooks/echo-hooks/Not%20An%20Account",
    ] {
        let app = build_ingest_router(fresh_state());
        let req = Request::post(path)
            .header("X-Hub-Signature-256", &sig)
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, text) = send(app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "path {path}");
        assert!(text.is_empty(), "no detail is disclosed: {text}");
    }
}

#[tokio::test]
async fn an_oversize_body_is_refused_before_it_is_stored() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());

    let big = vec![b'x'; MAX_BODY_BYTES + 1];
    let app = build_ingest_router(fresh_state());
    let (status, _) = send(app, post(&big, Some(&signed(&big)))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(inbox_events(cfg.path()).is_empty());

    // A body at exactly the cap is still accepted, so the boundary is not off
    // by one. It has to be valid JSON to be stored.
    let filler = "y".repeat(MAX_BODY_BYTES - r#"{"id":"evt-1","pad":""}"#.len());
    let at_cap = format!("{{\"id\":\"evt-1\",\"pad\":\"{filler}\"}}").into_bytes();
    assert_eq!(at_cap.len(), MAX_BODY_BYTES);
    let app = build_ingest_router(fresh_state());
    let (status, _) = send(app, post(&at_cap, Some(&signed(&at_cap)))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(inbox_events(cfg.path()).len(), 1);
}

#[tokio::test]
async fn the_challenge_is_echoed_only_on_an_exact_token_match() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());

    let app = build_ingest_router(fresh_state());
    let uri = format!(
        "/hooks/{CONNECTOR}/{ACCOUNT}?hub.mode=subscribe&hub.verify_token={TOKEN}&hub.challenge=1158201444"
    );
    let res = send_from(app, Request::get(&uri).body(Body::empty()).unwrap(), PEER).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/plain"),
        "the challenge is echoed as plain text"
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(String::from_utf8_lossy(&bytes), "1158201444");

    for query in [
        "hub.mode=subscribe&hub.verify_token=wrong&hub.challenge=1",
        "hub.mode=unsubscribe&hub.verify_token=verify-token-value&hub.challenge=1",
        "hub.challenge=1",
        "",
    ] {
        let app = build_ingest_router(fresh_state());
        let (status, text) = send(
            app,
            Request::get(format!("/hooks/{CONNECTOR}/{ACCOUNT}?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "query `{query}`");
        assert!(text.is_empty(), "no detail is disclosed: {text}");
    }
}

#[tokio::test]
async fn a_connector_without_a_challenge_dialect_answers_404_to_get() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    // Rewrite the fixture without a challenge dialect.
    let stripped = CONNECTOR_YAML
        .replace("  challenge: meta_hub\n", "")
        .replace("  verify_token: \"{{secret.verify_token}}\"\n", "");
    std::fs::write(
        cfg.path()
            .join("connectors")
            .join(CONNECTOR)
            .join("connector.yaml"),
        stripped,
    )
    .unwrap();

    let app = build_ingest_router(fresh_state());
    let (status, _) = send(
        app,
        Request::get(format!(
            "/hooks/{CONNECTOR}/{ACCOUNT}?hub.mode=subscribe&hub.verify_token={TOKEN}&hub.challenge=1"
        ))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn healthz_answers_without_any_connector_installed() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guard = set_var("APB_CONFIG_DIR", cfg.path());
    let app = build_ingest_router(fresh_state());
    let (status, text) = send(app, Request::get("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["ok"], true);
}

#[tokio::test]
async fn the_per_account_accept_cap_drops_with_a_200() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    // One state across the whole burst: the window lives in the state, not
    // in the router.
    let state = fresh_state();

    for i in 0..(ACCEPT_RATE_PER_MIN + 5) {
        let body = format!("{{\"id\":\"evt-{i}\"}}").into_bytes();
        let app = build_ingest_router(state.clone());
        let (status, _) = send(app, post(&body, Some(&signed(&body)))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "over-cap traffic is dropped with a 200 so the provider stops retrying"
        );
    }
    assert_eq!(
        inbox_events(cfg.path()).len() as u32,
        ACCEPT_RATE_PER_MIN,
        "exactly the cap was stored"
    );
    assert_eq!(
        state.dropped(CONNECTOR, ACCOUNT),
        5,
        "the drops are counted"
    );
}

#[tokio::test]
async fn the_ingest_router_cannot_reach_the_dashboard_api() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());

    // A pinned list of dashboard surfaces. Every one of them must be absent
    // from the ingest router, whatever the method. This is the structural
    // guarantee the whole design rests on: pointing a tunnel at the ingest
    // port must be incapable of reaching anything that can run a playbook.
    for (method, path) in [
        ("GET", "/api/health"),
        ("GET", "/api/playbooks"),
        ("POST", "/api/playbooks"),
        ("POST", "/api/playbooks/x/run"),
        ("GET", "/api/connectors"),
        ("POST", "/api/connectors/x/call"),
        ("POST", "/api/connectors/approve"),
        ("GET", "/api/profiles"),
        ("GET", "/api/runs"),
        ("POST", "/api/hooks/run-1/secret"),
        ("GET", "/api/ws"),
        ("GET", "/"),
        ("GET", "/index.html"),
        ("GET", "/assets/index.js"),
    ] {
        let app = build_ingest_router(fresh_state());
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let (status, _) = send(app, req).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} must not exist on the ingest router"
        );
    }

    // And the three routes that do exist are the only ones that answer.
    let app = build_ingest_router(fresh_state());
    let (status, _) = send(app, Request::get("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let app = build_ingest_router(fresh_state());
    let (status, _) = send(
        app,
        Request::delete(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::METHOD_NOT_ALLOWED,
        "the hook path answers only GET and POST"
    );
}

#[tokio::test]
async fn the_failure_limiter_and_the_log_key_on_the_forwarded_ip_behind_a_trusted_proxy() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let body = br#"{"id":"evt-1"}"#;

    // The documented topology puts a TLS-terminating proxy on the same host,
    // so without the forwarded-IP derivation every delivery would key on the
    // proxy's loopback address and one bad sender would lock out every
    // provider.
    let proxy: IpAddr = "127.0.0.1".parse().unwrap();
    let state = IngestState::with_trusted_proxies(BTreeSet::from([proxy]));

    // Exhaust one forwarded sender's failure budget.
    for _ in 0..=apb_server::ingest::MAX_FAILURES_PER_WINDOW {
        let req = Request::post(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
            .header("X-Forwarded-For", "203.0.113.9")
            .header("X-Hub-Signature-256", "sha256=deadbeef")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let res = send_from(build_ingest_router(state.clone()), req, PEER).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // A different forwarded sender is unaffected: a good delivery from it
    // still succeeds, which is exactly what would break if the limiter keyed
    // on the proxy address.
    let req = Request::post(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
        .header("X-Forwarded-For", "198.51.100.4")
        .header("X-Hub-Signature-256", signed(body))
        .body(Body::from(body.to_vec()))
        .unwrap();
    let res = send_from(build_ingest_router(state.clone()), req, PEER).await;
    assert_eq!(res.status(), StatusCode::OK);

    // An untrusted peer's forwarded header is ignored: it keys on the socket
    // peer, so a caller cannot spoof its way out of its own budget.
    let untrusted = IngestState::with_trusted_proxies(BTreeSet::default());
    for i in 0..=apb_server::ingest::MAX_FAILURES_PER_WINDOW {
        let req = Request::post(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
            .header("X-Forwarded-For", format!("203.0.113.{i}"))
            .header("X-Hub-Signature-256", "sha256=deadbeef")
            .body(Body::from(body.to_vec()))
            .unwrap();
        send_from(build_ingest_router(untrusted.clone()), req, PEER).await;
    }
    let req = Request::post(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
        .header("X-Forwarded-For", "198.51.100.4")
        .header("X-Hub-Signature-256", signed(body))
        .body(Body::from(body.to_vec()))
        .unwrap();
    let res = send_from(build_ingest_router(untrusted), req, PEER).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "an untrusted peer keys on its socket address, whatever it forwards"
    );
}

// `common.rs` documents that any test mutating process env (here,
// `APB_CONFIG_DIR` via `set_var`) must hold `env_lock` for its duration, since
// this binary runs every test function as a thread in one process and cargo
// runs them in parallel by default. This test needs `#[tokio::test]` (rather
// than the brief's plain `#[test]`) so it can await that lock; without it the
// test raced `the_per_account_accept_cap_drops_with_a_200` and intermittently
// made every `resolve_target` call in that test see this test's temporary,
// connector-less config dir, turning its expected 200s into 404s.
#[tokio::test]
async fn a_malformed_trusted_proxy_list_is_a_startup_error() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guard = set_var("APB_CONFIG_DIR", cfg.path());
    std::fs::write(
        cfg.path().join("config.yaml"),
        "server:\n  trusted_proxies: [\"10.0.0.0/8\"]\n",
    )
    .unwrap();
    let err = IngestState::new().unwrap_err();
    assert!(
        err.contains("10.0.0.0/8"),
        "the error names the value: {err}"
    );
    assert!(!err.contains('!'), "no exclamation marks: {err}");
}

#[tokio::test]
async fn a_non_json_body_is_refused_after_the_signature_verifies() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let body = b"not json at all";
    let app = build_ingest_router(fresh_state());
    let (status, text) = send(app, post(body, Some(&signed(body)))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.is_empty());
    assert!(inbox_events(cfg.path()).is_empty());
}
