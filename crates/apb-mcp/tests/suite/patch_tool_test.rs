use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, prepare_supervised_background};
use apb_mcp::tools::{playbook_patch, supervisor_capabilities};
use std::fs;
use std::path::Path;

const PLAYBOOK: &str = r#"
schema: 1
id: demo
name: Demo
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "one" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

const PATCH: &str = r#"
schema: 1
id: demo
name: Demo
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "one improved" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/demo/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/demo/current"), "1.0.0").unwrap();
}

fn prepared_run(root: &Path) -> String {
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
    // prepared is dropped: RunStarted is already recorded in prepare, the working
    // directory lock is released (we do not run drive in this test).
    prepared.run_id().to_string()
}

#[test]
fn playbook_patch_creates_version_and_posts_control() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = prepared_run(dir.path());

    let res = playbook_patch(dir.path(), &run_id, PATCH, "improvement", "p1").unwrap();
    let version = res["version"].as_str().unwrap().to_string();
    assert!(res["posted_seq"].is_number());

    // Patch version created (folder exists), current is NOT moved.
    assert!(
        dir.path()
            .join(".apb/playbooks/demo")
            .join(&version)
            .join("playbook.yaml")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/demo/current"))
            .unwrap()
            .trim(),
        "1.0.0"
    );

    // The patch command appears in control.jsonl (the Patch event does NOT exist yet - drive writes it).
    let control = fs::read_to_string(
        dir.path()
            .join(".apb/runs")
            .join(&run_id)
            .join("control.jsonl"),
    )
    .unwrap();
    let has_patch_cmd = control
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .any(|v| v.get("cmd").and_then(|c| c.as_str()) == Some("patch"));
    assert!(has_patch_cmd, "control.jsonl must contain a patch command");
    assert!(
        !read_all(&dir.path().join(".apb/runs").join(&run_id))
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::PatchApplied { .. }))
    );
}

#[test]
fn playbook_patch_rejects_bad_classification() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = prepared_run(dir.path());
    assert!(playbook_patch(dir.path(), &run_id, PATCH, "nonsense", "p1").is_err());
}

#[test]
fn default_capabilities_include_patch_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let caps = supervisor_capabilities(dir.path(), "demo", None).unwrap();
    assert!(caps.contains(&"observe".to_string()));
    assert!(caps.contains(&"retry".to_string()));
    assert!(caps.contains(&"patch_playbook".to_string()));
}
