//! Execution identity inside a real drive (PR 21a): the step an attempt
//! entered and why, per list item and across a deferred retry, and the
//! identity fold over the journal those runs leave.

use std::fs;
use std::path::Path;

use apb_core::overrides::RunOverrides;
use apb_core::profile_store::PlaybookOrigin;
use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::execution_identity::digest::{NodeExecKey, expected_exec_key};
use apb_engine::execution_identity::fold::identities;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

fn seed(root: &Path, yaml: &str, fallbacks: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/ident/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/ident/current"), "1.0.0").unwrap();
    common::seed_profile(root, "main", "claude", "haiku", fallbacks);
}

fn agent(dir: &Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("agent.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

fn starts(events: &[Event], node: &str) -> Vec<(u64, apb_engine::execution_identity::ExecStep)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted { node: n, exec, .. } if n == node => {
                Some((e.seq, exec.clone().expect("exec")))
            }
            _ => None,
        })
        .collect()
}

const ONE: &str = "schema: 2\nid: ident\nname: I\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: \"do\"NODE }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";

/// Plan row A1: a deferred retry re-enters its own step with no `entered`,
/// on the primary and on a fallback; only the fallback's first attempt
/// names the close that moved the walk.
#[test]
fn deferred_retry_reenters_without_entered() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        &ONE.replace("NODE", ", max_retries: 1, retry_delay: { initial: 1s }"),
        &[("claude", "opus")],
    );
    let d = root.display();
    let prog = agent(
        root,
        &format!(
            "for a in \"$@\"; do\n  if [ \"$a\" = opus ]; then\n    if [ -f '{d}/opus.marker' ]; then echo ok; exit 0; fi\n    touch '{d}/opus.marker'; echo 'the agent made a mistake' 1>&2; exit 1\n  fi\ndone\necho 'the agent made a mistake' 1>&2\nexit 1"
        ),
    );
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let res = run(root, "ident", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    let s = starts(&events, "w");
    let steps: Vec<u32> = s.iter().map(|(_, x)| x.step).collect();
    assert_eq!(steps, [0, 0, 1, 1], "two attempts per step");
    let entered: Vec<bool> = s.iter().map(|(_, x)| x.entered.is_some()).collect();
    assert_eq!(entered, [false, false, true, false]);
    let entry = s[2].1.entered.clone().unwrap();
    assert_eq!(entry.from_step, 0);
    assert_eq!(entry.closed_attempt, s[1].0);
    assert_eq!(entry.reason.as_str(), "failed");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::RetryDeferred { .. }))
            .count(),
        2,
        "both retries were deferred"
    );
}

/// Card A6 and design 2.7: an ephemeral override whose agent and model
/// equal the base primary is its own executor, while the manifest's
/// profile and bundle digests stay what a run of the base records; and the
/// pre-start key of each start equals its started run's.
#[test]
fn an_override_equal_to_the_base_is_its_own_executor() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, &ONE.replace("NODE", ""), &[]);
    let prog = agent(root, "echo ok");
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path().to_str().unwrap());
    let overrides = RunOverrides::from_yaml(
        "nodes:\n  w:\n    ephemeral_executor: { agent: claude, model: haiku }\n",
    )
    .unwrap();
    let playbook = apb_core::schema::Playbook::from_yaml(&ONE.replace("NODE", "")).unwrap();
    let key = |o: &RunOverrides| -> NodeExecKey {
        expected_exec_key(root, PlaybookOrigin::Project, &playbook, o)
            .unwrap()
            .remove("w")
            .expect("a key for w")
    };
    let started = |opts: RunOptions| {
        let res = run(root, "ident", None, opts).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded);
        let run_dir = root.join(".apb/runs").join(&res.run_id);
        apb_engine::manifest::read(&run_dir)
            .unwrap()
            .unwrap()
            .node_digests()
            .remove("w")
            .unwrap()
    };
    let base = started(RunOptions::default());
    let over = started(RunOptions {
        overrides: Some(overrides.clone()),
        ..RunOptions::default()
    });
    assert_eq!(
        over.profile, base.profile,
        "the manifest's profile digest is unchanged"
    );
    assert_eq!(
        over.bundle, base.bundle,
        "the manifest's bundle digest is unchanged"
    );
    assert_ne!(
        over.executor, base.executor,
        "the override is its own executor"
    );
    assert!(base.executor.starts_with("x1:"));
    let pre_base = key(&RunOverrides::default());
    let pre_over = key(&overrides);
    assert_eq!(
        (pre_base.profile, pre_base.bundle, pre_base.executor),
        (base.profile, base.bundle, base.executor),
        "pre-start and started keys agree"
    );
    assert_eq!(
        (pre_over.profile, pre_over.bundle, pre_over.executor),
        (over.profile, over.bundle, over.executor)
    );
}

