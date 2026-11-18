//! `apb diagram` and the doctor's diagrams line (people-in-the-loop D6,
//! "Surfaces"): the text a version's YAML renders, `--out`, `--rebuild
//! --all` with a version that does not read, the doctor's line (nothing,
//! OK, WARN), a hand-edited `playbook.mmd` never changes what prints, and
//! a rebuild overwrites it.

use std::fs;
use std::path::{Path, PathBuf};

use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::apb;

const YAML: &str = "schema: 2\nid: demo\nname: Demo\nversion: 1.0.0\nnodes:\n  - { id: start, type: start, title: Start }\n  - id: ask\n    type: human_review\n    title: Ask?\n    options: [approve, reject]\n  - { id: done, type: finish, outcome: success, title: Done }\nedges:\n  - { from: start, to: ask }\n  - { from: ask, to: done, condition: { type: review_status, equals: approve } }\n";

/// The 1.1.0 definition: another title, so a version's text tells which
/// YAML it came from.
fn yaml_11() -> String {
    YAML.replace("version: 1.0.0", "version: 1.1.0")
        .replace("title: Start", "title: Begin")
}

/// `demo` with versions 1.0.0 and 1.1.0 (`YAML` and [`yaml_11`]) under
/// the definition parent `parent`, `current` at 1.0.0, and no
/// `playbook.mmd` yet.
fn seed(parent: &Path) -> PathBuf {
    let dir = parent.join("playbooks/demo");
    for (v, yaml) in [("1.0.0", YAML.to_string()), ("1.1.0", yaml_11())] {
        fs::create_dir_all(dir.join(v)).unwrap();
        fs::write(dir.join(v).join("playbook.yaml"), yaml).unwrap();
    }
    fs::write(dir.join("current"), "1.0.0").unwrap();
    dir
}

fn project() -> (TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(t.path()).unwrap();
    let dir = seed(&t.path().join(".apb"));
    (t, dir)
}

/// The exact text `apb diagram` prints for `yaml`: the same call the
/// command makes, here on the seeded YAML.
fn expected(yaml: &str) -> String {
    let playbook = apb_core::schema::Playbook::from_yaml(yaml).unwrap();
    apb_core::diagram::mermaid(&playbook, &apb_core::scope::digest_str(yaml))
}

/// `apb doctor` on `project` with its own global store, so the diagrams
/// count is this project's alone (the shared test config dir may hold
/// playbooks other suites seeded).
fn doctor(cfg: &Path, project: &Path) -> (i32, String) {
    let out = apb()
        .env("APB_CONFIG_DIR", cfg)
        .args(["doctor"])
        .current_dir(project)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn diagram_prints_the_text() {
    let (t, _dir) = project();
    let out = apb()
        .args(["diagram", "demo"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "exit {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.starts_with("%% apb-diagram "), "{stdout:?}");
    assert!(stdout.contains("flowchart TD"), "{stdout:?}");
    assert_eq!(stdout, expected(YAML));

    // An explicit version selects that version's YAML.
    let v = apb()
        .args(["diagram", "demo", "--version", "1.1.0"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&v.stdout), expected(&yaml_11()));
    assert_ne!(expected(&yaml_11()), expected(YAML));
}

/// A version is one path segment: `--version` never reads a playbook.yaml
/// outside the playbook's directory.
#[test]
fn diagram_refuses_an_unsafe_version() {
    let (t, dir) = project();
    // A playbook.yaml the escape would reach.
    fs::create_dir_all(dir.join("../outside")).unwrap();
    fs::write(dir.join("../outside/playbook.yaml"), YAML).unwrap();
    apb()
        .args(["diagram", "demo", "--version", "../outside"])
        .current_dir(t.path())
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("is not a version name"));
}

#[test]
fn diagram_out_writes_a_file() {
    let (t, _dir) = project();
    let path = t.path().join("out.mmd");
    apb()
        .args(["diagram", "demo", "--out"])
        .arg(&path)
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "wrote {}",
            path.display()
        )));
    assert_eq!(fs::read_to_string(&path).unwrap(), expected(YAML));
}

#[test]
fn rebuild_all_writes_missing_diagrams() {
    // A private config dir: the global store holds nothing, so the count
    // is this project's alone and no other suite's store is written.
    let cfg = tempfile::tempdir().unwrap();
    let (t, dir) = project();
    // A version whose YAML does not parse is skipped, never rebuilt.
    let broken = dir.join("9.9.9");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("playbook.yaml"), ": not: [a: playbook").unwrap();

    // A playbook directory whose versions do not list is skipped and
    // named too.
    let locked = dir.join("../locked");
    fs::create_dir_all(&locked).unwrap();
    set_mode(&locked, 0o000);
    let out = apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "--all"])
        .current_dir(t.path())
        .output()
        .unwrap();
    set_mode(&locked, 0o700);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let n: usize = stdout
        .lines()
        .find_map(|l| {
            l.strip_prefix("rebuilt ")?
                .strip_suffix(" diagrams")?
                .parse()
                .ok()
        })
        .unwrap_or_else(|| panic!("no `rebuilt N diagrams` line: {stdout:?}"));
    assert_eq!(n, 2, "{stdout:?}");
    assert!(
        stdout.contains("skipped demo 9.9.9: "),
        "the unreadable version is skipped: {stdout:?}"
    );
    assert!(
        stdout.contains("skipped locked: its versions do not list"),
        "the unlisted playbook is skipped: {stdout:?}"
    );
    assert_eq!(out.status.code(), Some(1), "a skip exits 1: {stdout:?}");
    for (v, yaml) in [("1.0.0", YAML.to_string()), ("1.1.0", yaml_11())] {
        assert_eq!(
            fs::read_to_string(dir.join(v).join("playbook.mmd")).unwrap(),
            expected(&yaml)
        );
    }
    assert!(!broken.join("playbook.mmd").exists());
}

