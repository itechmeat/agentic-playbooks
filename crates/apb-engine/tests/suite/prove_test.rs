//! A check that must fail first (spec node-contracts D4) end to end: the
//! proof runs once before the first attempt of an effect scope, as an
//! observation; a green proof decides without the agent; a crash leaves an
//! open proof that a resume consumes without running the script again.

use std::fs;
use std::path::Path;
use std::process::Command;

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{recording_stub, run_with_stub, seed_playbook};

const OK: &str = "printf '\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n'";

/// The single run directory under `root`.
fn run_dir(root: &Path) -> std::path::PathBuf {
    fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_dir())
        .expect("one run")
}

/// A playbook whose `w` node proves first. `check` is the check's YAML map
/// after `script: scripts/prove.sh` (empty for the bare form), `extra` the
/// node's further keys.
fn playbook(check: &str, extra: &str) -> String {
    format!(
        r#"schema: 2
id: pv
name: PV
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: w, type: agent_task, prompt: "do", success_check: {{ script: scripts/prove.sh{check} }}{extra} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: w }}
  - {{ from: w, to: done }}
"#
    )
}

/// Seeds the project, a git work tree (the proof's tree state reads it) and
/// the proof script with the body `body`.
fn seed(root: &Path, body: &str, check: &str, extra: &str) {
    init_project(root).unwrap();
    fs::write(root.join("tracked.txt"), "seed\n").unwrap();
    fs::write(root.join(".gitignore"), ".apb/\ninv/\nstub.sh\n").unwrap();
    seed_playbook(root, "pv", &playbook(check, extra));
    let vdir = root.join(".apb/playbooks/pv/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("prove.sh"), format!("#!/bin/sh\n{body}\n")).unwrap();
    common::seed_main(root);
    let git = |args: &[&str]| {
        let mut full = vec!["-C", root.to_str().unwrap(), "-c", "commit.gpgsign=false"];
        full.extend_from_slice(args);
        Command::new("git")
            .args(&full)
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(git(&["init", "-q"]));
    assert!(git(&["config", "user.email", "t@t"]));
    assert!(git(&["config", "user.name", "t"]));
    assert!(git(&["add", "."]));
    assert!(git(&["commit", "-qm", "c1"]));
}

fn count(events: &[EventPayload], what: &str) -> usize {
    events
        .iter()
        .filter(|p| serde_json::to_value(p).unwrap()["type"] == *what)
        .count()
}

fn check_proved(events: &[EventPayload]) -> Vec<(bool, Option<String>, bool)> {
    events
        .iter()
        .filter_map(|p| match p {
            EventPayload::CheckProved {
                failed_first,
                on_green_before,
                changed_tree,
                ..
            } => Some((*failed_first, on_green_before.clone(), *changed_tree)),
            _ => None,
        })
        .collect()
}

fn payloads(root: &Path) -> Vec<EventPayload> {
    read_all(&run_dir(root))
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

/// C1.6.1: red first. The check fails while no marker exists, the agent
/// creates it, and the check passes after it: one red proof, one attempt,
/// the node succeeds.
#[test]
fn a_red_check_lets_the_agent_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "test -f marker",
        ", prove_fails_first: true",
        "",
    );
    let stub = recording_stub(dir.path(), &format!("touch marker; {OK}"));
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Succeeded
    );
    let all = payloads(dir.path());
    assert_eq!(check_proved(&all), vec![(true, None, false)]);
    assert_eq!(count(&all, "check_proved"), 1);
    assert_eq!(count(&all, "attempt_started"), 1);
}

/// C1.6.2: green first, the default. The node fails with
/// `check_green_before:`, no attempt ever starts, and the run outcome needs
/// a look for it.
#[test]
fn a_green_check_before_the_work_fails_the_node_by_default() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "exit 0", ", prove_fails_first: true", "");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Failed
    );
    let all = payloads(dir.path());
    assert_eq!(
        check_proved(&all),
        vec![(false, Some("fail".into()), false)]
    );
    assert_eq!(count(&all, "attempt_started"), 0);
    let out = all
        .iter()
        .find_map(|p| match p {
            EventPayload::NodeFinished {
                node,
                output,
                status,
                ..
            } if node == "w" && *status == "failed" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(out.starts_with("check_green_before:"), "{out}");
    let view = apb_engine::run_view::RunView::load(
        &run_dir(dir.path()),
        run_dir(dir.path()).file_name().unwrap().to_str().unwrap(),
    )
    .unwrap();
    let needs = apb_engine::run_outcome::OutcomeSummary::build(&view, &run_dir(dir.path()));
    assert!(
        needs
            .needs_look
            .as_ref()
            .map(|n| n.line())
            .unwrap_or_default()
            .contains("1 check green before work"),
        "{:?}",
        needs.needs_look.as_ref().map(|n| n.line())
    );
}

/// C1.6.3: green first with `on_green_before: skip`: the node settles
/// succeeded as already done, without the agent.
#[test]
fn a_green_check_with_skip_settles_the_node_as_done() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "exit 0",
        ", prove_fails_first: true, on_green_before: skip",
        "",
    );
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Succeeded
    );
    let all = payloads(dir.path());
    assert_eq!(
        check_proved(&all),
        vec![(false, Some("skip".into()), false)]
    );
    assert_eq!(count(&all, "attempt_started"), 0);
    let out = all
        .iter()
        .find_map(|p| match p {
            EventPayload::NodeFinished {
                node,
                output,
                status,
                ..
            } if node == "w" && *status == "succeeded" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(out.starts_with("already done:"), "{out}");
}

/// C1.6.4: a rejected attempt 1 and its retry: the proof runs once, on the
/// first attempt of the effect scope only.
#[test]
fn a_rejected_attempt_and_its_retry_prove_once() {
    let dir = tempfile::tempdir().unwrap();
    // The check always fails (no marker is ever made to satisfy it), so
    // every attempt is rejected; the proof runs on the first only.
    seed(
        dir.path(),
        "exit 1",
        ", prove_fails_first: true",
        ", max_retries: 5",
    );
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Failed
    );
    let all = payloads(dir.path());
    assert_eq!(count(&all, "check_proved"), 1, "the proof runs once");
    assert_eq!(count(&all, "attempt_started"), 6);
}

