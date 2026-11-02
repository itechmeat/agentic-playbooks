//! Deferred retries (`retry_delay`) inside a live drive: a sibling runs
//! while a retry waits and a join downstream waits for it, the delay is
//! frozen while the run is paused, the delayed attempt continues its
//! execution on the recorded chain step and binding (also across a pause
//! and a resume, and after a rebind), `max_retries` is never exceeded, a
//! fork policy or a lost race cancels a waiting node, and a run never
//! finishes with a deferral open.

use crate::common;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
use apb_engine::scheduler::deferred::{Due, due};
use apb_engine::scheduler::{ResumeReason, RunOptions, RunResult, plan_resume, resume_by, run};
use apb_engine::state::{RunState, RunStatus};

pub(crate) const DEADLINE: Duration = Duration::from_secs(60);

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

/// One stub for every node, steered by a word in its prompt: `FLAKY` fails
/// its first call, `FAILS` always fails, `SLOW` sleeps `slow` seconds, any
/// other prompt succeeds.
pub(crate) fn agent(dir: &Path, slow: &str) -> String {
    let path = dir.join("agent.sh");
    let d = dir.display();
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *FLAKY*) if [ -f '{d}/flaky.marker' ]; then echo ok; exit 0; fi; touch '{d}/flaky.marker'; echo 'the agent made a mistake' 1>&2; exit 1;;\n  *FAILS*) echo 'the agent made a mistake' 1>&2; exit 1;;\n  *SLOW*) sleep {slow}; echo ok;;\n  *) echo ok;;\nesac\n"
        ),
    );
    set_executable(&path);
    path.to_string_lossy().into_owned()
}

pub(crate) fn seed(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
    common::seed_main(root);
}

pub(crate) fn run_dir(root: &Path, res: &RunResult) -> std::path::PathBuf {
    root.join(".apb/runs").join(&res.run_id)
}

fn position(events: &[Event], f: impl Fn(&EventPayload) -> bool) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| f(&e.payload))
        .map(|(i, _)| i)
        .collect()
}

pub(crate) fn attempts_of<'a>(events: &'a [Event], node: &str) -> Vec<&'a Event> {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
        .collect()
}

fn node_started(events: &[Event], node: &str) -> usize {
    position(
        events,
        |p| matches!(p, EventPayload::NodeStarted { node: n, .. } if n == node),
    )
    .len()
}

fn finishes(events: &[Event], node: &str) -> Vec<usize> {
    position(
        events,
        |p| matches!(p, EventPayload::NodeFinished { node: n, .. } if n == node),
    )
}

pub(crate) fn deferrals(events: &[Event], node: &str) -> Vec<usize> {
    position(
        events,
        |p| matches!(p, EventPayload::RetryDeferred { node: n, .. } if n == node),
    )
}

/// No `run_finished` while a deferral is open, in the journal as written.
pub(crate) fn assert_never_finished_with_one_open(events: &[Event]) {
    for (i, e) in events.iter().enumerate() {
        if matches!(e.payload, EventPayload::RunFinished { .. }) {
            assert!(
                RunState::fold(&events[..i]).deferred.is_empty(),
                "run_finished with a deferral open"
            );
        }
    }
}

fn sibling_playbook(max_parallel: usize) -> String {
    format!(
        r#"
schema: 2
id: defpb
name: Deferred
version: 1.0.0
defaults: {{ profile: main, max_parallel: {max_parallel} }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: {{ initial: 1s }} }}
  - {{ id: b, type: agent_task, prompt: "SLOW" }}
  - {{ id: j, type: agent_task, prompt: "join" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: start, to: b }}
  - {{ from: a, to: j }}
  - {{ from: b, to: j }}
  - {{ from: j, to: done }}
"#
    )
}

