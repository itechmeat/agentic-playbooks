use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\nedges: []\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
    dir
}

async fn body_json(router: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn input_draft_get_put_clear() {
    let dir = seed();
    let router = build_router(AppState::new(dir.path().to_path_buf()));

    // empty on a fresh playbook
    let (st, v) = body_json(
        &router,
        Request::get("/api/playbooks/p/input-draft")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["instruction"].is_null());

    // put a draft
    let (st, _) = body_json(
        &router,
        Request::put("/api/playbooks/p/input-draft")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"instruction":"do the thing"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // get it back
    let (_, v) = body_json(
        &router,
        Request::get("/api/playbooks/p/input-draft")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(v["instruction"], "do the thing");

    // clear with an empty string
    body_json(
        &router,
        Request::put("/api/playbooks/p/input-draft")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"instruction":""}"#))
            .unwrap(),
    )
    .await;
    let (_, v) = body_json(
        &router,
        Request::get("/api/playbooks/p/input-draft")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(v["instruction"].is_null());
}