/// Rewrites the run's journal to keep only the events up to AND including
/// the first one matching `pred`: a crash that cut the run short there.
fn keep_journal_through<F: Fn(&EventPayload) -> bool>(root: &Path, pred: F) {
    let dir = run_dir(root);
    let events = read_all(&dir).unwrap();
    let cut = events
        .iter()
        .position(|e| pred(&e.payload))
        .expect("no matching event to truncate at");
    let mut buf = String::new();
    for e in &events[..=cut] {
        buf.push_str(&serde_json::to_string(e).unwrap());
        buf.push('\n');
    }
    fs::write(dir.join("events.jsonl"), buf).unwrap();
}

/// C1.6.5: a crash after a red proof, before the attempt started, resumes
/// without a second proof; the same holds for a green `fail` proof, whose
/// resume settles the node as the proof said, and for a proof that changed
/// the tree, whose resume fails the node for it again.
#[test]
fn the_proof_runs_once_across_a_crash() {
    // Red proof, then the crash: the resume runs the attempt (the marker
    // appears, the check passes) and journals no second proof.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "test -f marker",
        ", prove_fails_first: true",
        "",
    );
    let stub = recording_stub(dir.path(), &format!("touch marker; {OK}"));
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Succeeded
    );
    keep_journal_through(dir.path(), |p| {
        matches!(p, EventPayload::CheckProved { .. })
    });
    let again = resume(
        dir.path(),
        run_dir(dir.path()).file_name().unwrap().to_str().unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(again.outcome, RunStatus::Succeeded);
    let all = payloads(dir.path());
    assert_eq!(count(&all, "check_proved"), 1, "no second proof");
    assert_eq!(count(&all, "attempt_started"), 1);

    // Green `fail` proof, then the crash: the resume settles the node as
    // the proof said, with no attempt and no second proof.
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "exit 0", ", prove_fails_first: true", "");
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Failed
    );
    keep_journal_through(dir.path(), |p| {
        matches!(p, EventPayload::CheckProved { .. })
    });
    let again = resume(
        dir.path(),
        run_dir(dir.path()).file_name().unwrap().to_str().unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(again.outcome, RunStatus::Failed);
    let all = payloads(dir.path());
    assert_eq!(count(&all, "check_proved"), 1, "no second proof");
    assert_eq!(count(&all, "attempt_started"), 0);

    // A proof that changed the tree, then the crash: the resume fails the
    // node for the changed tree again, never as a green-before proof.
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "echo x >> tracked.txt\nexit 1",
        ", prove_fails_first: true",
        "",
    );
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Failed
    );
    keep_journal_through(dir.path(), |p| {
        matches!(p, EventPayload::CheckProved { .. })
    });
    let again = resume(
        dir.path(),
        run_dir(dir.path()).file_name().unwrap().to_str().unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(again.outcome, RunStatus::Failed);
    let all = payloads(dir.path());
    assert_eq!(count(&all, "check_proved"), 1, "no second proof");
    assert_eq!(count(&all, "attempt_started"), 0);
    let out = all
        .iter()
        .find_map(|p| match p {
            EventPayload::NodeFinished {
                node,
                output,
                status,
                ..
            } if node == "w" && *status == "failed" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(out.starts_with("check_changed_tree:"), "{out}");
}

/// C1.6.6: a check that modifies a tracked file and exits 1 fails the node
/// with `check_changed_tree:` and no attempt starts.
#[test]
fn a_check_that_edits_the_tree_fails_the_node() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        "echo x >> tracked.txt\nexit 1",
        ", prove_fails_first: true",
        "",
    );
    let stub = recording_stub(dir.path(), OK);
    assert_eq!(
        run_with_stub(dir.path(), "pv", &stub, None),
        RunStatus::Failed
    );
    let all = payloads(dir.path());
    assert_eq!(
        check_proved(&all),
        vec![(false, None, true)],
        "the proof is journaled with changed_tree: true"
    );
    assert_eq!(count(&all, "attempt_started"), 0);
    let out = all
        .iter()
        .find_map(|p| match p {
            EventPayload::NodeFinished {
                node,
                output,
                status,
                ..
            } if node == "w" && *status == "failed" => Some(output.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(out.starts_with("check_changed_tree:"), "{out}");
}

/// C1.6.7: a scenario run of a proving node journals no `check_proved` (the
/// scripted check decides, nothing proves).
#[test]
fn a_scenario_run_of_a_proving_node_proves_nothing() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;

    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "exit 1", ", prove_fails_first: true", "");
    let scenario_yaml = r#"schema: 1
id: pv
instruction: prove
answers:
  w:
    - { visit: 1, attempts: [ { status: succeeded, output: "ok" } ] }
"#;
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario_yaml).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:pv".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = run_scenario(dir.path(), "pv", None, &prep).unwrap();
    let all: Vec<EventPayload> = read_all(&quick.run_dir)
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect();
    assert_eq!(
        count(&all, "check_proved"),
        0,
        "a scenario run proves nothing"
    );
}

