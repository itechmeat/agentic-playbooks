//! Unattended mode through the real drive (`scheduler::run` and the resume
//! paths every surface ends in): each wait without a declared outcome ends
//! the run `failed` at once through the unhandled-failure stop, without a
//! failure route; a declared gate decision passes as `unattended`; a
//! question's default is posted at once; a budget refusal fails
//! `budget_exhausted` on all three admission sites; a deferred retry never
//! releases the driver; the mode survives a resume and a rewritten
//! `run.yaml`; a child inherits it and stops its parent; and a start the
//! mode cannot hold is refused before any run file exists, with no consent
//! or launch check implied.
//!
//! Every drive runs on a helper thread with a deadline ([`bounded`]): a run
//! that waits for a person, the bug under test, fails the test instead of
//! hanging it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use apb_core::budget::BudgetRequest;
use apb_core::execution::{ExecutionMode, ExecutionRequest};
use apb_engine::budget::GateDecision;
use apb_engine::consent::RunConsent;
use apb_engine::event::{DecidedBy, Event, EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
use apb_engine::run_config::RunMode;
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};
use crate::deferred_retry_test::{agent, seed};

const DEADLINE: Duration = Duration::from_secs(60);

/// Runs `drive` on a helper thread and returns its result; a drive still
/// going at [`DEADLINE`] (a run waiting for a person) is cancelled and the
/// test fails.
fn bounded<T: Send + 'static>(root: &Path, drive: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(drive());
    });
    match rx.recv_timeout(DEADLINE) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the drive thread panicked"),
        Err(e) => {
            for id in run_ids(root) {
                let _ = apb_engine::run_cancel(root, &id);
            }
            panic!("the drive did not end within {DEADLINE:?} ({e}): the run waits for someone")
        }
    }
}

fn run_ids(root: &Path) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn unattended() -> RunOptions {
    RunOptions {
        unattended: true,
        ..RunOptions::default()
    }
}

/// A project with playbook `id` (`yaml`), profile `main` and the steerable
/// stub agent; holds the env lock while it lives.
struct Proj {
    dir: tempfile::TempDir,
    _cfg: tempfile::TempDir,
    _env: Vec<EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Proj {
    fn new(id: &str, yaml: &str) -> Self {
        let lock = common::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        seed(dir.path(), id, yaml, &[]);
        let prog = agent(dir.path(), "0");
        let env = vec![
            EnvVar::set("APB_AGENT_CMD", prog),
            EnvVar::set("APB_CONFIG_DIR", cfg.path()),
        ];
        Proj {
            dir,
            _cfg: cfg,
            _env: env,
            _lock: lock,
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn run(&self, id: &str, opts: RunOptions) -> RunResult {
        let (root, id) = (self.root(), id.to_string());
        bounded(&self.root(), move || run(&root, &id, None, opts).unwrap())
    }

    fn events(&self, res: &RunResult) -> Vec<Event> {
        read_all(&self.root().join(".apb/runs").join(&res.run_id)).unwrap()
    }
}

fn run_error(events: &[Event]) -> (Option<String>, String) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunError { node, reason } => Some((node.clone(), reason.clone())),
            _ => None,
        })
        .expect("a run_error")
}

fn started(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

fn count(events: &[Event], want: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().filter(|e| want(&e.payload)).count()
}

fn header(id: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\ndefaults: {{ profile: main, on_failure: rescue }}\n"
    )
}

/// `start -> {body} ...`, with a `rescue` failure handler that must never
/// run in an unattended stop.
fn playbook(id: &str, nodes: &str, edges: &str) -> String {
    format!(
        "{}nodes:\n  - {{ id: start, type: start }}\n{nodes}  - {{ id: rescue, type: agent_task, prompt: rescue }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: failed, type: finish, outcome: failure }}\nedges:\n{edges}  - {{ from: rescue, to: failed }}\n",
        header(id)
    )
}

