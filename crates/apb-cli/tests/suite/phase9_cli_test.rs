use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

const VALID: &str = include_str!("../../../apb-core/tests/fixtures/valid.yaml");

fn playbook() -> Command {
    crate::common::apb()
}

fn seeded_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    let vdir = dir.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), VALID).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/implement-task/current"),
        "1.0.0",
    )
    .unwrap();
    // valid.yaml references the architect profile - it must exist in the project.
    seed_architect(dir.path());
    // ... and so must the `lint` node's script (V92).
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("scripts/node-lint.sh"), "true\n").unwrap();
    dir
}

/// Seeds a complete architect profile (profile.yaml + SOUL.md) so it
/// actually RESOLVES, rather than merely existing as a directory.
fn seed_architect(root: &std::path::Path) {
    let pdir = root.join(".apb/profiles/architect");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: architect\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
}

#[test]
fn doctor_runs_and_reports_playbook() {
    let dir = seeded_dir();
    playbook()
        .arg("doctor")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("playbook implement-task"));
}

#[test]
fn export_then_import_round_trip_via_cli() {
    let src = seeded_dir();
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success();
    assert!(bundle_path.is_file(), "bundle file must be written");

    // Import into a clean project (the architect profile must be set up -
    // it's a project-level dependency of the playbook, and import validates
    // references).
    let dst = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dst.path())
        .assert()
        .success();
    seed_architect(dst.path());
    playbook()
        .arg("import")
        .arg(&bundle_path)
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("imported implement-task"));

    // The imported playbook shows up in the list.
    playbook()
        .arg("list")
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("implement-task"));
}

#[test]
fn export_to_stdout_emits_bundle_json() {
    let dir = seeded_dir();
    playbook()
        .args(["export", "implement-task"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"apb_bundle\""))
        .stdout(predicate::str::contains("\"id\": \"implement-task\""));
}

#[test]
fn import_missing_file_fails() {
    let dir = seeded_dir();
    playbook()
        .args(["import", "nope.json"])
        .current_dir(dir.path())
        .assert()
        .failure();
}

#[test]
fn dev_without_frontend_fails_clearly() {
    // Empty directory without web/: dev must fail clearly, without hanging.
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("dev")
        .arg("--no-open")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("frontend not found"));
}

/// `apb import` never approves what it imported, even a bundle without
/// scripts: the bundle came from elsewhere, so its digest stays out of the
/// trust store and the import says how to approve it.
#[test]
fn import_of_a_bundle_without_scripts_leaves_it_untrusted() {
    let src = seeded_dir();
    fs::remove_dir_all(
        src.path()
            .join(".apb/playbooks/implement-task/1.0.0/scripts"),
    )
    .unwrap();
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success();
    let dst = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success();
    seed_architect(dst.path());
    playbook()
        .arg("import")
        .arg(&bundle_path)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("is untrusted until approved"));
    let pdir = dst.path().join(".apb/playbooks/implement-task");
    let version = fs::read_to_string(pdir.join("current")).unwrap();
    let saved = fs::read_to_string(pdir.join(version.trim()).join("playbook.yaml")).unwrap();
    let trust = fs::read_to_string(cfg.path().join("trust.json")).unwrap_or_default();
    assert!(
        !trust.contains(&apb_core::scope::digest_str(&saved)),
        "an imported digest must not be approved, trust store: {trust}"
    );
}

/// A bundle that carries scripts imports as an untrusted version, and the
/// import says so with the way to approve it; the scripts arrive with it.
#[test]
fn import_of_a_bundle_with_scripts_reports_it_untrusted() {
    // The seeded version carries the `lint` node's script.
    let src = seeded_dir();
    let bundle_path = src.path().join("bundle.json");
    playbook()
        .args(["export", "implement-task", "--out"])
        .arg(&bundle_path)
        .current_dir(src.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("(1 script file(s))"));

    let dst = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success();
    seed_architect(dst.path());
    playbook()
        .arg("import")
        .arg(&bundle_path)
        .env("APB_CONFIG_DIR", cfg.path())
        .current_dir(dst.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("is untrusted until approved"))
        .stdout(predicate::str::contains("playbook_approve"));
    let pdir = dst.path().join(".apb/playbooks/implement-task");
    let version = fs::read_to_string(pdir.join("current")).unwrap();
    assert_eq!(
        fs::read_to_string(pdir.join(version.trim()).join("scripts/node-lint.sh")).unwrap(),
        "true\n"
    );
}