/// C1.6.8: a proving node never meets the node cache, as a batch member
/// either. Validation is bypassed the way a resume bypasses it (it does not
/// re-validate the run's own `playbook.yaml`): two runs stop at `gate`
/// before the fan-out ran, their definitions then give `p` both
/// `cache: auto` and a proof (V173 would refuse a fresh start), and the two
/// resumes run `p` beside the script `q` one run after the other. Without
/// the guard the first stores an entry and the second is served from it;
/// with it neither journals a `node_cache_hit`, and each proves once.
#[test]
fn a_proving_batch_member_is_never_served_from_the_cache() {
    let plain = r#"schema: 2
id: pv
name: PV
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: gate, type: script, script: scripts/gate.sh, runner: sh }
  - { id: fan, type: script, script: scripts/q.sh, runner: sh }
  - { id: p, type: agent_task, prompt: "do" }
  - { id: q, type: script, script: scripts/q.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
  - { id: stop, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: fan, condition: { type: node_status, node: gate, equals: success } }
  - { from: fan, to: p }
  - { from: fan, to: q }
  - { from: gate, to: stop, condition: { type: node_status, node: gate, equals: failure } }
  - { from: p, to: done }
  - { from: q, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    fs::write(dir.path().join("tracked.txt"), "seed\n").unwrap();
    fs::write(dir.path().join(".gitignore"), ".apb/\ninv/\nstub.sh\n").unwrap();
    seed_playbook(dir.path(), "pv", plain);
    let vdir = dir.path().join(".apb/playbooks/pv/1.0.0/scripts");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("gate.sh"), "test -f inv/go\n").unwrap();
    fs::write(vdir.join("q.sh"), "echo q\n").unwrap();
    // Red until the agent leaves its (git-ignored) marker.
    fs::write(vdir.join("prove.sh"), "test -f inv/marker\n").unwrap();
    common::seed_main(dir.path());
    let git = |args: &[&str]| {
        let mut full = vec![
            "-C",
            dir.path().to_str().unwrap(),
            "-c",
            "commit.gpgsign=false",
        ];
        full.extend_from_slice(args);
        assert!(Command::new("git").args(&full).status().unwrap().success());
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "c1"]);

    let stub = recording_stub(dir.path(), &format!("mkdir -p inv; touch inv/marker; {OK}"));
    let _env = common::env_lock();
    let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let _backoff = common::EnvVar::set(apb_engine::failure_class::BACKOFF_ENV, "1,1");
    let stopped: Vec<String> = (0..2)
        .map(|_| {
            let res = run(dir.path(), "pv", None, RunOptions::default()).unwrap();
            assert_eq!(res.outcome, RunStatus::Failed, "the gate stops the run");
            res.run_id
        })
        .collect();
    fs::create_dir_all(dir.path().join("inv")).unwrap();
    fs::write(dir.path().join("inv/go"), "").unwrap();
    let proving = plain.replace(
        "{ id: p, type: agent_task, prompt: \"do\" }",
        "{ id: p, type: agent_task, prompt: \"do\", cache: auto, success_check: { script: scripts/prove.sh, prove_fails_first: true } }",
    );
    for run_id in &stopped {
        let rd = dir.path().join(".apb/runs").join(run_id);
        fs::write(rd.join("playbook.yaml"), &proving).unwrap();
        fs::remove_file(dir.path().join("inv/marker")).ok();
        let again = resume(dir.path(), run_id, Some("gate")).unwrap();
        assert_eq!(again.outcome, RunStatus::Succeeded, "run {run_id}");
        let all: Vec<EventPayload> = read_all(&rd)
            .unwrap()
            .into_iter()
            .map(|e| e.payload)
            .collect();
        assert_eq!(
            count(&all, "node_cache_hit"),
            0,
            "run {run_id}: a proving node never hits"
        );
        assert_eq!(count(&all, "check_proved"), 1, "run {run_id} proved once");
    }
}
