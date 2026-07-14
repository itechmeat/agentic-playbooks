use std::fs;
use std::path::Path;
use std::time::Instant;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

// Two script branches of ~0.4s each, converging in join:all. If the branches run
// concurrently, the wall-clock time is noticeably less than the sum (0.8s).
const PLAYBOOK: &str = r#"
schema: 1
id: conc
name: Conc
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: b, type: script, script: "scripts/slow.sh", runner: sh }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j, join: all }
  - { from: b, to: j, join: all }
  - { from: j, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/conc/1.0.0");
    let scripts = dir.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/conc/current"), "1.0.0").unwrap();
    fs::write(scripts.join("slow.sh"), "sleep 0.4\n").unwrap();
}

#[test]
fn parallel_script_branches_run_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let started = Instant::now();
    let res = run(dir.path(), "conc", None, RunOptions::default()).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // Both branches ran and converged.
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    for n in ["a", "b", "j"] {
        assert!(
            events.iter().any(
                |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == n)
            ),
            "node {n} must finish"
        );
    }
    // Concurrent: 0.8s of work total, but wall-clock should be noticeably
    // less (threshold with margin - under 0.75s).
    assert!(
        elapsed.as_millis() < 750,
        "two 0.4s branches ran in {elapsed:?}; expected concurrent (< 750ms)"
    );
}
