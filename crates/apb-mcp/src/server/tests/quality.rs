//! Run quality on the MCP tools: `run_status` carries the rules grade the
//! run got as it ended, a host grades the playbook's own criterion through
//! `run_grade_start`, `run_grade_read` and `run_grade_submit` (arguments
//! as a client sends them), `run_grades` lists both grades,
//! `playbook_candidate` answers `null` without a candidate, `run_grade`
//! grades by rules or names why the model grader stored nothing, and a
//! server that runs as a run step refuses every grading and known-problem
//! write while it still reads.

use super::*;

const GRADED: &str = r#"
schema: 2
id: graded
name: Graded
version: 1.0.0
quality:
  criteria:
    - id: report-clear
      reads: [outputs]
      levels: { good: "the report names every change", partial: "the report misses one change", poor: "the report is wrong" }
nodes:
  - { id: start, type: start }
  - { id: p1, type: script, script: "scripts/ok.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

fn seed_graded(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/graded/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), GRADED).unwrap();
    fs::write(vdir.join("scripts/ok.sh"), "echo changed the parser\n").unwrap();
    fs::write(root.join(".apb/playbooks/graded/current"), "1.0.0").unwrap();
}

fn json_of(res: &CallToolResult) -> serde_json::Value {
    serde_json::from_str(&result_text(res)).unwrap()
}

fn args<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).unwrap())
}

#[tokio::test]
async fn a_host_grades_a_run_through_the_tools_and_run_status_shows_the_grades() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_graded(proj.path());
    let run_id = apb_engine::run(
        proj.path(),
        "graded",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap()
    .run_id;
    let server = WfMcp::new(proj.path().to_path_buf());

    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: run_id.clone(),
                workspace: None,
            }))
            .await,
    );
    assert_eq!(status["grades"][0]["grader"], "rules/1", "{status}");
    assert_eq!(status["grades"][0]["levels"]["outcome"], "good");

    let start = json_of(
        &server
            .run_grade_start(args(json!({ "run_id": run_id })))
            .await,
    );
    let session = start["session"].as_str().expect("a session").to_string();
    assert_eq!(start["criteria"][0]["id"], "report-clear");

    // A read without its ref is refused before the engine is asked.
    let bad = json_of(
        &server
            .run_grade_read(args(
                json!({ "run_id": run_id, "session": session, "op": "read" }),
            ))
            .await,
    );
    assert_eq!(bad["error"], "invalid_arguments");

    let read = json_of(
        &server
            .run_grade_read(args(
                json!({ "run_id": run_id, "session": session, "op": "read", "ref": "o:p1" }),
            ))
            .await,
    );
    assert_eq!(read["items"][0]["citable"], true, "{read}");

    let wrong_level = json_of(
        &server
            .run_grade_submit(args(json!({
                "run_id": run_id, "session": session,
                "grades": [{ "criterion": "report-clear", "level": "great" }],
            })))
            .await,
    );
    assert_eq!(wrong_level["error"], "invalid_arguments");

    let submitted = json_of(
        &server
            .run_grade_submit(args(json!({
                "run_id": run_id, "session": session,
                "grades": [{
                    "criterion": "report-clear", "level": "good",
                    "reason_code": "names_changes", "why": "the report names the change",
                    "cites": [{ "ref": "o:p1" }],
                }],
            })))
            .await,
    );
    assert_eq!(submitted["status"], "graded", "{submitted}");
    assert_eq!(submitted["grade"]["grader"]["kind"], "host");
    // No candidate on trial: nothing to re-decide.
    assert!(submitted.get("candidate").is_none());

    let grades = json_of(&server.run_grades(args(json!({ "run_id": run_id }))).await);
    assert_eq!(grades["grades"].as_array().unwrap().len(), 2);
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: run_id.clone(),
                workspace: None,
            }))
            .await,
    );
    // In grader order: the rules grader, then the host.
    assert_eq!(status["grades"][0]["grader"], "rules/1");
    assert_eq!(status["grades"][1]["grader"], "host/1");
    assert_eq!(status["grades"][1]["levels"]["report-clear"], "good");
    // A run takes one host grade: a second session waits for a person.
    let again = server
        .run_grade_start(args(json!({ "run_id": run_id })))
        .await;
    assert_eq!(again.is_error, Some(true));
    assert!(result_text(&again).contains("--reopen"));

    let candidate = json_of(
        &server
            .playbook_candidate(args(json!({ "id": "graded" })))
            .await,
    );
    assert_eq!(candidate, json!({ "id": "graded", "candidate": null }));
}

