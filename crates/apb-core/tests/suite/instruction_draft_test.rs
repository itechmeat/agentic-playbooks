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

#[test]
fn draft_roundtrip_and_clear() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), "p");
    let reg = Registry::open_dir(tmp.path()).unwrap();

    assert_eq!(reg.read_instruction_draft("p").unwrap(), None);

    reg.write_instruction_draft("p", "translate the plan")
        .unwrap();
    assert_eq!(
        reg.read_instruction_draft("p").unwrap().as_deref(),
        Some("translate the plan")
    );
    // The draft lives outside any version dir.
    assert!(
        tmp.path()
            .join("playbooks/p/meta/instruction-draft.md")
            .is_file()
    );

    // Empty text clears the draft file.
    reg.write_instruction_draft("p", "").unwrap();
    assert_eq!(reg.read_instruction_draft("p").unwrap(), None);
    assert!(
        !tmp.path()
            .join("playbooks/p/meta/instruction-draft.md")
            .is_file()
    );

    // Unsafe id is rejected.
    assert!(reg.read_instruction_draft("../x").is_err());
    assert!(reg.write_instruction_draft("../x", "z").is_err());
}
