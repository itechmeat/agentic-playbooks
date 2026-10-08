//! `supervisor_patch_playbook` with `scope: next_runs` (issue #192): the
//! forward patch tool. Every refusal of the acceptance list has a test here;
//! the trial lifecycle (the next run picks the candidate, promotion and
//! rejection) is covered in apb-engine's `candidate_trial_test`.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::forward_patch::ForwardPatchRequest;
use apb_engine::scheduler::{RunMode, RunOptions, prepare_supervised_background, run};
use apb_mcp::tools::{DetailMode, playbook_forward_patch, playbook_get};

const BASE: &str = r#"
schema: 2
id: demo
name: Demo
version: 1.0.0
goal:
  statement: "the greeting is written"
  criteria:
    - description: "the answer says hello"
      check: { type: marker, marker: "hello" }
effects: [fs_read]
supervisor:
  policy:
    capabilities: [observe, retry, patch_playbook]
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "one" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/demo/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), BASE).unwrap();
    fs::write(root.join(".apb/playbooks/demo/current"), "1.0.0").unwrap();
}

/// A live run (prepared, not driven): `p1` has not run yet.
fn live_run(root: &Path) -> String {
    let prepared = prepare_supervised_background(
        root,
        "demo",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..Default::default()
        },
    )
    .unwrap();
    prepared.run_id().to_string()
}

/// A finished run: `p1` executed.
fn finished_run(root: &Path) -> String {
    run(root, "demo", None, RunOptions::default())
        .unwrap()
        .run_id
}

fn req(yaml: &str, classification: &str) -> ForwardPatchRequest {
    ForwardPatchRequest {
        yaml: yaml.to_string(),
        classification: classification.to_string(),
        rationale: Some("p1 repeats work every run".into()),
        evidence: vec!["node p1".into()],
    }
}

fn improved() -> String {
    BASE.replace(
        "prompt: \"one\"",
        "prompt: \"one, reusing the cached scraper\"",
    )
}

fn refusal(root: &Path, run_id: &str, yaml: &str, classification: &str) -> String {
    playbook_forward_patch(root, run_id, &req(yaml, classification))
        .unwrap_err()
        .to_string()
}

fn no_candidate(root: &Path) {
    assert!(!root.join(".apb/playbooks/demo/candidate").exists());
    assert!(!root.join(".apb/playbooks/demo/1.0.1").exists());
}

#[test]
fn a_forward_patch_of_an_executed_node_becomes_the_candidate() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());

    let res =
        playbook_forward_patch(dir.path(), &run_id, &req(&improved(), "improvement")).unwrap();
    assert_eq!(res["version"], "1.0.1");
    assert_eq!(res["scope"], "next_runs");
    assert_eq!(res["base_version"], "1.0.0");
    assert_eq!(res["candidate"], true);
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/demo/candidate")).unwrap(),
        "1.0.1"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/demo/current")).unwrap(),
        "1.0.0"
    );
    // playbook_get names the candidate with its provenance.
    let got = playbook_get(dir.path(), "demo", None, DetailMode::Summary).unwrap();
    assert_eq!(got["candidate"]["version"], "1.0.1");
    assert_eq!(got["candidate"]["provenance"]["scope"], "next_runs");
    assert_eq!(got["candidate"]["provenance"]["run_id"], run_id.as_str());
}

#[test]
fn a_live_run_keeps_its_version_and_gets_no_migration() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = live_run(dir.path());

    playbook_forward_patch(dir.path(), &run_id, &req(&improved(), "improvement")).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    // Nothing posted to the run, nothing journaled: it stays on 1.0.0.
    assert!(!run_dir.join("control.jsonl").exists());
    let (_, version) = apb_engine::scheduler::run_playbook_ref(dir.path(), &run_id).unwrap();
    assert_eq!(version, "1.0.0");
    assert!(!read_all(&run_dir).unwrap().iter().any(|e| matches!(
        e.payload,
        EventPayload::PatchApplied { .. } | EventPayload::RunMigrated { .. }
    )));
}

