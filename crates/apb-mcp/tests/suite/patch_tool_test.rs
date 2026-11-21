use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, prepare_supervised_background};
use apb_mcp::tools::{playbook_patch, supervisor_capabilities};
use std::fs;
use std::path::Path;

use crate::common::config_sandbox;

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
    let _cfg = config_sandbox();
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
    let _cfg = config_sandbox();
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

/// An in-run patch of a run of a global definition is refused before any
/// project definition is looked up: no patch version appears under the
/// project playbook of the same id.
#[test]
fn an_in_run_patch_of_a_global_run_is_refused() {
    // A config dir of its own holds the global store.
    let config = config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    apb_core::run_origin::ensure_key().unwrap();

    // The global store holds the same id; its run finishes in this project.
    let vdir = config.path().join("playbooks/demo/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(config.path().join("playbooks/demo/current"), "1.0.0").unwrap();
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Global,
        id: "demo".into(),
        version: Some("1.0.0".into()),
    };
    let resolved = apb_core::store::resolve(root, &wref).unwrap();
    let res = apb_engine::scheduler::run_resolved(
        &resolved,
        apb_engine::scheduler::RunOptions::default(),
    )
    .unwrap();
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(&e.payload, EventPayload::RunProvenance { origin, .. } if origin.as_deref() == Some("global"))));

    let err = playbook_patch(root, &res.run_id, PATCH, "improvement", "p1").unwrap_err();
    assert!(
        err.to_string().contains("patch_global_unsupported"),
        "{err}"
    );
    // The project playbook directory gained no patch version.
    assert_eq!(
        fs::read_dir(root.join(".apb/playbooks/demo"))
            .unwrap()
            .count(),
        2
    ); // 1.0.0 and current
}

/// An in-run patch of a run whose journal ends in a torn line is refused
/// `patch_origin_unreadable` before any project definition is read: the
/// run's scope is unknown.
#[test]
fn an_in_run_patch_of_an_unreadable_run_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let run_dir = root.join(".apb/runs/demo-torn");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("events.jsonl"),
        concat!(
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":2,"ts":2,"type":"run_provenance","origin":"global"}"#,
            "\n",
            r#"{"seq":3,"ts":3,"type":"no"#,
        ),
    )
    .unwrap();
    let err = playbook_patch(root, "demo-torn", PATCH, "improvement", "p1").unwrap_err();
    assert!(err.to_string().contains("patch_origin_unreadable"), "{err}");
    assert_eq!(
        fs::read_dir(root.join(".apb/playbooks/demo"))
            .unwrap()
            .count(),
        2
    ); // 1.0.0 and current
}

/// An in-run patch of a run directory without a journal is refused
/// `patch_origin_unreadable`: the run's scope is unknown, so no project
/// definition is read and the project playbook is unchanged.
#[test]
fn an_in_run_patch_of_a_run_without_a_journal_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    fs::create_dir_all(root.join(".apb/runs/demo-gone")).unwrap();
    let err = playbook_patch(root, "demo-gone", PATCH, "improvement", "p1").unwrap_err();
    assert!(err.to_string().contains("patch_origin_unreadable"), "{err}");
    assert_eq!(
        fs::read_dir(root.join(".apb/playbooks/demo"))
            .unwrap()
            .count(),
        2
    ); // 1.0.0 and current
}

/// A run id that is not one safe path segment is not found, before any
/// journal is read: a global journal outside the runs directory answers no
/// scope refusal.
#[test]
fn an_in_run_patch_checks_the_run_id_before_reading_a_journal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let outside = root.join(".apb/outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("events.jsonl"),
        concat!(
            r#"{"seq":1,"ts":1,"type":"run_started","playbook":"demo","version":"1.0.0"}"#,
            "\n",
            r#"{"seq":2,"ts":2,"type":"run_provenance","origin":"global"}"#,
            "\n",
        ),
    )
    .unwrap();
    let err = playbook_patch(root, "../outside", PATCH, "improvement", "p1").unwrap_err();
    assert!(
        matches!(err, apb_mcp::tools::ToolError::NotFound(_)),
        "{err:?}"
    );
}
