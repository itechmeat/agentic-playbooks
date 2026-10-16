//! The quota pause end to end (issue #200, design 6), in process:
//!   - a 429 with a `Retry-After` past the next infrastructure backoff
//!     pauses the run right away (`quota (<agent>/<model>)`, `by: engine`,
//!     `until`), the attempt is `interrupted` and the node unfinished; the
//!     wake-up probe passes and the resume finishes the node;
//!   - without `Retry-After` the infrastructure retries come first, then the
//!     pause, with `until` from the quota schedule;
//!   - a profile fallback comes before any pause;
//!   - a probe that hits the quota again re-pauses without touching the node;
//!   - the seventh quota stop of a node fails it as quota-exhausted.
//!
//! The automatic resume by the detached waker needs the shipped `apb`
//! binary; `apb-cli`'s `quota_pause_cli_test` covers it.

use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::quota::{QUOTA_PAUSE_CAP, QUOTA_PROBE_ACTION};
use apb_engine::pause::{PauseBy, WakeProbe, probe_before_wake};
use apb_engine::scheduler::{RunOptions, resume_by, run};
use apb_engine::state::RunStatus;

use crate::common;

const WF: &str = r#"
schema: 1
id: PLAYBOOK_ID
name: Quota
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
"#;

/// The field-report error, with a `Retry-After` of one second.
const QUOTA_429: &str =
    "429 FreeUsageLimitError: Rate limit exceeded. Please try again later. Retry-After: 1";

fn seed(root: &Path, id: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF.replace("PLAYBOOK_ID", id)).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// A stub agent that answers its calls from a script: call `n` (1-based)
/// fails with the `n`-th entry of `fails` printed on stderr while one is
/// left (an empty entry means success), and succeeds after them.
fn stub(dir: &Path, fails: &[&str]) -> String {
    use std::os::unix::fs::PermissionsExt;
    let calls = dir.join("calls");
    let mut body = format!(
        "#!/bin/sh\nprintf x >> '{c}'\nn=$(wc -c < '{c}' | tr -d ' ')\n",
        c = calls.display()
    );
    for (i, f) in fails.iter().enumerate() {
        if !f.is_empty() {
            body.push_str(&format!(
                "if [ \"$n\" -eq {} ]; then echo '{f}' 1>&2; exit 1; fi\n",
                i + 1
            ));
        }
    }
    body.push_str("echo done\n");
    let path = dir.join("agent.sh");
    common::write_sync(&path, &body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().into_owned()
}

fn calls(dir: &Path) -> usize {
    fs::read(dir.join("calls")).map(|b| b.len()).unwrap_or(0)
}

/// Holds the shared env lock and this suite's variables, removing them on
/// drop. An installation key in its own config dir, so the runs started
/// here are stamped as local (the wake-up probe verifies the run origin).
struct Env {
    _lock: std::sync::MutexGuard<'static, ()>,
    _cfg: tempfile::TempDir,
}

impl Env {
    fn set(prog: &str) -> Self {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_AGENT_CMD", prog);
            std::env::set_var("APB_INFRA_BACKOFF_MS", "20,20");
            std::env::set_var("APB_QUOTA_BACKOFF_MS", "600000,1200000");
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        apb_core::run_origin::ensure_key().unwrap();
        Self {
            _lock: lock,
            _cfg: cfg,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_INFRA_BACKOFF_MS");
            std::env::remove_var("APB_QUOTA_BACKOFF_MS");
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

fn events(root: &Path, run_id: &str) -> Vec<Event> {
    read_all(&root.join(".apb/runs").join(run_id)).unwrap()
}

fn attempts(events: &[Event]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptFinished {
                node,
                status,
                failure_kind,
                ..
            } if node == "work" => Some((status.clone(), failure_kind.clone())),
            _ => None,
        })
        .collect()
}

fn work_finishes(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                output,
                ..
            } if node == "work" => Some((status.clone(), output.clone())),
            _ => None,
        })
        .collect()
}