const PLAIN_GATE: &str = "  - { id: gate, type: human_review }\n";
const PLAIN_GATE_EDGES: &str = "  - { from: start, to: gate }\n  - { from: gate, to: done }\n";

#[test]
fn a_plain_review_fails_the_run_hitl_required_without_its_failure_route() {
    let p = Proj::new("ua", &playbook("ua", PLAIN_GATE, PLAIN_GATE_EDGES));
    let res = p.run("ua", unattended());
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = p.events(&res);
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("gate"));
    assert_eq!(
        reason,
        "hitl_required: human_review `gate` has no unattended outcome"
    );
    assert_eq!(started(&ev, "rescue"), 0, "no failure route is taken");
    assert_eq!(
        count(&ev, |p| matches!(p, EventPayload::ReviewRequested { .. })),
        0,
        "nobody is asked"
    );
}

const DECIDE_LOOP: &str = "  - { id: gate, type: human_review, options: [approve, reject], unattended: { decide: approve, note: ci } }\n  - { id: work, type: agent_task, prompt: w }\n";
const DECIDE_LOOP_EDGES: &str = "  - { from: start, to: gate }\n  - { from: gate, to: work, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: failed, condition: { type: review_status, equals: reject } }\n  - { from: work, to: gate, max_traversals: 1 }\n  - { from: work, to: done, fallback: true }\n";

#[test]
fn a_declared_decision_passes_the_gate_on_every_visit_as_unattended() {
    let p = Proj::new("ua", &playbook("ua", DECIDE_LOOP, DECIDE_LOOP_EDGES));
    let res = p.run("ua", unattended());
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = p.events(&res);
    let decided: Vec<(String, String, Option<DecidedBy>)> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided {
                decision,
                note,
                decided_by,
                ..
            } => Some((decision.clone(), note.clone(), *decided_by)),
            _ => None,
        })
        .collect();
    let one = (
        "approve".to_string(),
        "ci".to_string(),
        Some(DecidedBy::Unattended),
    );
    assert_eq!(decided, vec![one.clone(), one]);
    // The decision went through the review channel, stamped by the engine.
    let dir = p.root().join(".apb/runs").join(&res.run_id);
    let channel = apb_engine::review::read_reviews_after(&dir, None).unwrap();
    assert_eq!(channel.len(), 2);
    assert!(channel.iter().all(|e| e.by == Some(DecidedBy::Unattended)));
}

#[test]
fn an_attended_run_never_takes_the_declared_decision() {
    let p = Proj::new("ua", &playbook("ua", DECIDE_LOOP, DECIDE_LOOP_EDGES));
    let root = p.root();
    let (tx, rx) = mpsc::channel();
    let r = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "ua", None, RunOptions::default()).unwrap());
    });
    let dir = crate::deferred_retry_test::poll_until("the run dir", || {
        run_ids(&root)
            .first()
            .map(|id| root.join(".apb/runs").join(id))
    });
    crate::deferred_retry_test::poll_until("the review request", || {
        read_all(&dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewRequested { .. }))
            .then_some(())
    });
    assert!(
        apb_engine::review::read_reviews_after(&dir, None)
            .unwrap()
            .is_empty(),
        "an attended gate waits for a person"
    );
    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
    apb_engine::run_cancel(&root, &id).unwrap();
    assert_eq!(
        rx.recv_timeout(DEADLINE).unwrap().outcome,
        RunStatus::Aborted
    );
}

/// Starts `id` unattended, pauses it at its `hold` wait (before the gate)
/// and returns the run directory and id: a state the run's own definition
/// can be changed in before the resume, as an edit of the run's
/// `playbook.yaml` would (the resume executes it, recording the mismatch).
fn paused_at_hold(p: &Proj, id: &str, opts: RunOptions) -> (PathBuf, String) {
    let root = p.root();
    let (tx, rx) = mpsc::channel();
    let (r, pid) = (root.clone(), id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, &pid, None, opts).unwrap());
    });
    let dir = crate::deferred_retry_test::poll_until("the run dir", || {
        run_ids(&root)
            .first()
            .map(|id| root.join(".apb/runs").join(id))
    });
    let run_id = dir.file_name().unwrap().to_string_lossy().into_owned();
    crate::deferred_retry_test::poll_until("the wait", || {
        read_all(&dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::WaitStarted { .. }))
            .then_some(())
    });
    request_pause(
        &root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(
        rx.recv_timeout(DEADLINE).unwrap().outcome,
        RunStatus::Paused
    );
    (dir, run_id)
}

