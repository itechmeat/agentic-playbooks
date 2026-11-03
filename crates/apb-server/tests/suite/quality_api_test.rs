//! Run quality on the HTTP API: `GET` and `POST /api/runs/{id}/grades`
//! (the rules grade of an ended run, refused while the run is live; the
//! model grader names why it stored nothing),
//! `POST /api/runs/{id}/grades/reopen` (409 without a host grade), and a
//! playbook's candidate: `GET /api/playbooks/{id}/candidate` and a
//! person's Keep current or Promote from the dashboard, a promotion's
//! refusal answered as data.

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
        Some(json!({ "expected_version": "1.0.1", "expected_digest": "sha256:00" })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // An older dashboard promotes without naming what it showed: refused
    // by its shape, before the candidate is looked up.
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/playbooks/demo/candidate/promote")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let text = res.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&text).contains("expected_required"),
        "{text:?}"
    );
    let (st, _) = call(
        &router,
        "POST",
        "/api/playbooks/demo/candidate/keep-current",
        Some(json!({ "reason": "not now" })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// A project whose `demo` has a candidate (a clearer name) made by a
/// forward patch of one run; returns the candidate's version.
fn seed_candidate(root: &Path) -> String {
    seed_playbook(root);
    seed_candidate_on(root)
}

/// The candidate of [`seed_candidate`] on a project already seeded.
fn seed_candidate_on(root: &Path) -> String {
    let run_id = apb_engine::run(root, "demo", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    apb_engine::forward_patch::create(
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
    .version
}

#[tokio::test]
async fn a_dashboard_promotion_is_refused_as_data_until_it_names_what_stands() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let candidate = seed_candidate(root);
    // A scenario that never ran: an item to acknowledge.
    let scenarios = root.join(".apb/playbooks/demo/evals/scenarios");
    fs::create_dir_all(&scenarios).unwrap();
    fs::write(scenarios.join("walk.yaml"), "schema: 1\nid: walk\n").unwrap();
    let summary = apb_mcp::tools::candidate::candidate_summary(root, "demo")
        .unwrap()
        .unwrap();
    let digest = summary.candidate.digest.clone();
    let router = build_router(AppState::new(root.to_path_buf()));
    let promote = |d: &str, ack: Option<&str>| {
        let mut b = json!({ "expected_version": candidate, "expected_digest": d });
        if let Some(a) = ack {
            b["acknowledged"] = json!(a);
        }
        b
    };
    let uri = "/api/playbooks/demo/candidate/promote";

    // The candidate's own fitness, as the summary's fitness axis shows it.
    let fit = format!("/api/playbooks/demo/fit?version={candidate}");
    let (st, v) = call(&router, "GET", &fit, None).await;
    assert_eq!(
        (st, v["version"].as_str()),
        (StatusCode::OK, Some(candidate.as_str())),
        "{v}"
    );

    let (st, v) = call(&router, "POST", uri, Some(promote("sha256:00", None))).await;
    assert_eq!(
        (st, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("stale")),
        "{v}"
    );
    assert_eq!(v["found"]["digest"], digest.as_str());

    let (st, v) = call(&router, "POST", uri, Some(promote(&digest, None))).await;
    assert_eq!(
        (st, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("needs_acknowledgement")),
        "{v}"
    );
    assert_eq!(v["summary_id"], summary.summary_id.as_str());
    assert_eq!(v["items"][0]["code"], "scenario_not_run");

    let (st, v) = call(
        &router,
        "POST",
        uri,
        Some(promote(&digest, Some(&summary.summary_id))),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["summary_id"], summary.summary_id.as_str());
    assert_eq!(
        fs::read_to_string(root.join(".apb/playbooks/demo/current"))
            .unwrap()
            .trim(),
        candidate
    );
}

#[tokio::test]
async fn a_person_keeps_current_from_the_dashboard() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let candidate = seed_candidate(root);
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

// --- the judge sandbox and labelled examples --------------------------------

const JUDGE: &str = "schema: 2\nid: doc\nname: Doc\nversion: 1.0.0\nparams:\n  - { name: evidence, type: string, default: \"\" }\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    state: { task: \"{{run.instruction}}\", evidence: \"{{params.evidence}}\" }\n    questions:\n      verdict: { type: choice, instructions: \"Which?\", criteria: { clean: a, needs_fix: b } }\n    thresholds:\n      verdict: { min_confidence: 0.5, below: needs_fix }\n    on_unavailable: { default: { verdict: needs_fix } }\n  - { id: fix, type: finish, outcome: success }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { id: to-fix, from: triage, to: fix, condition: { type: output_field, node: triage, field: verdict, equals: needs_fix } }\n  - { from: triage, to: done, fallback: true }\n";

/// A project with `doc` 1.0.0 current and run `r1` of it: a baseline
/// observation before one judge execution the provider answered `clean`.
fn seed_judge(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let v = root.join(".apb/playbooks/doc/1.0.0");
    fs::create_dir_all(&v).unwrap();
    fs::write(v.join("playbook.yaml"), JUDGE).unwrap();
    fs::write(root.join(".apb/playbooks/doc/current"), "1.0.0").unwrap();
    let run = root.join(".apb/runs/r1");
    fs::create_dir_all(&run).unwrap();
    fs::write(run.join("playbook.yaml"), JUDGE).unwrap();
    fs::write(run.join("run.yaml"), "instruction: Triage the report\n").unwrap();
    let observed = serde_json::to_string(&json!({
        "count": apb_core::baseline::Observation::unknown("offline")
    }))
    .unwrap();
    fs::write(
        run.join("events.jsonl"),
        format!(
            "{{\"seq\":0,\"ts\":1790000000000,\"type\":\"run_started\",\"playbook\":\"doc\",\"version\":\"1.0.0\"}}\n\
             {{\"seq\":1,\"ts\":1790000000000,\"type\":\"baseline_observed\",\"check\":\"open_issues\",\"phase\":\"before\",\"status\":\"unknown\",\"observations\":{observed}}}\n\
             {{\"seq\":2,\"ts\":1790000000000,\"type\":\"node_started\",\"node\":\"triage\",\"attempt\":1}}\n\
             {{\"seq\":3,\"ts\":1790000000000,\"type\":\"decision_made\",\"use_site\":\"judge_node\",\"node\":\"triage\",\"provider\":\"fake\",\"model\":\"fake-1\",\"mode\":\"enforce\",\"answers\":{{\"verdict\":{{\"value\":\"clean\",\"p\":0.9,\"confidence\":0.9}}}}}}\n\
             {{\"seq\":4,\"ts\":1790000001000,\"type\":\"node_finished\",\"node\":\"triage\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"{{}}\",\"artifacts\":[]}}\n"
        ),
    )
    .unwrap();
}

/// An evaluation over a run's baseline observation placed into an input
/// answers alike on HTTP and on the tool layer MCP and the CLI share; a
/// dashboard without keys still evaluates given answers but neither asks a
/// provider nor saves, and one with keys saves and checks the example.
#[tokio::test]
async fn the_judge_sandbox_answers_alike_and_a_keyless_dashboard_never_spends() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_judge(root);
    let router = build_router(AppState::new(root.to_path_buf()));

    let (st, values) = call(&router, "GET", "/api/runs/r1/judge/triage/values", None).await;
    assert_eq!(st, StatusCode::OK, "{values}");
    let baseline = values["recorded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "baseline.open_issues.before")
        .map(|r| r["text"].as_str().unwrap().to_string())
        .expect("the baseline observation is offered");
    let mut inputs = values["inputs"].clone();
    inputs["params"]["evidence"] = json!(baseline);
    let body = json!({
        "inputs": inputs,
        "answers": { "verdict": { "value": "clean", "p": 0.9, "confidence": 0.9 } },
    });
    let (st, http) = call(
        &router,
        "POST",
        "/api/playbooks/doc/judge/triage/evaluate",
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{http}");
    let req: apb_engine::judge_eval::sandbox::SandboxRequest =
        serde_json::from_value(body).unwrap();
    let step = apb_core::step_env::StepCaller::default();
    let tool = apb_mcp::tools::judge::judge_evaluate(
        root,
        "doc",
        "triage",
        &req,
        apb_mcp::tools::judge::JudgeCaller::of(&step),
    )
    .unwrap();
    assert_eq!(http, serde_json::to_value(&tool).unwrap());
    let evidence = http["evaluation"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "evidence")
        .unwrap();
    assert_eq!(evidence["bytes"].as_u64().unwrap() as usize, baseline.len());
    assert_eq!(http["route_at_enforce"], json!(["e:triage>done#1"]));

    // Without keys: no live ask, no save.
    let (st, _) = call(
        &router,
        "POST",
        "/api/playbooks/doc/judge/triage/evaluate",
        Some(json!({ "inputs": {} })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let save = json!({ "expected": { "verdict": "clean" } });
    let (st, _) = call(
        &router,
        "POST",
        "/api/runs/r1/judge/triage/examples",
        Some(save.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert!(!root.join(".apb/playbooks/doc/evals").exists());

    // With keys: the save goes through and the check agrees.
    let (key, state) = crate::common::keyed(root.to_path_buf());
    let keyed = build_router(state);
    let send = |method: &'static str, uri: &'static str, body: Option<Value>| {
        let keyed = keyed.clone();
        let key = key.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {key}"));
            let res =
                keyed
                    .oneshot(
                        req.body(body.map_or_else(Body::empty, |b| {
                            Body::from(serde_json::to_vec(&b).unwrap())
                        }))
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let (st, saved) = send("POST", "/api/runs/r1/judge/triage/examples", Some(save)).await;
    assert_eq!(st, StatusCode::OK, "{saved}");
    assert_eq!(
        (saved["id"].clone(), saved["labelled_by"].clone()),
        (json!("ex-0001"), json!("dashboard"))
    );
    let (st, listed) = send("GET", "/api/playbooks/doc/judge/triage/examples", None).await;
    assert_eq!(st, StatusCode::OK, "{listed}");
    assert_eq!(listed["nodes"][0]["examples"].as_array().unwrap().len(), 1);
    assert!(listed.get("checks").is_none());
    let (st, checked) = send(
        "POST",
        "/api/playbooks/doc/judge/triage/examples/check",
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{checked}");
    assert_eq!(
        (
            checked["checks"][0]["agree"].clone(),
            checked["checks"][0]["disagree"].clone()
        ),
        (json!(1), json!(0))
    );
}

/// `GET /api/playbooks/{id}/candidate/summary`: the summary the promote
/// body names (`summary: null` without a candidate).
#[tokio::test]
async fn the_candidate_summary_route_answers_what_a_promotion_names() {
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(root);
    let router = build_router(AppState::new(root.to_path_buf()));
    let uri = "/api/playbooks/demo/candidate/summary";
    let (st, v) = call(&router, "GET", uri, None).await;
    assert_eq!(
        (st, v),
        (StatusCode::OK, json!({ "id": "demo", "summary": null }))
    );
    let candidate = seed_candidate_on(root);
    let (st, v) = call(&router, "GET", uri, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let s = &v["summary"];
    assert_eq!(s["candidate"]["version"], candidate.as_str());
    let (st, v) = call(
        &router,
        "POST",
        "/api/playbooks/demo/candidate/promote",
        Some(json!({
            "expected_version": s["candidate"]["version"],
            "expected_digest": s["candidate"]["digest"],
            "acknowledged": s["summary_id"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

/// `GET /api/demo` and `POST /api/demo`: the demo is installed once under
/// the config dir, registered, and its status says so.
#[tokio::test]
async fn the_demo_route_installs_once_and_reports_it() {
    let sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let router = build_router(AppState::new(dir.path().to_path_buf()));
    let (st, v) = call(&router, "GET", "/api/demo", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (v["installed"].as_bool(), v["modified"].as_bool()),
        (Some(false), Some(false))
    );
    let (st, first) = call(&router, "POST", "/api/demo", None).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    assert_eq!(first["playbook"], "demo-report");
    assert!(first["workspace_id"].is_string(), "{first}");
    assert!(
        sandbox
            .path()
            .join("demo/.apb/playbooks/demo-report/1.0.0/scripts/write-report.sh")
            .is_file()
    );
    let (_, again) = call(&router, "POST", "/api/demo", None).await;
    assert_eq!(again, first);
    let (_, v) = call(&router, "GET", "/api/demo", None).await;
    assert_eq!(v["installed"], true);
    assert_eq!(v["project"], first);
}

/// One request whose answer may be text (a refusal): status and body.
async fn call_text(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The scenario routes: the list shows a stored result fresh for the
/// version, a stored run reads back by id only under its own playbook, and
/// what is refused before any run is refused without starting one: a cap
/// at quick, a smoke run without a cap, a smoke run from a dashboard
/// without keys, a scenario that is not there.
#[tokio::test]
async fn the_scenario_routes_list_read_and_refuse_before_any_run() {
    use apb_engine::scenario::report::{ScenarioReport, Subject, Verdict};
    use apb_engine::scenario::store;
    let _sandbox = common::config_sandbox().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_playbook(root);
    let scenarios = root.join(".apb/playbooks/demo/evals/scenarios");
    fs::create_dir_all(&scenarios).unwrap();
    fs::write(
        scenarios.join("ends.yaml"),
        "schema: 1\nid: ends\nexpect: { finish: done }\n",
    )
    .unwrap();
    let router = build_router(AppState::new(root.to_path_buf()));

    let (st, listed) = call(&router, "GET", "/api/playbooks/demo/scenarios", None).await;
    assert_eq!(st, StatusCode::OK, "{listed}");
    assert_eq!(listed["scenarios"][0]["name"], "ends");
    assert_eq!(listed["scenarios"][0]["latest"], json!([]));

    let subject = Subject {
        level: apb_core::scenario::ScenarioLevel::Quick,
        scenario: "ends".into(),
        scenario_digest: listed["scenarios"][0]["digest"].as_str().unwrap().into(),
        playbook: "demo".into(),
        version: "1.0.0".into(),
        playbook_digest: listed["playbook_digest"].as_str().unwrap().into(),
    };
    let mut report = ScenarioReport::not_run(&subject, Verdict::Passed, "stored".into());
    report.run_id = Some("demo-s1".into());
    store::write_report(&store::runs_root(root).join("demo-s1"), &report).unwrap();
    store::append(root, "demo", &store::IndexLine::of(&report, 5).unwrap()).unwrap();

    let (_, listed) = call(&router, "GET", "/api/playbooks/demo/scenarios", None).await;
    let latest = &listed["scenarios"][0]["latest"][0];
    assert_eq!(
        (&latest["run_id"], &latest["fresh"]),
        (&json!("demo-s1"), &json!(true)),
        "{listed}"
    );
    let (st, view) = call(
        &router,
        "GET",
        "/api/playbooks/demo/scenario-runs/demo-s1",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(view["report"], serde_json::to_value(&report).unwrap());
    for uri in [
        "/api/playbooks/other/scenario-runs/demo-s1",
        "/api/playbooks/demo/scenario-runs/demo-s2",
    ] {
        let (st, _) = call(&router, "GET", uri, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{uri}");
    }

    let run = "/api/playbooks/demo/scenario-runs";
    let (st, why) = call_text(
        &router,
        "POST",
        run,
        json!({ "scenario": "ends", "level": "quick", "max_decision_requests": 2 }),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    let (st, why) = call_text(
        &router,
        "POST",
        run,
        json!({ "scenario": "ends", "level": "smoke" }),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
    assert!(why.contains("needs_cap"), "{why}");
    let (st, why) = call_text(
        &router,
        "POST",
        run,
        json!({ "scenario": "ends", "level": "smoke", "max_decision_requests": 2 }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{why}");
    assert!(why.contains("without keys"), "{why}");
    let (st, why) = call_text(
        &router,
        "POST",
        run,
        json!({ "scenario": "missing", "level": "quick" }),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{why}");
    let left: Vec<_> = fs::read_dir(store::runs_root(root))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n != store::INDEX_DIR)
        .collect();
    assert_eq!(left, ["demo-s1"], "a refused request ran");
}
