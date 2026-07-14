use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::overrides::RunOverrides;
use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

mod common;

const PLAYBOOK: &str = r#"
schema: 2
id: ov
name: Ov
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: main }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;

fn write_ok_agent(root: &Path) -> String {
    let path = root.join("ok-agent.sh");
    fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks/ov/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/ov/current"), "1.0.0").unwrap();
    common::seed_profile(root, "main", "claude-code", "haiku", &[]);
    common::seed_profile(root, "fast", "claude-code", "sonnet", &[]);
}

/// A profile with a distinctive SOUL and a skill - to check that the ephemeral
/// executor inherits the node profile's SOUL/skills.
fn seed_profile_with_soul_and_skill(root: &Path) {
    let dir = root.join(".apb/profiles/main");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\nskills:\n  - cs\n",
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), "SOUL-MARKER").unwrap();
    let sk = root.join(".agents/skills/cs");
    fs::create_dir_all(&sk).unwrap();
    fs::write(sk.join("SKILL.md"), "skillbody").unwrap();
}

#[test]
fn run_with_overrides_snapshots_effective_playbook() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let prog = write_ok_agent(dir.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        overrides: Some(RunOverrides::from_yaml("nodes:\n  w: { profile: fast }\n").unwrap()),
        ..RunOptions::default()
    };
    let res = run(dir.path(), "ov", None, opts);

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let res = res.unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    // The run snapshot reflects the effective playbook (profile from the override).
    let snapshot = fs::read_to_string(run_dir.join("playbook.yaml")).unwrap();
    assert!(
        snapshot.contains("fast"),
        "effective snapshot must carry the overridden profile: {snapshot}"
    );
    // run.yaml stores the applied overrides.
    let run_yaml = fs::read_to_string(run_dir.join("run.yaml")).unwrap();
    assert!(
        run_yaml.contains("overrides"),
        "run.yaml must record overrides: {run_yaml}"
    );
}

#[test]
fn ephemeral_executor_recorded_in_manifest() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let wdir = dir.path().join(".apb/playbooks/ov/1.0.0");
    fs::create_dir_all(&wdir).unwrap();
    fs::write(wdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(dir.path().join(".apb/playbooks/ov/current"), "1.0.0").unwrap();
    seed_profile_with_soul_and_skill(dir.path());
    let prog = write_ok_agent(dir.path());
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    // An ephemeral executor for node w: agent+model change, the profile's SOUL/skills
    // are preserved.
    let opts = RunOptions {
        overrides: Some(
            RunOverrides::from_yaml(
                "nodes:\n  w:\n    ephemeral_executor: { agent: codex, model: o1 }\n",
            )
            .unwrap(),
        ),
        ..RunOptions::default()
    };
    let res = run(dir.path(), "ov", None, opts);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    let res = res.unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let manifest = fs::read_to_string(run_dir.join("manifest.yaml")).unwrap();
    // A per-node entry, marked ephemeral, binding w -> ephemeral/w.
    assert!(
        manifest.contains("ephemeral: true"),
        "manifest missing ephemeral flag: {manifest}"
    );
    assert!(
        manifest.contains("ephemeral/w"),
        "manifest missing per-node ephemeral binding: {manifest}"
    );
    // A single ad-hoc agent+model invocation.
    assert!(
        manifest.contains("agent_id: codex") && manifest.contains("model: o1"),
        "manifest missing ephemeral invocation: {manifest}"
    );
    // The node profile's SOUL and skills are preserved.
    assert!(
        manifest.contains("SOUL-MARKER"),
        "ephemeral entry must preserve profile SOUL: {manifest}"
    );
    assert!(
        manifest.contains("cs"),
        "ephemeral entry must preserve profile skills: {manifest}"
    );
    // The skill snapshot lives under the ephemeral entry key.
    assert!(
        run_dir
            .join("profiles/ephemeral/w/skills/project/cs/SKILL.md")
            .is_file(),
        "skill snapshot must live under the ephemeral entry key"
    );
}
