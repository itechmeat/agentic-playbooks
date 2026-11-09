//! The cloud catalog on the HTTP API: `GET /api/models` carries a `cloud`
//! block that names the key variable and never its value, `POST
//! /api/models/refresh` is refused for a client that is not on this machine,
//! and `GET /api/profiles/{name}` carries the resolved privacy chain.

use std::net::SocketAddr;

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

const KEY: &str = "fake-key-1234567-near";

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_models_cloud_block_names_the_key_and_never_its_value() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_PROBE_TIMEOUT_MS", "300");
        std::env::set_var("NEARAI_API_KEY", KEY);
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    let app = build_router(AppState::new(proj.path().to_path_buf()));
    let (status, body) = call(&app, "GET", "/api/models", &[("host", "127.0.0.1:7321")]).await;
    unsafe {
        std::env::remove_var("NEARAI_API_KEY");
    }
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(KEY), "the key value leaked");
    let json: Value = serde_json::from_str(&body).unwrap();
    let near = &json["cloud"][0];
    assert_eq!(near["provider"], "near-ai-cloud");
    assert_eq!(near["key_env"], "NEARAI_API_KEY");
    assert_eq!(near["key_present"], true);
    assert_eq!(near["source"], "embedded");
    assert_eq!(near["base_url"], "https://cloud-api.near.ai/v1");
    assert!(near["digest"].as_str().unwrap().starts_with("sha256:"));
    let rows = near["models"].as_array().unwrap();
    let q = rows.iter().find(|m| m["id"] == "Qwen/Qwen3.8-27B").unwrap();
    assert_eq!(q["privacy_class"], "confidential_tee");
    let dsh: Vec<&str> = json["options_by_agent"]["dsh"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert!(dsh.contains(&"deepseek-flash"));
    assert!(dsh.contains(&"nearai/Qwen/Qwen3.8-27B"));
    assert!(!dsh.contains(&"nearai/Qwen/Qwen3-VL-30B-A3B-Instruct"));
}

#[tokio::test]
async fn the_refresh_is_refused_for_a_forwarded_or_foreign_client() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        // Nothing may be fetched: an unreachable stub.
        std::env::set_var("APB_TEST_CLOUD_CATALOG_URL", "http://127.0.0.1:9/v1/models");
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    let app = build_router(AppState::new(proj.path().to_path_buf()));
    let forwarded = [
        ("host", "127.0.0.1:7321"),
        ("x-forwarded-for", "198.51.100.4"),
    ];
    let (status, body) = call(&app, "POST", "/api/models/refresh", &forwarded).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("cloud_refresh_local_only"), "{body}");
    // A host name the dashboard does not answer is refused before the route.
    let (status, _) = call(
        &app,
        "POST",
        "/api/models/refresh",
        &[("host", "example.com")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A local client reaches the refresh; the unreachable listing fails it
    // and the catalog in use stays the embedded one.
    let (status, body) = call(
        &app,
        "POST",
        "/api/models/refresh",
        &[("host", "127.0.0.1:7321")],
    )
    .await;
    unsafe {
        std::env::remove_var("APB_TEST_CLOUD_CATALOG_URL");
    }
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("cloud_catalog_refresh_failed"), "{body}");
    assert_eq!(
        apb_core::cloud_models::catalog::load(&apb_core::cloud_models::near_ai::NEAR_AI).source,
        apb_core::cloud_models::catalog::CatalogSource::Embedded
    );
}

#[tokio::test]
async fn the_profile_route_returns_the_resolved_privacy_chain() {
    let _guard = crate::common::env_lock().await;
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    let dir = proj.path().join(".apb/profiles/mixed");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        "name: mixed\ndescription: t\nexecutor:\n  agent: dsh\n  model: nearai/Qwen/Qwen3.8-27B\n  fallbacks:\n    - { agent: dsh, model: nearai/anthropic/claude-sonnet-5-5 }\n",
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();
    let app = build_router(AppState::new(proj.path().to_path_buf()));
    let (status, body) = call(
        &app,
        "GET",
        "/api/profiles/mixed?scope=project",
        &[("host", "127.0.0.1:7321")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let json: Value = serde_json::from_str(&body).unwrap();
    let r = &json["resolved"];
    assert_eq!(r["chain"][0]["cloud"]["privacy_class"], "confidential_tee");
    assert_eq!(r["chain"][1]["role"], "fallback");
    assert_eq!(r["privacy"]["uniform"], false);
    assert_eq!(r["privacy"]["least_private"], "incognito");
    assert_eq!(r["privacy"]["warnings"][0], "profile_privacy_class_changes");
}
