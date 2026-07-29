//! GET and DELETE /api/suggestions. Mutates process env (APB_CONFIG_DIR), so
//! it takes `common::env_lock()` like the other env-mutating suites.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn send(app: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn seed(root: &std::path::Path, pattern: &str, synopsis: &str, kind: &str, scope: &str) {
    apb_core::dismiss::record_decision(
        root,
        apb_core::dismiss::DecisionInput {
            pattern: pattern.to_string(),
            synopsis: synopsis.to_string(),
            kind: apb_core::dismiss::DecisionKind::parse(kind).unwrap(),
            scope: apb_core::dismiss::DecisionScope::parse(scope).unwrap(),
            hard_ttl_days_override: None,
        },
    )
    .unwrap();
}

#[tokio::test]
async fn list_and_delete_suggestions() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    seed(
        proj.path(),
        "code-review-run",
        "Review a file",
        "soft",
        "project",
    );
    seed(
        proj.path(),
        "never-anywhere",
        "Never offer this",
        "hard",
        "global",
    );
    let root = proj.path().to_path_buf();

    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(
        app,
        Request::get("/api/suggestions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = json["suggestions"].as_array().expect("suggestions array");
    assert_eq!(rows.len(), 2, "{json}");
    let soft = rows
        .iter()
        .find(|r| r["pattern"] == "code-review-run")
        .unwrap();
    assert_eq!(soft["kind"], "soft");
    assert_eq!(soft["scope"], "project");
    assert_eq!(soft["synopsis"], "Review a file");
    assert!(soft["snoozed_until"].as_str().unwrap().ends_with('Z'));

    // DELETE with the default (project) scope.
    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(
        app,
        Request::delete("/api/suggestions/code-review-run")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["removed"], true);

    // A second delete of the same record is a 404.
    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/code-review-run")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The global record needs scope=global.
    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/never-anywhere?scope=global")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(
        app,
        Request::get("/api/suggestions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["suggestions"].as_array().unwrap().len(), 0, "{json}");

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

/// A pattern recorded in both scopes stays silenced after one scope is
/// removed, so the response has to say so: the dashboard used to show a
/// success toast next to the card it had just re-rendered.
#[tokio::test]
async fn delete_reports_that_the_other_scope_still_suppresses() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    seed(
        proj.path(),
        "both-scopes",
        "In both stores",
        "hard",
        "project",
    );
    seed(
        proj.path(),
        "both-scopes",
        "In both stores",
        "hard",
        "global",
    );
    let root = proj.path().to_path_buf();

    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(
        app,
        Request::delete("/api/suggestions/both-scopes?scope=project")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["removed"], true);
    assert_eq!(json["still_suppressed"], true, "{json}");
    assert_eq!(json["still_suppressed_by"], "global", "{json}");

    let app = build_router(AppState::new(root.clone()));
    let (status, json) = send(
        app,
        Request::delete("/api/suggestions/both-scopes?scope=global")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(
        json["still_suppressed"], false,
        "nothing silences the pattern now: {json}"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

/// On a machine with zero registered projects there is no `?workspace=` value
/// to send, so the machine-wide records were unreachable from the dashboard:
/// they could be neither listed nor removed. A workspace-less request on the
/// global server is the global-only view.
#[tokio::test]
async fn global_records_are_reachable_without_a_workspace() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    // Deliberately NOT registered: this is the zero-projects machine.
    seed(proj.path(), "machine-wide", "Never here", "hard", "global");
    seed(proj.path(), "project-only", "Just here", "soft", "project");

    let app = build_router(AppState::new_global());
    let (status, json) = send(
        app,
        Request::get("/api/suggestions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let rows = json["suggestions"].as_array().expect("suggestions array");
    assert_eq!(rows.len(), 1, "only the global scope is served: {json}");
    assert_eq!(rows[0]["pattern"], "machine-wide");
    assert_eq!(rows[0]["scope"], "global");

    let app = build_router(AppState::new_global());
    let (status, json) = send(
        app,
        Request::delete("/api/suggestions/machine-wide?scope=global")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["removed"], true);

    let app = build_router(AppState::new_global());
    let (status, json) = send(
        app,
        Request::get("/api/suggestions")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["suggestions"].as_array().unwrap().len(), 0, "{json}");

    // A PROJECT-scope request still needs a workspace: there is no root to
    // guess, and silently answering for some other project would be worse.
    let app = build_router(AppState::new_global());
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/project-only?scope=project")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[tokio::test]
async fn suggestions_reject_a_bad_workspace_or_pattern_or_scope() {
    let _guard = crate::common::env_lock().await;
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    apb_core::registry::init_project(proj.path()).unwrap();
    let root = proj.path().to_path_buf();

    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::get("/api/suggestions?workspace=../escape")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "workspace id is validated");

    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/..?scope=project")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "pattern is validated");

    let app = build_router(AppState::new(root.clone()));
    let (status, _json) = send(
        app,
        Request::delete("/api/suggestions/whatever?scope=everywhere")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "scope is validated");

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
