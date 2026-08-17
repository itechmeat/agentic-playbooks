//! The ingest listener: its routes, its refusals, and the structural
//! guarantee that it cannot reach the dashboard API.
//!
//! Every test takes `crate::common::env_lock().await` because the connector
//! store, the account config and the inbox all resolve through
//! `APB_CONFIG_DIR`, which is process-wide.

use apb_server::ingest::{
    ACCEPT_RATE_PER_MIN, IngestState, MAX_BODY_BYTES, MAX_FAILURES_PER_WINDOW, build_ingest_router,
};
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

/// A connector directory that is itself a symlink resolving outside
/// `connectors_dir()` must be refused, not read. This is the containment
/// check `load_connector_doc` restores (mirroring `store::load`'s
/// defense-in-depth): the name is already a validated `[a-z0-9-]` slug, but a
/// symlink planted at `connectors_dir()/<name>` could still point anywhere.
#[cfg(unix)]
#[tokio::test]
async fn a_connector_symlinked_outside_the_connectors_root_is_refused() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();

    // A fully valid webhook manifest, but planted outside the connectors
    // root and only reachable through a symlink.
    let real_dir = outside.path().join(CONNECTOR);
    std::fs::create_dir_all(&real_dir).unwrap();
    std::fs::write(real_dir.join("connector.yaml"), CONNECTOR_YAML).unwrap();

    let connectors_dir = cfg.path().join("connectors");
    std::fs::create_dir_all(&connectors_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, connectors_dir.join(CONNECTOR)).unwrap();

    let adir = cfg.path().join("connector-config");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join(format!("{CONNECTOR}.yaml")),
        format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{TOKEN_VAR}}}}}\"\n    app_secret: \"{{{{env.{SECRET_VAR}}}}}\"\n"
        ),
    )
    .unwrap();
    let _guards = [
        set_var("APB_CONFIG_DIR", cfg.path()),
        set_var(SECRET_VAR, SECRET),
        set_var(TOKEN_VAR, TOKEN),
    ];

    let body = br#"{"id":"evt-1"}"#;
    let app = build_ingest_router(fresh_state());
    let (status, text) = send(app, post(body, Some(&signed(body)))).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a connector dir symlinked outside connectors_dir() must not resolve"
    );
    assert!(text.is_empty(), "no detail is disclosed: {text}");
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
    // in the router. The accept window is anchored at the first accept
    // (rolling, the same shape the failure limiter uses), not at a calendar
    // minute boundary, so this burst gets exactly one budget regardless of
    // when in the real minute it happens to run.
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
    assert_eq!(
        apb_core::connector::inbox::Inbox::at(
            &cfg.path().join("connector-inbox"),
            CONNECTOR,
            ACCOUNT
        )
        .unwrap()
        .dropped_count()
        .unwrap(),
        5,
        "the persisted counter is the operator-visible source of truth, not just the in-process one"
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

    // Which address the budget landed on is observed through the challenge
    // handshake, not through a delivery: a valid signature is accepted
    // whatever the limiter thinks (see
    // `a_tripped_failure_limiter_still_accepts_a_validly_signed_delivery`),
    // so a POST cannot tell a blocked address from an allowed one. A GET
    // carries no signature, so the limiter is the only thing standing between
    // a correct token and its echo.
    let challenge = format!("hub.mode=subscribe&hub.verify_token={TOKEN}&hub.challenge=1158201444");
    let blocked = Request::get(format!("/hooks/{CONNECTOR}/{ACCOUNT}?{challenge}"))
        .header("X-Forwarded-For", "203.0.113.9")
        .body(Body::empty())
        .unwrap();
    let res = send_from(build_ingest_router(state.clone()), blocked, PEER).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the budget landed on the forwarded sender"
    );

    // A different forwarded sender is unaffected, which is exactly what would
    // break if the limiter keyed on the proxy address: one bad sender behind
    // a same-host proxy would lock out every provider.
    let other = Request::get(format!("/hooks/{CONNECTOR}/{ACCOUNT}?{challenge}"))
        .header("X-Forwarded-For", "198.51.100.4")
        .body(Body::empty())
        .unwrap();
    let res = send_from(build_ingest_router(state.clone()), other, PEER).await;
    assert_eq!(res.status(), StatusCode::OK);

    // And a delivery from the blocked forwarded sender still lands, because
    // it is validly signed.
    let req = Request::post(format!("/hooks/{CONNECTOR}/{ACCOUNT}"))
        .header("X-Forwarded-For", "203.0.113.9")
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
    let req = Request::get(format!("/hooks/{CONNECTOR}/{ACCOUNT}?{challenge}"))
        .header("X-Forwarded-For", "198.51.100.4")
        .body(Body::empty())
        .unwrap();
    let res = send_from(build_ingest_router(untrusted), req, PEER).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "an untrusted peer keys on its socket address, whatever it forwards"
    );
}