/// A playbook directory that is a link to a directory outside the store
/// is never followed: `--rebuild <id>` refuses it and writes nothing
/// there, `--rebuild --all` passes it by, a version directory that is a
/// link is passed by too, and printing does not read through the link.
#[test]
fn a_linked_playbook_directory_is_never_followed() {
    let cfg = tempfile::tempdir().unwrap();
    let (t, dir) = project();
    let outside = tempfile::tempdir().unwrap();
    let target = seed(outside.path());
    // `linked` is a link to `demo` of another tree.
    std::os::unix::fs::symlink(&target, dir.parent().unwrap().join("linked")).unwrap();
    // A version directory of `demo` that is a link out of the store.
    std::os::unix::fs::symlink(target.join("1.1.0"), dir.join("2.0.0")).unwrap();

    let out = apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "linked"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let out = apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "linked"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let out = apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "demo"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("rebuilt 2 diagrams"),
        "{out:?}"
    );
    let out = apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "--all"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("rebuilt 2 diagrams"),
        "{out:?}"
    );
    for v in ["1.0.0", "1.1.0"] {
        assert!(
            !target.join(v).join("playbook.mmd").exists(),
            "a diagram was written outside the store at {v}"
        );
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn doctor_names_drifted_diagrams() {
    let cfg = tempfile::tempdir().unwrap();
    let (t, dir) = project();
    // An empty project first: nothing found, so no diagrams line at all.
    let empty = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(empty.path()).unwrap();
    let (code, out) = doctor(cfg.path(), empty.path());
    assert_eq!(code, 0, "{out:?}");
    assert!(!out.contains("diagrams"), "{out:?}");

    // Seeded versions with no diagrams: the doctor names them missing.
    let (code, out) = doctor(cfg.path(), t.path());
    assert_eq!(code, 0, "{out:?}");
    assert!(
        out.contains("diagrams: 2 missing, 0 stale, 0 edited of 2; apb diagram --rebuild --all writes them again"),
        "{out:?}"
    );

    // A person's rebuild makes them current.
    apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "demo"])
        .current_dir(t.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("rebuilt 2 diagrams"));
    let (code, out) = doctor(cfg.path(), t.path());
    assert_eq!(code, 0, "{out:?}");
    assert!(out.contains("diagrams: 2 current"), "{out:?}");

    // A hand edit (the digest line stands, the body changed): named.
    let mmd = dir.join("1.0.0").join("playbook.mmd");
    let text = fs::read_to_string(&mmd).unwrap();
    fs::write(&mmd, text.replace("flowchart TD", "flowchart LR")).unwrap();
    let (code, out) = doctor(cfg.path(), t.path());
    assert_eq!(code, 0, "{out:?}");
    assert!(
        out.contains("diagrams: 0 missing, 0 stale, 1 edited of 2; apb diagram --rebuild --all writes them again"),
        "{out:?}"
    );
}

#[test]
fn an_edited_diagram_is_never_used() {
    let (t, dir) = project();
    let mmd = dir.join("1.0.0").join("playbook.mmd");
    apb()
        .args(["diagram", "demo", "--out"])
        .arg(&mmd)
        .current_dir(t.path())
        .assert()
        .success();
    // A person's edit of the file beside the YAML.
    let text = fs::read_to_string(&mmd).unwrap();
    fs::write(&mmd, text.replace("Start (start)", "HACKED (start)")).unwrap();

    let out = apb()
        .args(["diagram", "demo"])
        .current_dir(t.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "exit {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(stdout, expected(YAML), "the YAML's text, not the file's");
    assert!(!stdout.contains("HACKED"), "{stdout:?}");
}

/// `--rebuild` gives every version the diagram of exactly its YAML,
/// whatever sat at `playbook.mmd`: a hand edit is overwritten, and a link
/// at that name is replaced by a regular file without being followed. A
/// directory at that name is skipped and named, and stays.
#[test]
fn a_rebuild_overwrites_an_edit_and_replaces_what_is_not_a_file() {
    let cfg = tempfile::tempdir().unwrap();
    let (t, dir) = project();
    let edited = dir.join("1.0.0").join("playbook.mmd");
    fs::write(&edited, format!("{}%% a note\n", expected(YAML))).unwrap();
    let not_a_file = dir.join("1.1.0").join("playbook.mmd");
    fs::create_dir(&not_a_file).unwrap();
    apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "demo"])
        .current_dir(t.path())
        .assert()
        .code(1)
        .stdout(predicate::str::contains("rebuilt 1 diagram"))
        .stdout(predicate::str::contains(
            "skipped demo 1.1.0: demo/1.1.0/playbook.mmd",
        ))
        .stdout(predicate::str::contains("remove it and try again"));
    assert_eq!(fs::read_to_string(&edited).unwrap(), expected(YAML));
    assert!(not_a_file.is_dir(), "a directory is never removed");

    let outside = t.path().join("outside.mmd");
    fs::write(&outside, "kept\n").unwrap();
    fs::remove_dir(&not_a_file).unwrap();
    std::os::unix::fs::symlink(&outside, &not_a_file).unwrap();
    apb()
        .env("APB_CONFIG_DIR", cfg.path())
        .args(["diagram", "--rebuild", "demo"])
        .current_dir(t.path())
        .assert()
        .success();
    assert!(
        !fs::symlink_metadata(&not_a_file)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(&not_a_file).unwrap(),
        expected(&yaml_11())
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "kept\n");
}
