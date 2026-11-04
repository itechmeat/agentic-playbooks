//! Scenario runs through the real drive: one temp project per test, the
//! scenario run in this process (the process mode and its guards are the
//! `apb __scenario-run` entry's, tested there).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::scenario::ScenarioLevel;

use super::{ScenarioPrep, ScenarioRun};
use crate::event::{Event, EventPayload, read_all};
use crate::state::RunStatus;

/// The fix loop: an agent works, a script gates, a judge triages (a red
/// gate loops back once), a person approves.
const LOOP: &str = r#"schema: 2
id: loop
name: loop
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "Do {{run.instruction}}", max_retries: 1 }
  - { id: gate, type: script, script: scripts/gate.sh, runner: sh }
  - id: triage
    type: judge
    state: { out: "{{nodes.gate.output}}" }
    questions:
      verdict: { type: choice, instructions: "Is `out` green?", criteria: { green: a, red: b } }
    on_unavailable: fail
  - { id: approve, type: human_review }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: gate }
  - { from: gate, to: triage }
  - { id: again, from: triage, to: work, condition: { type: output_field, node: triage, field: verdict, equals: red }, max_traversals: 2 }
  - { from: triage, to: approve, fallback: true }
  - { from: approve, to: done, condition: { type: review_status, equals: approve } }
  - { from: approve, to: failed, fallback: true }
"#;

/// The scenario where the gate is red once and the loop repairs it.
const GREEN: &str = r#"schema: 1
id: green
instruction: Fix the parser
answers:
  work:
    - visit: 1
      attempts:
        - { status: failed, failure_kind: infrastructure, output: "spawn failed" }
        - { status: succeeded, output: "first try" }
    - { visit: 2, attempts: [ { status: succeeded, output: "fixed" } ] }
  gate:
    - { visit: 1, script: { exit: 0, stdout: "QA red" } }
    - { visit: 2, script: { exit: 0, stdout: "QA green" } }
  triage:
    - { visit: 1, judge: { verdict: { value: red, p: 0.9 } } }
    - { visit: 2, judge: { verdict: { value: green, p: 0.9 } } }
  approve:
    - { visit: 1, review: { decision: approve } }
"#;

/// A project with playbook `yaml` installed as version 1.0.0, the script
/// `scripts/gate.sh` beside it (it would fail loudly if it ever ran).
fn project(yaml: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let id = yaml_serde::from_str::<serde_json::Value>(yaml).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = dir.path().join(format!(".apb/playbooks/{id}/1.0.0"));
    std::fs::create_dir_all(v.join("scripts")).unwrap();
    std::fs::write(v.join("playbook.yaml"), yaml).unwrap();
    std::fs::write(
        v.join("scripts/gate.sh"),
        "echo the gate ran for real >&2\nexit 99\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(format!(".apb/playbooks/{id}/current")),
        "1.0.0",
    )
    .unwrap();
    dir
}

/// Writes scenario `yaml` into the scenarios of playbook `id` in the
/// project at `root`, named by its own `id`, and returns that name.
fn write_scenario(root: &Path, id: &str, yaml: &str) -> String {
    let name = yaml_serde::from_str::<serde_json::Value>(yaml).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let dir = apb_core::scenario::scenarios_dir(&root.join(format!(".apb/playbooks/{id}")));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    name
}

/// Scenario `name` of playbook `id` as `apb eval` reads it: through the
/// scenario loader, and refused on any error of the scenario rules against
/// the playbook's current version, so no test drives a scenario the
/// validator would turn away.
fn load_prep(root: &Path, id: &str, name: &str) -> ScenarioPrep {
    let dir = root.join(format!(".apb/playbooks/{id}"));
    let loaded = apb_core::scenario::load(&dir, name).unwrap();
    let version = std::fs::read_to_string(dir.join("current")).unwrap();
    let yaml = std::fs::read_to_string(dir.join(version.trim()).join("playbook.yaml")).unwrap();
    let playbook = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
    let errors: Vec<String> =
        apb_core::scenario::validate_scenario(name, &loaded.scenario, &playbook)
            .into_iter()
            .filter(|i| i.severity == apb_core::validate::Severity::Error)
            .map(|i| format!("{} {}", i.code, i.message))
            .collect();
    assert!(errors.is_empty(), "{name}: {errors:#?}");
    ScenarioPrep {
        scenario: loaded.scenario,
        level: ScenarioLevel::Quick,
        scenario_digest: loaded.digest,
        max_requests: 20,
        sent: Default::default(),
    }
}

/// Scenario `yaml` of playbook `id`, written and read back through the
/// loader ([`load_prep`]).
fn prep(root: &Path, id: &str, yaml: &str) -> ScenarioPrep {
    let name = write_scenario(root, id, yaml);
    load_prep(root, id, &name)
}

fn run(root: &Path, id: &str, scenario: &str) -> (ScenarioRun, Vec<Event>) {
    run_prep(root, id, &prep(root, id, scenario))
}

fn run_prep(root: &Path, id: &str, p: &ScenarioPrep) -> (ScenarioRun, Vec<Event>) {
    let r = crate::scheduler::run_scenario(root, id, None, p).unwrap();
    let events = read_all(&r.run_dir).unwrap();
    (r, events)
}

fn attempts_of<'a>(events: &'a [Event], node: &str) -> Vec<&'a EventPayload> {
    events
        .iter()
        .map(|e| &e.payload)
        .filter(|p| matches!(p, EventPayload::AttemptStarted { node: n, .. } if n == node))
        .collect()
}

fn finished<'a>(events: &'a [Event], node: &str) -> Vec<(&'a str, &'a str)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node: n,
                status,
                output,
                ..
            } if n == node => Some((status.as_str(), output.as_str())),
            _ => None,
        })
        .collect()
}

fn count(r: &ScenarioRun, kind: super::BoundaryKind) -> u32 {
    r.boundaries
        .iter()
        .find(|b| b.kind == kind)
        .map_or(0, |b| b.count)
}

/// Everything below the project's `.apb` except the scenario runs.
fn outside_scenario_runs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.ends_with(".apb/scenario-runs") {
                continue;
            }
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p.strip_prefix(root).unwrap().to_path_buf());
        }
    }
    out.sort();
    out
}

/// The fix loop runs on the real drive with every outside answer scripted:
/// the infrastructure retry spends no node retry and waits for nothing, the
/// red gate loops once through the judge's answer, the review takes the
/// scripted decision, and the run lands under `.apb/scenario-runs/` with a
/// scenario origin. Nothing else in the project changes, no script runs,
/// every answer is used once.
#[test]
fn the_fix_loop_runs_on_the_drive_with_scripted_answers() {
    let dir = project(LOOP);
    let root = dir.path();
    let p = prep(root, "loop", GREEN);
    let before = outside_scenario_runs(root);
    let started = std::time::Instant::now();
    let (r, events) = run_prep(root, "loop", &p);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "the infrastructure backoff was slept"
    );
    assert!(r.run_dir.starts_with(root.join(".apb/scenario-runs")));
    assert_eq!(outside_scenario_runs(root), before, "the project changed");
    let manifest = crate::manifest::read(&r.run_dir).unwrap().unwrap();
    assert!(matches!(
        manifest.origin,
        Some(crate::manifest::RunOrigin::Scenario { .. })
    ));
    // Scripted attempts: the scripted agent, no model, executed by the
    // scenario; the infrastructure retry is attempt 2 of visit 1.
    let work = attempts_of(&events, "work");
    assert_eq!(work.len(), 3);
    for a in &work {
        let EventPayload::AttemptStarted {
            agent, model, exec, ..
        } = a
        else {
            unreachable!()
        };
        assert_eq!(agent, "scenario");
        assert_eq!(*model, None);
        assert_eq!(exec.as_ref().unwrap().executed_by, "scenario");
    }
    assert_eq!(
        finished(&events, "work"),
        [("succeeded", "first try"), ("succeeded", "fixed")]
    );
    assert_eq!(
        finished(&events, "gate"),
        [("succeeded", "QA red"), ("succeeded", "QA green")]
    );
    // The judge asked the scenario, routed on red, then on green.
    let judged: Vec<(Option<String>, bool)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                provider,
                calibrated,
                use_site,
                ..
            } if use_site == "judge_node" => Some((provider.clone(), *calibrated)),
            _ => None,
        })
        .collect();
    assert_eq!(judged, vec![(Some("scenario".to_string()), false); 2]);
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::ReviewDecided { node, decision, .. } if node == "approve" && decision == "approve")));
    assert_eq!(finished(&events, "done").len(), 1);
    // Every answer used, none missing, none extra.
    assert!(r.answers.extra.is_empty(), "{:?}", r.answers.extra);
    assert!(r.answers.missing.is_empty(), "{:?}", r.answers.missing);
    assert_eq!(r.answers.used.len(), 8, "{:?}", r.answers.used);
    // The boundaries it reached.
    use super::BoundaryKind as K;
    assert_eq!(count(&r, K::AgentAttempt), 3);
    assert_eq!(count(&r, K::Script), 2);
    assert_eq!(count(&r, K::JudgeNative), 2);
    assert_eq!(count(&r, K::Review), 1);
    assert_eq!(count(&r, K::InfraBackoff), 1);
}