/// The latest `run_paused`: seq, ts, reason, by, until.
fn last_pause(events: &[Event]) -> (u64, u128, String, Option<PauseBy>, Option<u64>) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::RunPaused {
                reason, by, until, ..
            } => Some((e.seq, e.ts, reason.clone(), *by, *until)),
            _ => None,
        })
        .expect("a run_paused event")
}

fn probes(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction {
                action,
                node,
                detail,
            } if action == QUOTA_PROBE_ACTION && node.as_deref() == Some("work") => {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn a_429_with_retry_after_pauses_then_the_probe_and_resume_finish_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotaflow");
    common::seed_main(root);
    let prog = stub(root, &[QUOTA_429]);
    let _env = Env::set(&prog);

    let res = run(root, "quotaflow", None, RunOptions::default()).unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Paused,
        "a quota error pauses the run"
    );
    let ev = events(root, &res.run_id);
    let (seq, ts, reason, by, until) = last_pause(&ev);
    assert_eq!(reason, "quota (claude-code/haiku)");
    assert_eq!(by, Some(PauseBy::Engine));
    let until = until.expect("a quota pause has an until");
    let wait = u128::from(until).saturating_sub(ts);
    assert!(
        (500..=1_500).contains(&wait),
        "until comes from Retry-After (1 s), got {wait} ms"
    );
    assert_eq!(
        attempts(&ev),
        vec![("interrupted".into(), Some("quota".into()))],
        "a Retry-After past the next backoff pauses right away, and the attempt is interrupted, not failed"
    );
    assert!(
        work_finishes(&ev).is_empty(),
        "no node_finished while paused"
    );
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RetryStarted { .. })),
        "a quota pause spends no retry"
    );

    // Wake-up: the probe answers, then the resume runs the node again.
    assert_eq!(
        probe_before_wake(root, &res.run_id, seq).unwrap(),
        WakeProbe::Passed
    );
    assert_eq!(calls(root), 2, "one attempt, one probe");
    let after = events(root, &res.run_id);
    let p = probes(&after);
    assert_eq!(p.len(), 1);
    assert!(p[0].starts_with("passed"), "{p:?}");

    let resumed = resume_by(root, &res.run_id, None, false, PauseBy::Engine).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    let finishes = work_finishes(&ev);
    assert_eq!(finishes.len(), 1, "{finishes:?}");
    assert_eq!(finishes[0].0, "succeeded");
}

#[cfg(unix)]
#[test]
fn without_retry_after_the_infra_retries_come_first_and_until_follows_the_schedule() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotasched");
    common::seed_main(root);
    let msg = "API Error: 429 Too Many Requests";
    let prog = stub(root, &[msg, msg, msg]);
    let _env = Env::set(&prog);

    let res = run(root, "quotasched", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = events(root, &res.run_id);
    assert_eq!(
        attempts(&ev),
        vec![
            ("failed".into(), Some("quota".into())),
            ("failed".into(), Some("quota".into())),
            ("interrupted".into(), Some("quota".into())),
        ],
        "two infrastructure retries, then the pause"
    );
    let infra = ev
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::SupervisorAction { action, detail, .. }
                if action == "infra_retry" && detail.contains("quota"))
        })
        .count();
    assert_eq!(infra, 2);
    let (_, ts, _, _, until) = last_pause(&ev);
    let wait = u128::from(until.unwrap()).saturating_sub(ts);
    assert!(
        (590_000..=610_000).contains(&wait),
        "the first schedule step, got {wait} ms"
    );
}

#[cfg(unix)]
#[test]
fn a_profile_fallback_comes_before_the_pause() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotafallback");
    common::seed_profile(root, "main", "claude-code", "haiku", &[("claude", "opus")]);
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = opus ]; then echo ok; exit 0; fi\ndone\necho '{QUOTA_429}' 1>&2\nexit 1\n"
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    let _env = Env::set(&path.to_string_lossy());

    let res = run(root, "quotafallback", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    assert!(
        !ev.iter()
            .any(|e| matches!(e.payload, EventPayload::RunPaused { .. })),
        "the fallback succeeded, so no pause"
    );
    assert_eq!(
        attempts(&ev),
        vec![
            ("failed".into(), Some("quota".into())),
            ("succeeded".into(), None)
        ]
    );
    assert!(ev.iter().any(|e| matches!(
        &e.payload,
        EventPayload::FallbackTriggered { to_model, .. } if to_model.as_deref() == Some("opus")
    )));
}

