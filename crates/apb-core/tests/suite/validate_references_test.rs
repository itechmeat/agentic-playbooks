//! V91 and V92 (`apb_core::preflight::missing_references`): a profile no
//! scope resolves and a script a run would not find, reported before a run
//! instead of at its start.

use std::fs;
use std::path::Path;

use apb_core::preflight::missing_references;
use apb_core::profile_store::PlaybookOrigin;
use apb_core::schema::Playbook;
use apb_core::validate::{Issue, Severity};

use crate::common::config_sandbox;

/// The playbook from the bug report: `defaults.profile` bound to every agent
/// node, plus one script node.
fn playbook(profile: &str, script: &str) -> Playbook {
    Playbook::from_yaml(&format!(
        r#"
schema: 2
id: implement-task
name: Implement Task
version: 1.0.0
defaults:
  profile: {profile}
nodes:
  - {{ id: start, type: start }}
  - {{ id: plan, type: agent_task, prompt: "Write a plan" }}
  - {{ id: review, type: agent_task, prompt: "Review the plan" }}
  - {{ id: lint, type: script, script: {script}, runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: plan }}
  - {{ from: plan, to: review }}
  - {{ from: review, to: lint }}
  - {{ from: lint, to: done }}
"#
    ))
    .unwrap()
}

fn seed_profile(profiles: &Path, name: &str) {
    let dir = profiles.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        format!(
            "name: {name}\ndescription: d\nexecutor:\n  agent: claude\n  model: claude-opus-5-5\n"
        ),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "role").unwrap();
}

fn seed_script(version_dir: &Path, rel: &str) {
    let path = version_dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "true\n").unwrap();
}

fn found(root: &Path, version_dir: &Path, p: &Playbook) -> Vec<Issue> {
    missing_references(root, PlaybookOrigin::Project, version_dir, p)
}

#[test]
fn an_unresolvable_auto_profile_warns_and_a_missing_script_errors() {
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    fs::create_dir_all(&version_dir).unwrap();
    let issues = found(
        root.path(),
        &version_dir,
        &playbook("architect", "scripts/lint.sh"),
    );
    let summary: Vec<(&str, Option<&str>, Severity)> = issues
        .iter()
        .map(|i| (i.code, i.node.as_deref(), i.severity))
        .collect();
    // One V91 for the shared default, on the first node that uses it. A
    // warning: the global store was the last place looked, and another
    // machine (CI, a fresh clone) may have the profile there.
    assert_eq!(
        summary,
        vec![
            ("V91", Some("plan"), Severity::Warning),
            ("V92", Some("lint"), Severity::Error)
        ],
        "{issues:?}"
    );
    assert!(
        issues[0].message.contains("`architect`"),
        "{}",
        issues[0].message
    );
    assert!(
        issues[1].message.contains("scripts/lint.sh"),
        "{}",
        issues[1].message
    );
}

#[test]
fn a_missing_global_scope_profile_is_a_warning() {
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    seed_script(&version_dir, "scripts/lint.sh");
    let issues = found(
        root.path(),
        &version_dir,
        &playbook("{ name: architect, scope: global }", "scripts/lint.sh"),
    );
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(
        (issues[0].code, issues[0].severity),
        ("V91", Severity::Warning)
    );
    assert!(
        issues[0].message.contains("the global store"),
        "{}",
        issues[0].message
    );
}

#[test]
fn a_project_profile_that_cannot_be_loaded_is_an_error() {
    // Only the project can fix its own broken profile: an error, for an
    // `auto` reference too (the project is looked in first and wins).
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(".apb/profiles/architect")).unwrap();
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    seed_script(&version_dir, "scripts/lint.sh");
    for reference in ["architect", "{ name: architect, scope: project }"] {
        let issues = found(
            root.path(),
            &version_dir,
            &playbook(reference, "scripts/lint.sh"),
        );
        assert_eq!(issues.len(), 1, "{reference}: {issues:?}");
        assert_eq!(
            (issues[0].code, issues[0].severity),
            ("V91", Severity::Error),
            "{reference}"
        );
        assert!(
            issues[0].message.contains("cannot be loaded"),
            "{}",
            issues[0].message
        );
    }
}

