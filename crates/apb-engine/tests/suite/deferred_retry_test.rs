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
/// its first call (its success leaves `flaky.done`), `FAILS` always fails,
/// `SLOW` sleeps `slow` seconds, `UNTIL_RETRIED` ends only once `FLAKY`
/// succeeded, `UNTIL_DEFERRED` only once a `retry_deferred` is journaled
/// (each fails after 30 s without it), any other prompt succeeds.
pub(crate) fn agent(dir: &Path, slow: &str) -> String {
    let path = dir.join("agent.sh");
    let d = dir.display();
    let wait = |cond: &str| {
        format!(
            "n=0; until {cond}; do n=$((n+1)); if [ $n -gt 600 ]; then echo 'gate never opened' 1>&2; exit 1; fi; sleep 0.05; done; echo ok;;"
        )
    };
    let retried = wait(&format!("[ -f '{d}/flaky.done' ]"));
    let deferred = wait(&format!(
        "grep -qs retry_deferred '{d}'/.apb/runs/*/events.jsonl"
    ));
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *FLAKY*) if [ -f '{d}/flaky.marker' ]; then touch '{d}/flaky.done'; echo ok; exit 0; fi; touch '{d}/flaky.marker'; echo 'the agent made a mistake' 1>&2; exit 1;;\n  *FAILS*) echo 'the agent made a mistake' 1>&2; exit 1;;\n  *SLOW*) sleep {slow}; echo ok;;\n  *UNTIL_RETRIED*) {retried}\n  *UNTIL_DEFERRED*) {deferred}\n  *QUESTION_OUT*) if [ -f '{d}/asked' ]; then echo ok; exit 0; fi; touch '{d}/asked'; printf '%s\\n' '<<<apb:question>>>' '{{\"question\":\"Which DB?\"}}'; exit 0;;\n  *) echo ok;;\nesac\n"
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

fn sibling_playbook(max_parallel: usize, b: &str) -> String {
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
  - {{ id: b, type: agent_task, prompt: "{b}" }}
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
fn sibling_run(max_parallel: usize, b: &str) -> Vec<Event> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "defpb", &sibling_playbook(max_parallel, b), &[]);
    let prog = agent(root, "0");
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

/// A batch: the sibling still runs when the retry is due (it ends only once
/// the retry succeeded), and the retry is admitted beside it.
#[cfg(unix)]
#[test]
fn a_retry_waits_beside_a_running_sibling_in_a_batch() {
    let events = sibling_run(4, "UNTIL_RETRIED");
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
    // `b` ends as soon as `a`'s deferral is journaled, well inside its 1 s
    // delay.
    let events = sibling_run(4, "UNTIL_DEFERRED");
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
    let events = sibling_run(1, "go");
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
                external_data: false,
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
    // Any wait releases: a resume after the pause below releases again
    // however long the steps before it took, as long as the 3 s delay is
    // not used up.
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "0");
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

    // Ahead of its time `resume --all` re-arms the run, and says so only
    // when a waker holds the wait: none can start from this test binary.
    let wait = apb_engine::waker::WakerFor::Release(first.seq);
    assert_rearmed_only_with_a_waker(root, &rd, &run_id, wait, first.wake_at_ms);
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
    let seq = read_all(&rd)
        .unwrap()
        .iter()
        .rev()
        .find(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
        .unwrap()
        .seq;
    let wait = apb_engine::waker::WakerFor::Pause(seq);
    assert_rearmed_only_with_a_waker(root, &rd, &run_id, wait, until);
}

