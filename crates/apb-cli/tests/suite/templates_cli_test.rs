//! `apb templates list | show | install`: the example playbooks install as
//! drafts, the answer names what is still needed, and a global install
//! writes the global store the `--scope global` start reads.

use std::path::Path;

use predicates::prelude::*;

fn apb(cfg: Option<&Path>) -> assert_cmd::Command {
    let mut cmd = crate::common::apb();
    if let Some(cfg) = cfg {
        cmd.env("APB_CONFIG_DIR", cfg);
    }
    cmd
}

/// A project (`apb init`) with its own config dir.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    apb(Some(cfg.path()))
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    (dir, cfg)
}

#[test]
fn templates_list_names_every_template() {
    let (dir, cfg) = fixture();
    let out = apb(Some(cfg.path()))
        .args(["templates", "list"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 15, "15 templates: {stdout}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("dependency-security-fix\t")),
        "{stdout}"
    );
}

#[test]
fn templates_show_prints_the_yaml() {
    let (dir, cfg) = fixture();
    let out = apb(Some(cfg.path()))
        .args(["templates", "show", "inbox-triage"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(stdout.contains("id: inbox-triage"), "{stdout}");
}

#[test]
fn templates_install_prints_the_needs_and_the_next_commands() {
    let (dir, cfg) = fixture();
    let out = apb(Some(cfg.path()))
        .args(["templates", "install", "dependency-security-fix"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout
            .lines()
            .any(|l| l.trim_start().starts_with("needs:") && l.contains("github")),
        "{stdout}"
    );
    assert!(
        stdout.contains("next: apb adopt dependency-security-fix --scope project"),
        "{stdout}"
    );
    assert!(
        stdout.contains("next: apb run dependency-security-fix --scope project"),
        "{stdout}"
    );
    assert!(
        dir.path()
            .join(".apb/playbooks/dependency-security-fix/1.0.0/playbook.yaml")
            .is_file()
    );
}

#[test]
fn templates_install_global_writes_the_global_store() {
    let (dir, cfg) = fixture();
    apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "installed template inbox-triage as draft inbox-triage 1.0.0 (global)",
        ));
    assert!(
        cfg.path()
            .join("playbooks/inbox-triage/1.0.0/playbook.yaml")
            .is_file()
    );
    assert!(!dir.path().join(".apb/playbooks/inbox-triage").exists());
}

#[test]
fn a_duplicate_install_exits_1() {
    let (dir, cfg) = fixture();
    apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage"])
        .current_dir(dir.path())
        .assert()
        .success();
    apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "playbook `inbox-triage` already exists in the project scope; pass --as <other id>",
        ));
}

#[test]
fn an_unknown_template_exits_2() {
    let (dir, cfg) = fixture();
    apb(Some(cfg.path()))
        .args(["templates", "install", "no-such-template"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .code(2);
    apb(Some(cfg.path()))
        .args(["templates", "show", "no-such-template"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "template `no-such-template` not found",
        ));
}

#[test]
fn a_global_install_outside_a_project_has_no_fit_report() {
    let cfg = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage", "--scope", "global"])
        .current_dir(empty.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("no project was named"));
}

#[test]
fn a_project_install_outside_a_project_exits_2() {
    let cfg = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage"])
        .current_dir(empty.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no project here"));
}

#[test]
fn an_installed_global_template_meets_the_global_start() {
    let (dir, cfg) = fixture();
    apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .success();
    // Exit 2 naming draft_requires_trial, before any agent starts: the
    // start read the global draft.
    apb(Some(cfg.path()))
        .args(["run", "inbox-triage", "--scope", "global"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("draft_requires_trial"));
}

/// An id that is not one shell word is printed quoted in the next
/// commands, so a copied line addresses the installed draft.
#[test]
fn the_next_commands_quote_an_id_with_a_space() {
    let (dir, cfg) = fixture();
    let out = apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage", "--as", "my draft"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("next: apb adopt 'my draft' --scope project"),
        "{stdout}"
    );
    assert!(
        stdout.contains("next: apb run 'my draft' --scope project"),
        "{stdout}"
    );
}

/// The next commands keep the id exactly, repeated spaces included.
#[test]
fn the_next_commands_keep_repeated_spaces() {
    let (dir, cfg) = fixture();
    let out = apb(Some(cfg.path()))
        .args(["templates", "install", "inbox-triage", "--as", "my  draft"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("next: apb adopt 'my  draft' --scope project"),
        "{stdout}"
    );
    assert!(
        stdout.contains("next: apb run 'my  draft' --scope project"),
        "{stdout}"
    );
}