fn run_error(events: &[Event]) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunError { reason, .. } => Some(reason.clone()),
        _ => None,
    })
}

/// `on_missing: error`: the first boundary without an answer stops the run
/// at once, naming the boundary and its key; nothing after it runs.
#[test]
fn a_missing_answer_stops_the_run_under_on_missing_error() {
    let dir = project(LOOP);
    let scenario = GREEN.replace(
        "  approve:\n    - { visit: 1, review: { decision: approve } }\n",
        "",
    );
    let (r, events) = run(dir.path(), "loop", &scenario);
    assert_eq!(r.outcome, RunStatus::Failed);
    let halted = r.halted.expect("the run halted on the missing review");
    assert_eq!(
        (
            halted.boundary.as_str(),
            halted.node.as_deref(),
            halted.visit
        ),
        ("review", Some("approve"), 1)
    );
    assert!(
        run_error(&events).is_some_and(|e| e.contains(super::MISSING_ANSWER)),
        "{:?}",
        run_error(&events)
    );
    assert!(finished(&events, "done").is_empty() && finished(&events, "failed").is_empty());
}

/// `on_missing: fail`: an attempt without an answer fails the way an agent
/// fails (spending the node's retries), the failure route runs, and the
/// report lists what was missing.
#[test]
fn a_missing_attempt_fails_like_an_agent_under_on_missing_fail() {
    let yaml = LOOP.replace(
        "  - { from: work, to: gate }\n",
        "  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }\n  - { from: work, to: gate, fallback: true }\n",
    );
    let dir = project(&yaml);
    let scenario = "schema: 1\nid: s\non_missing: fail\nanswers: {}\n";
    let (r, events) = run(dir.path(), "loop", scenario);
    assert_eq!(r.outcome, RunStatus::Failed, "{events:#?}");
    assert_eq!(
        attempts_of(&events, "work").len(),
        2,
        "max_retries: 1 spent"
    );
    let (status, output) = finished(&events, "work")[0];
    assert_eq!(status, "failed");
    assert!(output.contains("scenario has no answer"), "{output}");
    assert_eq!(finished(&events, "failed").len(), 1);
    assert_eq!(r.answers.missing.len(), 2);
    assert!(r.halted.is_none());
}

/// An answer for an attempt that never ran is extra: here attempt 2 of a
/// work visit whose attempt 1 succeeded.
#[test]
fn an_answer_for_an_attempt_that_never_ran_is_extra() {
    let dir = project(LOOP);
    let scenario = GREEN.replace(
        "    - { visit: 2, attempts: [ { status: succeeded, output: \"fixed\" } ] }",
        "    - { visit: 2, attempts: [ { status: succeeded, output: \"fixed\" }, { status: succeeded } ] }",
    );
    let (r, _) = run(dir.path(), "loop", &scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded);
    let extra: Vec<String> = r
        .answers
        .extra
        .iter()
        .map(|a| {
            format!(
                "{}:{:?}:{}",
                a.node.as_deref().unwrap_or("-"),
                a.visit,
                a.part
            )
        })
        .collect();
    assert_eq!(extra, ["work:Some(Nth(2)):attempt 2"]);
}

/// A read-only node: no tree state is taken and no `read_only_checked` is
/// journaled; a scripted change fails the node at once, journaled
/// `read_only_violated`, with no retry.
#[test]
fn a_scripted_change_fails_a_read_only_node_without_a_retry() {
    let yaml = LOOP.replace(
        "{ id: work, type: agent_task, prompt: \"Do {{run.instruction}}\", max_retries: 1 }",
        "{ id: work, type: agent_task, prompt: \"Do {{run.instruction}}\", max_retries: 3, read_only: true }",
    );
    let dir = project(&yaml);
    let scenario = "schema: 1\nid: s\non_extra: warn\nanswers:\n  work:\n    - visit: 1\n      attempts:\n        - { status: succeeded, output: x, read_only: { dirty: [src/a.rs] } }\n        - { status: succeeded }\n";
    let (r, events) = run(dir.path(), "loop", scenario);
    assert_eq!(r.outcome, RunStatus::Failed);
    assert_eq!(attempts_of(&events, "work").len(), 1);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReadOnlyChecked { .. }))
    );
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::ReadOnlyViolated { node, changes, .. } if node == "work" && changes[0].path == "src/a.rs")));
    assert!(
        finished(&events, "work")[0]
            .1
            .starts_with("read_only_dirty")
    );
    assert_eq!(r.answers.extra.len(), 1, "attempt 2 never ran");
}