/// `resume --all` reports a wait re-armed only when a waker holds it: from
/// this test binary no waker can start, so it says nothing wakes the run;
/// with the wait's lease held (a live waker) it says re-armed.
fn assert_rearmed_only_with_a_waker(
    root: &Path,
    rd: &Path,
    run_id: &str,
    wait: apb_engine::waker::WakerFor,
    wake_at_ms: u64,
) {
    use apb_engine::pause::{ResumeAllAction, resume_all};
    let action = |root: &Path| {
        resume_all(root)
            .unwrap()
            .into_iter()
            .find(|e| e.run_id == run_id)
            .map(|e| e.action)
    };
    assert!(
        matches!(
            action(root),
            Some(ResumeAllAction::NotRearmed { wake_at_ms: at, .. }) if at == wake_at_ms
        ),
        "no waker started, none holds the wait"
    );
    let lease = apb_engine::waker::Lease::try_take(rd, wait)
        .unwrap()
        .expect("the wait's lease is free");
    assert_eq!(action(root), Some(ResumeAllAction::Rearmed { wake_at_ms }));
    drop(lease);
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
    let yaml = sibling_playbook(1, "COMMIT");
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

/// The node a test parks the drive on: its YAML lines (the parked node is
/// `gate`, entered from `start` through `first` and the `edges` lines), the
/// node `a` starts after, the scripts they run, the run's options and what
/// lets it go.
#[cfg(unix)]
struct Parked<'a> {
    nodes: &'a str,
    first: &'a str,
    edges: &'a str,
    a_after: &'a str,
    scripts: &'a [(&'a str, &'a str)],
    opts: RunOptions,
    release: Box<dyn FnOnce(&Path) + 'a>,
}

#[cfg(unix)]
impl<'a> Parked<'a> {
    /// A parked node of its own, entered straight from `start`.
    fn node(node: &'a str, release: impl FnOnce(&Path) + 'a) -> Self {
        Parked {
            nodes: node,
            first: "gate",
            edges: "",
            a_after: "start",
            scripts: &[],
            opts: RunOptions::default(),
            release: Box::new(release),
        }
    }
}

/// A retry that waits beside a node that parks the drive (a review, a
/// publish consent, a webhook or timer wait, a question out): once due, it
/// runs while the parked node still waits, and the parked node is picked up
/// again afterwards. A deferral never blocks the run behind a person or a
/// signal. Returns the run's journal.
#[cfg(unix)]
fn a_due_retry_runs_beside(parked: Parked<'_>, delay: &str) -> Vec<Event> {
    let dir = tempfile::tempdir().unwrap();
    a_due_retry_runs_beside_in(dir.path(), parked, delay)
}

#[cfg(unix)]
fn a_due_retry_runs_beside_in(root: &Path, parked: Parked<'_>, delay: &str) -> Vec<Event> {
    let Parked {
        nodes,
        first,
        edges,
        a_after,
        scripts,
        opts,
        release,
    } = parked;
    let yaml = format!(
        r#"
schema: 2
id: parkpb
name: Park
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: {{ initial: {delay} }} }}
  - {nodes}
  - {{ id: j, type: agent_task, prompt: "join" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: {a_after}, to: a }}
  - {{ from: start, to: {first} }}
{edges}  - {{ from: a, to: j }}
  - {{ from: gate, to: j }}
  - {{ from: j, to: done }}
"#
    );
    seed(root, "parkpb", &yaml, scripts);
    let prog = agent(root, "0");
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    {
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&root, "parkpb", None, opts));
        });
    }
    let runs = root.join(".apb/runs");
    let run_id = poll_until("the run directory", || {
        if let Ok(early) = rx.try_recv() {
            panic!("the run ended before it started: {early:?}");
        }
        fs::read_dir(&runs)
            .ok()?
            .flatten()
            .find(|e| e.path().join("events.jsonl").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
    });
    let rd = runs.join(&run_id);
    // An hour's delay is waived by a retry-now on the live driver, once the
    // retry waits and the drive parked on `gate`.
    if delay == "1h" {
        poll_until("the deferral and the parked gate", || {
            let events = read_all(&rd).ok()?;
            let parked = events.iter().any(|e| {
                matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "gate")
            });
            (!deferrals(&events, "a").is_empty() && parked).then_some(())
        });
        let expedited = apb_engine::scheduler::deferred::expedite_live(
            &rd,
            &run_id,
            Some("a"),
            apb_engine::scheduler::deferred::ExpediteBy::Operator,
        )
        .unwrap();
        assert_eq!(
            expedited,
            Some(vec!["a".to_string()]),
            "the live driver takes it"
        );
    }
    // Gated on the journal: the delayed attempt of `a` ends while the
    // parked node still waits for its person or its signal.
    let start = Instant::now();
    let retried = loop {
        let events = read_all(&rd).unwrap_or_default();
        if finishes(&events, "a").len() == 1 {
            break !events.iter().any(
                |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "gate"),
            );
        }
        if start.elapsed() > Duration::from_secs(15) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    release(&rd);
    let res = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    assert!(
        retried,
        "the due retry of `a` did not run while `gate` was parked"
    );
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&rd).unwrap();
    assert_eq!(attempts_of(&events, "a").len(), 2);
    assert_eq!(
        finishes(&events, "gate").len(),
        1,
        "the parked node ran once"
    );
    assert_never_finished_with_one_open(&events);
    events
}