/// The gate's option reaches a sub-playbook whose pinned tree declares
/// `irreversible`: `apb validate` cannot know a child's effects, so only
/// the run's recheck refuses it, and the child never starts.
#[test]
fn a_declared_decision_that_reaches_an_irreversible_child_fails_the_run() {
    let nodes = "  - { id: gate, type: human_review, unattended: { decide: approve } }\n  - { id: ship, type: playbook, playbook: child }\n";
    let edges = "  - { from: start, to: gate }\n  - { from: gate, to: ship }\n  - { from: ship, to: done }\n";
    let p = Proj::new("ua", &playbook("ua", nodes, edges));
    seed(
        &p.root(),
        "child",
        &format!(
            "{}effects: [irreversible]\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n",
            header("child")
        ),
        &[],
    );
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "ua".into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(
        &p.root(),
        &wref,
        true,
        false,
        &apb_engine::gate::launch::LaunchContext::dashboard(
            apb_engine::gate::launch::LaunchKind::Start,
        ),
    )
    .expect("the gate passes: the validator cannot see the child's effects");
    let res = p.run(
        "ua",
        RunOptions {
            consent: Some(RunConsent::irreversible("cli")),
            expected_children: Some(permit.children),
            ..unattended()
        },
    );
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = p.events(&res);
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("gate"));
    assert!(
        reason
            .starts_with("hitl_required: human_review `gate` cannot decide `approve` unattended:")
            && reason.contains("irreversible"),
        "{reason}"
    );
    assert_eq!(started(&ev, "ship"), 0);
    assert_eq!(started(&ev, "rescue"), 0);
}

/// The failure handler of `defaults.on_failure` is a sub-playbook whose
/// pinned tree declares `irreversible`. No edge leads there from the
/// gate's option, but a failure of `work` would: the run's recheck follows
/// the failure policy too, so it refuses the decision before anything after
/// the gate runs.
#[test]
fn a_declared_decision_whose_failure_handler_is_irreversible_fails_the_run() {
    let yaml = format!(
        "{}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: gate, type: human_review, unattended: {{ decide: approve }} }}\n  - {{ id: work, type: agent_task, prompt: w }}\n  - {{ id: rescue, type: playbook, playbook: child }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: failed, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: work }}\n  - {{ from: work, to: done }}\n  - {{ from: rescue, to: failed }}\n",
        header("ua")
    );
    let p = Proj::new("ua", &yaml);
    seed(
        &p.root(),
        "child",
        &format!(
            "{}effects: [irreversible]\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n",
            header("child")
        ),
        &[],
    );
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "ua".into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(
        &p.root(),
        &wref,
        true,
        false,
        &apb_engine::gate::launch::LaunchContext::dashboard(
            apb_engine::gate::launch::LaunchKind::Start,
        ),
    )
    .expect("the gate passes: the validator cannot see the child's effects");
    let res = p.run(
        "ua",
        RunOptions {
            consent: Some(RunConsent::irreversible("cli")),
            expected_children: Some(permit.children),
            ..unattended()
        },
    );
    let ev = p.events(&res);
    assert_eq!(res.outcome, RunStatus::Failed, "{ev:#?}");
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("gate"));
    assert!(
        reason
            .starts_with("hitl_required: human_review `gate` cannot decide `approve` unattended:")
            && reason.contains("irreversible"),
        "{reason}"
    );
    assert_eq!(started(&ev, "work"), 0);
    assert_eq!(started(&ev, "rescue"), 0);
}