/// A quota failure pauses the run as in a real one, but a scenario run
/// cannot wait: the pause is due at once, journaled paused and resumed by
/// the engine, and the node runs again.
#[test]
fn a_quota_pause_is_due_at_once_and_the_node_runs_again() {
    let dir = project(LOOP);
    let scenario = GREEN.replace(
        "        - { status: failed, failure_kind: infrastructure, output: \"spawn failed\" }\n",
        "        - { status: failed, failure_kind: quota }\n        - { status: failed, failure_kind: quota }\n        - { status: failed, failure_kind: quota }\n",
    );
    let (r, events) = run(dir.path(), "loop", &scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    let paused = events
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
        .expect("the quota pause is journaled");
    assert!(matches!(
        events[paused + 1].payload,
        EventPayload::RunResumed { .. }
    ));
    // Due at once by the journal's own clock: the engine resumed before the
    // pause's `until`, which a run that waited it out could not.
    let EventPayload::RunPaused {
        until: Some(until), ..
    } = events[paused].payload
    else {
        panic!("a quota pause names when it ends: {:?}", events[paused]);
    };
    assert!(
        events[paused + 1].ts < u128::from(until),
        "resumed at {} for a pause until {until}",
        events[paused + 1].ts
    );
    assert_eq!(finished(&events, "work")[0], ("succeeded", "first try"));
}

/// An unattended scenario run fails at a review with no unattended
/// outcome before the scenario's decision is read; that answer is extra.
#[test]
fn an_unattended_run_fails_at_a_review_before_the_scripted_decision() {
    let dir = project(LOOP);
    let scenario = GREEN.replace(
        "instruction: Fix the parser\n",
        "instruction: Fix the parser\nunattended: true\n",
    );
    let (r, events) = run(dir.path(), "loop", &scenario);
    assert_eq!(r.outcome, RunStatus::Failed);
    assert!(
        run_error(&events).is_some_and(|e| e.starts_with("hitl_required")),
        "{:?}",
        run_error(&events)
    );
    assert!(
        crate::manifest::read(&r.run_dir)
            .unwrap()
            .unwrap()
            .unattended
    );
    let extra: Vec<&str> = r.answers.extra.iter().map(|a| a.part.as_str()).collect();
    assert_eq!(extra, ["review"]);
}

/// A child playbook answered by a nested scenario run of the child: the
/// child runs at the parent's level, lives next to its parent under
/// `.apb/scenario-runs/` and carries a scenario origin. Nothing lands under
/// `.apb/runs/`.
#[test]
fn a_nested_child_scenario_runs_next_to_its_parent() {
    let kid = "schema: 2\nid: kid\nname: kid\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: w }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";
    let parent = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: kid }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: done }\n";
    let dir = project(parent);
    let root = dir.path();
    let k = root.join(".apb/playbooks/kid");
    std::fs::create_dir_all(k.join("1.0.0")).unwrap();
    std::fs::write(k.join("1.0.0/playbook.yaml"), kid).unwrap();
    std::fs::write(k.join("current"), "1.0.0").unwrap();
    std::fs::create_dir_all(k.join("evals/scenarios")).unwrap();
    std::fs::write(
        k.join("evals/scenarios/kid-green.yaml"),
        "schema: 1\nid: kid-green\nanswers:\n  w:\n    - { visit: 1, attempts: [ { status: succeeded, output: from the kid } ] }\n",
    )
    .unwrap();
    let scenario = "schema: 1\nid: p\nanswers:\n  sub:\n    - { visit: 1, playbook: { scenario: kid-green } }\n";
    let (r, events) = run(root, "parent", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    let child = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .unwrap();
    let child_dir = r.run_dir.parent().unwrap().join(&child);
    assert!(child_dir.is_dir(), "the child lives next to its parent");
    assert_eq!(
        std::fs::read_dir(root.join(".apb/runs")).unwrap().count(),
        0,
        "nothing under .apb/runs"
    );
    assert!(matches!(
        crate::manifest::read(&child_dir).unwrap().unwrap().origin,
        Some(crate::manifest::RunOrigin::Scenario { ref scenario, .. }) if scenario == "kid-green"
    ));
    // A promptless finish answers nothing: the node succeeds with it.
    assert_eq!(finished(&events, "sub"), [("succeeded", "")]);
    assert!(r.answers.extra.is_empty());
}

/// A nested child's scenario is read by the one scenario loader: a file
/// that is a link to a place outside the child playbook is refused, and so
/// is one whose `id` is not its name. The node fails and no child runs.
#[test]
fn a_nested_child_scenario_is_read_by_the_scenario_loader() {
    let kid = "schema: 2\nid: kid\nname: kid\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: w }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";
    let parent = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: kid }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: done, condition: { type: node_status, node: sub, equals: success } }\n  - { from: sub, to: failed, condition: { type: node_status, node: sub, equals: failure } }\n";
    let body = "schema: 1\nid: kid-green\nanswers:\n  w:\n    - { visit: 1, attempts: [ { status: succeeded, output: from the kid } ] }\n";
    for case in ["link", "id"] {
        let dir = project(parent);
        let root = dir.path();
        let k = root.join(".apb/playbooks/kid");
        std::fs::create_dir_all(k.join("1.0.0")).unwrap();
        std::fs::write(k.join("1.0.0/playbook.yaml"), kid).unwrap();
        std::fs::write(k.join("current"), "1.0.0").unwrap();
        std::fs::create_dir_all(k.join("evals/scenarios")).unwrap();
        let file = k.join("evals/scenarios/kid-green.yaml");
        if case == "link" {
            let outside = root.join("outside.yaml");
            std::fs::write(&outside, body).unwrap();
            std::os::unix::fs::symlink(&outside, &file).unwrap();
        } else {
            std::fs::write(&file, body.replace("id: kid-green", "id: other")).unwrap();
        }
        let scenario = "schema: 1\nid: p\nanswers:\n  sub:\n    - { visit: 1, playbook: { scenario: kid-green } }\n";
        let (r, events) = run(root, "parent", scenario);
        assert_eq!(r.outcome, RunStatus::Failed, "{case}: {events:#?}");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::ChildRunStarted { .. })),
            "{case}: no child run"
        );
        let out = finished(&events, "sub");
        assert_eq!(out.len(), 1, "{case}");
        assert_eq!(out[0].0, "failed", "{case}");
        assert!(out[0].1.contains("kid-green"), "{case}: {}", out[0].1);
    }
}

/// A child answered with its result as written: no child run at all.
#[test]
fn a_child_answered_by_its_result_starts_no_run() {
    let parent = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: kid }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: done }\n";
    let dir = project(parent);
    let scenario = "schema: 1\nid: p\nanswers:\n  sub:\n    - { visit: 1, playbook: { status: succeeded, output: { pr: 7 } } }\n";
    let (r, events) = run(dir.path(), "parent", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ChildRunStarted { .. }))
    );
    assert_eq!(finished(&events, "sub"), [("succeeded", "{\"pr\":7}")]);
}

/// A deferred retry is journaled with its delay as in a run, then due at
/// once: nothing waits and the run is never released to a waker.
#[test]
fn a_retry_delay_is_journaled_and_due_at_once() {
    let yaml = LOOP.replace(
        "max_retries: 1 }",
        "max_retries: 1, retry_delay: { initial: 2h } }",
    );
    let dir = project(&yaml);
    let scenario = GREEN.replace(
        "        - { status: failed, failure_kind: infrastructure, output: \"spawn failed\" }\n",
        "        - { status: failed, output: \"wrong fix\" }\n",
    );
    let started = std::time::Instant::now();
    let (r, events) = run(dir.path(), "loop", &scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    let deferred = events
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RetryDeferred { .. }))
        .expect("the delay is journaled");
    assert!(events[deferred..].iter().any(
        |e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "work")
    ));
    assert!(!r.run_dir.join("wakers").exists(), "no waker");
    assert!(count(&r, super::BoundaryKind::RetryDelay) > 0);
}

/// An interactive attempt asks; the scenario's person answers at once
/// through the answer channel, and the same attempt goes on with `then`.
#[test]
fn a_question_is_answered_and_the_attempt_goes_on_with_then() {
    let yaml = LOOP.replace("max_retries: 1 }", "max_retries: 1, interactive: true }");
    let dir = project(&yaml);
    let scenario = GREEN.replace(
        "        - { status: failed, failure_kind: infrastructure, output: \"spawn failed\" }\n        - { status: succeeded, output: \"first try\" }\n",
        "        - { question: { text: Which DB, answer: postgres }, then: { status: succeeded, output: \"first try\" } }\n",
    );
    let (r, events) = run(dir.path(), "loop", &scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::QuestionAsked { node, .. } if node == "work")));
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::QuestionAnswered { node, answer, answered_by } if node == "work" && answer == "postgres" && answered_by == "scenario")));
    assert_eq!(finished(&events, "work")[0], ("succeeded", "first try"));
    assert!(r.answers.extra.is_empty() && r.answers.missing.is_empty());
}