#[cfg(unix)]
#[test]
fn a_probe_that_hits_the_quota_re_pauses_without_touching_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotaprobe");
    common::seed_main(root);
    // Call 1: the node's attempt; call 2: the first probe (quota again);
    // call 3: the second probe (answers); call 4: the node again.
    let prog = stub(root, &[QUOTA_429, "429 Too Many Requests, Retry-After: 2"]);
    let _env = Env::set(&prog);

    let res = run(root, "quotaprobe", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let (seq, ..) = last_pause(&events(root, &res.run_id));

    let WakeProbe::Repaused {
        seq: seq2,
        until: until2,
    } = probe_before_wake(root, &res.run_id, seq).unwrap()
    else {
        panic!("a probe that hits the quota re-pauses");
    };
    let ev = events(root, &res.run_id);
    let (last_seq, ts, reason, by, until) = last_pause(&ev);
    assert_eq!((last_seq, until), (seq2, Some(until2)));
    assert_eq!(reason, "quota (claude-code/haiku)");
    assert_eq!(by, Some(PauseBy::Engine));
    let wait = u128::from(until2).saturating_sub(ts);
    assert!(
        (1_500..=2_500).contains(&wait),
        "the probe's Retry-After, got {wait} ms"
    );
    assert_eq!(
        attempts(&ev).len(),
        1,
        "the probe is not a node attempt: {:?}",
        attempts(&ev)
    );
    let p = probes(&ev);
    assert!(p.len() == 1 && p[0].starts_with("quota"), "{p:?}");
    // The superseded pause is not probed again.
    assert_eq!(
        probe_before_wake(root, &res.run_id, seq).unwrap(),
        WakeProbe::Superseded
    );

    assert_eq!(
        probe_before_wake(root, &res.run_id, seq2).unwrap(),
        WakeProbe::Passed
    );
    let resumed = resume_by(root, &res.run_id, None, false, PauseBy::Engine).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    assert_eq!(calls(root), 4);
}

#[cfg(unix)]
#[test]
fn the_seventh_quota_stop_fails_the_node_as_quota_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotacap");
    common::seed_main(root);
    let always: Vec<&str> = vec![QUOTA_429; 20];
    let prog = stub(root, &always);
    let _env = Env::set(&prog);

    let res = run(root, "quotacap", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let mut outcome = res.outcome;
    for _ in 0..QUOTA_PAUSE_CAP {
        assert_eq!(outcome, RunStatus::Paused);
        outcome = resume_by(root, &res.run_id, None, false, PauseBy::Engine)
            .unwrap()
            .outcome;
    }
    assert_eq!(outcome, RunStatus::Failed, "the cap fails the node");
    let ev = events(root, &res.run_id);
    let pauses = ev
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::RunPaused { reason, .. } if reason.starts_with("quota (")))
        .count();
    assert_eq!(pauses, QUOTA_PAUSE_CAP as usize);
    let finishes = work_finishes(&ev);
    assert_eq!(finishes.len(), 1, "{finishes:?}");
    assert_eq!(finishes[0].0, "failed");
    assert!(
        finishes[0].1.starts_with("quota exhausted"),
        "{}",
        finishes[0].1
    );
    assert_eq!(
        calls(root),
        QUOTA_PAUSE_CAP as usize + 1,
        "one attempt per drive, no infrastructure retry"
    );
}

const PARALLEL: &str = r#"
schema: 1
id: quotapar
name: Quota Parallel
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: "a" }
  - { id: b, type: agent_task, prompt: "b" }
  - { id: j, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: start, to: b }
  - { from: a, to: j }
  - { from: b, to: j }
  - { from: j, to: done }
"#;