#[test]
fn present_project_profile_and_script_give_nothing() {
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    seed_profile(&root.path().join(".apb/profiles"), "architect");
    seed_script(&version_dir, "scripts/lint.sh");
    let issues = found(
        root.path(),
        &version_dir,
        &playbook("architect", "scripts/lint.sh"),
    );
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn a_global_profile_resolves_an_auto_reference() {
    let _cfg = config_sandbox();
    let global = apb_core::profile_store::global_dir().unwrap();
    seed_profile(&global, "architect");
    let root = tempfile::tempdir().unwrap();
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    seed_script(&version_dir, "scripts/lint.sh");
    let issues = found(
        root.path(),
        &version_dir,
        &playbook("architect", "scripts/lint.sh"),
    );
    assert!(issues.is_empty(), "{issues:?}");
    // A `scope: project` reference does not fall back to the global store,
    // but its absence is V14's finding (the validator), not repeated here.
    let issues = found(
        root.path(),
        &version_dir,
        &playbook("{ name: architect, scope: project }", "scripts/lint.sh"),
    );
    assert!(issues.is_empty(), "{issues:?}");
}

#[cfg(unix)]
#[test]
fn a_script_symlink_counts_only_inside_scripts() {
    // A run copies `scripts/` keeping each symlink as is: a relative link to
    // a file inside `scripts/` still resolves there, one that leaves it (or
    // an absolute one) does not.
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    seed_profile(&root.path().join(".apb/profiles"), "architect");
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    seed_script(&version_dir, "scripts/real.sh");
    seed_script(&version_dir, "outside.sh");
    let scripts = version_dir.join("scripts");
    std::os::unix::fs::symlink("real.sh", scripts.join("inside.sh")).unwrap();
    std::os::unix::fs::symlink("../outside.sh", scripts.join("escape.sh")).unwrap();
    std::os::unix::fs::symlink(scripts.join("real.sh"), scripts.join("absolute.sh")).unwrap();
    let p = |s: &str| playbook("architect", s);
    assert!(
        found(root.path(), &version_dir, &p("scripts/inside.sh")).is_empty(),
        "a link inside scripts/ is found"
    );
    for link in ["scripts/escape.sh", "scripts/absolute.sh"] {
        let issues = found(root.path(), &version_dir, &p(link));
        assert_eq!(issues.len(), 1, "{link}: {issues:?}");
        assert_eq!(issues[0].code, "V92");
        assert!(
            issues[0].message.contains("symlink"),
            "{}",
            issues[0].message
        );
    }
}

#[test]
fn a_script_outside_scripts_dir_is_not_copied_into_a_run() {
    // A run copies only `scripts/` next to its snapshot, so a file at the
    // version root fails at runtime although it exists.
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    seed_profile(&root.path().join(".apb/profiles"), "architect");
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    seed_script(&version_dir, "lint.sh");
    let issues = found(root.path(), &version_dir, &playbook("architect", "lint.sh"));
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0].code, "V92");
    assert!(
        issues[0].message.contains("outside `scripts/`"),
        "{}",
        issues[0].message
    );
}

#[test]
fn an_escaping_script_path_is_left_to_v12() {
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    seed_profile(&root.path().join(".apb/profiles"), "architect");
    let version_dir = root.path().join(".apb/playbooks/implement-task/1.0.0");
    let issues = found(
        root.path(),
        &version_dir,
        &playbook("architect", "../lint.sh"),
    );
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn a_missing_success_check_script_is_v92() {
    let _cfg = config_sandbox();
    let root = tempfile::tempdir().unwrap();
    seed_profile(&root.path().join(".apb/profiles"), "architect");
    let version_dir = root.path().join(".apb/playbooks/p/1.0.0");
    let p = Playbook::from_yaml(
        r#"
schema: 2
id: p
name: P
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: architect, success_check: scripts/check.sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#,
    )
    .unwrap();
    let issues = found(root.path(), &version_dir, &p);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(
        (issues[0].code, issues[0].node.as_deref()),
        ("V92", Some("a"))
    );
}