/// The failure limiter guards the rejection path only (spec
/// 2026-08-16-webhook-ingest-design: "a tripped limiter never rejects a
/// validly signed delivery").
///
/// This is the property the documented topology depends on. Behind a
/// same-host TLS proxy with `server.trusted_proxies` unset, every delivery is
/// attributed to the proxy's loopback address, so eleven bad-signature POSTs
/// from anywhere on the internet would otherwise block every provider for the
/// rest of the window and the events would be lost past the provider's retry
/// budget.
#[tokio::test]
async fn a_tripped_failure_limiter_still_accepts_a_validly_signed_delivery() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let body = br#"{"id":"evt-1"}"#;
    let state = fresh_state();

    // Exhaust the failure budget with bad signatures.
    for _ in 0..=apb_server::ingest::MAX_FAILURES_PER_WINDOW {
        let app = build_ingest_router(state.clone());
        let (status, _) = send(app, post(body, Some("sha256=deadbeef"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let app = build_ingest_router(state.clone());
    let (status, text) = send(app, post(body, Some(&signed(body)))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a valid signature is accepted whatever the limiter thinks of this address"
    );
    assert!(text.is_empty());
    assert_eq!(
        inbox_events(cfg.path()).len(),
        1,
        "and the delivery is actually stored"
    );

    // What the tripped limiter does buy: a request from the same address that
    // carries nothing which could verify is refused without resolving a
    // secret or hashing anything.
    for signature in [None, Some("sha256=deadbeef"), Some("not-even-prefixed")] {
        let app = build_ingest_router(state.clone());
        let (status, text) = send(app, post(br#"{"id":"evt-2"}"#, signature)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "signature {signature:?}");
        assert!(text.is_empty(), "the refusal carries no detail: {text}");
    }
    assert_eq!(
        inbox_events(cfg.path()).len(),
        1,
        "nothing more was stored for the blocked client"
    );
}

/// The Critical of the final review: a webhook secret that resolves to the
/// empty string must authenticate nobody.
///
/// An env var that exists but is empty (`APB_INGEST_TEST_APP_SECRET=`) makes
/// `resolve_var` return `Some("")`, which every layer above it reads as a
/// successfully resolved secret. `HMAC-SHA256` takes a zero-length key, so
/// without a guard the correct signature is one any caller on the internet
/// can compute.
#[tokio::test]
async fn an_empty_resolved_secret_authenticates_nobody() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let _empty = set_var(SECRET_VAR, "");

    let body = br#"{"id":"evt-1"}"#;
    let forged = format!(
        "sha256={}",
        apb_core::connector::webhook::hmac_sha256_hex(b"", body)
    );
    for signature in [
        Some(forged.as_str()),
        Some("sha256=0000000000000000000000000000000000000000000000000000000000000000"),
        None,
    ] {
        let app = build_ingest_router(fresh_state());
        let (status, text) = send(app, post(body, signature)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "an empty secret is a flat refusal, signature {signature:?}"
        );
        assert!(text.is_empty(), "no detail is disclosed: {text}");
    }
    assert!(
        inbox_events(cfg.path()).is_empty(),
        "nothing is stored under an empty secret"
    );
}

/// The accept cap is charged for a genuinely new delivery only.
///
/// Charging it before the dedupe decision let a provider's retry storm on one
/// message spend the whole per-minute budget, after which new deliveries were
/// dropped with a 200 the provider reads as "accepted" and never retries:
/// permanent, silent loss. The spec and SECURITY.md both describe the cap as
/// bounding accepted appends, and this is what makes that true.
#[tokio::test]
async fn a_retry_storm_on_one_message_does_not_spend_the_accept_budget() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());
    let state = fresh_state();

    let first = br#"{"id":"evt-1"}"#;
    let sig = signed(first);
    // Far more redeliveries of one message than the whole budget.
    for _ in 0..(ACCEPT_RATE_PER_MIN + 50) {
        let app = build_ingest_router(state.clone());
        let (status, _) = send(app, post(first, Some(&sig))).await;
        assert_eq!(status, StatusCode::OK, "a retry is acknowledged");
    }
    assert_eq!(
        state.dropped(CONNECTOR, ACCOUNT),
        0,
        "a duplicate is not a drop: it appends nothing, so it costs nothing"
    );

    // A genuinely new delivery still lands, which is the whole point.
    let second = br#"{"id":"evt-2"}"#;
    let app = build_ingest_router(state.clone());
    let (status, _) = send(app, post(second, Some(&signed(second)))).await;
    assert_eq!(status, StatusCode::OK);
    let events = inbox_events(cfg.path());
    assert_eq!(events.len(), 2, "the new delivery was stored, not dropped");
    assert_eq!(events[1].provider_id, "evt-2");
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

/// The webhook secret is resolved at most once per account within the cache
/// TTL, not on every request. Without the cache a flooding client sending
/// well-formed but bogus signatures would re-run a `{{cmd:...}}` secret
/// (spawn_blocking, up to 10s) per request. Uses a secret command that appends
/// one byte to a counter file each time it runs, then asserts it ran once
/// across several distinct deliveries that all reach signature verification.
#[cfg(unix)]
#[tokio::test]
async fn the_webhook_secret_is_resolved_at_most_once_within_the_ttl() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();

    let cdir = cfg.path().join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("connector.yaml"), CONNECTOR_YAML).unwrap();

    // The secret command appends a byte to this file on every invocation.
    let counter = cfg.path().join("resolve-count");
    let cmd_secret = "cmd-secret-value";
    let adir = cfg.path().join("connector-config");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join(format!("{CONNECTOR}.yaml")),
        format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"tok\"\n    app_secret: \"{{{{cmd:sh -c 'printf x >> {counter}; printf %s {cmd_secret}'}}}}\"\n",
            counter = counter.display(),
        ),
    )
    .unwrap();
    let _cfg_guard = set_var("APB_CONFIG_DIR", cfg.path());

    // One shared state, so its secret cache persists across deliveries.
    let state = fresh_state();
    let sign = |body: &[u8]| {
        format!(
            "sha256={}",
            apb_core::connector::webhook::hmac_sha256_hex(cmd_secret.as_bytes(), body)
        )
    };

    for i in 0..5 {
        let body = format!(r#"{{"id":"evt-{i}"}}"#).into_bytes();
        let sig = sign(&body);
        let app = build_ingest_router(state.clone());
        let (status, _) = send(app, post(&body, Some(&sig))).await;
        assert_eq!(status, StatusCode::OK, "delivery {i} is accepted");
    }

    let invocations = std::fs::read(&counter).map(|b| b.len()).unwrap_or(0);
    assert_eq!(
        invocations, 1,
        "the secret command ran once within the TTL, not once per request"
    );
    assert_eq!(
        inbox_events(cfg.path()).len(),
        5,
        "all five distinct deliveries were stored"
    );
}

