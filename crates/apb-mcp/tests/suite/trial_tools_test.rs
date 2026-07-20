use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::trust::{Lifecycle, write_lifecycle};
use apb_mcp::policy::check_run;
use apb_mcp::profile_tools::{self, ExecutorInput};
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

/// Like `EnvGuard`, but also clears the agent-invocation overrides a
/// stub-agent test needs (`APB_AGENT_CMD`, `HOME`), mirroring
/// `profile_e2e_test::EnvGuard`.
struct AgentEnvGuard;
impl Drop for AgentEnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
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

    let res = playbook_trial(
        proj.path(),
        "writer",
        None,
        Default::default(),
        None,
        "project",
    )
    .unwrap();
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
    let res = playbook_trial(
        proj.path(),
        "irr",
        None,
        Default::default(),
        None,
        "project",
    )
    .unwrap();
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
    let res = playbook_trial(proj.path(), "g", None, Default::default(), None, "global").unwrap();
    assert_eq!(res["status"], "succeeded", "global trial result: {res}");

    // approve of a global draft.
    let ap = playbook_approve(proj.path(), "g", None, "global").unwrap();
    assert_eq!(ap["lifecycle"], "active");
    assert_eq!(ap["trusted"], true);
    assert_eq!(ap["scope"], "global");
}

/// A stub agent script: the claude adapter's argv template is
/// `-p {prompt} --model {model}`, so `$2` is the rendered prompt. The stub
/// overwrites a tracked file with it, so a run's rendered prompt shows up in
/// `git diff` the same way the WRITER fixture's file write does.
fn stub_agent_echoing_prompt_to_file(dir: &Path, out_file: &str) -> String {
    let path = dir.join("stub-agent.sh");
    std::fs::write(
        &path,
        format!("#!/bin/sh\nprintf '%s' \"$2\" > {out_file}\necho done\n"),
    )
    .unwrap();
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

const RUN_INSTRUCTION_PB: &str = "schema: 2\nid: __ID__\nname: __ID__\nversion: 1.0.0\ndefaults: { profile: stub }\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"{{run.instruction}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";

/// TDD step 1 (spec 2026-07-20-run-reliability, Task 10): `playbook_trial`
/// must accept a run instruction and thread it into `RunOptions.instruction`,
/// exactly like `playbook_run` already does. A node whose prompt is exactly
/// `{{run.instruction}}` renders to the trial's `instruction` argument, and
/// the stub agent's rendered prompt (which it writes to a tracked file) ends
/// up in the trial's `diff`.
#[test]
fn trial_instruction_reaches_a_node_prompt_of_run_instruction() {
    let _l = lock();
    if !git_available() {
        eprintln!("git not available, skipping");
        return;
    }
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        std::env::set_var(
            "APB_AGENT_CMD",
            stub_agent_echoing_prompt_to_file(bin.path(), "note.txt"),
        );
    }
    let _g = AgentEnvGuard;

    init_project(proj.path()).unwrap();
    seed(proj.path(), "instr", RUN_INSTRUCTION_PB, None);

    // A profile the agent_task node's `defaults.profile: stub` resolves to;
    // its agent id "claude" maps to the builtin `-p {prompt} --model {model}`
    // invocation, and APB_AGENT_CMD overrides the actual program run.
    profile_tools::profile_write(
        proj.path(),
        profile_tools::ProfileWrite {
            name: "stub".into(),
            scope: "project".into(),
            description: "stub".into(),
            soul_md: "You are a stub.".into(),
            skills: vec![],
            executor: ExecutorInput {
                agent: "claude".into(),
                model: "haiku".into(),
                fallbacks: vec![],
            },
            ..Default::default()
        },
    )
    .expect("profile_write ok");

    // Git repository with a tracked file the stub agent overwrites, so its
    // rendered-prompt write shows up as content in `git diff` (an untracked
    // file's content would not - see WRITER's filename-only assertion above).
    std::fs::write(proj.path().join("note.txt"), "before\n").unwrap();
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

    let res = playbook_trial(
        proj.path(),
        "instr",
        None,
        Default::default(),
        Some("PING".to_string()),
        "project",
    )
    .unwrap();
    assert_eq!(res["status"], "succeeded", "trial result: {res}");
    assert!(
        res["diff"].as_str().unwrap().contains("PING"),
        "the node's rendered prompt (run.instruction) must reach the agent \
         and show up in the trial diff: {}",
        res["diff"]
    );
}