/// A wait is due at once with its scripted end; a publish takes its
/// scripted consent and result, and nothing is planned or pushed.
#[test]
fn a_wait_and_a_publish_take_their_scripted_ends() {
    let yaml = "schema: 2\nid: ship\nname: ship\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: scripts/gate.sh, runner: sh }\n  - { id: ci, type: wait, wait_for: { type: timer, seconds: 3600 }, timeout_seconds: 7200 }\n  - { id: push, type: publish, from: work, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\n  - { id: late, type: finish, outcome: failure }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: ci }\n  - { from: ci, to: late, condition: { type: node_status, node: ci, equals: failure } }\n  - { from: ci, to: push, fallback: true }\n  - { from: push, to: done }\n";
    let dir = project(yaml);
    let scenario = "schema: 1\nid: s\nanswers:\n  work:\n    - { visit: 1, script: { exit: 0, stdout: built } }\n  ci:\n    - { visit: 1, wait: { elapsed: true } }\n  push:\n    - { visit: 1, publish: { decision: publish } }\n";
    let started = std::time::Instant::now();
    let (r, events) = run(dir.path(), "ship", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert_eq!(finished(&events, "push"), [("succeeded", "published")]);
    assert!(finished(&events, "done").len() == 1);
    // A timed-out wait takes the failure route.
    let scenario = scenario.replace("wait: { elapsed: true }", "wait: { timed_out: true }");
    let (r, events) = run(dir.path(), "ship", &scenario);
    assert_eq!(finished(&events, "ci")[0].0, "timed_out");
    assert_eq!(finished(&events, "late").len(), 1);
    assert_eq!(r.answers.extra.len(), 1, "the publish answer is unused");
}

/// `on_missing: fail` outside an attempt: a script with no answer fails
/// like a script that exited non-zero, and its failure route runs.
#[test]
fn a_missing_script_fails_and_its_route_runs_under_on_missing_fail() {
    let yaml = "schema: 2\nid: s\nname: s\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: script, script: scripts/gate.sh, runner: sh }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: failed, condition: { type: node_status, node: gate, equals: failure } }\n  - { from: gate, to: done, fallback: true }\n";
    let dir = project(yaml);
    let (r, events) = run(dir.path(), "s", "schema: 1\nid: x\non_missing: fail\n");
    assert!(r.halted.is_none());
    let (status, output) = finished(&events, "gate")[0];
    assert_eq!(status, "failed");
    assert!(output.contains("scenario has no answer"), "{output}");
    assert_eq!(finished(&events, "failed").len(), 1);
    assert_eq!(r.answers.missing.len(), 1);
}

/// A scenario run names every template reference that rendered empty, with
/// why, in the diagnostics sidecar: a node that never ran, and a field an
/// output does not carry. A run reference that renders (`{{run.id}}`, read
/// from the run directory) is not named.
#[test]
fn empty_template_references_are_named_in_the_diagnostics() {
    let yaml = "schema: 2\nid: t\nname: t\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: \"{{nodes.a.output.missing}} and {{nodes.never.output}} in {{run.id}}\" }\n  - { id: never, type: agent_task, prompt: n }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: never, condition: { type: node_status, node: a, equals: failure } }\n  - { from: a, to: b, fallback: true }\n  - { from: never, to: done }\n  - { from: b, to: done }\n";
    let dir = project(yaml);
    let scenario = "schema: 1\nid: x\nanswers:\n  a:\n    - { visit: 1, attempts: [ { status: succeeded, output: \"{\\\"other\\\": 1}\" } ] }\n  b:\n    - { visit: 1, attempts: [ { status: succeeded } ] }\n";
    let (r, _) = run(dir.path(), "t", scenario);
    let records: Vec<(String, String)> = crate::diagnostics::index::read_raw(&r.run_dir)
        .into_iter()
        .filter(|d| d["kind"] == super::TEMPLATE_EMPTY_RECORD)
        .map(|d| {
            (
                d["reference"].as_str().unwrap_or_default().to_string(),
                d["why"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        records,
        [
            (
                "nodes.a.output.missing".to_string(),
                "field absent".to_string()
            ),
            ("nodes.never.output".to_string(), "never ran".to_string()),
        ]
    );
}

/// List items take their own answers (else `"*"`), a rejected item retries
/// with its next attempt, and a judge checks each item from the scenario.
#[test]
fn list_items_take_their_answers_and_a_judge_checks_them() {
    let yaml = "schema: 2\nid: lst\nname: lst\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - id: work\n    type: agent_task\n    prompt: \"Handle one item.\"\n    list:\n      items: { inline: [ { id: a }, { id: b } ] }\n      batch: { size: 2, parallel: 1, retries: 1 }\n      check:\n        judge:\n          state: { item: \"{{item.id}}\" }\n          instructions: Is it done?\n          criteria: { accept: done, reject: not done }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
    let dir = project(yaml);
    let scenario = "schema: 1\nid: x\nanswers:\n  work:\n    - visit: 1\n      items:\n        a: [ { status: succeeded, output: a1 }, { status: succeeded, output: a2 } ]\n        \"*\": { status: succeeded, output: any }\n      judge: { verdict: { value: accept, p: 0.9, confidence: 0.9 } }\n";
    let (r, events) = run(dir.path(), "lst", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    let settled: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ListItemSettled { item, state, .. } => {
                Some((item.clone(), state.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        settled,
        [
            ("a".to_string(), "passed".to_string()),
            ("b".to_string(), "passed".to_string())
        ]
    );
    assert_eq!(count(&r, super::BoundaryKind::ListJudgeCheck), 2);
    let extra: Vec<&str> = r.answers.extra.iter().map(|a| a.part.as_str()).collect();
    assert_eq!(extra, ["item a attempt 2"], "a passed at its first attempt");
}

/// Judge edges are answered by edge address: the answer routes the source,
/// and the decision is journaled under the scenario's provider.
#[test]
fn judge_edges_route_on_the_scripted_answer() {
    let yaml = "schema: 2\nid: je\nname: je\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: review, type: script, script: scripts/gate.sh, runner: sh }\n  - { id: fix, type: finish, outcome: success }\n  - { id: ship, type: finish, outcome: success }\nedges:\n  - { from: start, to: review }\n  - { id: needs-fix, from: review, to: fix, condition: { type: judge, question: \"Does `output` ask for a fix?\", min_p: 0.7, on_unavailable: false } }\n  - { from: review, to: ship, fallback: true }\n";
    let dir = project(yaml);
    for (p, finish) in [(0.9, "fix"), (0.1, "ship")] {
        let scenario = format!(
            "schema: 1\nid: x\nanswers:\n  review:\n    - {{ visit: 1, script: {{ exit: 0, stdout: found a bug }}, judge_edges: {{ \"id:needs-fix\": {{ p: {p} }} }} }}\n"
        );
        let (r, events) = run(dir.path(), "je", &scenario);
        assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
        assert_eq!(finished(&events, finish).len(), 1, "p={p}");
        assert!(events.iter().any(|e| matches!(&e.payload,
            EventPayload::DecisionMade { use_site, provider, .. } if use_site == "judge_edge" && provider.as_deref() == Some("scenario"))));
        assert!(r.answers.extra.is_empty(), "{:?}", r.answers.extra);
    }
}

/// The decision answers `node`'s judge asks were journaled with.
fn decided(events: &[Event], node: &str) -> Vec<BTreeMap<String, crate::event::DecisionAnswer>> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                node: Some(n),
                answers,
                cached,
                ..
            } if n == node => {
                assert!(!cached, "a scripted answer is never a cache hit");
                Some(answers.clone())
            }
            _ => None,
        })
        .collect()
}

/// Two judges asking the same question of the same state: each takes its
/// own scripted answer, never the other's from the decision cache.
#[test]
fn twin_judges_each_take_their_own_scripted_answer() {
    let judge = |id: &str| {
        format!(
            "  - id: {id}\n    type: judge\n    state: {{ out: same }}\n    questions:\n      verdict: {{ type: choice, instructions: \"Is `out` clear?\", criteria: {{ clear: a, unclear: b }} }}\n    on_unavailable: fail\n"
        )
    };
    let yaml = format!(
        "schema: 2\nid: twin\nname: twin\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{}{}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: j1 }}\n  - {{ from: j1, to: j2 }}\n  - {{ from: j2, to: done }}\n",
        judge("j1"),
        judge("j2")
    );
    let dir = project(&yaml);
    let scenario = "schema: 1\nid: twin\nanswers:\n  j1:\n    - { visit: 1, judge: { verdict: { value: clear, p: 0.9 } } }\n  j2:\n    - { visit: 1, judge: { verdict: { value: unclear, p: 0.9 } } }\n";
    let (r, events) = run(dir.path(), "twin", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    let value = |node: &str| decided(&events, node)[0]["verdict"].value.clone();
    assert_eq!(value("j1"), Some(serde_json::json!("clear")));
    assert_eq!(value("j2"), Some(serde_json::json!("unclear")));
}

/// A score is answered by a level's name, as the validator takes it, and
/// reads as that level's index, never as the lowest level; a yes or no
/// question answered by its `value` reads as that answer, never as yes.
#[test]
fn a_scripted_score_level_reads_as_its_index() {
    let yaml = "schema: 2\nid: sized\nname: sized\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - id: j\n    type: judge\n    state: { out: same }\n    questions:\n      size: { type: score, instructions: \"How big is `out`?\", levels: [small, big] }\n      risky: { type: noul, instructions: \"Is `out` risky?\" }\n    on_unavailable: fail\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: j }\n  - { from: j, to: done }\n";
    let dir = project(yaml);
    let scenario = "schema: 1\nid: sized\nanswers:\n  j:\n    - { visit: 1, judge: { size: { value: big, p: 0.9 }, risky: { value: false } } }\n";
    let (r, events) = run(dir.path(), "sized", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    let size = &decided(&events, "j")[0]["size"];
    assert_eq!(size.invalid, None);
    assert_eq!(size.value.as_ref().and_then(|v| v.as_f64()), Some(1.0));
    // A yes or no question answered `false` reads as no.
    assert_eq!(decided(&events, "j")[0]["risky"].p, Some(0.0));
}

/// A list item's script or connector check is answered by the `check` of
/// the item's scripted attempt: the script never runs and the connector is
/// never called; a pass accepts the item, a reject sends it round again,
/// with two items in flight at once.
#[test]
fn a_list_script_or_connector_check_takes_the_scripted_verdict() {
    for check in [
        "{ script: scripts/gate.sh }",
        "{ connector: { name: crm, function: get_deal, args: { id: \"{{item.id}}\" }, accept_when: { path: done, exists: true } } }",
    ] {
        let yaml = format!(
            "schema: 2\nid: chk\nname: chk\nversion: 1.0.0\ndefaults: {{ profile: main }}\nnodes:\n  - {{ id: start, type: start }}\n  - id: work\n    type: agent_task\n    prompt: \"Handle one item.\"\n    list:\n      items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\n      batch: {{ size: 2, parallel: 2, retries: 1 }}\n      check: {check}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
        );
        let dir = project(&yaml);
        let scenario = "schema: 1\nid: chk\nanswers:\n  work:\n    - visit: 1\n      items:\n        a: [ { status: succeeded, output: a1, check: pass } ]\n        b: [ { status: succeeded, output: b1, check: reject }, { status: succeeded, output: b2, check: pass } ]\n";
        let (r, events) = run(dir.path(), "chk", scenario);
        let settled: Vec<(String, String)> = events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ListItemSettled { item, state, .. } => {
                    Some((item.clone(), state.clone()))
                }
                _ => None,
            })
            .collect();
        assert!(
            settled.contains(&("a".to_string(), "passed".to_string()))
                && settled.contains(&("b".to_string(), "passed".to_string())),
            "{check}: {settled:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::ConnectorCall { .. })),
            "{check}: a connector was called"
        );
        assert_eq!(r.outcome, RunStatus::Succeeded, "{check}: {events:#?}");
        assert!(r.answers.extra.is_empty(), "{check}: {:?}", r.answers.extra);
    }
}

/// An unattended run answers an interactive question with the node's
/// default, and the asking attempt goes on with its scripted `then`.
#[test]
fn an_unattended_question_goes_on_with_then() {
    let yaml = "schema: 2\nid: ask\nname: ask\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: w, interactive: true, question_timeout_seconds: 60, default_answer: pg }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
    let dir = project(yaml);
    let scenario = "schema: 1\nid: ask\nunattended: true\nanswers:\n  work:\n    - visit: 1\n      attempts:\n        - { question: { text: Which DB, answer: postgres }, then: { status: succeeded, output: done } }\n";
    let (r, events) = run(dir.path(), "ask", scenario);
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert!(events.iter().any(|e| matches!(&e.payload,
        EventPayload::QuestionAnswered { node, answer, .. } if node == "work" && answer == "pg")));
    assert_eq!(finished(&events, "work"), [("succeeded", "done")]);
    assert!(r.answers.missing.is_empty(), "{:?}", r.answers.missing);
}

/// A smoke scenario's request cap holds for the whole scenario and per
/// HTTP request: an ask reserves the most requests it can send (each
/// provider's retries included) and a nested child run takes from its
/// parent's count. Against a provider that answers 500 every time (so every
/// ask sends all it may), the provider sees exactly what the cap allowed:
/// no ask under one ask's worst case, one ask with room for one, both with
/// room for two.
#[cfg(unix)]
#[test]
fn a_nested_smoke_child_shares_its_parents_request_cap() {
    apb_core::process_mode::testing::in_plain_child(
        "scenario::session::drive_tests::a_nested_smoke_child_shares_its_parents_request_cap",
        || {
            let home = tempfile::tempdir().unwrap();
            let cfg = tempfile::tempdir().unwrap();
            // SAFETY: the child process runs this one test on one thread.
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("APB_CONFIG_DIR", cfg.path());
            }
            let judge = "  - id: j\n    type: judge\n    state: { out: same }\n    questions:\n      verdict: { type: choice, instructions: \"Is `out` green?\", criteria: { green: a, red: b } }\n    on_unavailable: { default: { verdict: green } }\n";
            let parent = format!(
                "schema: 2\nid: par\nname: par\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{judge}  - {{ id: sub, type: playbook, playbook: kid }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: j }}\n  - {{ from: j, to: sub }}\n  - {{ from: sub, to: done }}\n"
            );
            let kid = format!(
                "schema: 2\nid: kid\nname: kid\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{judge}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: j }}\n  - {{ from: j, to: done }}\n"
            );
            // Who was asked, and how it ended.
            let asked = |events: &[Event]| -> Vec<(Option<String>, Option<String>)> {
                events
                    .iter()
                    .filter_map(|e| match &e.payload {
                        EventPayload::DecisionMade {
                            provider, error, ..
                        } => Some((provider.clone(), error.clone())),
                        _ => None,
                    })
                    .collect()
            };
            let sent = || (Some("stub".to_string()), Some("unavailable".to_string()));
            let held = || (None, Some("budget".to_string()));
            let worst = crate::decision::PROVIDER_ATTEMPTS;
            for (cap, parent_ask, child_ask, requests) in [
                (worst - 1, held(), held(), 0),
                (2 * worst - 1, sent(), held(), worst),
                (2 * worst, sent(), sent(), 2 * worst),
            ] {
                let stub = apb_decide::testing::StubServer::start(vec![]);
                std::fs::write(
                    cfg.path().join("decisions.yaml"),
                    format!(
                        "version: 1\nmode: enforce\ntimeout_ms: 2000\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
                        stub.base_url
                    ),
                )
                .unwrap();
                let dir = project(&parent);
                let root = dir.path();
                install(root, &kid);
                write_scenario(root, "kid", "schema: 1\nid: kid-ask\n");
                let mut p = prep(
                    root,
                    "par",
                    "schema: 1\nid: par\nanswers:\n  sub:\n    - { visit: 1, playbook: { scenario: kid-ask } }\n",
                );
                p.level = ScenarioLevel::Smoke;
                p.max_requests = cap;
                let (r, events) = run_prep(root, "par", &p);
                assert_eq!(r.outcome, RunStatus::Succeeded, "cap {cap}: {events:#?}");
                let child = events
                    .iter()
                    .find_map(|e| match &e.payload {
                        EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
                        _ => None,
                    })
                    .expect("the nested child ran");
                let child_events = read_all(&r.run_dir.parent().unwrap().join(child)).unwrap();
                assert_eq!(asked(&events), [parent_ask], "cap {cap}");
                assert_eq!(asked(&child_events), [child_ask], "cap {cap}");
                assert_eq!(stub.count(), requests as usize, "cap {cap}");
            }
        },
    );
}
/// A reservation comes back only when nothing went out: when a later
/// provider answers from the cache, the providers before it have already
/// sent. Four judges ask the same question of the same state through a
/// provider that always fails and one that answers; whatever the cache
/// serves, the two together see no more requests than the cap.
#[cfg(unix)]
#[test]
fn a_later_providers_cache_hit_keeps_the_reservation() {
    apb_core::process_mode::testing::in_plain_child(
        "scenario::session::drive_tests::a_later_providers_cache_hit_keeps_the_reservation",
        || {
            let home = tempfile::tempdir().unwrap();
            let cfg = tempfile::tempdir().unwrap();
            // SAFETY: the child process runs this one test on one thread;
            // the stubs start their threads after.
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("APB_CONFIG_DIR", cfg.path());
            }
            let failing = apb_decide::testing::StubServer::start(vec![]);
            let reply = serde_json::json!({
                "model": "m",
                "answers": { "verdict": { "type": "choice", "choice": "green", "probabilities": { "green": 0.9, "red": 0.1 }, "confidence": 0.9 } },
            });
            let answering = apb_decide::testing::StubServer::start_with_fallback(
                vec![],
                apb_decide::testing::StubResponse::json(200, reply.to_string()),
            );
            std::fs::write(
                cfg.path().join("decisions.yaml"),
                format!(
                    "version: 1\nmode: enforce\ntimeout_ms: 2000\nproviders:\n  - {{ id: a, kind: systemone, base_url: \"{}\", model: m }}\n  - {{ id: b, kind: systemone, base_url: \"{}\", model: m }}\nuses:\n  judge_node: {{ mode: enforce }}\n",
                    failing.base_url, answering.base_url
                ),
            )
            .unwrap();
            let judge = |id: &str| {
                format!(
                    "  - id: {id}\n    type: judge\n    state: {{ out: same }}\n    questions:\n      verdict: {{ type: choice, instructions: \"Is `out` green?\", criteria: {{ green: a, red: b }} }}\n    on_unavailable: {{ default: {{ verdict: green }} }}\n"
                )
            };
            let yaml = format!(
                "schema: 2\nid: four\nname: four\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n{}{}{}{}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: j1 }}\n  - {{ from: j1, to: j2 }}\n  - {{ from: j2, to: j3 }}\n  - {{ from: j3, to: j4 }}\n  - {{ from: j4, to: done }}\n",
                judge("j1"),
                judge("j2"),
                judge("j3"),
                judge("j4")
            );
            let dir = project(&yaml);
            let root = dir.path();
            let cap = 4 * crate::decision::PROVIDER_ATTEMPTS;
            let mut p = prep(root, "four", "schema: 1\nid: four\n");
            p.level = ScenarioLevel::Smoke;
            p.max_requests = cap;
            let (r, events) = run_prep(root, "four", &p);
            assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
            let sent = failing.count() + answering.count();
            assert!(
                sent <= cap as usize,
                "{sent} requests left under a cap of {cap} (a: {}, b: {})",
                failing.count(),
                answering.count()
            );
        },
    );
}

/// Every file below `dirs` with its size and modification time, skipping
/// `skip` (the scenario write root).
fn tree(dirs: &[&Path], skip: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.starts_with(skip) {
                continue;
            }
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                // A directory by its path alone: creating the write root
                // touches its parent, and a new entry shows as itself.
                stack.push(p.clone());
                out.push((p, 0, std::time::SystemTime::UNIX_EPOCH));
            } else {
                out.push((p, meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// A shim directory for `PATH`: one stub per program a run could start,
/// each leaving `<name>.ran` beside it when it runs.
#[cfg(unix)]
fn shims(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    // Every built-in agent's programs, from the registry, and the tools a
    // run starts on its own.
    let agents = apb_core::agents::all()
        .iter()
        .flat_map(|a| a.descriptor().bins.iter().copied());
    for name in ["sh", "bash", "git", "ps", "bwrap", "python3", "node"]
        .into_iter()
        .chain(agents)
    {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n: > '{}'\nexit 0\n",
                dir.join(format!("{name}.ran")).display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A playbook that reaches the engine's own outside sites: a worktree, a
/// baseline check with a script source, a script goal criterion and a
/// protected path on its agent node.
const WIDE: &str = r#"schema: 2
id: wide
name: wide
version: 1.0.0
defaults: { profile: main }
worktree: "feat/x"
baseline_check:
  checks:
    - id: deals
      script: scripts/gate.sh
      observe:
        - { id: count, type: number, path: count, expect: { equals: 0 } }
goal:
  statement: It holds
  criteria:
    - { description: the gate agrees, check: { type: script, path: scripts/gate.sh } }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "Do it", protect: ["src/**"], success_check: scripts/gate.sh }
  - id: triage
    type: judge
    state: { out: "{{nodes.work.output}}" }
    questions:
      verdict: { type: choice, instructions: "Is `out` done?", criteria: { done: a, open: b } }
    on_unavailable: emulate
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: triage }
  - { from: triage, to: done }
"#;

/// Installs playbook `yaml` as version 1.0.0 in the project at `root`,
/// next to the one [`project`] installed.
fn install(root: &Path, yaml: &str) {
    let id = yaml_serde::from_str::<serde_json::Value>(yaml).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let p = root.join(format!(".apb/playbooks/{id}"));
    std::fs::create_dir_all(p.join("1.0.0/scripts")).unwrap();
    std::fs::write(p.join("1.0.0/playbook.yaml"), yaml).unwrap();
    std::fs::write(
        p.join("1.0.0/scripts/gate.sh"),
        "echo the gate ran for real >&2\nexit 99\n",
    )
    .unwrap();
    std::fs::write(p.join("current"), "1.0.0").unwrap();
}

/// The scenarios the guarded test drives, written into their playbooks:
/// playbook id, scenario name, outcome.
fn guarded_cases(root: &Path) -> Vec<(&'static str, String, RunStatus)> {
    scenario_cases(root)
        .into_iter()
        .map(|(id, yaml, outcome)| (id, write_scenario(root, id, &yaml), outcome))
        .collect()
}

/// The scenarios of [`guarded_cases`]: playbook id, scenario, outcome.
fn scenario_cases(root: &Path) -> Vec<(&'static str, String, RunStatus)> {
    let with = |id: &str, from: &str, to: &str| {
        install(
            root,
            &LOOP
                .replace("id: loop", &format!("id: {id}"))
                .replace(from, to),
        );
    };
    with(
        "loop-ro",
        "max_retries: 1 }",
        "max_retries: 3, read_only: true }",
    );
    with(
        "loop-delay",
        "max_retries: 1 }",
        "max_retries: 1, retry_delay: { initial: 2h } }",
    );
    with(
        "loop-ask",
        "max_retries: 1 }",
        "max_retries: 1, interactive: true }",
    );
    install(
        root,
        "schema: 2\nid: ship\nname: ship\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: scripts/gate.sh, runner: sh }\n  - { id: ci, type: wait, wait_for: { type: timer, seconds: 3600 }, timeout_seconds: 7200 }\n  - { id: push, type: publish, from: work, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\n  - { id: late, type: finish, outcome: failure }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: ci }\n  - { from: ci, to: late, condition: { type: node_status, node: ci, equals: failure } }\n  - { from: ci, to: push, fallback: true }\n  - { from: push, to: done }\n",
    );
    install(
        root,
        "schema: 2\nid: lst\nname: lst\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - id: work\n    type: agent_task\n    prompt: \"Handle one item.\"\n    list:\n      items: { inline: [ { id: a }, { id: b } ] }\n      batch: { size: 2, parallel: 1, retries: 1 }\n      check:\n        judge:\n          state: { item: \"{{item.id}}\" }\n          instructions: Is it done?\n          criteria: { accept: done, reject: not done }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
    );
    install(
        root,
        "schema: 2\nid: je\nname: je\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: review, type: script, script: scripts/gate.sh, runner: sh }\n  - { id: fix, type: finish, outcome: success }\n  - { id: ship, type: finish, outcome: success }\nedges:\n  - { from: start, to: review }\n  - { id: needs-fix, from: review, to: fix, condition: { type: judge, question: \"Does `output` ask for a fix?\", min_p: 0.7, on_unavailable: false } }\n  - { from: review, to: ship, fallback: true }\n",
    );
    install(
        root,
        "schema: 2\nid: kid\nname: kid\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: w }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n",
    );
    let scenarios = root.join(".apb/playbooks/kid/evals/scenarios");
    std::fs::create_dir_all(&scenarios).unwrap();
    std::fs::write(
        scenarios.join("kid-green.yaml"),
        "schema: 1\nid: kid-green\nanswers:\n  w:\n    - { visit: 1, attempts: [ { status: succeeded, output: from the kid } ] }\n",
    )
    .unwrap();
    install(
        root,
        "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: kid }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: done }\n",
    );
    install(root, WIDE);
    let infra =
        "        - { status: failed, failure_kind: infrastructure, output: \"spawn failed\" }\n";
    vec![
        ("loop", GREEN.to_string(), RunStatus::Succeeded),
        (
            "loop",
            GREEN.replace("id: green\n", "id: green-quota\n").replace(infra, "        - { status: failed, failure_kind: quota }\n        - { status: failed, failure_kind: quota }\n        - { status: failed, failure_kind: quota }\n"),
            RunStatus::Succeeded,
        ),
        (
            "loop-ro",
            "schema: 1\nid: s\non_extra: warn\nanswers:\n  work:\n    - visit: 1\n      attempts:\n        - { status: succeeded, output: x, read_only: { dirty: [src/a.rs] } }\n".to_string(),
            RunStatus::Failed,
        ),
        (
            "loop-delay",
            GREEN.replace(infra, "        - { status: failed, output: \"wrong fix\" }\n"),
            RunStatus::Succeeded,
        ),
        (
            "loop-ask",
            GREEN.replace(
                "        - { status: failed, failure_kind: infrastructure, output: \"spawn failed\" }\n        - { status: succeeded, output: \"first try\" }\n",
                "        - { question: { text: Which DB, answer: postgres }, then: { status: succeeded, output: \"first try\" } }\n",
            ),
            RunStatus::Succeeded,
        ),
        (
            "ship",
            "schema: 1\nid: s\nanswers:\n  work:\n    - { visit: 1, script: { exit: 0, stdout: built } }\n  ci:\n    - { visit: 1, wait: { elapsed: true } }\n  push:\n    - { visit: 1, publish: { decision: publish } }\n".to_string(),
            RunStatus::Succeeded,
        ),
        (
            "lst",
            "schema: 1\nid: x\nanswers:\n  work:\n    - visit: 1\n      items:\n        \"*\": { status: succeeded, output: any }\n      judge: { verdict: { value: accept, p: 0.9, confidence: 0.9 } }\n".to_string(),
            RunStatus::Succeeded,
        ),
        (
            "je",
            "schema: 1\nid: x\nanswers:\n  review:\n    - { visit: 1, script: { exit: 0, stdout: found a bug }, judge_edges: { \"id:needs-fix\": { p: 0.9 } } }\n".to_string(),
            RunStatus::Succeeded,
        ),
        (
            "parent",
            "schema: 1\nid: p\nanswers:\n  sub:\n    - { visit: 1, playbook: { scenario: kid-green } }\n".to_string(),
            RunStatus::Succeeded,
        ),
        (
            "wide",
            "schema: 1\nid: w\nbaseline: { before: { deals: { count: 0 } }, after: { deals: { count: 0 } } }\ngoal: { \"0\": pass }\nanswers:\n  work:\n    - { visit: 1, attempts: [ { status: succeeded, output: done, check: pass } ] }\n  triage:\n    - { visit: 1, judge: { unavailable: timeout }, attempts: [ { status: succeeded, output: \"{\\\"q1\\\": {\\\"done\\\": 0.9, \\\"open\\\": 0.1}}\" } ] }\n".to_string(),
            RunStatus::Succeeded,
        ),
    ]
}

/// The shims of [`shims`] in `dir` that ran.
fn started(dir: &Path) -> Vec<String> {
    let mut ran: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".ran"))
        .collect();
    ran.sort();
    ran
}

/// The fix loop as `apb __scenario-run` drives it: the process enters the
/// scenario guards at quick with the project's `.apb/scenario-runs` as its
/// write root, then runs the scenario. Every program a run could start is
/// a shim on `PATH`; none runs. Nothing changes in the project outside the
/// write root, in the config dir or in the home directory. A boundary site
/// that acted for real would either start a shim or be refused by a guard,
/// and both fail this test.
#[cfg(unix)]
#[test]
fn a_guarded_quick_run_writes_only_below_its_root_and_starts_nothing() {
    apb_core::process_mode::testing::in_plain_child(
        "scenario::session::drive_tests::a_guarded_quick_run_writes_only_below_its_root_and_starts_nothing",
        || {
            let home = tempfile::tempdir().unwrap();
            let cfg = tempfile::tempdir().unwrap();
            let bin = tempfile::tempdir().unwrap();
            shims(bin.path());
            // SAFETY: the child process runs this one test on one thread.
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("APB_CONFIG_DIR", cfg.path());
                std::env::set_var("PATH", bin.path());
            }
            let dir = project(LOOP);
            let root = dir.path().canonicalize().unwrap();
            let cases = guarded_cases(&root);
            let write_root = root.join(".apb/scenario-runs");
            let watched = [root.as_path(), cfg.path(), home.path()];
            let before = tree(&watched, &write_root);

            crate::scenario::guard::enter(ScenarioLevel::Quick, &write_root).unwrap();
            let mut events = Vec::new();
            let mut reached = std::collections::BTreeSet::new();
            for (id, name, outcome) in &cases {
                let r =
                    crate::scheduler::run_scenario(&root, id, None, &load_prep(&root, id, name))
                        .unwrap();
                let journal = read_all(&r.run_dir).unwrap();
                assert_eq!(
                    apb_core::process_mode::scenario_refusals(),
                    Vec::<String>::new(),
                    "{id}: refused by a guard"
                );
                assert_eq!(
                    started(bin.path()),
                    Vec::<String>::new(),
                    "{id}: programs started"
                );
                assert_eq!(&r.outcome, outcome, "{id}: {journal:#?}");
                assert!(r.run_dir.starts_with(&write_root), "{id}");
                if *outcome == RunStatus::Succeeded {
                    assert!(
                        r.answers.missing.is_empty(),
                        "{id}: {:?}",
                        r.answers.missing
                    );
                }
                events.extend(journal);
                for b in &r.boundaries {
                    if b.count > 0 {
                        reached.insert(b.kind);
                    }
                }
            }
            let unreached: Vec<super::BoundaryKind> = super::BoundaryKind::ALL
                .iter()
                .copied()
                .filter(|k| !reached.contains(k))
                .collect();
            assert!(
                unreached.is_empty(),
                "boundary kinds never reached: {unreached:?}"
            );
            assert_eq!(
                started(bin.path()),
                Vec::<String>::new(),
                "programs started"
            );
            assert_eq!(
                tree(&watched, &write_root),
                before,
                "a file changed outside the write root"
            );
            // No guard refused anything on the way, not even a write its
            // caller ignores: every outside act was a boundary site's.
            assert_eq!(
                apb_core::process_mode::scenario_refusals(),
                Vec::<String>::new(),
                "refused by a guard"
            );
            let refused: Vec<&Event> = events
                .iter()
                .filter(|e| {
                    serde_json::to_string(&e.payload)
                        .unwrap()
                        .contains(apb_core::process_mode::SCENARIO_REFUSAL)
                })
                .collect();
            assert!(refused.is_empty(), "refused by a guard: {refused:#?}");
        },
    );
}
/// What a guard of the scenario process refuses while a scenario plays is
/// in its report: here a provider key read through a command, which the
/// program guard refuses while the run goes on without that provider, makes
/// the scenario invalid and names the refused operation.
#[cfg(unix)]
#[test]
fn a_guard_refusal_during_a_drive_makes_the_scenario_invalid() {
    apb_core::process_mode::testing::in_plain_child(
        "scenario::session::drive_tests::a_guard_refusal_during_a_drive_makes_the_scenario_invalid",
        || {
            let home = tempfile::tempdir().unwrap();
            let cfg = tempfile::tempdir().unwrap();
            std::fs::write(
                cfg.path().join("decisions.yaml"),
                "version: 1\nmode: enforce\ntimeout_ms: 2000\nproviders:\n  - { id: keyed, kind: systemone, base_url: \"http://127.0.0.1:1\", model: m, api_key: \"{{cmd:keyhelper}}\" }\nuses:\n  judge_node: { mode: enforce }\n",
            )
            .unwrap();
            // SAFETY: the child process runs this one test on one thread.
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("APB_CONFIG_DIR", cfg.path());
            }
            let dir = project(LOOP);
            let root = dir.path().canonicalize().unwrap();
            let name = write_scenario(&root, "loop", GREEN);
            let pdir = root.join(".apb/playbooks/loop");
            crate::scenario::store::enter(ScenarioLevel::Smoke, &root).unwrap();
            let req = crate::scenario::bind::DriveRequest {
                root: root.clone(),
                scope: apb_core::scope::Scope::Project,
                playbook_id: "loop".into(),
                version: "1.0.0".into(),
                playbook_digest: "sha256:test".into(),
                playbook: apb_core::schema::Playbook::from_yaml(LOOP).unwrap(),
                scenario: apb_core::scenario::load(&pdir, &name).unwrap(),
                level: ScenarioLevel::Smoke,
                max_decision_requests: Some(2),
            };
            let out = crate::scenario::bind::drive(&req).unwrap();
            let invalid = out.invalid.expect("the refusal makes the scenario invalid");
            assert!(
                invalid.contains(apb_core::process_mode::SCENARIO_REFUSAL)
                    && invalid.contains("program start"),
                "{invalid}"
            );
        },
    );
}

