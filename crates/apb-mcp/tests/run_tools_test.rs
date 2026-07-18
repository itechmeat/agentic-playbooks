use apb_mcp::tools::{playbook_run, run_events, run_status, runs_list};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
}

#[test]
fn run_then_inspect() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run = playbook_run(dir.path(), "noagent", None, params, None, None, None).unwrap();
    assert_eq!(run["outcome"], "succeeded");
    let run_id = run["run_id"].as_str().unwrap().to_string();

    let listed = runs_list(dir.path()).unwrap();
    assert_eq!(listed[0]["run_id"], run_id.as_str());

    let status = run_status(dir.path(), &run_id).unwrap();
    assert_eq!(status["run_status"], "succeeded");
    assert_eq!(status["nodes"]["note"], "succeeded");

    let ev = run_events(dir.path(), &run_id, None).unwrap();
    assert!(ev["events"].as_array().unwrap().len() >= 3);
    // pagination: from_seq cuts off earlier events
    let ev2 = run_events(dir.path(), &run_id, Some(2)).unwrap();
    let first_seq = ev2["events"][0]["seq"].as_u64().unwrap();
    assert!(first_seq >= 2);
}

#[test]
fn status_unknown_run_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    assert!(run_status(dir.path(), "ghost-1").is_err());
}

#[test]
fn resume_unknown_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = apb_mcp::tools::run_resume(dir.path(), "ghost-1", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn status_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    for bad in ["../../etc", "/etc", "..", "a/b"] {
        let err = run_status(dir.path(), bad).unwrap_err();
        assert!(
            matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
            "id {bad:?}: expected NotFound, got {err:?}"
        );
    }
}

#[test]
fn events_traversal_run_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let err = run_events(dir.path(), "../../etc", None).unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// Same linear playbook as `apb_engine::progress::tests::linear_pb` (weights
/// 100 and 300 by `expected_duration`), so the expected percent below is
/// independently anchored to that module's own `weights_by_expected_seconds`
/// test rather than re-derived here.
const PROGRESS_PB: &str = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }
  - { id: b, type: agent_task, prompt: hi, expected_duration: 300 }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: a }
  - { from: a, to: b }
  - { from: b, to: f }
"#;

/// A bare run directory - `run_status` only reads `runs/<id>/{events.jsonl,
/// playbook.yaml}` (via `resolve_run_dir`), no registry entry needed. Mirrors
/// the fixture style already used by `apb_mcp::tools::progress_tests::
/// run_progress_report_posts_a_command`.
fn bare_run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    let run_dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    run_dir
}

/// Populated path: a run dir with a playbook.yaml snapshot (nodes carrying
/// `expected_duration`) and an events.jsonl reporting node `a` succeeded.
/// `run_status`'s "progress" key must carry the computed summary, with the
/// full `{ percent, label, waiting_on }` shape present.
#[test]
fn run_status_progress_reflects_expected_duration_weighting() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-progress");
    fs::write(run_dir.join("playbook.yaml"), PROGRESS_PB).unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n",
    )
    .unwrap();

    let status = run_status(dir.path(), "r-progress").unwrap();
    let progress = &status["progress"];
    assert!(
        !progress.is_null(),
        "expected a progress object, got {status}"
    );
    // a (100s) done of a+b (400s) total = 25%.
    assert_eq!(progress["percent"], 25);
    assert!(
        progress.get("label").is_some(),
        "label field must be present in the progress shape"
    );
    assert!(
        progress.get("waiting_on").is_some(),
        "waiting_on field must be present in the progress shape"
    );
    assert!(
        progress.get("waiting_kind").is_some(),
        "waiting_kind field must be present in the progress shape"
    );
    // plan_key is the work-plan identity the web uses as its reset signal; it
    // is always a string (version plus cyclic-group totals), never null.
    assert!(
        progress.get("plan_key").and_then(|v| v.as_str()).is_some(),
        "plan_key must be present as a string in the progress shape"
    );
    // No RunProgress event and no human_review/wait node running - all three
    // default to null, but the KEYS must still be present (asserted above).
    assert!(progress["label"].is_null());
    assert!(progress["waiting_on"].is_null());
    assert!(progress["waiting_kind"].is_null());
}

/// Null path: a run dir with events.jsonl but no playbook.yaml snapshot (e.g.
/// a legacy run whose snapshot was never captured) must report
/// `"progress": null`, not omit the key or error.
#[test]
fn run_status_progress_is_null_without_playbook_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let run_dir = bare_run_dir(dir.path(), "r-no-snapshot");
    fs::write(
        run_dir.join("events.jsonl"),
        "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();

    let status = run_status(dir.path(), "r-no-snapshot").unwrap();
    assert!(
        status["progress"].is_null(),
        "expected progress: null without a playbook snapshot, got {status}"
    );
}

#[test]
fn run_status_carries_answer_key() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let started = playbook_run(dir.path(), "noagent", None, params, None, None, None).unwrap();
    let run_id = started["run_id"].as_str().unwrap();
    let status = run_status(dir.path(), run_id).unwrap();
    assert!(status.get("answer").is_some(), "answer key present");
    assert!(
        status["answer"].is_null(),
        "no-prompt finish -> null answer"
    );
}

#[test]
fn run_status_children_empty_for_childless_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let started = playbook_run(dir.path(), "noagent", None, params, None, None, None).unwrap();
    let status = run_status(dir.path(), started["run_id"].as_str().unwrap()).unwrap();
    assert_eq!(status["children"].as_array().unwrap().len(), 0);
}