/// Runs the sibling playbook and checks what every arm shares: the retry
/// waited at least its delay, continued the execution on the recorded step
/// and binding, and the join ran after both branches.
fn sibling_run(max_parallel: usize, slow: &str) -> Vec<Event> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "defpb", &sibling_playbook(max_parallel), &[]);
    let prog = agent(root, slow);
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let res = run(root, "defpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();

    let deferred = deferrals(&events, "a");
    assert_eq!(deferred.len(), 1, "one ordinary retry, deferred once");
    let EventPayload::RetryDeferred {
        retry,
        step,
        binding,
        delay_ms,
        after_attempt,
        ..
    } = &events[deferred[0]].payload
    else {
        unreachable!()
    };
    assert_eq!((*retry, *step, *delay_ms), (1, 0, 1000));
    let tries = attempts_of(&events, "a");
    assert_eq!(tries.len(), 2);
    assert_eq!(tries[0].seq, *after_attempt);
    let EventPayload::AttemptStarted { attempt, exec, .. } = &tries[1].payload else {
        unreachable!()
    };
    assert_eq!(*attempt, 2, "the delayed attempt continues the numbering");
    let exec = exec.as_ref().expect("the delayed attempt records its step");
    assert_eq!((&exec.binding, exec.step), (binding, 0));
    assert!(
        tries[1].ts >= events[deferred[0]].ts + 1000,
        "the retry waited its delay"
    );
    assert_eq!(node_started(&events, "a"), 1, "one execution of a");
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { node, .. } if node == "a"))
    );
    // The join waited for the deferred branch.
    let a_done = finishes(&events, "a")[0];
    let j_start = position(
        &events,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "j"),
    )[0];
    assert!(j_start > a_done, "the join waited for the deferred node");
    assert!(RunState::fold(&events).deferred.is_empty());
    assert_never_finished_with_one_open(&events);
    events
}

/// A batch: the sibling still runs when the retry is due, and the retry is
/// admitted beside it.
#[cfg(unix)]
#[test]
fn a_retry_waits_beside_a_running_sibling_in_a_batch() {
    let events = sibling_run(4, "3");
    let second_a = attempts_of(&events, "a")[1].seq;
    let b_done = events[finishes(&events, "b")[0]].seq;
    assert!(
        second_a < b_done,
        "the delayed attempt starts while the sibling still runs"
    );
}

/// A batch whose sibling finishes before the retry is due: the batch ends
/// with only the waiting member left, and the drive picks the node up again
/// once it is due.
#[cfg(unix)]
#[test]
fn a_retry_waits_after_its_batch_sibling_finished() {
    // `b` sleeps well under the 1 s delay but not 0 s: `a` fails at once,
    // so its deferral is journaled before `b` ends.
    let events = sibling_run(4, "0.3");
    let deferred = deferrals(&events, "a")[0];
    let b_done = finishes(&events, "b")[0];
    let second_a = events
        .iter()
        .position(|e| e.seq == attempts_of(&events, "a")[1].seq)
        .unwrap();
    assert!(deferred < b_done && b_done < second_a);
}

/// `max_parallel: 1`: the waiting node takes no slot, so the sibling runs
/// during the delay.
#[cfg(unix)]
#[test]
fn a_waiting_retry_takes_no_slot_with_one_slot() {
    let events = sibling_run(1, "0.2");
    let deferred = deferrals(&events, "a")[0];
    let b_done = finishes(&events, "b")[0];
    let second_a = events
        .iter()
        .position(|e| e.seq == attempts_of(&events, "a")[1].seq)
        .unwrap();
    assert!(deferred < b_done && b_done < second_a);
}

fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
    Event { seq, ts, payload }
}