/// V143 refuses a `decide` only a bounded edge routes; a run whose own
/// definition was changed to that shape after the start (the resume
/// executes the run's `playbook.yaml`) ends at the dead end, naming the
/// gate and the spent edge.
#[test]
fn a_declared_decision_only_an_exhausted_edge_routes_is_a_dead_end() {
    let nodes = "  - { id: hold, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 600 }\n  - { id: gate, type: human_review, options: [approve, reject], unattended: { decide: approve } }\n  - { id: work, type: agent_task, prompt: w }\n";
    let edges = "  - { from: start, to: hold }\n  - { from: hold, to: gate }\n  - { from: gate, to: work, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: done, condition: { type: review_status, equals: reject } }\n  - { from: work, to: gate, max_traversals: 3 }\n";
    let p = Proj::new("ua", &playbook("ua", nodes, edges));
    let (dir, id) = paused_at_hold(&p, "ua", unattended());
    let def = dir.join("playbook.yaml");
    let yaml = fs::read_to_string(&def).unwrap().replace(
        "{ from: gate, to: work, condition: { type: review_status, equals: approve } }",
        "{ from: gate, to: work, condition: { type: review_status, equals: approve }, max_traversals: 1 }",
    );
    assert!(yaml.contains("max_traversals: 1"), "{yaml}");
    fs::write(&def, yaml).unwrap();
    let root = p.root();
    let resumed = bounded(&p.root(), move || {
        apb_engine::scheduler::resume_by(&root, &id, None, false, PauseBy::Operator).unwrap()
    });
    assert_eq!(resumed.outcome, RunStatus::Failed);
    let ev = read_all(&dir).unwrap();
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("gate"));
    assert!(
        reason.contains("no edge can be taken from `gate`") && reason.contains("exhausted"),
        "{reason}"
    );
    assert_eq!(started(&ev, "work"), 1);
}

/// `start -> hold -> gate`, the gate deciding `approve` unattended.
const HELD_GATE: &str = "  - { id: hold, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 600 }\n  - { id: gate, type: human_review, options: [approve, reject], unattended: { decide: approve } }\n";
const HELD_GATE_EDGES: &str =
    "  - { from: start, to: hold }\n  - { from: hold, to: gate }\n  - { from: gate, to: done }\n";

/// The gate's request was journaled and the drive stopped before the
/// declared decision reached the review channel (a crash between the two
/// writes): the resumed drive finds an open request and no decision. It
/// posts the decision for that request and goes on, instead of waiting for
/// nobody.
#[test]
fn a_request_left_open_by_a_crash_is_decided_on_resume() {
    let p = Proj::new("ua", &playbook("ua", HELD_GATE, HELD_GATE_EDGES));
    let (dir, id) = paused_at_hold(&p, "ua", unattended());
    // What the crashed drive left: the request, and nothing in the channel.
    apb_engine::event::EventLog::open(&dir)
        .unwrap()
        .append(EventPayload::ReviewRequested {
            node: "gate".into(),
            options: vec!["approve".into(), "reject".into()],
            title: None,
            instruction: String::new(),
            prompt: None,
            recommendation: None,
            plan_digest: None,
        })
        .unwrap();
    assert!(!dir.join("reviews.jsonl").exists());
    let root = p.root();
    let resumed = bounded(&p.root(), move || {
        apb_engine::scheduler::resume_by(&root, &id, None, false, PauseBy::Operator).unwrap()
    });
    let ev = read_all(&dir).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded, "{ev:#?}");
    let decided: Vec<(String, Option<DecidedBy>)> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided {
                decision,
                decided_by,
                ..
            } => Some((decision.clone(), *decided_by)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        vec![("approve".to_string(), Some(DecidedBy::Unattended))]
    );
}