/// The challenge verify token is resolved at most once per account within the
/// cache TTL, not on every GET. Without the cache an unauthenticated challenge
/// flood against a `{{cmd:...}}` verify token would shell out per request and
/// pin the shared runtime. Uses a token command that appends one byte to a
/// counter file on each invocation, asserts it ran once across several
/// challenge GETs, and confirms the handshake still echoes on a token match and
/// refuses on a mismatch.
#[cfg(unix)]
#[tokio::test]
async fn the_challenge_token_is_resolved_at_most_once_within_the_ttl() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();

    let cdir = cfg.path().join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("connector.yaml"), CONNECTOR_YAML).unwrap();

    // The token command appends a byte to this file on every invocation.
    let counter = cfg.path().join("token-resolve-count");
    let cmd_token = "verify-token-value";
    let adir = cfg.path().join("connector-config");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join(format!("{CONNECTOR}.yaml")),
        format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{cmd:sh -c 'printf x >> {counter}; printf %s {cmd_token}'}}}}\"\n    app_secret: \"{{{{env.{SECRET_VAR}}}}}\"\n",
            counter = counter.display(),
        ),
    )
    .unwrap();
    let _cfg_guard = set_var("APB_CONFIG_DIR", cfg.path());
    let _secret_guard = set_var(SECRET_VAR, SECRET);

    // One shared state, so its token cache persists across challenge GETs.
    let state = fresh_state();

    let matching = format!(
        "/hooks/{CONNECTOR}/{ACCOUNT}?hub.mode=subscribe&hub.verify_token={cmd_token}&hub.challenge=42"
    );
    for _ in 0..5 {
        let app = build_ingest_router(state.clone());
        let res = send_from(
            app,
            Request::get(&matching).body(Body::empty()).unwrap(),
            PEER,
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK, "a matching token is echoed");
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(String::from_utf8_lossy(&bytes), "42");
    }

    // A mismatch is still refused, and still against the cached token (no fresh
    // resolution), so the counter stays at one.
    let app = build_ingest_router(state.clone());
    let (status, text) = send(
        app,
        Request::get(format!(
            "/hooks/{CONNECTOR}/{ACCOUNT}?hub.mode=subscribe&hub.verify_token=wrong&hub.challenge=42"
        ))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a mismatched token is refused"
    );
    assert!(text.is_empty(), "no detail is disclosed: {text}");

    let invocations = std::fs::read(&counter).map(|b| b.len()).unwrap_or(0);
    assert_eq!(
        invocations, 1,
        "the token command ran once within the TTL, not once per challenge GET"
    );
}

