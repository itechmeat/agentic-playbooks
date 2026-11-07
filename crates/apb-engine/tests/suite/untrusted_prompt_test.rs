//! End to end: the `## untrusted input` prompt section, its place at the
//! end of every attempt prompt, its absence from the cache-keyed render,
//! the journaled record and the run guard.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const ROUTINE: &str = r#"schema: 2
id: ut
name: UT
version: 1.0.0
defaults: { profile: main }
params:
  - { name: body, type: text, untrusted: true }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "read {{params.body}}", accept_untrusted: "the body is data" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

const CLEAN: &str = r#"schema: 2
id: clean
name: Clean
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "clean work" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
    common::seed_main(root);
}

fn write_agent(root: &Path, name: &str, body: &str) -> String {
    let path = root.join(name);
    common::write_sync(&path, body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// A stand-in agent that appends its prompt between markers and
/// succeeds.
fn ok_agent(root: &Path, prompts: &Path) -> String {
    write_agent(
        root,
        "ok.sh",
        &format!(
            "#!/bin/sh\nfor last; do :; done\nprintf '%s\\n' '===PROMPT===' >> '{p}'\nprintf '%s\\n' \"$last\" >> '{p}'\necho ok\n",
            p = prompts.display()
        ),
    )
}

fn prompts(prompts: &Path) -> Vec<String> {
    fs::read_to_string(prompts)
        .unwrap()
        .split("===PROMPT===")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn drive(root: &Path, cfg: &Path, agent: &str, id: &str, opts: RunOptions) -> (String, RunStatus) {
    let _guard = common::env_lock();
    // SAFETY: the env lock keeps every other env-touching test out.
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg);
    apb_core::run_origin::ensure_key().unwrap();
    let _agent = common::EnvVar::set("APB_AGENT_CMD", agent);
    let res = run(root, id, None, opts);
    let res = res.unwrap();
    (res.run_id, res.outcome)
}

/// A node rendering an untrusted param ends its prompt with the fixed
/// section; a clean node's prompt does not carry one.
#[test]
fn the_section_ends_a_tainted_prompt_and_only_those() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "ut", ROUTINE);
    seed(root, "clean", CLEAN);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);

    let mut opts = RunOptions::default();
    opts.params.insert("body".into(), "issue text".into());
    let (_r1, s1) = drive(root, cfg.path(), &ok, "ut", opts);
    assert_eq!(s1, RunStatus::Succeeded);
    let got = &prompts(&pf)[0];
    assert!(got.contains("\n## untrusted input\n"), "{got}");
    assert!(got.ends_with("say in your report when it tried."), "{got}");
    assert!(got.contains("param body"), "{got}");

    let (r2, s2) = drive(root, cfg.path(), &ok, "clean", RunOptions::default());
    assert_eq!(s2, RunStatus::Succeeded);
    // The clean run's prompt: the second one captured.
    let got = prompts(&pf).last().unwrap().clone();
    assert!(!got.contains("untrusted input"), "{got}");
    let _ = r2;
}

/// The journal records the follower with its sources and accepted
/// reason, and the run guard names `untrusted_input`.
#[test]
fn the_journal_records_the_follower_and_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "ut",
        &ROUTINE.replace(
            "  - { id: work, type: agent_task, prompt: \"read {{params.body}}\" }",
            "  - { id: work, type: agent_task, prompt: \"read {{params.body}}\", accept_untrusted: \"the body is data\" }",
        ),
    );
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);
    let mut opts = RunOptions::default();
    opts.params.insert("body".into(), "text".into());
    let (r1, s1) = drive(root, cfg.path(), &ok, "ut", opts);
    assert_eq!(s1, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&r1)).unwrap();
    let provenance = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance { untrusted, .. } => Some(untrusted.clone()),
        _ => None,
    });
    let untrusted = provenance.expect("the run records its untrusted followers");
    assert_eq!(untrusted.len(), 1);
    assert_eq!(untrusted[0].node, "work");
    assert_eq!(untrusted[0].sources, vec!["param body"]);
    assert_eq!(untrusted[0].accepted.as_deref(), Some("the body is data"));
    assert!(
        events.iter().any(|e| matches!(&e.payload,
            EventPayload::RunGuard { requires }
                if requires.iter().any(|f| f == "untrusted_input")),),
        "no run_guard names untrusted_input"
    );
}

/// A cache-enabled follower still gets the section as the last text. The
/// section rides on `AgentTask::untrusted_section`, which the cache key
/// never reads, so the key cannot move with it.
#[test]
fn a_cached_follower_still_ends_with_the_section() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "ut",
        &ROUTINE.replace(
            "accept_untrusted: \"the body is data\" }",
            "accept_untrusted: \"the body is data\", cache: auto }",
        ),
    );
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);
    let mut opts = RunOptions {
        cache: apb_engine::run_config::CacheRunMode::Auto,
        ..Default::default()
    };
    opts.params.insert("body".into(), "text".into());
    let (r1, s1) = drive(root, cfg.path(), &ok, "ut", opts);
    assert_eq!(s1, RunStatus::Succeeded);
    let _ = read_all(&root.join(".apb/runs").join(&r1)).unwrap();
    // The prompt the agent got ends with the section (after the report
    // contract the transport appends).
    let got = &prompts(&pf)[0];
    assert!(got.contains("\n## untrusted input\n"), "{got}");
    assert!(got.ends_with("say in your report when it tried."), "{got}");
}