/// A pause freezes the delay: the retry is due later by exactly the time
/// the run was paused, and while paused its remaining delay stands still.
#[test]
fn a_pause_shifts_the_due_time_by_exactly_its_length() {
    let mut events = vec![
        ev(
            0,
            0,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
                definition: None,
            },
        ),
        ev(
            1,
            1_000,
            EventPayload::RetryDeferred {
                node: "a".into(),
                after_attempt: 0,
                retry: 1,
                step: 0,
                binding: "b".into(),
                delay_ms: 5_000,
                failure_kind: None,
            },
        ),
    ];
    let d = RunState::fold(&events).deferred["a"].clone();
    assert_eq!(due(&events, &d, 3_000), Due::At(6_000));
    let pause = PauseRequest {
        by: Some(PauseBy::Operator),
        ..PauseRequest::default()
    };
    events.push(ev(2, 3_000, pause.to_event()));
    assert_eq!(
        due(&events, &d, 5_000),
        Due::Frozen {
            remaining_ms: 3_000
        }
    );
    assert_eq!(
        due(&events, &d, 9_000),
        Due::Frozen {
            remaining_ms: 3_000
        }
    );
    events.push(ev(
        3,
        10_000,
        EventPayload::resumed("a", Some(PauseBy::Operator), false),
    ));
    // Paused for 7 s: due at 6 s + 7 s.
    assert_eq!(due(&events, &d, 11_000), Due::At(13_000));
    assert_eq!(due(&events, &d, 13_000), Due::Now);
    // An expedited retry is due at once.
    events.push(ev(
        4,
        11_000,
        EventPayload::RetryExpedited {
            node: "a".into(),
            by: None,
        },
    ));
    let d = RunState::fold(&events).deferred["a"].clone();
    assert_eq!(due(&events, &d, 11_000), Due::Now);
}

const ALWAYS_FAILS: &str = r#"
schema: 2
id: failpb
name: Fails
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "FAILS", max_retries: 2, retry_delay: { initial: 1s, factor: 1 } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

