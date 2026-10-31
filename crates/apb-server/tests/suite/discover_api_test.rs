//! History discovery on the HTTP API: a discovery over a synthetic root
//! named in the config, its stored result listed and read without an
//! absolute path, a save and a decline; a body naming a path refused (422),
//! an unknown root refused (422), on every route a client off the loopback
//! interface, behind a proxy or naming another host refused (403) before
//! its body is read, a second discovery while one runs refused (409 busy),
//! and every route refused when the server runs as a run step.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::common;

const SESSION: &str = include_str!("../../../apb-core/tests/fixtures/history/claude_session.jsonl");

/// A project under `base` with six synthetic claude sessions of it on two
/// days (three with `bun test` for `cargo test`), and the sandbox config
/// naming their directory as the root `synthetic`. Returns the project.
fn history(base: &Path, config: &Path) -> PathBuf {
    let base = std::fs::canonicalize(base).unwrap();
    let project = base.join("work/proj");
    std::fs::create_dir_all(&project).unwrap();
    apb_core::registry::init_project(&project).unwrap();
    let claude = base.join("claude/projects");
    let dir = claude.join(apb_core::agent_actions::claude::encode_cwd(
        &project.to_string_lossy(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, age) in [1u32, 1, 2, 1, 1, 2].iter().enumerate() {
        let path = dir.join(format!("s{i}.jsonl"));
        let mut text = SESSION.replace(
            "__CWD__",
            serde_json::to_string(&project).unwrap().trim_matches('"'),
        );
        if i >= 3 {
            text = text.replace("cargo test", "bun test");
        }
        std::fs::write(&path, text).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(86_400) * *age)
            .unwrap();
    }
    std::fs::write(
        config.join("config.yaml"),
        format!(
            "discover:\n  roots:\n    - {{ name: synthetic, agent: claude_code, path: {} }}\n",
            claude.display()
        ),
    )
    .unwrap();
    project
}

/// A client a test plays: why it is refused, its peer and its headers.
type Client = (&'static str, [u8; 4], Vec<(&'static str, &'static str)>);

/// The `Host` a browser on this machine sends.
const LOCAL_HOST: (&str, &str) = ("host", "127.0.0.1:7357");

/// One request from `peer` with `headers` (a `Host` among them, unless the
/// test leaves it out) and a JSON body.
async fn call_from(
    router: &axum::Router,
    peer: [u8; 4],
    headers: &[(&str, &str)],
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let mut req = req.body(body).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 40000))));
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}

/// One request from a browser on this machine.
async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_from(router, [127, 0, 0, 1], &[LOCAL_HOST], method, uri, body).await
}

