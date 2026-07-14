use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use apb_core::content::{ContentError, TreeLimits, bundle_digest, snapshot_tree, tree_digest};

#[test]
fn snapshot_copies_and_digest_is_of_the_copy() {
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join("SKILL.md"), "v1").unwrap();
    let dst = tempfile::tempdir().unwrap();
    let d1 = snapshot_tree(src.path(), &dst.path().join("s"), &TreeLimits::default()).unwrap();
    // Source drift AFTER the snapshot does not change the copy's digest.
    fs::write(src.path().join("SKILL.md"), "v2").unwrap();
    let d2 = tree_digest(&dst.path().join("s"), &TreeLimits::default()).unwrap();
    assert_eq!(d1, d2);
}

#[test]
fn digest_covers_paths_and_bytes_with_domain_separation() {
    // {a: "xy"} and {ax: "y"} must produce different digests (length-prefixed).
    let t1 = tempfile::tempdir().unwrap();
    fs::write(t1.path().join("a"), "xy").unwrap();
    let t2 = tempfile::tempdir().unwrap();
    fs::write(t2.path().join("ax"), "y").unwrap();
    let l = TreeLimits::default();
    let s1 = tempfile::tempdir().unwrap();
    let s2 = tempfile::tempdir().unwrap();
    assert_ne!(
        snapshot_tree(t1.path(), &s1.path().join("s"), &l).unwrap(),
        snapshot_tree(t2.path(), &s2.path().join("s"), &l).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_rejected_and_inner_symlink_ok() {
    let outer = tempfile::tempdir().unwrap();
    fs::write(outer.path().join("secret"), "s").unwrap();
    let skill = outer.path().join("skill");
    fs::create_dir(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "x").unwrap();
    symlink(outer.path().join("secret"), skill.join("leak")).unwrap();
    let st = tempfile::tempdir().unwrap();
    let err = snapshot_tree(&skill, &st.path().join("s"), &TreeLimits::default()).unwrap_err();
    assert!(matches!(err, ContentError::Escape(_)));

    // An internal RELATIVE symlink (to a file inside the root) is allowed and
    // gets hashed as a link. Absolute targets are forbidden (see snapshot_tree).
    fs::remove_file(skill.join("leak")).unwrap();
    symlink("SKILL.md", skill.join("alias")).unwrap();
    let st2 = tempfile::tempdir().unwrap();
    snapshot_tree(&skill, &st2.path().join("s"), &TreeLimits::default()).unwrap();

    // An absolute target (even inside the root) is rejected.
    fs::remove_file(skill.join("alias")).unwrap();
    symlink(skill.join("SKILL.md"), skill.join("abs_alias")).unwrap();
    let st3 = tempfile::tempdir().unwrap();
    assert!(matches!(
        snapshot_tree(&skill, &st3.path().join("s"), &TreeLimits::default()).unwrap_err(),
        ContentError::Escape(_)
    ));
}

#[test]
fn limits_enforced() {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("big"), vec![0u8; 100]).unwrap();
    let l = TreeLimits {
        max_file_bytes: 10,
        ..Default::default()
    };
    let st = tempfile::tempdir().unwrap();
    assert!(matches!(
        snapshot_tree(t.path(), &st.path().join("s"), &l).unwrap_err(),
        ContentError::TooLarge(_)
    ));
}

#[test]
fn bundle_digest_is_order_independent() {
    let a = ("project/x".to_string(), "sha256:aa".to_string());
    let b = ("global/y".to_string(), "sha256:bb".to_string());
    assert_eq!(
        bundle_digest("sha256:pp", &[a.clone(), b.clone()]),
        bundle_digest("sha256:pp", &[b, a])
    );
}

#[test]
fn nested_dirs_snapshot_and_roundtrip() {
    let src = tempfile::tempdir().unwrap();
    fs::create_dir_all(src.path().join("sub/deep")).unwrap();
    fs::write(src.path().join("SKILL.md"), "root").unwrap();
    fs::write(src.path().join("sub/a.txt"), "a").unwrap();
    fs::write(src.path().join("sub/deep/b.txt"), "b").unwrap();
    let dst = tempfile::tempdir().unwrap();
    let d1 = snapshot_tree(src.path(), &dst.path().join("s"), &TreeLimits::default()).unwrap();
    let d2 = tree_digest(&dst.path().join("s"), &TreeLimits::default()).unwrap();
    assert_eq!(d1, d2);
    assert!(dst.path().join("s/sub/deep/b.txt").is_file());
}