/// A quota stop inside a parallel batch pauses the run once the sibling
/// finished (drain), and the resume runs only the stopped member again.
#[cfg(unix)]
#[test]
fn a_quota_stop_in_a_parallel_batch_drains_the_siblings_and_reruns_only_its_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quotapar/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PARALLEL).unwrap();
    fs::write(root.join(".apb/playbooks/quotapar/current"), "1.0.0").unwrap();
    common::seed_main(root);
    // The members run at once, so a call counter would race: the first
    // caller to create the marker file (an exclusive create through the
    // shell's noclobber; some `mkdir` builds are not race-safe) gets the
    // quota error. The marker lives outside the run's working tree.
    let marker = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nif (set -C; : > '{}/quota_once') 2>/dev/null; then echo '{QUOTA_429}' 1>&2; exit 1; fi\necho done\n",
            marker.path().display()
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    let _env = Env::set(&path.to_string_lossy());

    let res = run(root, "quotapar", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = events(root, &res.run_id);
    let finished = |ev: &[Event], n: &str| {
        ev.iter()
            .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == n))
            .count()
    };
    // The member that hit the quota is unfinished with an interrupted
    // attempt; its sibling either finished (it was in flight: drain) or was
    // never admitted.
    let quota_stopped: Vec<&str> = ["a", "b"]
        .into_iter()
        .filter(|n| {
            ev.iter().any(|e| {
                matches!(&e.payload, EventPayload::AttemptFinished { node, status, failure_kind, .. }
                    if node == n && status == "interrupted" && failure_kind.as_deref() == Some("quota"))
            })
        })
        .collect();
    assert_eq!(quota_stopped.len(), 1, "one member hit the quota");
    assert_eq!(finished(&ev, quota_stopped[0]), 0);
    assert!(
        ev.iter().all(|e| !matches!(&e.payload,
            EventPayload::NodeFinished { status, .. } if status != "succeeded")),
        "no member failed"
    );
    assert!(last_pause(&ev).2.starts_with("quota ("));

    let resumed = resume_by(root, &res.run_id, None, false, PauseBy::Engine).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let ev = events(root, &res.run_id);
    for n in ["a", "b", "j"] {
        assert_eq!(finished(&ev, n), 1, "node {n} finishes once");
    }
}

