use std::fs;

use apb_core::registry::init_project;
use apb_core::store::ResolvedPlaybook;
use apb_engine::scheduler::{RunOptions, run_background_resolved};

const MINI: &str = "schema: 1\nid: mini\nname: Mini\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

// F2 (anti-TOCTOU): a resolved.digest that does not match the content on disk
// is rejected synchronously by the engine, before the run is even spawned.
#[test]
fn stale_resolved_digest_is_rejected() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    let vdir = proj.path().join(".apb/playbooks/mini/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), MINI).unwrap();
    fs::write(proj.path().join(".apb/playbooks/mini/current"), "1.0.0").unwrap();

    let resolved = ResolvedPlaybook {
        definition_parent: proj.path().join(".apb"),
        execution_root: proj.path().to_path_buf(),
        id: "mini".into(),
        version: "1.0.0".into(),
        digest: "sha256:deadbeef".into(), // not what is on disk
        origin_label: "project",
    };
    let err = run_background_resolved(&resolved, RunOptions::default()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("digest mismatch"),
        "expected digest mismatch, got: {msg}"
    );
}
