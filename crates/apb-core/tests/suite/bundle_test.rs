use std::fs;
use std::path::Path;

use apb_core::bundle::{PlaybookBundle, export_bundle, import_bundle};
use apb_core::registry::{Registry, init_project};

const PLAYBOOK: &str = r#"schema: 1
id: va
name: Valid Agent
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

const LAYOUT: &str = "nodes:\n  start: { x: 0, y: 0 }\n  w: { x: 120, y: 0 }\n";

fn seed(root: &Path) {
    init_project(root).unwrap();
    let base = root.join(".apb/playbooks/va");
    fs::create_dir_all(base.join("1.0.0")).unwrap();
    fs::write(base.join("1.0.0/playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(base.join("current"), "1.0.0").unwrap();
    fs::create_dir_all(base.join("layouts")).unwrap();
    fs::write(base.join("layouts/1.0.0.yaml"), LAYOUT).unwrap();
}

#[test]
fn export_import_round_trip_preserves_playbook_and_layout() {
    // Export from project A.
    let a = tempfile::tempdir().unwrap();
    seed(a.path());
    let bundle = export_bundle(a.path(), "va", None).unwrap();
    assert_eq!(bundle.apb_bundle, 1);
    assert_eq!(bundle.id, "va");
    assert_eq!(bundle.version, "1.0.0");
    assert!(bundle.playbook.contains("agent_task"));
    assert!(bundle.layout.is_some(), "layout must be captured");

    // JSON round-trip.
    let json = bundle.to_json().unwrap();
    let back = PlaybookBundle::from_json(&json).unwrap();
    assert_eq!(back.playbook, bundle.playbook);
    assert_eq!(back.layout, bundle.layout);

    // Import into a clean project B: new project -> version 1.0.0.
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let assigned = import_bundle(b.path(), &back, true).unwrap();
    assert_eq!(assigned, "1.0.0");

    // The playbook and layout are in place in B.
    let reg = Registry::open(b.path()).unwrap();
    let loaded = reg.load("va", None).unwrap();
    assert_eq!(loaded.version, "1.0.0");
    assert!(loaded.playbook.nodes.iter().any(|n| n.id == "w"));
    assert!(loaded.layout.is_some(), "imported layout must be restored");
}

#[test]
fn import_rejects_unknown_schema() {
    let b = tempfile::tempdir().unwrap();
    init_project(b.path()).unwrap();
    let bad = PlaybookBundle {
        apb_bundle: 99,
        id: "va".to_string(),
        version: "1.0.0".to_string(),
        playbook: PLAYBOOK.to_string(),
        layout: None,
    };
    let err = import_bundle(b.path(), &bad, true).unwrap_err();
    assert!(
        format!("{err}").contains("unsupported bundle schema"),
        "got: {err}"
    );
}