#[tokio::test]
async fn run_grade_grades_by_rules_or_names_why_the_model_did_not() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_graded(proj.path());
    let run_id = apb_engine::run(
        proj.path(),
        "graded",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap()
    .run_id;
    let server = WfMcp::new(proj.path().to_path_buf());
    let rules = json_of(&server.run_grade(args(json!({ "run_id": run_id }))).await);
    assert_eq!(rules["grader"], "rules", "{rules}");
    assert_eq!(rules["grade"]["grader"]["kind"], "rules");
    // No decision model is configured: nothing is asked or stored.
    let model = json_of(
        &server
            .run_grade(args(json!({ "run_id": run_id, "grader": "model" })))
            .await,
    );
    assert_eq!(model["skipped"], "off", "{model}");
    assert!(model.get("grade").is_none());
    let bad = json_of(
        &server
            .run_grade(args(json!({ "run_id": run_id, "grader": "judge" })))
            .await,
    );
    assert_eq!(bad["error"], "invalid_arguments");
    assert!(bad["detail"].as_str().unwrap().contains("rules or model"));
}

#[tokio::test]
async fn the_grade_tools_refuse_an_unknown_run() {
    let proj = tempfile::tempdir().unwrap();
    seed_graded(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());
    let res = server
        .run_grade_start(args(json!({ "run_id": "../nope" })))
        .await;
    assert_eq!(res.is_error, Some(true));
    let res = server.run_grades(args(json!({ "run_id": "nope" }))).await;
    assert_eq!(res.is_error, Some(true));
}

/// Sets `APB_RUN_ID` for one test and puts the earlier value back when
/// dropped, even when the test panics.
struct RunIdGuard(Option<std::ffi::OsString>);

impl RunIdGuard {
    fn set(value: &str) -> Self {
        let old = std::env::var_os("APB_RUN_ID");
        // SAFETY: under CROSS_ENV_LOCK, like every env change in these tests.
        unsafe { std::env::set_var("APB_RUN_ID", value) };
        RunIdGuard(old)
    }
}

impl Drop for RunIdGuard {
    fn drop(&mut self) {
        // SAFETY: under CROSS_ENV_LOCK, like every env change in these tests.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("APB_RUN_ID", v),
                None => std::env::remove_var("APB_RUN_ID"),
            }
        }
    }
}

#[tokio::test]
async fn a_server_that_runs_as_a_run_step_refuses_to_grade_or_decide() {
    // A server an agent CLI started inside a run inherits `APB_RUN_ID`.
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let proj = tempfile::tempdir().unwrap();
    seed_graded(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());
    let results = {
        let _env = RunIdGuard::set("r-step");
        let start = server
            .run_grade_start(args(json!({ "run_id": "any" })))
            .await;
        let submit = server
            .run_grade_submit(args(
                json!({ "run_id": "any", "session": "s", "grades": [] }),
            ))
            .await;
        let grade = server
            .run_grade(args(json!({ "run_id": "any", "grader": "model" })))
            .await;
        let decide = server
            .known_problem_decide(args(json!({
                "id": "graded", "problem": "kp-0000000000", "action": "confirm",
            })))
            .await;
        let recheck = server
            .known_problem_recheck(args(json!({
                "id": "graded", "problem": "kp-0000000000", "by": "model",
            })))
            .await;
        // Reading is allowed from a run step.
        let read = server
            .playbook_known_problems(args(json!({ "id": "graded" })))
            .await;
        assert_ne!(read.is_error, Some(true), "{}", result_text(&read));
        [start, submit, grade, decide, recheck]
    };
    for res in results {
        assert_eq!(res.is_error, Some(true));
        assert!(
            result_text(&res).contains("run step"),
            "{}",
            result_text(&res)
        );
    }
}