#[cfg(unix)]
#[test]
fn a_due_retry_runs_while_a_review_waits() {
    let _lock = common::env_lock();
    a_due_retry_runs_beside(
        Parked::node(
            "{ id: gate, type: human_review, options: [approved, rejected] }",
            approve,
        ),
        "1s",
    );
}

/// A retry-now on a live driver parked on a review starts the attempt at
/// once, as every retry-now surface tells the person it does.
#[cfg(unix)]
#[test]
fn a_retry_now_starts_the_attempt_while_a_review_waits() {
    let _lock = common::env_lock();
    a_due_retry_runs_beside(
        Parked::node(
            "{ id: gate, type: human_review, options: [approved, rejected] }",
            approve,
        ),
        "1h",
    );
}

#[cfg(unix)]
fn approve(rd: &Path) {
    apb_engine::review::post_review(
        rd,
        apb_engine::review::ReviewCommand {
            node: "gate".into(),
            decision: "approved".into(),
            note: String::new(),
        },
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn a_due_retry_runs_while_a_wait_waits_for_its_signal() {
    let _lock = common::env_lock();
    a_due_retry_runs_beside(
        Parked::node(
            "{ id: gate, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 600 }",
            |rd| {
                apb_engine::signals::post_signal(
                    rd,
                    apb_engine::signals::SignalCommand { key: "ci".into() },
                )
                .unwrap();
            },
        ),
        "1s",
    );
}

/// A timer wait the drive gives way from keeps its clock: the wait starts
/// once, the retry runs inside it, and the wait ends when its own time is
/// up, counted from its one start, not from the drive's return to it.
#[cfg(unix)]
#[test]
fn a_timer_wait_keeps_its_clock_while_a_retry_runs_beside_it() {
    let _lock = common::env_lock();
    let events = a_due_retry_runs_beside(
        Parked::node(
            "{ id: gate, type: wait, wait_for: { type: timer, seconds: 6 }, timeout_seconds: 600 }",
            |_| {},
        ),
        // The retry ends about two seconds into the six-second wait, so a
        // clock restarted after it (ending two seconds late) stays far from
        // one kept (ending within a wait tick of six seconds), however late
        // the wait itself started.
        "2s",
    );
    let at = |want: fn(&EventPayload) -> bool| -> Vec<&Event> {
        events.iter().filter(|e| want(&e.payload)).collect()
    };
    let started = at(|p| matches!(p, EventPayload::WaitStarted { node, .. } if node == "gate"));
    let ended = at(|p| matches!(p, EventPayload::WaitSignalled { node } if node == "gate"));
    assert_eq!((started.len(), ended.len()), (1, 1), "one wait, one end");
    let retry_done = finishes(&events, "a")[0];
    let (begin, end) = (started[0], ended[0]);
    assert!(
        begin.seq < events[retry_done].seq && events[retry_done].seq < end.seq,
        "the retry ran inside the wait"
    );
    let waited = end.ts - begin.ts;
    // The timer counts from its one start: it ends once six seconds passed
    // since then, not six seconds after the retry handed the drive back.
    let retry_end_ts = events[retry_done].ts;
    assert!(waited >= 6000, "the wait ended early: {waited} ms");
    let into = retry_end_ts - begin.ts;
    assert!(
        into >= 500,
        "the retry ended {into} ms into the wait, too soon to tell the clocks apart"
    );
    assert!(
        end.ts < retry_end_ts + 6000,
        "the wait restarted its clock after the retry: it ended {} ms after the retry",
        end.ts - retry_end_ts
    );
}

/// A publish node waiting for its consent parks the drive like a review:
/// the due retry beside it runs, and the consent (here a skip) moves it on.
#[cfg(unix)]
#[test]
fn a_due_retry_runs_while_a_publish_waits_for_its_consent() {
    let _lock = common::env_lock();
    let f = crate::common::bare_remote::Fixture::new();
    let _home = f.isolate();
    // A commit of the run (its trailer names it), for the plan to publish.
    let work = "printf x > work.txt\ngit add work.txt\ngit -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q -m work -m \"Apb-Run: $APB_RUN_ID\"\n";
    a_due_retry_runs_beside_in(
        &f.root,
        Parked {
            nodes: "{ id: w, type: script, script: \"scripts/work.sh\", runner: sh }\n  - { id: gate, type: publish, from: w, remote: origin, branch: \"apb/out\" }",
            first: "w",
            edges: "  - { from: w, to: gate }\n",
            // `a` starts after the commit, so the commit is `w`'s alone.
            a_after: "w",
            scripts: &[("work.sh", work)],
            opts: RunOptions {
                consent: Some(apb_engine::consent::RunConsent::irreversible("cli")),
                ..RunOptions::default()
            },
            release: Box::new(|rd| {
                poll_until("the publish request", || {
                    read_all(rd).ok()?.iter().any(|e| {
                        matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "gate")
                    }).then_some(())
                });
                apb_engine::review::post_decision(
                    rd,
                    apb_engine::review::ReviewCommand {
                        node: "gate".into(),
                        decision: "skip".into(),
                        note: String::new(),
                    },
                    None,
                )
                .unwrap();
            }),
        },
        "1s",
    );
    assert_eq!(
        f.remote_ref("apb/out"),
        None,
        "a skipped publish pushes nothing"
    );
}

/// An interactive node with a question out parks the drive: the due retry
/// beside it runs, and the answer posted after the drive gave way is the
/// one the node takes when it is invoked again.
#[cfg(unix)]
#[test]
fn a_due_retry_runs_while_a_question_waits_for_its_answer() {
    let _lock = common::env_lock();
    let events = a_due_retry_runs_beside(
        Parked::node(
            "{ id: gate, type: agent_task, prompt: \"QUESTION_OUT\", interactive: true }",
            |rd| {
                poll_until("the question", || {
                    read_all(rd).ok()?.iter().any(|e| {
                        matches!(&e.payload, EventPayload::QuestionAsked { node, .. } if node == "gate")
                    }).then_some(())
                });
                apb_engine::question::post_answer(rd, Some("gate"), "pg", "human").unwrap();
            },
        ),
        "1s",
    );
    let answered: Vec<&Event> = events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::QuestionAnswered { node, .. } if node == "gate"),
        )
        .collect();
    assert_eq!(answered.len(), 1, "the answer was taken once");
    assert!(
        events[finishes(&events, "a")[0]].seq < answered[0].seq,
        "the retry ran while the question was out"
    );
    assert_eq!(
        attempts_of(&events, "gate").len(),
        2,
        "asked, then answered"
    );
}

