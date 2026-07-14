use apb_core::registry::init_project;
use apb_core::versioning::{create_patch_version, list_versions_with_provenance};
use std::fs;

const PLAYBOOK: &str = r#"
schema: 1
id: demo
name: Demo
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/demo/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/demo/current"), "1.0.0").unwrap();
}

#[test]
fn lists_versions_with_current_flag_and_patch_provenance() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let patch = create_patch_version(
        dir.path(),
        "demo",
        "1.0.0",
        PLAYBOOK,
        "run-1",
        "improvement",
    )
    .unwrap();

    let infos = list_versions_with_provenance(dir.path(), "demo").unwrap();
    // Both versions are present.
    assert!(infos.iter().any(|i| i.version == "1.0.0"));
    let patched = infos
        .iter()
        .find(|i| i.version == patch)
        .expect("patch version listed");
    // current didn't move on the patch bump - 1.0.0 remains current.
    assert!(
        infos
            .iter()
            .find(|i| i.version == "1.0.0")
            .unwrap()
            .is_current
    );
    assert!(!patched.is_current);
    // The patch's provenance is populated.
    let prov = patched.provenance.as_ref().expect("patch has provenance");
    assert_eq!(prov.classification.as_deref(), Some("improvement"));
    assert_eq!(prov.run_id.as_deref(), Some("run-1"));
    assert!(!prov.promoted);
}

#[test]
fn unknown_playbook_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    assert!(list_versions_with_provenance(dir.path(), "nope").is_err());
}
