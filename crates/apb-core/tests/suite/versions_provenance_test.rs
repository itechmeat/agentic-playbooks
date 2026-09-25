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
}

#[test]
fn unknown_playbook_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    assert!(list_versions_with_provenance(dir.path(), "nope").is_err());
}

/// Issue #139 F12: `apb list`, MCP `playbook_list`, `/api/playbooks`
/// (`Registry::list`) and the version history (`list_versions_with_provenance`)
/// list the same versions in semver order, without the scratch directory an
/// interrupted save leaves behind.
#[test]
fn every_version_listing_is_semver_ordered_without_scratch_dirs() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let pb = dir.path().join(".apb/playbooks/demo");
    for name in ["1.10.0", "1.2.0", "1.9.0", ".tmp-1.11.0-1790000000"] {
        fs::create_dir_all(pb.join(name)).unwrap();
        fs::write(pb.join(name).join("playbook.yaml"), PLAYBOOK).unwrap();
    }
    let expected = ["1.0.0", "1.2.0", "1.9.0", "1.10.0"];

    let listed = apb_core::registry::Registry::open(dir.path())
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(listed[0].versions, expected);
    let history: Vec<String> = list_versions_with_provenance(dir.path(), "demo")
        .unwrap()
        .into_iter()
        .map(|i| i.version)
        .collect();
    assert_eq!(history, expected);
}

/// Issue #139 F12: `current` is the one authority for the version in use. A
/// version that stopped being current (a rollback, a save that did not move
/// `current`) must not still read `promoted` next to the current one.
#[test]
fn no_version_but_the_current_one_reads_promoted() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let newer = apb_core::versioning::create_version(
        dir.path(),
        "demo",
        &PLAYBOOK.replace("name: Demo", "name: Demo 2"),
        None,
        true,
    )
    .unwrap();
    apb_core::versioning::promote_version(dir.path(), "demo", "1.0.0").unwrap();

    let infos = list_versions_with_provenance(dir.path(), "demo").unwrap();
    assert!(
        !infos
            .iter()
            .find(|i| i.version == newer)
            .unwrap()
            .is_current
    );
    for info in infos {
        let json = serde_json::to_value(&info).unwrap();
        let promoted = json["provenance"]["promoted"].as_bool();
        assert!(
            promoted.is_none_or(|p| p == info.is_current),
            "{} reads promoted={promoted:?} but is_current={}",
            info.version,
            info.is_current
        );
    }
}