/// A deferral on a fallback step (step 1): the delayed attempt re-enters
/// that step with its own retry number and the fallback's model, never the
/// primary's, and the chain spends exactly its budget: two attempts per
/// step, one fallback.
#[cfg(unix)]
#[test]
fn a_deferral_on_a_fallback_step_continues_on_that_step() {
    let yaml = r#"
schema: 2
id: fbpb
name: Fallback
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "go", max_retries: 1, retry_delay: { initial: 1s } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fbpb", yaml, &[]);
    common::seed_profile(
        root,
        "main",
        "claude-code",
        "haiku",
        &[("claude-code", "fbmodel")],
    );
    // The primary model always fails; the fallback fails once, then works.
    let d = root.display();
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n  *fbmodel*) if [ -f '{d}/fb.marker' ]; then echo ok; exit 0; fi; touch '{d}/fb.marker'; echo 'the agent made a mistake' 1>&2; exit 1;;\n  *) echo 'the agent made a mistake' 1>&2; exit 1;;\nesac\n"
        ),
    );
    set_executable(&path);
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &path);
    let res = run(root, "fbpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();

    let steps: Vec<(u32, u32)> = deferrals(&events, "work")
        .iter()
        .map(|&i| match &events[i].payload {
            EventPayload::RetryDeferred { step, retry, .. } => (*step, *retry),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(steps, vec![(0, 1), (1, 1)], "one deferral per chain step");
    let tries: Vec<(u32, u32, Option<String>)> = attempts_of(&events, "work")
        .iter()
        .map(|e| match &e.payload {
            EventPayload::AttemptStarted {
                attempt,
                exec,
                model,
                ..
            } => (*attempt, exec.as_ref().unwrap().step, model.clone()),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        tries,
        vec![
            (1, 0, Some("haiku".into())),
            (2, 0, Some("haiku".into())),
            (3, 1, Some("fbmodel".into())),
            (4, 1, Some("fbmodel".into())),
        ],
        "the delayed attempt of step 1 stays on the fallback"
    );
    let fallbacks = events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::FallbackTriggered { .. }))
        .count();
    assert_eq!(fallbacks, 1);
    assert_never_finished_with_one_open(&events);
}

/// The delay grows by its factor across the node's real retries: the
/// second retry waits twice the first, and it is not admitted earlier.
#[cfg(unix)]
#[test]
fn the_delay_grows_across_real_retries() {
    let yaml = ALWAYS_FAILS.replace("factor: 1", "factor: 2");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "failpb", &yaml, &[]);
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let res = run(root, "failpb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let events = read_all(&run_dir(root, &res)).unwrap();
    let deferred: Vec<(usize, u32, u64)> = deferrals(&events, "work")
        .into_iter()
        .map(|i| match &events[i].payload {
            EventPayload::RetryDeferred {
                retry, delay_ms, ..
            } => (i, *retry, *delay_ms),
            _ => unreachable!(),
        })
        .collect();
    let shape: Vec<(u32, u64)> = deferred.iter().map(|&(_, r, d)| (r, d)).collect();
    assert_eq!(shape, vec![(1, 1000), (2, 2000)]);
    let tries = attempts_of(&events, "work");
    assert_eq!(tries.len(), 3);
    assert!(
        tries[2].ts >= events[deferred[1].0].ts + 2000,
        "the second retry waited its grown delay"
    );
}

/// An infrastructure-class failure (a dropped connection) on a node with a
/// long `retry_delay` is retried by the bounded infrastructure backoff at
/// once, never deferred.
#[cfg(unix)]
#[test]
fn an_infrastructure_failure_is_never_deferred() {
    let yaml = r#"
schema: 2
id: infrapb
name: Infra
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "go", max_retries: 1, retry_delay: { initial: 1h } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "infrapb", yaml, &[]);
    let d = root.display();
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nif [ -f '{d}/net.marker' ]; then echo ok; exit 0; fi\ntouch '{d}/net.marker'\necho 'Error: connection reset by peer' 1>&2\nexit 1\n"
        ),
    );
    set_executable(&path);
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &path);
    let _backoff = common::EnvVar::set("APB_INFRA_BACKOFF_MS", "20,20");
    let res = run(root, "infrapb", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert!(deferrals(&events, "work").is_empty(), "never deferred");
    assert_eq!(attempts_of(&events, "work").len(), 2);
}

