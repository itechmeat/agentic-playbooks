//! Known problems on the HTTP API: the playbook's list, one problem with
//! its changed passages, a person's confirm, dismiss and reopen from the
//! dashboard, a close refused without a quote, a recheck by the person's
//! word, the run detail's `known_problems` block, and every write refused
//! when the server itself runs as a step of a run.

use apb_server::{AppState, build_router};
use axum::http::StatusCode;
use serde_json::json;
use std::fs;
use std::path::Path;

use crate::common;
use crate::quality_api_test::call;

const EVENTS: &str = include_str!("../../../apb-engine/tests/fixtures/known_problems/events.jsonl");
const SHOWN: &str = r#"{"seq":7,"ts":1007,"type":"known_problems_shown","shown":[{"id":"kp-1","scope":"path:scripts/build.sh","symptom":"the build fails","basis":"unchanged"}],"hidden":[{"id":"kp-2","reason":"source_changed_unchecked"}]}"#;

/// A project whose one ended run of `fix-build` reported its build script
/// and was collected; returns the build-script problem's id.
fn seeded(root: &Path) -> String {
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::registry::init_project(root).unwrap();
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::write(
        root.join("scripts/build.sh"),
        "cc -Werror=deprecated -c z.c\n",
    )
    .unwrap();
    let run = root.join(".apb/runs/fix-build-1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("events.jsonl"), format!("{EVENTS}{SHOWN}\n")).unwrap();
    apb_core::run_origin::stamp(&run, "fix-build-1").unwrap();
    apb_engine::known_problems::observe(root, &run).unwrap();
    apb_engine::known_problems::view_current(root, apb_core::scope::Scope::Project, "fix-build")
        .unwrap()
        .problems
        .iter()
        .find(|p| p.problem.scope.key() == "path:scripts/build.sh")
        .expect("the build-script problem")
        .problem
        .id
        .clone()
}

#[tokio::test]
async fn the_dashboard_reads_and_decides_known_problems() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let id = seeded(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let base = format!("/api/playbooks/fix-build/known-problems/{id}");

    let (st, v) = call(
        &router,
        "GET",
        "/api/playbooks/fix-build/known-problems",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["playbook"].clone(), v["scope"].clone()),
        (json!("fix-build"), json!("project"))
    );
    assert!(v["signed"].as_bool().unwrap());
    assert!(
        v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == id.as_str())
    );

    let (st, v) = call(&router, "GET", &base, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["problem"]["source_now"], "unchanged");
    assert_eq!(v["changed_passages"], json!([]));

    let (st, v) = call(
        &router,
        "POST",
        &format!("{base}/confirm"),
        Some(json!({ "reason": "seen it twice" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["confirmed"]["by"], "dashboard");
    let (_, v) = call(&router, "POST", &format!("{base}/dismiss"), None).await;
    assert_eq!(v["status"], "dismissed");
    let (_, v) = call(&router, "POST", &format!("{base}/reopen"), None).await;
    assert_eq!(v["status"], "open");

    let (st, _) = call(&router, "POST", &format!("{base}/close"), Some(json!({}))).await;
    assert_eq!(
        st,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a close needs a quote"
    );
    let (st, _) = call(&router, "POST", &format!("{base}/delete"), None).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    // The old `holds` field is refused, never read as a model recheck.
    let (st, _) = call(
        &router,
        "POST",
        &format!("{base}/recheck"),
        Some(json!({ "holds": true })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _) = call(
        &router,
        "POST",
        &format!("{base}/recheck"),
        Some(json!({ "verdict": "fixed" })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, v) = call(
        &router,
        "POST",
        &format!("{base}/recheck"),
        Some(json!({ "verdict": "holds" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["verdict"].clone(), v["basis"].clone()),
        (json!("holds"), json!("person"))
    );
    // No decision model is on: a model recheck is a conflict.
    let (st, _) = call(&router, "POST", &format!("{base}/recheck"), None).await;
    assert_eq!(st, StatusCode::CONFLICT);

    let (st, _) = call(
        &router,
        "GET",
        "/api/playbooks/fix-build/known-problems/kp-nope",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &router,
        "GET",
        "/api/playbooks/fix-build/known-problems?scope=team",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    let (st, v) = call(&router, "GET", "/api/runs/fix-build-1", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        v["known_problems"]["shown"][0]["id"], "kp-1",
        "{}",
        v["known_problems"]
    );
    assert_eq!(
        v["known_problems"]["hidden"][0]["reason"],
        "source_changed_unchecked"
    );
}

#[tokio::test]
async fn a_server_running_as_a_run_step_reads_but_never_writes_problems() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let id = seeded(dir.path());
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let _step = common::EnvVar::set("APB_RUN_ID", "fix-build-2");
    let base = format!("/api/playbooks/fix-build/known-problems/{id}");
    let (st, v) = call(&router, "GET", &base, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    for (action, body) in [
        ("confirm", None),
        ("dismiss", None),
        (
            "close",
            Some(json!({ "quote": "cc -Werror=deprecated -c z.c" })),
        ),
        ("recheck", Some(json!({ "verdict": "holds" }))),
    ] {
        let (st, v) = call(&router, "POST", &format!("{base}/{action}"), body).await;
        assert_eq!(st, StatusCode::CONFLICT, "{action}: {v}");
    }
    let stored = apb_core::known_problems::store::read(
        dir.path(),
        apb_core::scope::Scope::Project,
        "fix-build",
    )
    .unwrap();
    let problem = stored.file.get(&id).unwrap();
    assert!(problem.confirmed.is_none() && problem.check.is_none());
}
