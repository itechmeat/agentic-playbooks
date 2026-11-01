//! Moving `current` on the HTTP API: `POST /api/playbooks/{id}/current`
//! with the version the person saw (200 with what moved; 409 `stale`,
//! `frozen`, `is_candidate`; 404 for what is not there), the deprecated
//! promote route, `GET /api/playbooks/{id}/current-history`, a move from
//! an unset `current`, a body of the wrong shape, and a cross-site post to
//! the keyless dashboard.

use std::fs;
use std::path::Path;

use apb_server::{AppState, build_router};
use axum::http::StatusCode;
use serde_json::json;

use crate::quality_api_test::call;

const YAML: &str = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

/// A project whose `demo` has versions 1.0.0, 1.1.0 and 1.2.0, `current`
/// at 1.0.0.
fn seed(root: &Path) -> std::path::PathBuf {
    apb_core::registry::init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/demo");
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        fs::create_dir_all(dir.join(v)).unwrap();
        fs::write(dir.join(v).join("playbook.yaml"), YAML).unwrap();
    }
    fs::write(dir.join("current"), "1.0.0").unwrap();
    dir
}

#[tokio::test]
async fn a_person_moves_current_and_the_history_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let pb = seed(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, moved) = call(
        &router,
        "POST",
        "/api/playbooks/demo/current",
        Some(json!({ "version": "1.1.0", "expected_current": "1.0.0", "reason": "roll back" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["from"], "1.0.0");
    assert_eq!(moved["to"], "1.1.0");
    assert_eq!(moved["audit_pending"], false);
    assert_eq!(moved["awaiting_approval"], false);
    assert!(moved["change_id"].as_str().is_some_and(|c| !c.is_empty()));
    assert_eq!(fs::read_to_string(pb.join("current")).unwrap(), "1.1.0");

    let (status, h) = call(&router, "GET", "/api/playbooks/demo/current-history", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h["incomplete"], false);
    assert_eq!(h["pending"], false);
    let e = &h["entries"][0];
    assert_eq!(e["change_id"], moved["change_id"]);
    assert_eq!(e["from"], "1.0.0");
    assert_eq!(e["to"], "1.1.0");
    assert_eq!(e["by"], json!({ "kind": "person", "surface": "dashboard" }));
    assert_eq!(e["reason"], "roll back");
}

#[tokio::test]
async fn a_refused_move_names_why() {
    let dir = tempfile::tempdir().unwrap();
    let pb = seed(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let post = |body: serde_json::Value| {
        let router = router.clone();
        async move { call(&router, "POST", "/api/playbooks/demo/current", Some(body)).await }
    };

    let (status, body) = post(json!({ "version": "1.1.0", "expected_current": "1.2.0" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "stale");
    assert_eq!(body["actual"], "1.0.0");

    fs::write(pb.join("candidate"), "1.2.0").unwrap();
    let (status, body) = post(json!({ "version": "1.2.0", "expected_current": "1.0.0" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "is_candidate");
    assert_eq!(body["version"], "1.2.0");

    fs::write(pb.join("frozen"), "1").unwrap();
    let (status, body) = post(json!({ "version": "1.1.0", "expected_current": "1.0.0" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "frozen");
    fs::remove_file(pb.join("frozen")).unwrap();

    let (status, body) = post(json!({ "version": "9.9.9", "expected_current": "1.0.0" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) = call(
        &router,
        "POST",
        "/api/playbooks/nope/current",
        Some(json!({ "version": "1.1.0", "expected_current": "1.0.0" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&router, "GET", "/api/playbooks/nope/current-history", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(fs::read_to_string(pb.join("current")).unwrap(), "1.0.0");
}

#[tokio::test]
async fn the_deprecated_promote_route_moves_through_the_same_operation() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, body) = call(
        &router,
        "POST",
        "/api/playbooks/demo/versions/1.2.0/promote",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["promoted"], "1.2.0");
    assert_eq!(body["moved"]["from"], "1.0.0");
    let (_, h) = call(&router, "GET", "/api/playbooks/demo/current-history", None).await;
    assert_eq!(h["entries"][0]["to"], "1.2.0");
}

#[tokio::test]
async fn an_unset_current_moves_for_a_person_who_saw_it_unset() {
    let dir = tempfile::tempdir().unwrap();
    let pb = seed(dir.path());
    fs::remove_file(pb.join("current")).unwrap();
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, body) = call(
        &router,
        "POST",
        "/api/playbooks/demo/current",
        Some(json!({ "version": "1.1.0", "expected_current": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["from"], serde_json::Value::Null);
    assert_eq!(fs::read_to_string(pb.join("current")).unwrap(), "1.1.0");
    // Set now, the same expectation is stale.
    let (status, body) = call(
        &router,
        "POST",
        "/api/playbooks/demo/current",
        Some(json!({ "version": "1.0.0", "expected_current": null })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "stale");
    assert_eq!(body["actual"], "1.1.0");
}

#[tokio::test]
async fn a_body_of_the_wrong_shape_answers_json() {
    let dir = tempfile::tempdir().unwrap();
    let pb = seed(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    for body in [json!({ "version": "1.1.0" }), json!({ "version": 1 })] {
        let (status, answer) = call(
            &router,
            "POST",
            "/api/playbooks/demo/current",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answer["error"], "invalid", "{body}: {answer}");
        assert!(answer["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    assert_eq!(fs::read_to_string(pb.join("current")).unwrap(), "1.0.0");
}

/// A cross-site form post to the keyless dashboard never moves `current`.
#[tokio::test]
async fn a_keyless_dashboard_refuses_a_cross_site_move() {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let dir = tempfile::tempdir().unwrap();
    let pb = seed(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let req = Request::post("/api/playbooks/demo/current")
        .header("host", "127.0.0.1:7321")
        .header("content-type", "text/plain")
        .header("origin", "http://attacker.example")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from(
            r#"{"version":"1.1.0","expected_current":"1.0.0"}"#,
        ))
        .unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "csrf", "{body}");
    assert_eq!(fs::read_to_string(pb.join("current")).unwrap(), "1.0.0");
    assert!(!pb.join("current-history.jsonl").exists());
}
