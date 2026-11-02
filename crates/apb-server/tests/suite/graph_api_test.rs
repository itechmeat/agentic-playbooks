//! The graph layers' endpoints: the editor's draft validation (reads only),
//! the overlay of a run on a version, and the run detail's graph.

use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

const NOAGENT: &str = r#"
schema: 2
id: noagent
name: No Agent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
  - { id: spare, type: finish, outcome: failure }
edges:
  - { from: start, to: note }
  - { from: note, to: done, condition: { type: node_status, node: note, equals: success } }
  - { from: note, to: spare, condition: { type: node_status, node: note, equals: failure } }
"#;

/// A project with the playbook and one finished run of it.
fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
    apb_engine::run(root, "noagent", None, apb_engine::RunOptions::default()).unwrap();
    dir
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&b).unwrap())
        }
        None => Body::empty(),
    };
    let res = app.oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// Every file under `root`, with its bytes.
fn snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((p.clone(), fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn draft_validation_reports_issues_and_writes_nothing() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seeded();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let before = snapshot(dir.path());
    let broken = NOAGENT.replace("{ from: note, to: done,", "{ from: note, to: nowhere,");
    let (status, json) = call(
        app.clone(),
        "POST",
        "/api/validate?id=noagent&version=1.0.0",
        Some(serde_json::json!({ "yaml": broken })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["parse_error"].is_null());
    let issues = json["issues"].as_array().unwrap();
    let v06 = issues.iter().find(|i| i["code"] == "V06").expect("V06");
    assert_eq!(v06["edges"][0], "e:note>nowhere#1");
    let (_, json) = call(
        app,
        "POST",
        "/api/validate",
        Some(serde_json::json!({ "yaml": "nodes: [" })),
    )
    .await;
    assert!(json["parse_error"].is_string());
    assert_eq!(json["issues"], serde_json::json!([]));
    assert_eq!(
        snapshot(dir.path()),
        before,
        "draft validation writes nothing"
    );
}

#[tokio::test]
async fn a_playbook_named_validate_still_loads() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/validate/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        NOAGENT.replace("id: noagent", "id: validate"),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/validate/current"), "1.0.0").unwrap();
    let app = build_router(AppState::new(root.to_path_buf()));
    let (status, json) = call(app, "GET", "/api/playbooks/validate", None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["id"], "validate");
}

/// A declared secret in what the chosen run produced never reaches the
/// overlay's graph.
#[tokio::test]
async fn the_overlay_redacts_the_chosen_runs_secrets() {
    let _cfg = crate::common::config_sandbox().await;
    let secret = "s3cr3t_overlay_value_0123456789";
    let yaml = format!(
        "schema: 2\nid: judged\nname: judged\nversion: 1.0.0\nparams:\n  - {{ name: token, type: text, secret: true }}\nnodes:\n  - {{ id: start, type: start }}\n  - id: triage\n    type: judge\n    state: {{ x: \"a\" }}\n    questions:\n      verdict: {{ type: choice, instructions: \"Pick\", criteria: {{ {secret}: a, no: b }} }}\n    on_unavailable: {{ default: {{ verdict: {secret} }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: triage }}\n  - {{ from: triage, to: done }}\n"
    );
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/judged/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), &yaml).unwrap();
    fs::write(root.join(".apb/playbooks/judged/current"), "1.0.0").unwrap();
    let mut opts = apb_engine::RunOptions::default();
    opts.params.insert("token".into(), secret.into());
    apb_engine::run(root, "judged", None, opts).unwrap();
    let app = build_router(AppState::new(root.to_path_buf()));
    let (status, json) = call(app, "GET", "/api/playbooks/judged/overlay", None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let graph = json["graph"].to_string();
    assert!(graph.contains("triage"), "the graph is there: {graph}");
    assert!(!graph.contains(secret), "{graph}");
}

#[tokio::test]
async fn the_overlay_lays_the_newest_exact_run_over_its_version() {
    let _cfg = crate::common::config_sandbox().await;
    let dir = seeded();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = call(app.clone(), "GET", "/api/playbooks/noagent/overlay", None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let run = json["default_run"]
        .as_str()
        .expect("an exact run")
        .to_string();
    assert_eq!(json["chosen"], run.as_str());
    assert_eq!(json["alignment"], "exact");
    assert_eq!(json["candidates"][0]["alignment"], "exact");
    let edges = json["graph"]["edges"].as_array().unwrap();
    let state = |r: &str| {
        edges
            .iter()
            .find(|e| e["rendered"] == r)
            .map(|e| e["state"].clone())
    };
    assert_eq!(state("e:note>done#1"), Some(serde_json::json!("taken")));
    assert_eq!(
        state("e:note>spare#1"),
        Some(serde_json::json!("not_taken"))
    );
    assert_eq!(
        json["mapping"][0]["missing_in_shown"],
        serde_json::json!([])
    );
    let (status, _) = call(
        app.clone(),
        "GET",
        "/api/playbooks/noagent/overlay?run=..%2Fx",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The HTML report has the route as text.
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/api/runs/{run}/report.html"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(html.contains("<h2>Route</h2>"), "{html}");
    assert!(html.contains("e:note&gt;done#1"), "{html}");
    assert!(
        html.contains("taken 1") && html.contains("not taken"),
        "{html}"
    );
    assert!(html.contains("not run spare: observed"), "{html}");

    // The run page carries the same graph and the definition it drew.
    let (status, detail) = call(app, "GET", &format!("/api/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["graph"]["shown_segment"], 0);
    assert_eq!(detail["graph_model"]["id"], "noagent");
    let spare = detail["graph"]["not_run"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "spare")
        .unwrap();
    assert_eq!(spare["class"], "observed", "{spare}");
}
