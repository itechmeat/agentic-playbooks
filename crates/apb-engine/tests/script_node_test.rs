use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use std::fs;

// Pipeline start -> lint(script) -> done(finish success). The script lives in
// the playbook's version directory (.apb/playbooks/<id>/1.0.0/scripts), not in the
// run's snapshot - we check that `run()` copies it into run_dir before execution.
const SCRIPTWF: &str = r#"
schema: 1
id: scriptwf
name: Script Playbook
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: lint, type: script, script: "scripts/lint.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: lint }
  - { from: lint, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/scriptwf/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), SCRIPTWF).unwrap();
    fs::write(vdir.join("scripts/lint.sh"), "echo lint-ok\n").unwrap();
    fs::write(root.join(".apb/playbooks/scriptwf/current"), "1.0.0").unwrap();
}

#[test]
fn script_node_finds_script_copied_from_version_dir() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let opts = RunOptions::default();
    let res = run(dir.path(), "scriptwf", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    // the script was copied into the run's snapshot
    assert!(run_dir.join("scripts/lint.sh").is_file());
    // the script's output landed in context.md
    let ctx_md = fs::read_to_string(run_dir.join("context.md")).unwrap();
    assert!(ctx_md.contains("lint-ok"), "context.md: {ctx_md}");
}
