use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::fs;

// A pipeline without agent_task: start -> prompt -> finish. A real agent is not needed.
// Note: the `params:` block with the `who` description was added beyond the brief's
// literal text - V13 (validate.rs, a task before Task 9) requires that `{{params.X}}`
// reference a declared playbook parameter; without it `run()` rejects the playbook as invalid.
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
fn runs_linear_no_agent_playbook_to_success() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut opts = RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    let res = run(dir.path(), "noagent", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // events are recorded, the version snapshot is in place
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RunStarted { .. }))
    );
    assert!(events.iter().any(
        |e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "succeeded")
    ));
    assert!(run_dir.join("playbook.yaml").is_file());
    assert!(run_dir.join("context.md").is_file());
}
