use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/implement-task/current"),
        "1.0.0",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join(".apb/profiles/architect")).unwrap();
    dir
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn json_request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn health_ok() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn playbooks_list() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/playbooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["id"], "implement-task");
    assert_eq!(json[0]["current"], "1.0.0");
}

#[tokio::test]
async fn playbook_detail_includes_model_and_validation() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.0.0");
    assert_eq!(json["playbook"]["nodes"][0]["type"], "start");
    assert!(json["yaml"].as_str().unwrap().contains("implement-task"));
    assert!(json["validation"].as_array().is_some());
}

#[tokio::test]
async fn unknown_playbook_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/playbooks/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn playbook_id_path_traversal_is_rejected() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app.clone(), "/api/playbooks/..%2F..%2Fetc").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(app, "/api/playbooks/%2Fetc%2Fpasswd").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn playbook_version_path_traversal_is_rejected() {
    let dir = seed();
    // A marker file outside the playbook directory - if traversal succeeds,
    // its content will leak into the response via the `layout` field.
    fs::write(dir.path().join("secret.yaml"), "leaked: true\n").unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(
        app.clone(),
        "/api/playbooks/implement-task?version=..%2F..%2F..%2Fsecret",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let res = app
        .oneshot(
            Request::get("/api/playbooks/implement-task?version=..%2F..%2F..%2Fsecret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        !body.contains("leaked"),
        "response leaked file contents: {body}"
    );
}

#[tokio::test]
async fn run_report_returns_seeded_text() {
    let dir = seed();
    let run_dir = dir.path().join(".apb/runs/run-1/supervisor");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("report.md"),
        "# Supervisor report\n\nall good\n",
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/runs/run-1/report").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["report"].as_str().unwrap().contains("all good"));
}

#[tokio::test]
async fn run_report_unknown_run_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/runs/ghost/report").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_report_path_traversal_is_rejected() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/runs/..%2F..%2Fetc/report").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn lock_write_and_remove() {
    let dir = seed();
    let info = apb_server::lock::write_lock(dir.path(), 7321).unwrap();
    assert_eq!(info.port, 7321);
    let raw = fs::read_to_string(dir.path().join(".apb/serve.lock")).unwrap();
    assert!(raw.contains("root_fingerprint"));
    apb_server::lock::remove_lock(dir.path()).unwrap();
    assert!(!dir.path().join(".apb/serve.lock").exists());
}

#[tokio::test]
async fn post_playbook_creates_then_get_finds_it() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID.replace("id: implement-task", "id: brand-new");
    let (status, json) = json_request(
        app.clone(),
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "brand-new", "yaml": yaml }),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::OK,
        "status={status}"
    );
    assert_eq!(json["id"], "brand-new");
    assert_eq!(json["version"], "1.0.0");

    let (status, json) = get_json(app, "/api/playbooks/brand-new").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.0.0");
}

#[tokio::test]
async fn put_playbook_creates_new_minor_version() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID.replace("name: Implement Task", "name: Implement Task v2");
    let (status, json) = json_request(
        app.clone(),
        "PUT",
        "/api/playbooks/implement-task",
        serde_json::json!({ "yaml": yaml }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["id"], "implement-task");
    assert_eq!(json["version"], "1.1.0");

    let (status, json) = get_json(app, "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.1.0");
}

#[tokio::test]
async fn delete_playbook_moves_to_trash() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = json_request(
        app.clone(),
        "DELETE",
        "/api/playbooks/implement-task",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["trashed"].as_str().unwrap().contains(".apb/trash/"));

    let (status, _) = get_json(app, "/api/playbooks/implement-task").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_playbook_diff_between_versions() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let yaml = VALID
        .replace(
            "Write a plan: {{params.task}}",
            "Write a detailed plan: {{params.task}}",
        )
        .replace(
            "  - { from: fix, to: lint }",
            "  - { from: fix, to: check }",
        );
    let (status, _) = json_request(
        app.clone(),
        "PUT",
        "/api/playbooks/implement-task",
        serde_json::json!({ "yaml": yaml }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, json) = get_json(
        app,
        "/api/playbooks/implement-task/diff?from=1.0.0&to=1.1.0",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        json["nodes_changed"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("plan"))
    );
    assert!(!json["yaml_diff"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn put_layout_saves_canvas_layout() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app.clone(),
        "PUT",
        "/api/playbooks/implement-task/layout?version=1.0.0",
        serde_json::json!({ "layout": "nodes:\n  - { id: plan, x: 11, y: 22 }\n" }),
    )
    .await;
    assert!(
        status == StatusCode::NO_CONTENT || status == StatusCode::OK,
        "status={status}"
    );

    let (status, json) = get_json(app, "/api/playbooks/implement-task?version=1.0.0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["layout"]["nodes"][0]["x"], 11);
}

#[tokio::test]
async fn post_playbook_invalid_yaml_is_400() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let invalid = VALID.replace(
        "  - id: plan",
        "  - id: start2\n    type: start\n    title: Second start\n  - id: plan",
    );
    let (status, json) = json_request(
        app,
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "implement-task", "yaml": invalid }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = json.to_string();
    assert!(
        body.contains("V03"),
        "expected validation codes in body: {body}"
    );
}

#[tokio::test]
async fn get_versions_returns_provenance() {
    let dir = seed();
    let patch = apb_core::versioning::create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        VALID,
        "run-x",
        "improvement",
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/playbooks/implement-task/versions").await;
    assert_eq!(status, StatusCode::OK);
    let arr = json.as_array().unwrap();
    let base = arr.iter().find(|v| v["version"] == "1.0.0").unwrap();
    assert_eq!(base["is_current"], true);
    let patched = arr
        .iter()
        .find(|v| v["version"] == serde_json::json!(patch))
        .unwrap();
    assert_eq!(patched["is_current"], false);
    assert_eq!(patched["provenance"]["classification"], "improvement");
    assert_eq!(patched["provenance"]["run_id"], "run-x");
    assert_eq!(patched["provenance"]["promoted"], false);
}

#[tokio::test]
async fn post_promote_moves_current() {
    let dir = seed();
    let patch = apb_core::versioning::create_patch_version(
        dir.path(),
        "implement-task",
        "1.0.0",
        VALID,
        "run-x",
        "improvement",
    )
    .unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = json_request(
        app.clone(),
        "POST",
        &format!("/api/playbooks/implement-task/versions/{patch}/promote"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["promoted"], serde_json::json!(patch));

    let (status, json) = get_json(app, "/api/playbooks/implement-task/versions").await;
    assert_eq!(status, StatusCode::OK);
    let patched = json
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version"] == serde_json::json!(patch))
        .unwrap()
        .clone();
    assert_eq!(patched["is_current"], true);
    assert_eq!(patched["provenance"]["promoted"], true);
}

#[tokio::test]
async fn promote_unknown_version_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app,
        "POST",
        "/api/playbooks/implement-task/versions/9.9.9/promote",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn versions_endpoint_rejects_path_traversal() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/playbooks/..%2F..%2Fetc/versions").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn write_endpoints_reject_path_traversal() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = json_request(
        app.clone(),
        "POST",
        "/api/playbooks",
        serde_json::json!({ "id": "../evil", "yaml": VALID }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = json_request(
        app,
        "PUT",
        "/api/playbooks/..%2Fevil",
        serde_json::json!({ "yaml": VALID }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