#[tokio::test]
async fn the_dashboard_discovers_lists_saves_and_declines() {
    let sandbox = common::config_sandbox().await;
    let base = tempfile::tempdir().unwrap();
    let project = history(base.path(), sandbox.path());
    let router = build_router(AppState::new(project.clone()));
    let base_text = std::fs::canonicalize(base.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let (st, v) = call(&router, "GET", "/api/discover/roots", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!([{ "name": "synthetic", "agent": "claude_code", "available": true }])
    );

    let (st, v) = call(
        &router,
        "POST",
        "/api/discover",
        Some(json!({ "roots": ["synthetic"], "since_days": 30 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains(&base_text), "no absolute path");
    assert_eq!(v["read"]["sessions"], 6);
    assert_eq!(v["signed"], true);
    let id = v["id"].as_str().unwrap().to_string();
    let slugs: Vec<String> = v["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(slugs.len() >= 2, "{v}");

    let (st, v) = call(&router, "GET", "/api/discover", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v[0]["id"], id.as_str());
    let (st, v) = call(&router, "GET", &format!("/api/discover/{id}"), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(!v.to_string().contains(&base_text));

    let (st, v) = call(
        &router,
        "POST",
        &format!("/api/discover/{id}/{}/save", slugs[0]),
        Some(json!({ "scope": "project" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["provenance"]["created_by"], "discover");
    assert!(project.join(".apb/playbooks").join(&slugs[0]).is_dir());

    let (st, v) = call(
        &router,
        "POST",
        &format!("/api/discover/{id}/{}/dismiss", slugs[1]),
        Some(json!({ "kind": "soft" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["kind"], "soft");

    let (st, v) = call(&router, "GET", "/api/discover", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v[0]["saved"].clone(), v[0]["dismissed"].clone()),
        (json!(1), json!(1))
    );
}

#[tokio::test]
async fn a_path_an_unknown_root_and_a_bad_client_are_refused() {
    let sandbox = common::config_sandbox().await;
    let base = tempfile::tempdir().unwrap();
    let project = history(base.path(), sandbox.path());
    let router = build_router(AppState::new(project));

    let (st, _) = call(
        &router,
        "POST",
        "/api/discover",
        Some(json!({ "roots": ["synthetic"], "path": "/tmp" })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = call(
        &router,
        "POST",
        "/api/discover",
        Some(json!({ "roots": ["elsewhere"] })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // Every route, with a body that would read: the refusal comes first.
    let routes = [
        ("GET", "/api/discover/roots".to_string(), None),
        ("GET", "/api/discover".to_string(), None),
        ("GET", "/api/discover/disc-0123456789".to_string(), None),
        ("POST", "/api/discover".to_string(), Some(json!({}))),
        (
            "POST",
            "/api/discover/disc-0123456789/seq-0123456789/save".to_string(),
            Some(json!({ "scope": "project" })),
        ),
        (
            "POST",
            "/api/discover/disc-0123456789/seq-0123456789/dismiss".to_string(),
            Some(json!({ "kind": "soft" })),
        ),
    ];
    let remote = [198, 51, 100, 4];
    let loopback = [127, 0, 0, 1];
    let refused: Vec<Client> = vec![
        ("a remote peer", remote, vec![LOCAL_HOST]),
        ("no Host", loopback, vec![]),
        ("another Host", loopback, vec![("host", "example.com")]),
        (
            "another Origin",
            loopback,
            vec![LOCAL_HOST, ("origin", "https://example.com")],
        ),
        (
            "forwarded",
            loopback,
            vec![LOCAL_HOST, ("forwarded", "for=198.51.100.4")],
        ),
        (
            "x-forwarded-for",
            loopback,
            vec![LOCAL_HOST, ("x-forwarded-for", "198.51.100.4")],
        ),
        (
            "x-forwarded-host",
            loopback,
            vec![LOCAL_HOST, ("x-forwarded-host", "example.com")],
        ),
        (
            "x-forwarded-proto",
            loopback,
            vec![LOCAL_HOST, ("x-forwarded-proto", "https")],
        ),
        (
            "x-real-ip",
            loopback,
            vec![LOCAL_HOST, ("x-real-ip", "198.51.100.4")],
        ),
        ("via", loopback, vec![LOCAL_HOST, ("via", "1.1 proxy")]),
        (
            "cf-connecting-ip",
            loopback,
            vec![LOCAL_HOST, ("cf-connecting-ip", "198.51.100.4")],
        ),
    ];
    for (method, uri, body) in &routes {
        for (why, peer, headers) in &refused {
            let (st, v) = call_from(&router, *peer, headers, method, uri, body.clone()).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{why}: {method} {uri}");
            // A foreign Host or Origin the dashboard's own gate refuses
            // first; every other case is this API's refusal.
            if !why.starts_with("another") {
                assert_eq!(v["error"], "discover_local_only", "{why}");
            }
        }
        // A remote client never learns how its body would have read.
        let (st, _) = call_from(
            &router,
            remote,
            &[LOCAL_HOST],
            method,
            uri,
            Some(json!({ "path": 1 })),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    for (uri, origin) in [
        ("/api/discover", None),
        ("/api/discover/roots", Some("http://localhost:7357")),
    ] {
        let mut headers = vec![LOCAL_HOST];
        headers.extend(origin.map(|o| ("origin", o)));
        let (st, _) = call_from(&router, loopback, &headers, "GET", uri, None).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn a_second_discovery_and_a_run_step_are_refused_with_409() {
    let sandbox = common::config_sandbox().await;
    let base = tempfile::tempdir().unwrap();
    let project = history(base.path(), sandbox.path());
    let router = build_router(AppState::new(project));

    let held = apb_core::fsutil::lock_dir_with_lifetime(
        &sandbox.path().join("discover"),
        "discover.lock",
        Duration::from_secs(600),
    )
    .unwrap();
    let (st, body) = call(
        &router,
        "POST",
        "/api/discover",
        Some(json!({ "roots": ["synthetic"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(
        body.as_str().is_some_and(|b| b.starts_with("busy")),
        "{body}"
    );
    drop(held);

    let _step = common::EnvVar::set("APB_RUN_ID", "r1");
    for (method, uri, body) in [
        ("GET", "/api/discover/roots", None),
        ("GET", "/api/discover", None),
        ("GET", "/api/discover/disc-0123456789", None),
        ("POST", "/api/discover", Some(json!({}))),
        (
            "POST",
            "/api/discover/disc-0123456789/seq-0123456789/save",
            Some(json!({ "scope": "project" })),
        ),
        (
            "POST",
            "/api/discover/disc-0123456789/seq-0123456789/dismiss",
            Some(json!({ "kind": "soft" })),
        ),
    ] {
        let (st, _): (StatusCode, Value) = call(&router, method, uri, body).await;
        assert_eq!(st, StatusCode::CONFLICT, "{method} {uri}");
    }
}