/// The fix loop at smoke as `apb __scenario-run` drives it: the judge asks
/// this machine's configured provider (a stub that refuses), and nothing
/// else leaves the process. No program starts, nothing is written outside
/// the write root (the run's decisions stay in its run directory) and no
/// guard is reached.
#[cfg(unix)]
#[test]
fn a_guarded_smoke_run_asks_only_its_providers_and_writes_only_below_its_root() {
    apb_core::process_mode::testing::in_plain_child(
        "scenario::session::drive_tests::a_guarded_smoke_run_asks_only_its_providers_and_writes_only_below_its_root",
        || {
            let home = tempfile::tempdir().unwrap();
            let cfg = tempfile::tempdir().unwrap();
            let bin = tempfile::tempdir().unwrap();
            shims(bin.path());
            // SAFETY: the child process runs this one test on one thread;
            // the stub starts its thread after.
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("APB_CONFIG_DIR", cfg.path());
                std::env::set_var("PATH", bin.path());
            }
            let stub = apb_decide::testing::StubServer::start(vec![]);
            std::fs::write(
                cfg.path().join("decisions.yaml"),
                format!(
                    "version: 1\nmode: enforce\ntimeout_ms: 2000\nproviders:\n  - {{ id: stub, kind: systemone, base_url: \"{}\", model: jev-1.13.0 }}\nuses:\n  judge_node: {{ mode: enforce }}\n  completion_check: {{ mode: enforce }}\n  retry_advice: {{ mode: enforce }}\n",
                    stub.base_url
                ),
            )
            .unwrap();
            let dir = project(LOOP);
            let root = dir.path().canonicalize().unwrap();
            let write_root = root.join(".apb/scenario-runs");
            let name = write_scenario(&root, "loop", GREEN);
            let watched = [root.as_path(), cfg.path(), home.path()];
            let before = tree(&watched, &write_root);

            crate::scenario::guard::enter(ScenarioLevel::Smoke, &write_root).unwrap();
            let mut p = load_prep(&root, "loop", &name);
            p.level = ScenarioLevel::Smoke;
            p.max_requests = crate::decision::PROVIDER_ATTEMPTS;
            let r = crate::scheduler::run_scenario(&root, "loop", None, &p).unwrap();
            let events = read_all(&r.run_dir).unwrap();

            // The stub refuses, so the judge asked it and has no answer.
            assert_eq!(
                stub.count(),
                crate::decision::PROVIDER_ATTEMPTS as usize,
                "one ask, every retry within the cap"
            );
            assert!(
                events.iter().any(|e| matches!(&e.payload,
                    EventPayload::DecisionMade { use_site, provider, error, .. }
                        if use_site == "judge_node"
                            && provider.as_deref() == Some("stub")
                            && error.as_deref() == Some("unavailable"))),
                "{events:#?}"
            );
            assert_eq!(finished(&events, "triage")[0].0, "failed");
            // Only the judge uses are kept, and the run's request budget is
            // the scenario's cap: the engine's own uses never ask.
            let decisions = crate::manifest::read(&r.run_dir)
                .unwrap()
                .unwrap()
                .decisions
                .expect("the machine's settings are snapshotted");
            assert_eq!(
                decisions.budget.max_requests_per_run,
                crate::decision::PROVIDER_ATTEMPTS
            );
            assert_eq!(
                decisions.uses.keys().collect::<Vec<_>>(),
                ["judge_node"],
                "the engine's own uses are off"
            );
            let sites: Vec<&str> = events
                .iter()
                .filter_map(|e| match &e.payload {
                    EventPayload::DecisionMade { use_site, .. } => Some(use_site.as_str()),
                    _ => None,
                })
                .collect();
            assert!(sites.iter().all(|s| s.starts_with("judge_")), "{sites:?}");
            assert!(r.run_dir.starts_with(&write_root));
            assert_eq!(
                apb_core::process_mode::scenario_refusals(),
                Vec::<String>::new(),
                "refused by a guard"
            );
            assert_eq!(
                started(bin.path()),
                Vec::<String>::new(),
                "programs started"
            );
            assert_eq!(
                tree(&watched, &write_root),
                before,
                "a file changed outside the write root"
            );
        },
    );
}

/// A finish with a prompt composes the run answer through the scripted
/// agent, as an agent node does: its attempt takes the scenario's answer
/// and starts nothing.
#[test]
fn a_finish_with_a_prompt_takes_its_scripted_attempt() {
    let yaml = "schema: 2\nid: fin\nname: fin\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"Do it.\" }\n  - { id: done, type: finish, outcome: success, prompt: \"Summarize {{nodes.work.output}}.\" }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
    let dir = project(yaml);
    let (r, events) = run(
        dir.path(),
        "fin",
        "schema: 1\nid: x\nanswers:\n  work:\n    - { visit: 1, attempts: [ { status: succeeded, output: worked } ] }\n  done:\n    - { visit: 1, attempts: [ { status: succeeded, output: \"SUMMARY: worked\" } ] }\n",
    );
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    assert_eq!(
        finished(&events, "done"),
        vec![("succeeded", "SUMMARY: worked")]
    );
    let started = attempts_of(&events, "done");
    assert_eq!(started.len(), 1);
    assert!(
        matches!(started[0], EventPayload::AttemptStarted { agent, exec: Some(x), .. } if agent == "scenario" && x.executed_by == "scenario"),
        "{:?}",
        started[0]
    );
}