/// Repeated unknown-pair requests from one address are eventually
/// rate-limited. The unknown-pair 404 used to record no failure, so the
/// filesystem resolve it triggers was bounded only by bandwidth; each 404 now
/// counts toward the peer's failure window, so a probe from one IP is
/// eventually refused with a 401 rather than answered forever.
#[tokio::test]
async fn repeated_unknown_pair_requests_are_eventually_rate_limited() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let _guards = setup(cfg.path());

    // One shared state, so the failure window accumulates across requests.
    let state = fresh_state();
    let body = br#"{"id":"evt-1"}"#;

    let mut statuses = Vec::new();
    for _ in 0..(MAX_FAILURES_PER_WINDOW + 2) {
        let app = build_ingest_router(state.clone());
        let req = Request::post("/hooks/echo-hooks/nope")
            .header("content-type", "application/json")
            .body(Body::from(body.to_vec()))
            .unwrap();
        let (status, _) = send(app, req).await;
        statuses.push(status);
    }

    assert_eq!(
        statuses[0],
        StatusCode::NOT_FOUND,
        "the first unknown-pair request is a flat 404"
    );
    assert_eq!(
        *statuses.last().unwrap(),
        StatusCode::UNAUTHORIZED,
        "a peer over its failure budget is refused rather than answered forever"
    );
}

/// A FAILING secret resolution is remembered too, so a misconfigured
/// `{{cmd:...}}` secret does not re-enter spawn_blocking (and burn up to
/// CMD_SECRET_TIMEOUT) on every request. Uses a secret command that appends one
/// byte to a counter file and then exits non-zero, so resolution fails every
/// time it actually runs; the counter proves it ran once across several
/// deliveries, and every delivery is still refused.
#[cfg(unix)]
#[tokio::test]
async fn a_failing_secret_resolution_is_negatively_cached() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();

    let cdir = cfg.path().join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join("connector.yaml"), CONNECTOR_YAML).unwrap();

    let counter = cfg.path().join("failed-resolve-count");
    let adir = cfg.path().join("connector-config");
    std::fs::create_dir_all(&adir).unwrap();
    std::fs::write(
        adir.join(format!("{CONNECTOR}.yaml")),
        format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"tok\"\n    app_secret: \"{{{{cmd:sh -c 'printf x >> {counter}; exit 1'}}}}\"\n",
            counter = counter.display(),
        ),
    )
    .unwrap();
    let _cfg_guard = set_var("APB_CONFIG_DIR", cfg.path());

    // One shared state, so the negative cache persists across deliveries.
    let state = fresh_state();
    for i in 0..5 {
        let body = format!(r#"{{"id":"evt-{i}"}}"#).into_bytes();
        // A well-formed but unverifiable signature, so the request reaches
        // secret resolution rather than being refused on shape alone.
        let sig = format!("sha256={}", "a".repeat(64));
        let app = build_ingest_router(state.clone());
        let (status, _) = send(app, post(&body, Some(&sig))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "delivery {i} is refused: an unresolvable secret verifies nobody"
        );
    }

    let invocations = std::fs::read(&counter).map(|b| b.len()).unwrap_or(0);
    assert_eq!(
        invocations, 1,
        "the failing secret command ran once within the negative-cache TTL, not once per request"
    );
    assert!(
        inbox_events(cfg.path()).is_empty(),
        "nothing is stored under an unresolvable secret"
    );
}