/// The declared decision cannot be written to the review channel at all:
/// the run fails `hitl_required` naming the gate, instead of waiting for a
/// decision that never comes.
#[test]
fn a_decision_that_cannot_be_posted_fails_the_run() {
    let p = Proj::new("ua", &playbook("ua", HELD_GATE, HELD_GATE_EDGES));
    let (dir, id) = paused_at_hold(&p, "ua", unattended());
    // A directory where the channel file goes: every append fails.
    fs::create_dir(dir.join("reviews.jsonl")).unwrap();
    let root = p.root();
    let resumed = bounded(&p.root(), move || {
        apb_engine::scheduler::resume_by(&root, &id, None, false, PauseBy::Operator).unwrap()
    });
    let ev = read_all(&dir).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Failed, "{ev:#?}");
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("gate"));
    assert!(
        reason.starts_with(
            "hitl_required: human_review `gate` could not record its unattended decision `approve`:"
        ),
        "{reason}"
    );
    assert_eq!(started(&ev, "rescue"), 0);
}

fn question(default: &str) -> String {
    format!(
        "  - {{ id: ask, type: agent_task, prompt: QUESTION_OUT, interactive: true{default} }}\n"
    )
}
const QUESTION_EDGES: &str = "  - { from: start, to: ask }\n  - { from: ask, to: done }\n";

#[test]
fn a_question_with_a_default_is_answered_at_once_as_unattended() {
    let p = Proj::new(
        "ua",
        &playbook(
            "ua",
            &question(", question_timeout_seconds: 600, default_answer: pg"),
            QUESTION_EDGES,
        ),
    );
    let res = p.run("ua", unattended());
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let answers: Vec<(String, String)> = p
        .events(&res)
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAnswered {
                answer,
                answered_by,
                ..
            } => Some((answer.clone(), answered_by.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(answers, vec![("pg".to_string(), "unattended".to_string())]);
    // The live sidecar would block the attempt on an answer nobody gives:
    // an unattended run never injects it.
    let downgraded: Vec<String> = p
        .events(&res)
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, detail, .. }
                if action == "interaction_downgraded" =>
            {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(downgraded.len(), 1, "{downgraded:?}");
    assert!(downgraded[0].contains("unattended run"), "{downgraded:?}");
}

#[test]
fn a_question_without_a_default_fails_the_run_hitl_required() {
    let p = Proj::new("ua", &playbook("ua", &question(""), QUESTION_EDGES));
    let res = p.run("ua", unattended());
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = p.events(&res);
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("ask"));
    assert_eq!(
        reason,
        "hitl_required: question `ask` has no unattended outcome"
    );
    let finished: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. } if node == "ask" => {
                Some(status.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(finished, vec!["failed".to_string()], "the visit is closed");
    assert_eq!(started(&ev, "rescue"), 0);
}

#[test]
fn a_publish_step_fails_the_run_hitl_required_and_pushes_nothing() {
    let _env = common::env_lock();
    let f = common::bare_remote::Fixture::new();
    let _home = f.isolate();
    apb_core::registry::init_project(&f.root).unwrap();
    let dir = f.root.join(".apb/playbooks/pub/1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("scripts/work.sh"),
        "printf x > work.txt\ngit add work.txt\ngit -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work\n",
    )
    .unwrap();
    fs::write(
        dir.join("playbook.yaml"),
        "schema: 2\nid: pub\nname: pub\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: \"scripts/work.sh\", runner: sh }\n  - { id: publish, type: publish, from: work, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: publish }\n  - { from: publish, to: done }\n",
    )
    .unwrap();
    fs::write(f.root.join(".apb/playbooks/pub/current"), "1.0.0").unwrap();
    let root = f.root.clone();
    let res = bounded(&f.root, move || {
        run(
            &root,
            "pub",
            None,
            RunOptions {
                consent: Some(RunConsent::irreversible("cli")),
                ..unattended()
            },
        )
        .unwrap()
    });
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = read_all(&f.root.join(".apb/runs").join(&res.run_id)).unwrap();
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("publish"));
    assert_eq!(
        reason,
        "hitl_required: publish `publish` has no unattended outcome"
    );
    assert_eq!(
        count(&ev, |p| matches!(p, EventPayload::ReviewRequested { .. })),
        0
    );
    assert_eq!(f.remote_ref("apb/out"), None, "nothing was pushed");
}

fn tokens(n: u64) -> Option<BudgetRequest> {
    Some(BudgetRequest {
        tokens: Some(n),
        ..BudgetRequest::default()
    })
}

/// An unattended run of `bud` with `budget`, after the history the
/// estimate reads: its result and journal.
fn budget_run(p: &crate::budget_gate_test::Project, budget: u64) -> (RunResult, Vec<Event>) {
    p.history(3);
    let root = p.root().to_path_buf();
    let res = bounded(p.root(), move || {
        run(
            &root,
            "bud",
            None,
            RunOptions {
                budget: tokens(budget),
                ..unattended()
            },
        )
        .unwrap()
    });
    let ev = read_all(&p.root().join(".apb/runs").join(&res.run_id)).unwrap();
    (res, ev)
}

/// The run failed `budget_exhausted` before `node` and never paused.
fn assert_exhausted(res: &RunResult, ev: &[Event], node: &str) {
    assert_eq!(res.outcome, RunStatus::Failed);
    let (at, reason) = run_error(ev);
    assert_eq!(at.as_deref(), Some(node));
    assert!(reason.starts_with("budget_exhausted: budget ("), "{reason}");
    assert_eq!(
        count(ev, |p| matches!(p, EventPayload::RunPaused { .. })),
        0,
        "an unattended budget never pauses"
    );
    let decisions: Vec<GateDecision> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BudgetGate { decision, .. } => Some(*decision),
            _ => None,
        })
        .collect();
    assert_eq!(decisions, vec![GateDecision::Failed]);
}