/// Design 2.6 item 4: the identity of an attempt carries the digests of the
/// binding it ran on; a journal without `exec` and without a rebind reads
/// them from the manifest.
#[test]
fn identities_carry_the_digests_of_the_binding_each_attempt_ran_on() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, &ONE.replace("NODE", ""), &[("claude", "opus")]);
    let prog = agent(
        root,
        "for a in \"$@\"; do\n  if [ \"$a\" = opus ]; then echo ok; exit 0; fi\ndone\nexit 1",
    );
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let res = run(root, "ident", None, RunOptions::default()).unwrap();
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let digests = apb_engine::manifest::read(&run_dir)
        .unwrap()
        .unwrap()
        .node_digests()
        .remove("w")
        .unwrap();
    let ids: Vec<_> = identities(&run_dir, &events)
        .into_iter()
        .filter(|i| i.node == "w")
        .collect();
    assert_eq!(ids.len(), 2);
    for id in &ids {
        assert_eq!(
            id.executor_digest.as_deref(),
            Some(digests.executor.as_str())
        );
        assert_eq!(id.profile_digest.as_deref(), Some(digests.profile.as_str()));
        assert!(id.model_requested_by_apb.is_some());
    }
    assert_eq!(ids[1].step.as_ref().map(|s| s.step), Some(1));
    // The same journal as an older apb wrote it: no `exec`, no rebind.
    let old: Vec<Event> = events
        .into_iter()
        .map(|mut e| {
            if let EventPayload::AttemptStarted { exec, .. } = &mut e.payload {
                *exec = None;
            }
            e
        })
        .collect();
    let ids = identities(&run_dir, &old);
    assert!(
        ids.iter()
            .filter(|i| i.node == "w")
            .all(|i| i.step.is_none()
                && i.executor_digest.as_deref() == Some(digests.executor.as_str())),
        "without a rebind the manifest entry is the binding"
    );
}

/// Plan row A6: the items of a list node walk the chain each on their own;
/// every item's fallback names the close of that item's own primary.
#[test]
fn each_list_item_enters_its_fallback_on_its_own_close() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = "schema: 2\nid: ident\nname: I\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - id: w\n    type: agent_task\n    prompt: \"Handle one item.\"\n    list:\n      items: { inline: [ { id: a }, { id: b } ] }\n      batch: { size: 2, parallel: 1, retries: 0 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";
    seed(root, yaml, &[("claude", "opus")]);
    let prog = agent(
        root,
        "for a in \"$@\"; do\n  if [ \"$a\" = opus ]; then echo ok; exit 0; fi\ndone\necho 'the agent made a mistake' 1>&2\nexit 1",
    );
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let res = run(root, "ident", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    for item in ["a", "b"] {
        let s: Vec<(u64, apb_engine::execution_identity::ExecStep)> = events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::AttemptStarted {
                    node,
                    item: Some(i),
                    exec,
                    ..
                } if node == "w" && i == item => Some((e.seq, exec.clone().expect("exec"))),
                _ => None,
            })
            .collect();
        assert_eq!(s.len(), 2, "item {item}: the primary and the fallback");
        assert_eq!(s[0].1.entered, None);
        let entry = s[1].1.entered.clone().expect("the fallback's entry");
        assert_eq!(
            (entry.from_step, entry.closed_attempt, entry.reason.as_str()),
            (0, s[0].0, "failed"),
            "item {item}"
        );
    }
}