/// One node whose only retry waits 2 s (and any wait releases a run that may
/// release, with the threshold at 0).
const SHORT_DELAY: &str = r#"
schema: 2
id: shortpb
name: Short
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 2s } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

fn released_ever(events: &[Event]) -> bool {
    events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::DriveReleased { .. }))
}

/// Runs `id` with the release threshold at 0 and returns its result: a run
/// that may release does so on its first wait
/// ([`the_same_wait_in_an_autonomous_run_releases`]).
fn run_never_released(root: &Path, id: &str, opts: RunOptions) -> RunResult {
    let prog = agent(root, "0");
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &prog);
    let _release = common::EnvVar::set("APB_RELEASE_AFTER_MS", "0");
    run(root, id, None, opts).unwrap()
}

/// The control of the two tests below: the same wait with the same
/// threshold in a plain top-level autonomous run releases the run, and the
/// retry does not run in this drive.
#[cfg(unix)]
#[test]
fn the_same_wait_in_an_autonomous_run_releases() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "shortpb", SHORT_DELAY, &[]);
    let res = run_never_released(root, "shortpb", RunOptions::default());
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert!(released_ever(&events), "the autonomous run did not release");
    assert_eq!(attempts_of(&events, "work").len(), 1, "the retry waits");
}

/// The release threshold applies to an autonomous top-level run only: the
/// same wait in a supervised run is slept in the driver (the supervisor's
/// session lives there), and so is the wait of an eval repetition (its
/// deadline lives in the driver).
#[cfg(unix)]
#[test]
fn supervised_and_eval_runs_never_release() {
    use apb_engine::run_config::{EvalRunSettings, RunMode};
    for (label, opts) in [
        (
            "supervised",
            RunOptions {
                mode: RunMode::Supervised,
                ..RunOptions::default()
            },
        ),
        (
            "eval",
            RunOptions {
                eval: Some(EvalRunSettings::default()),
                ..RunOptions::default()
            },
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed(root, "shortpb", SHORT_DELAY, &[]);
        let res = run_never_released(root, "shortpb", opts);
        assert_eq!(res.outcome, RunStatus::Succeeded, "{label}");
        let events = read_all(&run_dir(root, &res)).unwrap();
        assert!(!released_ever(&events), "a {label} run released");
        let tries = attempts_of(&events, "work");
        assert_eq!(tries.len(), 2, "{label}");
        assert!(
            tries[1].ts >= events[deferrals(&events, "work")[0]].ts + 2000,
            "{label}: the retry still waited its delay"
        );
    }
}

/// A sub-playbook child's wait is slept in its parent's driver: neither
/// journal releases, and the parent sees the child succeed.
#[cfg(unix)]
#[test]
fn a_child_run_never_releases() {
    let parent = r#"
schema: 2
id: parentpb
name: Parent
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: sub, type: playbook, playbook: shortpb }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: sub }
  - { from: sub, to: f }
"#;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "shortpb", SHORT_DELAY, &[]);
    let vdir = root.join(".apb/playbooks/parentpb/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), parent).unwrap();
    fs::write(root.join(".apb/playbooks/parentpb/current"), "1.0.0").unwrap();
    let res = run_never_released(root, "parentpb", RunOptions::default());
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let runs = root.join(".apb/runs");
    let (mut journals, mut waited) = (0, 0);
    for e in fs::read_dir(&runs).unwrap().flatten() {
        let Ok(events) = read_all(&e.path()) else {
            continue;
        };
        journals += 1;
        assert!(!released_ever(&events), "{:?} released", e.file_name());
        // The child's retry waited its delay in place, in this one drive.
        if let Some(&deferred) = deferrals(&events, "work").first() {
            waited += 1;
            let tries = attempts_of(&events, "work");
            assert_eq!(tries.len(), 2, "the child's retry ran");
            assert!(
                tries[1].ts >= events[deferred].ts + 2000,
                "the child's retry still waited its delay"
            );
        }
    }
    assert_eq!(
        (journals, waited),
        (2, 1),
        "the parent and its waiting child"
    );
}