pub(crate) fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A pause while a retry waits, a rebind of the node, then a resume: the
/// delayed attempt continues on the recorded step and binding (never the
/// rebound one), the numbering goes on, and the node never gets more than
/// `max_retries` retries.
#[cfg(unix)]
#[test]
fn the_delayed_attempt_continues_on_its_binding_across_a_pause_and_a_rebind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "failpb", ALWAYS_FAILS, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    {
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&root, "failpb", None, RunOptions::default()));
        });
    }
    let runs = root.join(".apb/runs");
    let run_id = poll_until("the run directory", || {
        fs::read_dir(&runs)
            .ok()?
            .flatten()
            .find(|e| e.path().join("events.jsonl").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
    });
    let rd = runs.join(&run_id);
    poll_until("the first deferral", || {
        let events = read_all(&rd).ok()?;
        (!deferrals(&events, "work").is_empty()).then_some(())
    });
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    let paused = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    let events = read_all(&rd).unwrap();
    // The pause is guarded: an apb that cannot honour the deferral refuses.
    let at = events
        .iter()
        .rposition(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
        .unwrap();
    assert!(matches!(
        &events[at + 1].payload,
        EventPayload::DeferredRetryGuard { open } if open == &vec!["work".to_string()]
    ));
    let recorded = match &events[deferrals(&events, "work")[0]].payload {
        EventPayload::RetryDeferred { binding, .. } => binding.clone(),
        _ => unreachable!(),
    };
    // Rebind the node to another model behind the drive's back.
    let manifest = apb_engine::manifest::read(&rd).unwrap().unwrap();
    let mut rebound = manifest.for_node("work").unwrap().clone();
    rebound.chain[0].model = "rebound-model".into();
    let overlay = serde_json::json!({ "nodes": { "work": rebound } });
    fs::write(
        rd.join("rebinds.yaml"),
        yaml_serde::to_string(&overlay).unwrap(),
    )
    .unwrap();

    let plan = plan_resume(root, &run_id, None).unwrap();
    assert_eq!(plan.reason, ResumeReason::DeferredRetry);
    assert_eq!(plan.start_node, "work");
    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);

    let events = read_all(&rd).unwrap();
    let tries = attempts_of(&events, "work");
    assert_eq!(tries.len(), 3, "max_retries 2: three attempts, no more");
    let numbers: Vec<u32> = tries
        .iter()
        .map(|e| match &e.payload {
            EventPayload::AttemptStarted { attempt, .. } => *attempt,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(numbers, vec![1, 2, 3]);
    for t in &tries {
        let EventPayload::AttemptStarted { exec, model, .. } = &t.payload else {
            unreachable!()
        };
        assert_eq!(exec.as_ref().unwrap().binding, recorded);
        assert_ne!(model.as_deref(), Some("rebound-model"));
    }
    assert_eq!(deferrals(&events, "work").len(), 2);
    assert_eq!(node_started(&events, "work"), 1);
    assert_never_finished_with_one_open(&events);
}

fn script(body: &str) -> String {
    format!("#!/bin/sh\n{body}\n")
}

/// `cancel_siblings`: a branch that fails cancels a sibling whose retry
/// waits, and the run never waits out the delay.
#[cfg(unix)]
#[test]
fn a_fork_policy_cancels_a_waiting_retry() {
    let yaml = r#"
schema: 2
id: forkpb
name: Fork
version: 1.0.0
defaults: { profile: main, max_parallel: 1 }
nodes:
  - { id: start, type: start, fork: { on_branch_failure: cancel_siblings } }
  - { id: a, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: b, type: script, script: "scripts/fail.sh", runner: sh }
  - { id: j, type: agent_task, prompt: "join" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "forkpb", yaml, &[("fail.sh", &script("exit 1"))]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let started = Instant::now();
    let res = run(root, "forkpb", None, RunOptions::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(deferrals(&events, "a").len(), 1);
    assert_eq!(attempts_of(&events, "a").len(), 1, "the retry never ran");
    let last = *finishes(&events, "a").last().unwrap();
    assert!(matches!(
        &events[last].payload,
        EventPayload::NodeFinished { status, .. } if status == "cancelled"
    ));
    assert!(RunState::fold(&events).deferred.is_empty());
    assert_never_finished_with_one_open(&events);
}

/// `join: any`: the branch that wins the race cancels a branch whose retry
/// waits.
#[cfg(unix)]
#[test]
fn a_lost_race_cancels_a_waiting_retry() {
    let yaml = r#"
schema: 2
id: racepb
name: Race
version: 1.0.0
defaults: { profile: main, max_parallel: 1 }
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: b, type: agent_task, prompt: "wins" }
  - { id: m, type: agent_task, prompt: "merge" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: m, join: any }
  - { from: b, to: m, join: any }
  - { from: m, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "racepb", yaml, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let started = Instant::now();
    let res = run(root, "racepb", None, RunOptions::default()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(attempts_of(&events, "a").len(), 1, "the retry never ran");
    let last = *finishes(&events, "a").last().unwrap();
    assert!(matches!(
        &events[last].payload,
        EventPayload::NodeFinished { status, .. } if status == "cancelled"
    ));
    assert_never_finished_with_one_open(&events);
}

/// The first finish ends the run: a retry still waiting on another branch
/// is written off before `run_finished`.
#[cfg(unix)]
#[test]
fn a_finish_reached_elsewhere_writes_off_a_waiting_retry() {
    let yaml = r#"
schema: 2
id: endpb
name: End
version: 1.0.0
defaults: { profile: main, max_parallel: 1 }
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: b, type: agent_task, prompt: "fast" }
  - { id: other, type: finish, outcome: success }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: other }
  - { from: b, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "endpb", yaml, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let res = run(root, "endpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(deferrals(&events, "a").len(), 1);
    assert!(RunState::fold(&events).deferred.is_empty());
    assert_eq!(attempts_of(&events, "a").len(), 1, "the retry never ran");
    // The write-off closes the visit `a` started: no second visit opens.
    assert_eq!(node_started(&events, "a"), 1);
    let last = *finishes(&events, "a").last().unwrap();
    assert!(matches!(
        &events[last].payload,
        EventPayload::NodeFinished { status, .. } if status == "cancelled"
    ));
    assert_never_finished_with_one_open(&events);
}

pub(crate) const LONG_DELAY: &str = r#"
schema: 2
id: relpb
name: Release
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 3s } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

pub(crate) fn released(run_dir: &Path) -> apb_engine::state::Released {
    RunState::fold(&read_all(run_dir).unwrap())
        .released
        .expect("the run is released")
}

pub(crate) fn wait_driver_gone(run_dir: &Path, run_id: &str) {
    poll_until("the detached driver to exit", || {
        (apb_engine::liveness::driver_alive(run_dir, run_id) != Some(true)).then_some(())
    });
}

/// A drive with nothing to run but a retry due beyond the release threshold
/// releases the run: `drive_released`, the busy lock freed, no driver. Its
/// waker acts only on its own release (a pause supersedes it), `resume
/// --all` re-arms one ahead of time and resumes one whose time passed, a
/// person's resume recomputes the remaining delay instead of skipping it,
/// and the delayed attempt then continues the execution.
#[cfg(unix)]
#[test]
fn a_released_run_is_woken_rearmed_and_resumed_without_skipping_its_delay() {
    use apb_engine::pause::{
        ResumeAllAction, ResumeCause, WakeOutcome, resume_all, run_release_waker,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "relpb", LONG_DELAY, &[]);
    let prog = agent(root, "0");
    let cfg = tempfile::tempdir().unwrap();
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = common::EnvVar::set("APB_WAKER_POLL_MS", "50");
    // An installation key, so the run is stamped as created here.
    apb_core::run_origin::ensure_key().unwrap();

    let res = run(root, "relpb", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Running,
        "the drive released the run"
    );
    let rd = run_dir(root, &res);
    let run_id = res.run_id.clone();
    let events = read_all(&rd).unwrap();
    let deferred_at = events[deferrals(&events, "work")[0]].ts;
    let first = released(&rd);
    assert_eq!(first.nodes, vec!["work".to_string()]);
    assert!(u128::from(first.wake_at_ms).abs_diff(deferred_at + 3_000) <= 5);
    assert!(matches!(
        events.last().map(|e| &e.payload),
        Some(EventPayload::DriveReleased { .. })
    ));
    assert_eq!(
        apb_engine::workdir::lock_owner(&root.join(".apb/workdir.lock")),
        None,
        "the release frees the busy lock"
    );
    assert_ne!(apb_engine::liveness::driver_alive(&rd, &run_id), Some(true));

    // Ahead of its time `resume --all` gives the run a new waker.
    let entries = resume_all(root).unwrap();
    let action = entries
        .iter()
        .find(|e| e.run_id == run_id)
        .map(|e| &e.action);
    assert_eq!(
        action,
        Some(&ResumeAllAction::Rearmed {
            wake_at_ms: first.wake_at_ms
        })
    );
    // A waker acts only on its own release; a pause supersedes it.
    assert_eq!(
        run_release_waker(root, &run_id, first.seq + 1).unwrap(),
        WakeOutcome::Superseded
    );
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    assert_eq!(
        run_release_waker(root, &run_id, first.seq).unwrap(),
        WakeOutcome::Superseded
    );
    std::thread::sleep(Duration::from_millis(400));

    // A person's resume before the time: the drive recomputes what is left
    // of the delay (the pause did not count) and releases again.
    let again = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(again.outcome, RunStatus::Running);
    let second = released(&rd);
    assert!(second.seq > first.seq);
    assert!(
        second.wake_at_ms >= first.wake_at_ms + 400,
        "the paused time moved the retry later"
    );
    assert_eq!(attempts_of(&read_all(&rd).unwrap(), "work").len(), 1);

    // Its waker resumes it at the time (the detached driver needs the
    // shipped `apb`; that the run was handed to one is the property).
    let woke = run_release_waker(root, &run_id, second.seq).unwrap();
    assert!(matches!(woke, WakeOutcome::Resumed { .. }), "{woke:?}");
    assert!(apb_core::clock::now_ms() >= u128::from(second.wake_at_ms));
    wait_driver_gone(&rd, &run_id);
    // Its time passed and nothing drives it: `resume --all` resumes it.
    let entries = resume_all(root).unwrap();
    let action = entries
        .iter()
        .find(|e| e.run_id == run_id)
        .map(|e| &e.action);
    assert!(
        matches!(
            action,
            Some(ResumeAllAction::Resumed {
                cause: ResumeCause::RetryDue,
                ..
            })
        ),
        "{entries:?}"
    );
    wait_driver_gone(&rd, &run_id);

    let done = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(done.outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    let tries = attempts_of(&events, "work");
    assert_eq!(tries.len(), 2);
    assert!(u128::from(second.wake_at_ms) <= tries[1].ts);
    assert_eq!(node_started(&events, "work"), 1);
    assert_never_finished_with_one_open(&events);
}

/// A pause with an `until` still ahead gets its waker back from `resume
/// --all`.
#[cfg(unix)]
#[test]
fn resume_all_rearms_a_pause_whose_until_is_ahead() {
    use apb_engine::pause::{ResumeAllAction, resume_all};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "relpb", LONG_DELAY, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    // No release: the drive waits in-process, and the pause lands there.
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "600000");
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    {
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&root, "relpb", None, RunOptions::default()));
        });
    }
    let runs = root.join(".apb/runs");
    let run_id = poll_until("the run directory", || {
        fs::read_dir(&runs)
            .ok()?
            .flatten()
            .find(|e| e.path().join("events.jsonl").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
    });
    let rd = runs.join(&run_id);
    poll_until("the deferral", || {
        let events = read_all(&rd).ok()?;
        (!deferrals(&events, "work").is_empty()).then_some(())
    });
    let until = u64::try_from(apb_core::clock::now_ms()).unwrap() + 3_600_000;
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            until: Some(until),
            ..PauseRequest::default()
        },
    )
    .unwrap();
    let paused = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    let entries = resume_all(root).unwrap();
    let action = entries
        .iter()
        .find(|e| e.run_id == run_id)
        .map(|e| &e.action);
    assert_eq!(
        action,
        Some(&ResumeAllAction::Rearmed { wake_at_ms: until })
    );
}

