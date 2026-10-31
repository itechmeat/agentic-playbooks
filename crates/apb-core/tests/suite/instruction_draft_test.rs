use apb_core::registry::Registry;
use std::fs;

fn seed(base: &std::path::Path, id: &str) {
    let vdir = base.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!("schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\nedges: []\n"),
    )
    .unwrap();
    fs::write(base.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

/// The legacy draft is read only: a file committed under `meta/` reads back,
/// a missing one is `None`, and an unsafe id is refused.
#[test]
fn the_legacy_draft_is_read_from_meta() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), "p");
    let reg = Registry::open_dir(tmp.path()).unwrap();
    assert_eq!(reg.read_instruction_draft("p").unwrap(), None);

    let meta = tmp.path().join("playbooks/p/meta");
    fs::create_dir_all(&meta).unwrap();
    fs::write(meta.join("instruction-draft.md"), "translate the plan").unwrap();
    assert_eq!(
        reg.read_instruction_draft("p").unwrap().as_deref(),
        Some("translate the plan")
    );
    assert!(reg.read_instruction_draft("../x").is_err());
}
