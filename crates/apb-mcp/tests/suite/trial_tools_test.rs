use std::path::Path;
use std::process::Command;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::trust::{Lifecycle, write_lifecycle};
use apb_mcp::policy::check_run;
use apb_mcp::tools::{playbook_approve, playbook_trial};

use crate::common::env_lock as lock;

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "git {args:?} failed");
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn seed(root: &Path, id: &str, body: &str, script: Option<(&str, &str)>) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), body.replace("__ID__", id)).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    if let Some((name, content)) = script {
        std::fs::create_dir_all(vdir.join("scripts")).unwrap();
        std::fs::write(vdir.join("scripts").join(name), content).unwrap();
    }
    write_lifecycle(&root.join(".apb/playbooks").join(id), Lifecycle::Draft).unwrap();
}

const WRITER: &str = "schema: 1\nid: __ID__\nname: __ID__\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: \"scripts/write.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";

const PLAIN: &str = "schema: 1\nid: __ID__\nname: __ID__\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: note, type: prompt, prompt: \"hi\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: note }\n  - { from: note, to: done }\n";

const IRREVERSIBLE: &str = "schema: 1\nid: __ID__\nname: __ID__\nversion: 1.0.0\neffects: [irreversible]\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

#[test]
fn trial_of_fs_writing_playbook_runs_in_worktree_and_reports_diff() {
    let _l = lock();
    if !git_available() {
        eprintln!("git not available, skipping");
        return;
    }
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    init_project(proj.path()).unwrap();
    seed(
        proj.path(),
        "writer",
        WRITER,
        Some(("write.sh", "#!/bin/sh\necho hi > out.txt\n")),
    );

    // Git repository with an initial commit (HEAD is needed for worktree add).
    git(proj.path(), &["init", "-q"]);
    git(proj.path(), &["add", "-A"]);
    git(
        proj.path(),
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );

    let res = playbook_trial(proj.path(), "writer", None, Default::default(), "project").unwrap();
    assert_eq!(res["status"], "succeeded", "trial result: {res}");
    assert!(
        res["diff"].as_str().unwrap().contains("out.txt"),
        "diff must mention out.txt: {}",
        res["diff"]
    );
    // The change was not applied to the project itself.
    assert!(
        !proj.path().join("out.txt").exists(),
        "workspace must be untouched"
    );
}

#[test]
fn approve_activates_and_unlocks_run() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    init_project(proj.path()).unwrap();
    seed(proj.path(), "p", PLAIN, None);

    // Before approve - draft, the gate refuses.
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "p".into(),
        version: None,
    };
    assert_eq!(
        check_run(proj.path(), &wref, false, false).unwrap_err()["policy"],
        "draft_requires_trial"
    );

    let res = playbook_approve(proj.path(), "p", None, "project").unwrap();
    assert_eq!(res["lifecycle"], "active");
    assert_eq!(res["trusted"], true);

    // After approve - the gate lets it through.
    assert!(check_run(proj.path(), &wref, false, false).is_ok());
}

#[test]
fn irreversible_effects_forbid_trial() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    init_project(proj.path()).unwrap();
    seed(proj.path(), "irr", IRREVERSIBLE, None);
    let res = playbook_trial(proj.path(), "irr", None, Default::default(), "project").unwrap();
    assert_eq!(res["rejected"], "trial_forbidden_irreversible");
}

#[test]
fn global_draft_can_be_trialed_and_approved() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    init_project(proj.path()).unwrap();
    // Global draft (in <config_dir>/playbooks), without a filesystem entry.
    let gdir = cfg.path().join("playbooks/g/1.0.0");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::write(gdir.join("playbook.yaml"), PLAIN.replace("__ID__", "g")).unwrap();
    std::fs::write(cfg.path().join("playbooks/g/current"), "1.0.0").unwrap();
    write_lifecycle(&cfg.path().join("playbooks/g"), Lifecycle::Draft).unwrap();

    // trial of a global draft (scope=global) runs in the current project.
    let res = playbook_trial(proj.path(), "g", None, Default::default(), "global").unwrap();
    assert_eq!(res["status"], "succeeded", "global trial result: {res}");

    // approve of a global draft.
    let ap = playbook_approve(proj.path(), "g", None, "global").unwrap();
    assert_eq!(ap["lifecycle"], "active");
    assert_eq!(ap["trusted"], true);
    assert_eq!(ap["scope"], "global");
}
