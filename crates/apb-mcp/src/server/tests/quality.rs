//! Run quality on the MCP tools: `run_status` carries the rules grade the
//! run got as it ended, a host grades the playbook's own criterion through
//! `run_grade_start`, `run_grade_read` and `run_grade_submit` (arguments
//! as a client sends them), `run_grades` lists both grades,
//! `playbook_candidate` answers `null` without a candidate, `run_grade`
//! grades by rules or names why the model grader stored nothing, and a
//! server that runs as a run step refuses every grading and known-problem
//! write while it still reads, and a grading session over a run tree whose
//! secrets cannot be read is refused.

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

/// A child whose declared output carries its sensitive param, and a graded
/// parent that takes the child's outputs.
const SECRET_CHILD: &str = r#"
schema: 2
id: schild
name: schild
version: 1.0.0
params:
  - { name: password, type: text, default: orchard-candle }
outputs:
  pw: { from: { node: echo } }
nodes:
  - { id: start, type: start }
  - { id: echo, type: prompt, prompt: "{{params.password}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: echo }
  - { from: echo, to: done }
"#;

/// Its ordinary param `topic` holds the child's password, and its task
/// type renders it into every grade.
const GRADED_PARENT: &str = r#"
schema: 2
id: gparent
name: gparent
version: 1.0.0
params:
  - { name: topic, type: text, default: orchard-candle }
quality:
  task_type: "{{params.topic}}"
  criteria:
    - id: report-clear
      reads: [outputs]
      levels: { good: "the report names every change", partial: "the report misses one change", poor: "the report is wrong" }
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: schild, output: outputs }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sub }
  - { from: sub, to: done }
"#;

fn install(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
}

/// The child run the run `run_id` started.
fn child_of(root: &Path, run_id: &str) -> String {
    let journal =
        fs::read_to_string(root.join(".apb/runs").join(run_id).join("events.jsonl")).unwrap();
    journal
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|e| e["type"] == "child_run_started")
        .and_then(|e| e["run_id"].as_str().map(str::to_string))
        .expect("a child run")
}

/// A parent whose child's `run.yaml` no longer parses: the child's secret
/// cannot be discovered, so every grading surface refuses rather than hand
/// on the run's text (a record of the child's output, the task type the
/// child's password was rendered into). While it reads, a grade shows the
/// value replaced.
#[tokio::test]
async fn grading_refuses_a_run_tree_whose_secrets_cannot_be_read() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    install(root, "schild", SECRET_CHILD, &[]);
    install(root, "gparent", GRADED_PARENT, &[]);
    let run_id = apb_engine::run(root, "gparent", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    let server = WfMcp::new(root.to_path_buf());

    let grades = result_text(&server.run_grades(args(json!({ "run_id": run_id }))).await);
    assert!(grades.contains("\"task_type\""), "{grades}");
    assert!(!grades.contains("orchard-candle"), "{grades}");

    let child = child_of(root, &run_id);
    let child_yaml = root.join(".apb/runs").join(&child).join("run.yaml");
    fs::write(&child_yaml, "params: [unclosed\n").unwrap();
    let start = server
        .run_grade_start(args(json!({ "run_id": run_id })))
        .await;
    let graded = server
        .run_grade(args(json!({ "run_id": run_id, "grader": "rules" })))
        .await;
    let grades = server.run_grades(args(json!({ "run_id": run_id }))).await;
    for (tool, res) in [
        ("run_grade_start", start),
        ("run_grade", graded),
        ("run_grades", grades),
    ] {
        let shown = result_text(&res);
        assert!(!shown.contains("orchard-candle"), "{tool}: {shown}");
        assert_eq!(res.is_error, Some(true), "{tool}: {shown}");
        assert!(shown.contains("secrets_unresolved"), "{tool}: {shown}");
    }
}

/// A run whose output says a value no run of its tree declares yet.
const SAYS: &str = r#"
schema: 2
id: says
name: says
version: 1.0.0
quality:
  criteria:
    - id: report-clear
      reads: [outputs]
      levels: { good: "the report names every change", partial: "the report misses one change", poor: "the report is wrong" }