/// The wake-up probe runs an executor from the run's manifest, so it is held
/// to the same checks a resume is: a run directory this machine did not
/// create is neither probed nor resumed, by the probe itself, by
/// `apb resume --all` or by the waker.
#[cfg(unix)]
#[test]
fn the_run_origin_is_verified_before_the_wake_up_probe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotaorigin");
    common::seed_main(root);
    let prog = stub(root, &[QUOTA_429]);
    let _env = Env::set(&prog);

    let res = run(root, "quotaorigin", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    assert_eq!(calls(root), 1, "the node's one attempt");
    let (seq, _, _, _, until) = last_pause(&events(root, &res.run_id));
    // The run directory loses its origin stamp, as one that came from
    // elsewhere would have none.
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    fs::remove_file(run_dir.join("origin.stamp")).unwrap();
    // Let the 1 s Retry-After pass, so the pause is due.
    let until = u128::from(until.unwrap());
    while apb_core::clock::now_ms() <= until {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    assert!(probe_before_wake(root, &res.run_id, seq).is_err());
    let entries = apb_engine::pause::resume_all(root).unwrap();
    assert!(
        matches!(
            entries.as_slice(),
            [apb_engine::pause::ResumeAllEntry {
                action: apb_engine::pause::ResumeAllAction::Refused { .. },
                ..
            }]
        ),
        "{entries:?}"
    );
    let woke = apb_engine::pause::waker::run_waker(root, &res.run_id, seq).unwrap();
    assert!(
        matches!(woke, apb_engine::pause::waker::WakeOutcome::Refused(_)),
        "{woke:?}"
    );
    assert_eq!(calls(root), 1, "no probe ran");
    assert!(probes(&events(root, &res.run_id)).is_empty());
}

/// A wake-up probe whose outcome cannot be journaled right away (the run's
/// event lock is held past its wait) keeps that outcome and retries only the
/// journal write: the run is not resumed while the outcome is unrecorded,
/// the paid probe never runs a second time, and the quota that still holds
/// re-pauses the run once the lock frees.
#[cfg(unix)]
#[test]
fn a_probe_outcome_that_cannot_be_journaled_never_resumes_the_run() {
    use apb_engine::pause::waker::{WakeOutcome, run_waker};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotalock");
    common::seed_main(root);
    // Call 1: the node (quota, 1 s); call 2: the probe (quota, 10 min). A
    // third call would succeed, so a probe run again would resume the run.
    let prog = stub(
        root,
        &[QUOTA_429, "429 Too Many Requests, Retry-After: 600"],
    );
    let _env = Env::set(&prog);
    struct Poll;
    impl Drop for Poll {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("APB_WAKER_POLL_MS") };
        }
    }
    unsafe { std::env::set_var("APB_WAKER_POLL_MS", "50") };
    let _poll = Poll;

    let res = run(root, "quotalock", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let (seq, _, _, _, until) = last_pause(&events(root, &res.run_id));
    while apb_core::clock::now_ms() <= u128::from(until.unwrap()) {
        std::thread::sleep(Duration::from_millis(50));
    }

    let held = apb_core::fsutil::lock_dir(&run_dir, "events.jsonl.lock").unwrap();
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), res.run_id.clone());
    std::thread::spawn(move || {
        let _ = tx.send(run_waker(&r, &id, seq));
    });
    let start = Instant::now();
    while calls(root) < 2 {
        assert!(start.elapsed() < Duration::from_secs(20), "no probe ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Held past at least one full wait of the event lock (2 s), so the
    // first journal write has failed by now. The assertions below hold for
    // any hold time shorter than the retry budget.
    std::thread::sleep(Duration::from_millis(3_000));
    assert!(
        rx.try_recv().is_err(),
        "the waker must not act on a probe outcome nobody journaled"
    );
    assert!(probes(&events(root, &res.run_id)).is_empty());
    drop(held);

    let start = Instant::now();
    let repaused = loop {
        let (s, ..) = last_pause(&events(root, &res.run_id));
        if s != seq {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no re-pause");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(repaused > seq);
    assert_eq!(
        calls(root),
        2,
        "the probe ran once and the node never again"
    );
    let p = probes(&events(root, &res.run_id));
    assert!(p.len() == 1 && p[0].starts_with("quota"), "{p:?}");

    apb_engine::stop::stop_run(root, &res.run_id).unwrap();
    let woke = rx.recv_timeout(Duration::from_secs(20)).unwrap().unwrap();
    assert_eq!(woke, WakeOutcome::Superseded);
}

/// `apb resume --all` walks the runs one after another: a probe outcome it
/// cannot journal because the run's event lock stays busy reports the run
/// busy after a short wait, not a refusal after a minute.
#[cfg(unix)]
#[test]
fn resume_all_reports_a_busy_journal_quickly_as_busy() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quotaall");
    common::seed_main(root);
    let prog = stub(
        root,
        &[QUOTA_429, "429 Too Many Requests, Retry-After: 600"],
    );
    let _env = Env::set(&prog);

    let res = run(root, "quotaall", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let (_, _, _, _, until) = last_pause(&events(root, &res.run_id));
    while apb_core::clock::now_ms() <= u128::from(until.unwrap()) {
        std::thread::sleep(Duration::from_millis(50));
    }

    let held = apb_core::fsutil::lock_dir(&run_dir, "events.jsonl.lock").unwrap();
    let start = Instant::now();
    let entries = apb_engine::pause::resume_all(root).unwrap();
    let took = start.elapsed();
    drop(held);
    assert!(
        matches!(
            entries.as_slice(),
            [apb_engine::pause::ResumeAllEntry {
                action: apb_engine::pause::ResumeAllAction::Busy { .. },
                ..
            }]
        ),
        "{entries:?}"
    );
    assert!(took < Duration::from_secs(30), "took {took:?}");
    assert!(probes(&events(root, &res.run_id)).is_empty());
}