#[test]
fn an_unattended_budget_fails_before_a_sequential_node() {
    let p = crate::budget_gate_test::project(crate::budget_gate_test::LINE);
    let (res, ev) = budget_run(&p, 300);
    assert_exhausted(&res, &ev, "b");
    assert_eq!((started(&ev, "a"), started(&ev, "b")), (1, 0));
}

#[test]
fn an_unattended_budget_fails_before_a_batch_member_and_cancels_it() {
    const FORK: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main, max_parallel: 2 }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: start, to: b }\n  - { from: a, to: done }\n  - { from: b, to: done }\n";
    let p = crate::budget_gate_test::project(FORK);
    let (res, ev) = budget_run(&p, 300);
    assert_exhausted(&res, &ev, "b");
    // `a` was in flight and reported back; `b` never started and is
    // written off with the frontier.
    let b: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. } if node == "b" => Some(status.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(b, vec!["cancelled".to_string()]);
    assert!(
        ev.iter().any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, status, .. } if node == "a" && status == "succeeded"))
    );
}

#[test]
fn an_unattended_budget_fails_before_a_list_batch() {
    let yaml = crate::list_run_test::playbook(
        &format!(
            "items: {{ inline: {} }}\nbatch: {{ size: 1, parallel: 1 }}",
            crate::list_run_test::inline_items(4)
        ),
        "",
        "",
    )
    .replace("id: lst", "id: bud");
    let p = crate::budget_gate_test::project(&yaml);
    let (res, ev) = budget_run(&p, 500);
    assert_exhausted(&res, &ev, "work");
    let batches = count(&ev, |p| matches!(p, EventPayload::ListBatchStarted { .. }));
    assert_eq!(batches, 2, "the refused batch never starts");
    let work: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, status, .. } if node == "work" => {
                Some(status.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(work, vec!["failed".to_string()], "the visit is closed");
}

#[test]
fn a_deferred_retry_of_an_unattended_run_never_releases_the_driver() {
    let p = Proj::new("relpb", crate::deferred_retry_test::LONG_DELAY);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "0");
    let res = p.run("relpb", unattended());
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the drive slept through the delay itself"
    );
    let ev = p.events(&res);
    assert_eq!(
        count(&ev, |p| matches!(p, EventPayload::DriveReleased { .. })),
        0
    );
    assert_eq!(
        count(&ev, |p| matches!(p, EventPayload::RetryDeferred { .. })),
        1
    );
}