nodes:
  - { id: start, type: start }
  - { id: p1, type: script, script: "scripts/say.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

/// An open session reads the run tree as it is at each read: a sibling
/// that starts later with that value as its password hides it from the
/// next read, and once the sibling's `run.yaml` cannot be read, the next
/// read is refused, though the graded run's own journal never changed.
#[tokio::test]
async fn an_open_grading_session_reads_the_run_tree_anew_at_every_read() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    install(root, "says", SAYS, &[("say.sh", "echo orchard-candle\n")]);
    let run_id = apb_engine::run(root, "says", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    // The run is the child of a parent that starts a sibling later.
    let runs = root.join(".apb/runs");
    let parent = runs.join("p-1");
    fs::create_dir_all(&parent).unwrap();
    apb_engine::run_config::write_run_config(&parent, &Default::default()).unwrap();
    let started = |child: &str| apb_engine::event::EventPayload::ChildRunStarted {
        node_id: format!("n-{child}"),
        run_id: child.to_string(),
    };
    let mut log = apb_engine::event::EventLog::open(&parent).unwrap();
    log.append(started(&run_id)).unwrap();
    let dir = runs.join(&run_id);
    let mut cfg = apb_engine::run_config::read_run_config(&dir).unwrap();
    cfg.parent_run = Some("p-1".into());
    apb_engine::run_config::write_run_config(&dir, &cfg).unwrap();

    let server = WfMcp::new(root.to_path_buf());
    let start = json_of(
        &server
            .run_grade_start(args(json!({ "run_id": run_id })))
            .await,
    );
    let session = start["session"].as_str().expect("a session").to_string();
    let read = || async {
        server
            .run_grade_read(args(
                json!({ "run_id": run_id, "session": session, "op": "read", "ref": "o:p1" }),
            ))
            .await
    };
    let first = result_text(&read().await);
    assert!(first.contains("orchard-candle"), "{first}");

    let sibling = runs.join("c-1");
    fs::create_dir_all(&sibling).unwrap();
    let secret = apb_engine::run_config::RunConfig {
        params: [("password".to_string(), "orchard-candle".to_string())].into(),
        parent_run: Some("p-1".into()),
        ..Default::default()
    };
    apb_engine::run_config::write_run_config(&sibling, &secret).unwrap();
    log.append(started("c-1")).unwrap();
    let hidden = result_text(&read().await);
    assert!(!hidden.contains("orchard-candle"), "{hidden}");
    assert!(hidden.contains("o:p1"), "{hidden}");

    fs::write(sibling.join("run.yaml"), "params: [unclosed\n").unwrap();
    let refused = read().await;
    let shown = result_text(&refused);
    assert_eq!(refused.is_error, Some(true), "{shown}");
    assert!(shown.contains("secrets_unresolved"), "{shown}");
}

/// The secret values of the graded run's own params, set after it ran.
fn set_secrets(root: &Path, run_id: &str, params: &[(&str, &str)]) {
    let dir = root.join(".apb/runs").join(run_id);
    let mut cfg = apb_engine::run_config::read_run_config(&dir).unwrap();
    cfg.params = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    apb_engine::run_config::write_run_config(&dir, &cfg).unwrap();
}

/// A secret value replaced by another of the same length, while another
/// value repeats: the open session's cached sources were redacted without
/// the new value, so the next read hides it from the sources read anew.
#[tokio::test]
async fn an_open_grading_session_hides_a_secret_value_that_replaced_a_repeated_one() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    install(root, "says", SAYS, &[("say.sh", "echo cedar3\n")]);
    let run_id = apb_engine::run(root, "says", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    let secrets = |third| {
        [
            ("api_token", "amber1"),
            ("password", "birch2"),
            ("secret_key", third),
        ]
    };
    set_secrets(root, &run_id, &secrets("amber1"));
    let server = WfMcp::new(root.to_path_buf());
    let start = json_of(
        &server
            .run_grade_start(args(json!({ "run_id": run_id })))
            .await,
    );
    let session = start["session"].as_str().expect("a session").to_string();
    let read = || async {
        result_text(
            &server
                .run_grade_read(args(
                    json!({ "run_id": run_id, "session": session, "op": "read", "ref": "o:p1" }),
                ))
                .await,
        )
    };
    let first = read().await;
    assert!(first.contains("cedar3"), "{first}");
    set_secrets(root, &run_id, &secrets("cedar3"));
    let next = read().await;
    assert!(next.contains("o:p1"), "{next}");
    assert!(!next.contains("cedar3"), "{next}");
}

/// A secret value that is also a word of the grade's own vocabulary (a
/// level): every grade answer still returns the grade, its level intact.
#[tokio::test]
async fn a_secret_equal_to_a_level_leaves_the_grade_answers_whole() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    install(
        root,
        "says",
        SAYS,
        &[("say.sh", "echo changed the parser\n")],
    );
    let run_id = apb_engine::run(root, "says", None, apb_engine::RunOptions::default())
        .unwrap()
        .run_id;
    set_secrets(root, &run_id, &[("password", "partial")]);
    let server = WfMcp::new(root.to_path_buf());

    let graded = server
        .run_grade(args(json!({ "run_id": run_id, "grader": "rules" })))
        .await;
    assert_ne!(graded.is_error, Some(true), "{}", result_text(&graded));
    let graded = json_of(&graded);
    assert_eq!(graded["grade"]["criteria"][0]["level"], "good", "{graded}");

    let start = json_of(
        &server
            .run_grade_start(args(json!({ "run_id": run_id })))
            .await,
    );
    let session = start["session"].as_str().expect("a session").to_string();
    server
        .run_grade_read(args(
            json!({ "run_id": run_id, "session": session, "op": "read", "ref": "o:p1" }),
        ))
        .await;
    let submitted = server
        .run_grade_submit(args(json!({
            "run_id": run_id, "session": session,
            "grades": [{
                "criterion": "report-clear", "level": "partial",
                "reason_code": "misses_one", "why": "the report misses one change",
                "cites": [{ "ref": "o:p1" }],
            }],
        })))
        .await;
    assert_ne!(
        submitted.is_error,
        Some(true),
        "{}",
        result_text(&submitted)
    );
    let submitted = json_of(&submitted);
    assert_eq!(submitted["status"], "graded", "{submitted}");
    assert_eq!(submitted["grade"]["criteria"][0]["level"], "partial");

    let grades = json_of(&server.run_grades(args(json!({ "run_id": run_id }))).await);
    let host = &grades["grades"][grades["grades"].as_array().unwrap().len() - 1];
    assert_eq!(host["criteria"][0]["level"], "partial", "{grades}");
}
