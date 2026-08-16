//! The read-only inbox endpoints behind the dashboard panel. Machine-scoped
//! like the store itself, so they take the shared env lock for
//! `APB_CONFIG_DIR`.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::path::Path;
use tower::ServiceExt;

const CONNECTOR_YAML: &str = r#"
name: echo-hooks
version: 0.1.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
account_fields:
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

const PLAIN_YAML: &str = "name: plain\nversion: 0.1.0\nfunctions:\n  - name: ping\n    description: d\n    mock: { status: 200, body: {} }\n";

struct EnvGuard(String, Option<std::ffi::OsString>);
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.1 {
                Some(v) => std::env::set_var(&self.0, v),
                None => std::env::remove_var(&self.0),
            }
        }
    }
}
fn set_var(var: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    EnvGuard(var.to_string(), prior)
}

fn seed(cfg: &Path) {
    for (name, yaml) in [("echo-hooks", CONNECTOR_YAML), ("plain", PLAIN_YAML)] {
        let dir = cfg.join("connectors").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
    }
    std::fs::write(
        cfg.join("config.yaml"),
        "ingest:\n  enabled: true\n  public_base_url: https://hooks.example.com\n",
    )
    .unwrap();
    let inbox =
        apb_core::connector::inbox::Inbox::at(&cfg.join("connector-inbox"), "echo-hooks", "main")
            .unwrap();
    for i in 1..=3u32 {
        inbox
            .append(
                &format!("e{i}"),
                &serde_json::json!({ "text": format!("m{i}") }),
            )
            .unwrap();
    }
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn the_inbox_endpoint_reports_depth_and_the_callback_url() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    apb_core::registry::init_project(root.path()).unwrap();
    seed(cfg.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/connectors/echo-hooks/inbox").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["has_webhook"], true);
    assert_eq!(json["public_base_url_set"], true);
    let accounts = json["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["account"], "main");
    assert_eq!(accounts[0]["pending"], 3);
    assert_eq!(accounts[0]["total"], 3);
    assert!(accounts[0]["last_received_at"].as_u64().unwrap() > 0);
    assert_eq!(
        accounts[0]["callback_url"],
        "https://hooks.example.com/hooks/echo-hooks/main"
    );
    // The summary carries counts only: no body, no provider id.
    let raw = json.to_string();
    assert!(!raw.contains("m1") && !raw.contains("e1"), "was: {raw}");
}

#[tokio::test]
async fn a_connector_without_a_webhook_block_reports_no_inbox() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    apb_core::registry::init_project(root.path()).unwrap();
    seed(cfg.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/connectors/plain/inbox").await;
    assert_eq!(status, StatusCode::OK, "not an error, just nothing to show");
    assert_eq!(json["has_webhook"], false);
    assert!(json["accounts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn the_events_endpoint_returns_bodies_only_when_asked() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    apb_core::registry::init_project(root.path()).unwrap();
    seed(cfg.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) =
        get_json(app, "/api/connectors/echo-hooks/inbox/main/events?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "the limit is honored");
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[0]["body"]["text"], "m1");
    assert!(
        events[0].get("provider_id").is_none(),
        "the dedupe identity is not part of the view: {json}"
    );
    assert_eq!(json["cursor"], 0);
}

#[tokio::test]
async fn unknown_names_and_traversal_are_404() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    apb_core::registry::init_project(root.path()).unwrap();
    seed(cfg.path());

    for uri in [
        "/api/connectors/ghost/inbox",
        "/api/connectors/..%2F..%2Fetc/inbox",
        "/api/connectors/echo-hooks/inbox/ghost/events",
        "/api/connectors/echo-hooks/inbox/..%2Fetc/events",
    ] {
        let app = build_router(AppState::new(root.path().to_path_buf()));
        let (status, _) = get_json(app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "uri {uri}");
    }
}

#[tokio::test]
async fn the_events_limit_is_clamped() {
    let _lock = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _g = set_var("APB_CONFIG_DIR", cfg.path());
    apb_core::registry::init_project(root.path()).unwrap();
    seed(cfg.path());

    let app = build_router(AppState::new(root.path().to_path_buf()));
    let (status, json) = get_json(
        app,
        "/api/connectors/echo-hooks/inbox/main/events?limit=100000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json["events"].as_array().unwrap().len(),
        3,
        "an absurd limit is clamped, not refused"
    );
}