#[test]
fn the_mode_survives_a_pause_a_resume_and_a_rewritten_run_config() {
    let nodes = "  - { id: hold, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 600 }\n  - { id: gate, type: human_review }\n";
    let edges = "  - { from: start, to: hold }\n  - { from: hold, to: gate }\n  - { from: gate, to: done }\n";
    let p = Proj::new("ua", &playbook("ua", nodes, edges));
    let root = p.root();
    let (tx, rx) = mpsc::channel();
    let r = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "ua", None, unattended()).unwrap());
    });
    let dir = crate::deferred_retry_test::poll_until("the run dir", || {
        run_ids(&root)
            .first()
            .map(|id| root.join(".apb/runs").join(id))
    });
    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
    crate::deferred_retry_test::poll_until("the wait", || {
        read_all(&dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::WaitStarted { .. }))
            .then_some(())
    });
    request_pause(
        &root,
        &id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    let paused = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    // An older apb rewrites `run.yaml` without the field it does not know.
    let mut cfg = apb_engine::run_config::read_run_config(&dir).unwrap();
    assert!(cfg.unattended);
    cfg.unattended = false;
    apb_engine::run_config::write_run_config(&dir, &cfg).unwrap();
    // A resume, the drive every resume path (a person, MCP, HTTP, the
    // pause and release wakers, `resume --all`) ends in.
    let r = root.clone();
    let resumed = bounded(&root, move || {
        apb_engine::scheduler::resume_by(&r, &id, None, false, PauseBy::Operator).unwrap()
    });
    assert_eq!(resumed.outcome, RunStatus::Failed);
    let ev = read_all(&dir).unwrap();
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("gate"));
    assert!(reason.starts_with("hitl_required:"), "{reason}");
    assert!(
        ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunResumed { .. }))
    );
}

#[test]
fn a_child_inherits_the_mode_and_its_hitl_stop_stops_the_parent() {
    let parent = playbook(
        "parent",
        "  - { id: c, type: playbook, playbook: child }\n",
        "  - { from: start, to: c }\n  - { from: c, to: done }\n",
    );
    let p = Proj::new("parent", &parent);
    seed(
        &p.root(),
        "child",
        "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: g, type: human_review }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: g }\n  - { from: g, to: f }\n",
        &[],
    );
    let res = p.run("parent", unattended());
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = p.events(&res);
    let child = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
            _ => None,
        })
        .expect("a child run");
    let child_dir = p.root().join(".apb/runs").join(&child);
    let manifest = apb_engine::manifest::read(&child_dir).unwrap().unwrap();
    assert!(manifest.unattended, "the child inherits the mode");
    let (_, child_reason) = run_error(&read_all(&child_dir).unwrap());
    assert_eq!(
        child_reason,
        "hitl_required: human_review `g` has no unattended outcome"
    );
    let (node, reason) = run_error(&ev);
    assert_eq!(node.as_deref(), Some("c"));
    assert_eq!(
        reason,
        format!("hitl_required: via child {child}: human_review `g` has no unattended outcome")
    );
    assert_eq!(
        started(&ev, "rescue"),
        0,
        "the parent takes no failure route"
    );
}

