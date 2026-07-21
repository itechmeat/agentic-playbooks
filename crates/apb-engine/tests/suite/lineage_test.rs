use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::run_config::read_run_config;
use apb_engine::scheduler::{RunOptions, list_runs, run};
use apb_engine::state::RunStatus;
use std::fs;

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hello {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".apb/playbooks/noagent/current"), "1.0.0").unwrap();
}

#[test]
fn continued_from_links_predecessor_and_successor() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut base = RunOptions::default();
    base.params.insert("who".into(), "world".into());

    let first = run(dir.path(), "noagent", None, base).unwrap();
    assert_eq!(first.outcome, RunStatus::Succeeded);

    let mut successor_opts = RunOptions::default();
    successor_opts.params.insert("who".into(), "world".into());
    successor_opts.continued_from = Some(first.run_id.clone());
    let second = run(dir.path(), "noagent", None, successor_opts).unwrap();
    assert_eq!(second.outcome, RunStatus::Succeeded);

    let pred_dir = dir.path().join(".apb/runs").join(&first.run_id);
    let succ_dir = dir.path().join(".apb/runs").join(&second.run_id);
    let pred_cfg = read_run_config(&pred_dir).unwrap();
    let succ_cfg = read_run_config(&succ_dir).unwrap();
    assert_eq!(
        pred_cfg.superseded_by.as_deref(),
        Some(second.run_id.as_str())
    );
    assert_eq!(
        succ_cfg.continued_from.as_deref(),
        Some(first.run_id.as_str())
    );

    let pred_events = read_all(&pred_dir).unwrap();
    assert!(pred_events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::RunSupersededBy { by } if by == &second.run_id
        )
    }));
    let succ_events = read_all(&succ_dir).unwrap();
    assert!(succ_events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::RunContinuedFrom { from } if from == &first.run_id
        )
    }));

    let listed = list_runs(dir.path()).unwrap();
    let pred_summary = listed.iter().find(|r| r.run_id == first.run_id).unwrap();
    let succ_summary = listed.iter().find(|r| r.run_id == second.run_id).unwrap();
    assert_eq!(
        pred_summary.superseded_by.as_deref(),
        Some(second.run_id.as_str())
    );
    assert_eq!(
        succ_summary.continued_from.as_deref(),
        Some(first.run_id.as_str())
    );
}

#[test]
fn continued_from_refuses_already_superseded_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());

    let first = run(dir.path(), "noagent", None, {
        let mut o = RunOptions::default();
        o.params.insert("who".into(), "world".into());
        o
    })
    .unwrap();
    let mut second_opts = RunOptions::default();
    second_opts.params.insert("who".into(), "world".into());
    second_opts.continued_from = Some(first.run_id.clone());
    let second = run(dir.path(), "noagent", None, second_opts).unwrap();

    let mut third_opts = RunOptions::default();
    third_opts.params.insert("who".into(), "world".into());
    third_opts.continued_from = Some(first.run_id.clone());
    let err = run(dir.path(), "noagent", None, third_opts).unwrap_err();
    assert!(
        err.to_string().contains("already superseded"),
        "expected superseded refusal, got: {err}"
    );
    assert!(
        matches!(err, apb_engine::EngineError::Conflict(_)),
        "expected Conflict so HTTP/MCP surfaces can map 409, got: {err:?}"
    );
    let _ = second;
}

const OTHER: &str = r#"
schema: 1
id: other
name: Other
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hello {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed_other(root: &std::path::Path) {
    let vdir = root.join(".apb/playbooks/other/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), OTHER).unwrap();
    fs::write(root.join(".apb/playbooks/other/current"), "1.0.0").unwrap();
}

#[test]
fn continued_from_rejects_different_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    seed_other(dir.path());

    let first = run(dir.path(), "noagent", None, {
        let mut o = RunOptions::default();
        o.params.insert("who".into(), "world".into());
        o
    })
    .unwrap();

    let mut opts = RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    opts.continued_from = Some(first.run_id.clone());
    let err = run(dir.path(), "other", None, opts).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("noagent") && msg.contains("other"),
        "expected both playbook ids in error, got: {msg}"
    );
    assert!(
        msg.contains("continued_from") || msg.contains("belongs to playbook"),
        "expected clear cross-playbook lineage refusal, got: {msg}"
    );
}

#[test]
fn continued_from_accepts_same_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    seed_other(dir.path());

    let first = run(dir.path(), "other", None, {
        let mut o = RunOptions::default();
        o.params.insert("who".into(), "world".into());
        o
    })
    .unwrap();

    let mut opts = RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    opts.continued_from = Some(first.run_id.clone());
    let second = run(dir.path(), "other", None, opts).unwrap();
    assert_eq!(second.outcome, RunStatus::Succeeded);

    let pred_cfg = read_run_config(&dir.path().join(".apb/runs").join(&first.run_id)).unwrap();
    assert_eq!(
        pred_cfg.superseded_by.as_deref(),
        Some(second.run_id.as_str())
    );
}