#[test]
fn a_workaround_is_refused_for_next_runs() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    let err = refusal(dir.path(), &run_id, &improved(), "workaround");
    assert!(err.contains("workaround_refused"), "{err}");
    no_candidate(dir.path());
}

#[test]
fn an_invalid_patch_is_refused_by_the_validator() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    // An edge to a node that does not exist.
    let broken = improved().replace("to: done }", "to: nowhere }");
    let err = refusal(dir.path(), &run_id, &broken, "improvement");
    assert!(err.contains("nowhere"), "{err}");
    no_candidate(dir.path());
}

#[test]
fn a_goal_criteria_change_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    let weaker = improved().replace("marker: \"hello\"", "marker: \"h\"");
    let err = refusal(dir.path(), &run_id, &weaker, "improvement");
    assert!(err.contains("goal_changed"), "{err}");
    no_candidate(dir.path());
}

#[test]
fn effects_changes_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    // Declaring `secrets` on the playbook.
    let secrets = improved().replace("effects: [fs_read]", "effects: [fs_read, secrets]");
    let err = refusal(dir.path(), &run_id, &secrets, "improvement");
    assert!(err.contains("effects_changed"), "{err}");
    // A node declaring an irreversible step.
    let irreversible = improved().replace(
        "{ id: p1, type: prompt,",
        "{ id: p1, effects: [irreversible], type: prompt,",
    );
    let err = refusal(dir.path(), &run_id, &irreversible, "improvement");
    assert!(err.contains("effects_changed"), "{err}");
    // A new script node infers file writes and network access.
    let script = improved()
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: s, type: script, script: \"scripts/x.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }",
        )
        .replace("{ from: p1, to: done }", "{ from: p1, to: s }\n  - { from: s, to: done }");
    let err = refusal(dir.path(), &run_id, &script, "improvement");
    assert!(err.contains("effects_changed"), "{err}");
    no_candidate(dir.path());
}

#[test]
fn trust_relevant_fields_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    // The supervisor widening its own capabilities.
    let caps = improved().replace(
        "capabilities: [observe, retry, patch_playbook]",
        "capabilities: [observe, retry, rebind, patch_playbook]",
    );
    let err = refusal(dir.path(), &run_id, &caps, "improvement");
    assert!(err.contains("trust_fields_changed"), "{err}");
    // The supervisor changing the promotion policy.
    let promote = improved().replace(
        "    capabilities: [observe, retry, patch_playbook]",
        "    capabilities: [observe, retry, patch_playbook]\n    promote_supervisor_patches: always",
    );
    let err = refusal(dir.path(), &run_id, &promote, "improvement");
    assert!(err.contains("trust_fields_changed"), "{err}");
    // `requires`.
    let requires = improved().replace(
        "effects: [fs_read]",
        "effects: [fs_read]\nrequires: { commands: [git] }",
    );
    let err = refusal(dir.path(), &run_id, &requires, "improvement");
    assert!(err.contains("trust_fields_changed"), "{err}");
    no_candidate(dir.path());
}

#[test]
fn the_window_closes_some_time_after_the_run_ended() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = finished_run(dir.path());
    // Age the run's end past the window.
    let journal = dir
        .path()
        .join(".apb/runs")
        .join(&run_id)
        .join("events.jsonl");
    let aged: Vec<String> = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            if v["type"] == "run_finished" {
                v["ts"] = serde_json::json!(1_000);
            }
            v.to_string()
        })
        .collect();
    fs::write(&journal, aged.join("\n") + "\n").unwrap();

    let err = refusal(dir.path(), &run_id, &improved(), "improvement");
    assert!(err.contains("window_closed"), "{err}");
    no_candidate(dir.path());
}

#[test]
fn max_patches_per_run_counts_forward_patches() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = run(
        dir.path(),
        "demo",
        None,
        RunOptions {
            max_patches_per_run: Some(1),
            ..Default::default()
        },
    )
    .unwrap()
    .run_id;
    playbook_forward_patch(dir.path(), &run_id, &req(&improved(), "improvement")).unwrap();
    let again = improved().replace("cached scraper", "cached scraper and logo");
    let err = refusal(dir.path(), &run_id, &again, "improvement");
    assert!(err.contains("max_patches"), "{err}");
}
