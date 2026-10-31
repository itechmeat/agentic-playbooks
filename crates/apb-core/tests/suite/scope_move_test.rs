//! Copying a playbook into another scope: the whole definition with the
//! same digest, without the legacy draft, never over an existing or claimed
//! id, never removing through a link, and nothing left behind when the copy
//! fails.

use std::fs;
use std::path::Path;

use apb_core::registry::Registry;
use apb_core::scope_move::{ScopeMoveError, copy_playbook_to};

const YAML: &str =
    "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\nedges: []\n";

fn seed(parent: &Path) -> std::path::PathBuf {
    let dir = parent.join("playbooks/p");
    fs::create_dir_all(dir.join("1.0.0/scripts")).unwrap();
    fs::write(dir.join("1.0.0/playbook.yaml"), YAML).unwrap();
    fs::write(dir.join("1.0.0/scripts/run.sh"), "echo\n").unwrap();
    fs::write(dir.join("current"), "1.0.0").unwrap();
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::write(dir.join("meta/instruction-draft.md"), "draft").unwrap();
    fs::write(dir.join("meta/1.0.0.yaml"), "created_by: user\n").unwrap();
    dir
}

fn digest(parent: &Path) -> String {
    Registry::open_dir(parent)
        .unwrap()
        .load("p", None)
        .unwrap()
        .trust_digest()
        .unwrap()
}

#[test]
fn the_copy_is_whole_keeps_its_digest_and_leaves_the_draft_out() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());

    let copied = copy_playbook_to(&dir, dst.path(), "p").unwrap();
    assert_eq!(copied, dst.path().join("playbooks/p"));
    assert_eq!(digest(dst.path()), digest(src.path()));
    assert!(copied.join("meta/1.0.0.yaml").is_file());
    assert!(!copied.join("meta/instruction-draft.md").exists());
    assert!(staging_is_empty(dst.path()));
    assert_eq!(
        Registry::open_dir(dst.path()).unwrap().playbook_ids(),
        vec!["p"]
    );
}

#[test]
fn an_existing_id_is_refused() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    seed(dst.path());
    assert!(matches!(
        copy_playbook_to(&dir, dst.path(), "p"),
        Err(ScopeMoveError::IdTaken(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_failed_copy_leaves_no_staging_behind() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    // A FIFO cannot be copied: the copy fails part way.
    let fifo = dir.join("1.0.0/scripts/pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert!(copy_playbook_to(&dir, dst.path(), "p").is_err());
    assert!(!dst.path().join("playbooks/p").exists());
    assert!(staging_is_empty(dst.path()));
}

#[cfg(unix)]
#[test]
fn a_linked_meta_directory_never_lets_the_copy_remove_outside_it() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    fs::write(outside.path().join("instruction-draft.md"), "keep me").unwrap();
    fs::remove_dir_all(dir.join("meta")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join("meta")).unwrap();

    assert!(copy_playbook_to(&dir, dst.path(), "p").is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("instruction-draft.md")).unwrap(),
        "keep me"
    );
    assert!(!dst.path().join("playbooks/p").exists());
}

#[test]
fn an_id_claimed_by_another_writer_is_taken() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    // A new draft claims its id with an empty directory first.
    fs::create_dir_all(dst.path().join("playbooks/p")).unwrap();
    assert!(matches!(
        copy_playbook_to(&dir, dst.path(), "p"),
        Err(ScopeMoveError::IdTaken(_))
    ));
}

/// Nothing of a copy is left in the staging area.
fn staging_is_empty(parent: &Path) -> bool {
    fs::read_dir(parent.join("playbooks-staging")).map_or(true, |mut d| d.next().is_none())
}
