//! Run quality on the HTTP API: `GET` and `POST /api/runs/{id}/grades`
//! (the rules grade of an ended run, refused while the run is live; the
//! model grader names why it stored nothing),
//! `POST /api/runs/{id}/grades/reopen` (409 without a host grade), and a
//! playbook's candidate: `GET /api/playbooks/{id}/candidate` and a
//! person's Keep current or Promote from the dashboard.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tower::ServiceExt;

use crate::common;

const SNAPSHOT: &str = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nquality: {}\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

/// A run of `demo` under `root`; ended when `outcome` is given.
fn seed_run(root: &Path, id: &str, outcome: Option<&str>) {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut journal = String::from(
        "{\"seq\":0,\"ts\":1790000000000,\"type\":\"run_started\",\"playbook\":\"demo\",\"version\":\"1.0.0\"}\n",
    );
    if let Some(o) = outcome {
        journal.push_str(&format!(
            "{{\"seq\":1,\"ts\":1790000001000,\"type\":\"run_finished\",\"outcome\":\"{o}\"}}\n"
        ));
    }
    fs::write(dir.join("events.jsonl"), journal).unwrap();
    fs::write(dir.join("playbook.yaml"), SNAPSHOT).unwrap();
}

/// A project whose `demo` 1.0.0 is current.
fn seed_playbook(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let v = root.join(".apb/playbooks/demo/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), SNAPSHOT).unwrap();
    fs::write(root.join(".apb/playbooks/demo/current"), "1.0.0").unwrap();
}

pub(crate) async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let res = router
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn an_ended_run_is_graded_by_rules_and_its_grades_listed() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    seed_playbook(dir.path());
    seed_run(dir.path(), "r1", Some("succeeded"));
    seed_run(dir.path(), "live", None);
    let router = build_router(AppState::new(dir.path().to_path_buf()));

    let (st, v) = call(&router, "GET", "/api/runs/r1/grades", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!({ "run_id": "r1", "grades": [] }));

    let (st, v) = call(&router, "POST", "/api/runs/r1/grades", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        v["grade"]["grader"],
        json!({ "kind": "rules", "version": 1 })
    );
    assert_eq!(v["grade"]["criteria"][0]["id"], "outcome");
    assert_eq!(v["grade"]["criteria"][0]["level"], "good");
    // No candidate on trial: nothing re-decided.
    assert!(v.get("candidate").is_none());

    let (_, v) = call(&router, "GET", "/api/runs/r1/grades", None).await;
    assert_eq!(v["grades"].as_array().unwrap().len(), 1);

    // The model grader: no decision model is on, so nothing is stored and
    // the reason is named; an unknown grader is refused.
    let (st, v) = call(
        &router,
        "POST",
        "/api/runs/r1/grades",
        Some(json!({ "grader": "model" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["grader"].clone(), v["skipped"].clone()),
        (json!("model"), json!("off"))
    );
    assert!(v.get("grade").is_none());
    let (st, _) = call(
        &router,
        "POST",
        "/api/runs/r1/grades",
        Some(json!({ "grader": "judge" })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, v) = call(&router, "GET", "/api/runs/r1/grades", None).await;
    assert_eq!(v["grades"].as_array().unwrap().len(), 1);

    let (st, _) = call(&router, "POST", "/api/runs/live/grades", None).await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "a live run is graded once it ends"
    );
    let (st, _) = call(&router, "GET", "/api/runs/nope/grades", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn without_a_candidate_the_card_is_empty_and_a_decision_is_404() {
    let dir = tempfile::tempdir().unwrap();
    seed_playbook(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (st, v) = call(&router, "GET", "/api/playbooks/demo/candidate", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!({ "id": "demo", "candidate": null }));
    let (st, _) = call(
        &router,
        "POST",
        "/api/playbooks/demo/candidate/promote",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &router,
        "POST",
        "/api/playbooks/demo/candidate/keep-current",
        Some(json!({ "reason": "not now" })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_person_keeps_current_from_the_dashboard() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(root);
    let run_id = apb_engine::run(root, "demo", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    let candidate = apb_engine::forward_patch::create(
        root,
        &run_id,
        &apb_engine::forward_patch::ForwardPatchRequest {
            yaml: SNAPSHOT.replace("name: Demo", "name: Demo improved"),
            classification: "improvement".into(),
            rationale: Some("a clearer name".into()),
            evidence: vec!["the name".into()],
        },
    )
    .unwrap()
    .version;
    let router = build_router(AppState::new(root.to_path_buf()));

    let (st, v) = call(&router, "GET", "/api/playbooks/demo/candidate", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["candidate"]["version"], candidate.as_str());
    assert_eq!(v["candidate"]["current"], "1.0.0");
    // Not a quality policy: no gate and no comparison.
    assert!(v["candidate"].get("gate").is_none());

    let (st, v) = call(
        &router,
        "POST",
        "/api/playbooks/demo/candidate/keep-current",
        Some(json!({ "reason": "  the old name\nreads better " })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["verdict"], "keep_current");
    assert_eq!(v["reason"], "the old name reads better");
    assert_eq!(v["decided_by"], "dashboard");
    let (_, v) = call(&router, "GET", "/api/playbooks/demo/candidate", None).await;
    assert_eq!(v["candidate"], Value::Null);
    assert_eq!(
        fs::read_to_string(root.join(".apb/playbooks/demo/current"))
            .unwrap()
            .trim(),
        "1.0.0"
    );
}

#[tokio::test]
async fn reopening_a_run_without_a_host_grade_is_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    seed_playbook(dir.path());
    seed_run(dir.path(), "r1", Some("succeeded"));
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (st, _) = call(&router, "POST", "/api/runs/r1/grades/reopen", None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = call(&router, "POST", "/api/runs/nope/grades/reopen", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
