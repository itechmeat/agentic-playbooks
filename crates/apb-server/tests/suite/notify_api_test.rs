//! "Notify me" and the HTML report on the HTTP API: the machine target
//! the Run dialog reads (no secret in it), a start that asks for
//! notifications nobody can send (409 before a run exists) or names an
//! unknown transition (400), a change on an ended run (409) or an unknown
//! one (404), and the report page served as a download under its own
//! Content Security Policy.

use std::fs;
use std::path::Path;

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::common;

const QUICK: &str = "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), QUICK).unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap())))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let res = send(app, method, uri, body).await;
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn runs(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs")).map_or(0, |rd| rd.count())
}

#[tokio::test]
async fn the_machine_target_is_shown_and_the_project_can_turn_the_default_off() {
    let cfg = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let (status, none) = call(&app, "GET", "/api/notify", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(none.get("target").is_none(), "{none}");
    assert_eq!(none["default_on"], false);

    fs::write(
        cfg.path().join("config.yaml"),
        "notify:\n  default: on\n  target: { connector: telegram, account: me, function: send_message, to: \"123456789\" }\n  on: [review, finished]\n",
    )
    .unwrap();
    let (_, on) = call(&app, "GET", "/api/notify", None).await;
    assert_eq!(
        on["target"],
        json!({ "connector": "telegram", "account": "me", "function": "send_message", "to": "123456789" })
    );
    assert_eq!(on["on"], json!(["review", "finished"]));
    assert_eq!(on["default_on"], true);

    fs::write(
        dir.path().join(".apb/config.yaml"),
        "notify:\n  enabled: false\n",
    )
    .unwrap();
    let (_, off) = call(&app, "GET", "/api/notify", None).await;
    assert_eq!(off["default_on"], false);
    assert!(off.get("target").is_some());
}

#[tokio::test]
async fn a_start_whose_notifications_cannot_be_sent_is_refused_before_a_run_exists() {
    let _cfg = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let (status, body) = call(
        &app,
        "POST",
        "/api/playbooks/quick/run",
        Some(json!({ "notify": true })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "notify_unavailable");
    let (status, body) = call(
        &app,
        "POST",
        "/api/playbooks/quick/run",
        Some(json!({ "notify": { "on": ["sometimes"] } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_notify");
    // A value of the wrong shape answers the same, not the extractor's text.
    for notify in [json!({ "on": "review" }), json!({ "via": 1 }), json!("yes")] {
        let (status, body) = call(
            &app,
            "POST",
            "/api/playbooks/quick/run",
            Some(json!({ "notify": notify })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{notify}");
        assert_eq!(body["error"], "invalid_notify", "{notify}");
    }
    assert_eq!(runs(dir.path()), 0, "no run was created");
}

#[tokio::test]
async fn a_malformed_change_answers_invalid_notify() {
    let _cfg = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    for body in [
        json!({}),
        json!({ "notify": { "on": "review" } }),
        json!({ "notify": { "target": { "connector": "telegram" } } }),
        json!({ "notify": false, "extra": 1 }),
    ] {
        let (status, answer) = call(&app, "POST", "/api/runs/r1/notify", Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answer["error"], "invalid_notify", "{body}");
    }
    // What did not parse is named.
    let (_, answer) = call(
        &app,
        "POST",
        "/api/runs/r1/notify",
        Some(json!({ "notify": { "on": "review" } })),
    )
    .await;
    assert!(
        answer["detail"].as_str().unwrap().contains("notify"),
        "{answer}"
    );
    // A transport problem keeps its own answer.
    let req = Request::builder()
        .method("POST")
        .uri("/api/runs/r1/notify")
        .header("content-type", "text/plain")
        .body(Body::from("{\"notify\":false}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn the_report_page_is_a_download_under_its_own_csp_and_an_ended_run_refuses_changes() {
    let _cfg = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = apb_engine::run(dir.path(), "quick", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    let app = build_router(AppState::new(dir.path().to_path_buf()));

    let res = send(
        &app,
        "GET",
        &format!("/api/runs/{run_id}/report.html"),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let h = res.headers();
    let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.starts_with("default-src 'none'"), "{csp}");
    assert!(csp.contains("style-src 'sha256-"));
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    let disposition = h[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(
        h[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let page =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(page.starts_with("<!doctype html>"));
    assert!(!page.contains("<script"));

    let missing = send(&app, "GET", "/api/runs/no-such-run/report.html", None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/runs/{run_id}/notify"),
        Some(json!({ "notify": false })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "run_ended");
    let (status, _) = call(
        &app,
        "POST",
        "/api/runs/no-such-run/notify",
        Some(json!({ "notify": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A run that never had a subscription carries no notify block.
    let (_, detail) = call(&app, "GET", &format!("/api/runs/{run_id}"), None).await;
    assert!(detail.get("notify").is_none());
}