/// Refused before anything is written: no run directory.
fn assert_refused_without_a_run(p: &Proj, id: &str, opts: RunOptions, want: &str) {
    let (root, pb) = (p.root(), id.to_string());
    let err = bounded(&p.root(), move || {
        run(&root, &pb, None, opts).map(|r| r.outcome)
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains(want), "{err}");
    assert!(
        run_ids(&p.root()).is_empty(),
        "a refused start leaves no run"
    );
}

#[test]
fn an_unattended_start_is_refused_with_supervision_or_host_execution() {
    let p = Proj::new("ua", &playbook("ua", PLAIN_GATE, PLAIN_GATE_EDGES));
    for mode in [RunMode::Supervised, RunMode::AgentSupervised] {
        assert_refused_without_a_run(
            &p,
            "ua",
            RunOptions {
                mode,
                ..unattended()
            },
            "unattended_unsupported",
        );
    }
    assert_refused_without_a_run(
        &p,
        "ua",
        RunOptions {
            execution: ExecutionRequest {
                mode: Some(ExecutionMode::Host),
                host_session: true,
                ..ExecutionRequest::default()
            },
            ..unattended()
        },
        "unattended_unsupported",
    );
    // The surfaces' check gives the same refusal, structured, with a
    // `fix_requirement` start action carrying a manual remedy.
    let refusal = apb_engine::gate::check_unattended(
        &p.root(),
        &apb_engine::gate::UnattendedStart {
            unattended: true,
            mode: RunMode::Supervised,
            execution: &ExecutionRequest::default(),
        },
    )
    .unwrap_err();
    assert_eq!(refusal["policy"], "unattended_unsupported");
    let actions = apb_engine::next_action::start::from_refusal(
        apb_engine::next_action::start::StartOf {
            playbook: "ua",
            version: None,
        },
        &refusal,
    );
    assert_eq!(actions.len(), 1);
    assert_eq!(
        actions[0].code,
        apb_engine::next_action::start::StartActionCode::FixRequirement
    );
    assert!(matches!(
        actions[0].remedy,
        Some(apb_engine::next_action::start::Remedy::Manual { .. })
    ));
}

#[test]
fn an_unattended_start_of_a_host_session_runs_on_the_cli_without_the_host_fallback() {
    let p = Proj::new("ua", &playbook("ua", "", "  - { from: start, to: done }\n"));
    let host_session = ExecutionRequest {
        host_session: true,
        ..ExecutionRequest::default()
    };
    let res = p.run(
        "ua",
        RunOptions {
            execution: host_session.clone(),
            ..unattended()
        },
    );
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let dir = p.root().join(".apb/runs").join(&res.run_id);
    let m = apb_engine::manifest::read(&dir).unwrap().unwrap();
    // A plain CLI run without the host fallback: recorded as such, or not
    // recorded at all (the manifest of a CLI run before the field existed).
    assert!(
        m.execution
            .as_ref()
            .is_none_or(|e| e.mode == "cli" && !e.fallback_to_host),
        "{:?}",
        m.execution
    );
    // The same request attended keeps the fallback.
    let res = p.run(
        "ua",
        RunOptions {
            execution: host_session,
            ..RunOptions::default()
        },
    );
    let dir = p.root().join(".apb/runs").join(&res.run_id);
    let m = apb_engine::manifest::read(&dir).unwrap().unwrap();
    assert!(m.execution.is_some_and(|e| e.fallback_to_host));
}

#[test]
fn the_mode_implies_no_consent_and_no_launch_permission() {
    let irreversible = playbook(
        "ua",
        "  - { id: ship, type: agent_task, prompt: ship, effects: [irreversible] }\n",
        "  - { from: start, to: ship }\n  - { from: ship, to: done }\n",
    );
    let p = Proj::new("ua", &irreversible);
    let err = run(&p.root(), "ua", None, unattended())
        .unwrap_err()
        .to_string();
    assert!(err.contains("irreversible"), "{err}");
    assert!(run_ids(&p.root()).is_empty());
    let ask = playbook(
        "ask",
        "  - { id: w, type: agent_task, prompt: w }\n",
        "  - { from: start, to: w }\n  - { from: w, to: done }\n",
    )
    .replace("version: 1.0.0\n", "version: 1.0.0\nagent_launch: ask\n");
    seed(&p.root(), "ask", &ask, &[]);
    let err = run(&p.root(), "ask", None, unattended())
        .unwrap_err()
        .to_string();
    assert!(err.contains("launch_unchecked"), "{err}");
    assert!(run_ids(&p.root()).is_empty());
}