fn git(dir: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

/// `max_parallel: 1`: `a` defers its retry, `b` commits while it waits, then
/// `a` finishes without a commit of its own. `b`'s commit is recorded for
/// `b` only, never for the waiting node.
#[cfg(unix)]
#[test]
fn a_waiting_node_is_never_credited_with_a_siblings_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = sibling_playbook(1).replace("prompt: \"SLOW\"", "prompt: \"COMMIT\"");
    seed(root, "defpb", &yaml, &[]);
    git(root, &["init", "-q"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let path = root.join("agent.sh");
    let d = root.display();
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *FLAKY*) if [ -f '{d}/flaky.marker' ]; then echo ok; exit 0; fi; touch '{d}/flaky.marker'; echo 'the agent made a mistake' 1>&2; exit 1;;\n  *COMMIT*) git -C '{d}' -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q --allow-empty -m sibling; echo ok;;\n  *) echo ok;;\nesac\n"
        ),
    );
    set_executable(&path);
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", path.to_str().unwrap());
    let res = run(root, "defpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    let b_done = finishes(&events, "b")[0];
    assert!(deferrals(&events, "a")[0] < b_done, "b ran while a waited");
    let credited: Vec<(&str, usize)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ArtifactsCommitted { node, commits, .. } => {
                Some((node.as_str(), commits.len()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(credited, vec![("b", 1)], "only b made a commit");
}
