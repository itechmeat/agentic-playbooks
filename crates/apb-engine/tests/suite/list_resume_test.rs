//! List mode across stops: a `--now` pause, a drain pause, a quota pause of
//! one item, a crashed attempt, unknown sends and a supervisor retry of a
//! failed list. Every case reads the journal and the stub's own record of
//! what it did (a side-effect file appended once per finished item), the way
//! a person counts what was filed or sent.

use std::fs;
use std::path::Path;
use std::sync::mpsc;

use apb_engine::EngineError;
use apb_engine::control::Control;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
use apb_engine::scheduler::{
    RunMode, RunOptions, RunResult, drive_prepared, post_supervisor_command,
    prepare_supervised_background, resume, run,
};
use apb_engine::state::{RunState, RunStatus};

use crate::list_run_test::{
    DEADLINE, Env, events, inline_items, last_state, playbook, poll_until, runs_of, seed, settled,
    stub, stub_dir, work_finishes,
};

const CHECK: &str =
    "item_outputs: [blocking]\ncheck: { output: { field: blocking, equals: \"0\" } }";

/// The stub's side effect: one line per item that finished its work.
const FILE_IT: &str = r#"echo "$I" >> "$D/filed""#;

/// Holds every item but the first `free` (by number) at the `gate` file.
fn gated(free: u32) -> String {
    format!(
        r#"k=${{I#i}}; if [ "$k" -gt {free} ]; then while [ -f "$D/gate" ]; do sleep 0.05; done; fi
{FILE_IT}"#
    )
}

/// Every line of the side-effect file.
fn filed(root: &Path) -> Vec<String> {
    let mut lines: Vec<String> = fs::read_to_string(stub_dir(root).join("filed"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

/// The ids `i1`..`in`, sorted as `filed` sorts them.
fn ids(n: usize) -> Vec<String> {
    let mut v: Vec<String> = (1..=n).map(|i| format!("i{i}")).collect();
    v.sort();
    v
}

fn spawn(root: &Path) -> mpsc::Receiver<Result<RunResult, EngineError>> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "lst", None, RunOptions::default()));
    });
    rx
}

fn joined(rx: &mpsc::Receiver<Result<RunResult, EngineError>>) -> RunResult {
    rx.recv_timeout(DEADLINE)
        .expect("the drive returns")
        .expect("the drive does not fail")
}

/// The only run of `lst`.
fn run_id(root: &Path) -> String {
    poll_until("the run directory", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("lst-"))
    })
}

fn passed_count(root: &Path, run_id: &str) -> usize {
    let ev = read_all(&root.join(".apb/runs").join(run_id)).unwrap_or_default();
    settled(&ev).iter().filter(|s| s.state == "passed").count()
}

fn operator(mode: PauseMode) -> PauseRequest {
    PauseRequest {
        reason: Some("later".into()),
        by: Some(PauseBy::Operator),
        mode,
        ..PauseRequest::default()
    }
}

/// An inbox of 43 messages, 4 filed when a `--now` pause lands: the items
/// in flight are interrupted (no round spent), the resume runs exactly the
/// rest, and every message is filed exactly once.
#[test]
fn a_now_pause_interrupts_items_and_the_resume_files_each_message_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: {}, max_items: 100 }}\nbatch: {{ size: 10, parallel: 3, retries: 1 }}\n{CHECK}",
        inline_items(43)
    );
    seed(root, &playbook(&list, "", ""), &[]);
    let _env = Env::set(&stub(root, &gated(4)), &[("APB_PAUSE_GRACE_MS", "300")]);
    fs::write(stub_dir(root).join("gate"), "").unwrap();
    let rx = spawn(root);
    let id = run_id(root);
    poll_until("4 items filed and 3 held", || {
        let held = (5..=7).all(|k| stub_dir(root).join(format!("started-i{k}")).is_file());
        (passed_count(root, &id) == 4 && held).then_some(())
    });
    request_pause(root, &id, &operator(PauseMode::Now)).unwrap();
    assert_eq!(joined(&rx).outcome, RunStatus::Paused);
    assert_eq!(filed(root).len(), 4);
    let ev = events(root, &id);
    assert!(work_finishes(&ev).is_empty(), "the visit stays open");
    let interrupted = ev
        .iter()
        .filter(|e| {
            matches!(&e.payload,
            EventPayload::AttemptFinished { item: Some(_), status, .. } if status == "interrupted")
        })
        .count();
    assert_eq!(interrupted, 3, "the three items in flight");

    fs::remove_file(stub_dir(root).join("gate")).unwrap();
    let res = resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(filed(root), ids(43), "every message filed exactly once");
    let ev = events(root, &id);
    assert!(
        settled(&ev).iter().all(|s| s.state != "rejected"),
        "nothing was rejected"
    );
    for k in 5..=7 {
        let s = last_state(&ev, &format!("i{k}"));
        assert_eq!((s.state.as_str(), s.round), ("passed", 0), "no round spent");
    }
}

/// A drain pause while item 9 runs: it finishes and passes, no item starts
/// after it, and the resume runs exactly the remaining items.
#[test]
fn a_drain_pause_finishes_the_running_item_and_the_resume_runs_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: {} }}\nbatch: {{ size: 5, parallel: 1, retries: 0 }}\n{CHECK}",
        inline_items(15)
    );
    seed(root, &playbook(&list, "", ""), &[]);
    let hook = format!(
        r#"if [ "$I" = i9 ]; then while [ -f "$D/gate" ]; do sleep 0.05; done; fi
{FILE_IT}"#
    );
    let _env = Env::set(&stub(root, &hook), &[]);
    fs::write(stub_dir(root).join("gate"), "").unwrap();
    let rx = spawn(root);
    let id = run_id(root);
    poll_until("item 9 to start", || {
        stub_dir(root).join("started-i9").is_file().then_some(())
    });
    request_pause(root, &id, &operator(PauseMode::Drain)).unwrap();
    fs::remove_file(stub_dir(root).join("gate")).unwrap();
    assert_eq!(joined(&rx).outcome, RunStatus::Paused);
    assert_eq!(filed(root), ids(9));
    assert_eq!(
        runs_of(root, "i10"),
        0,
        "no item starts after a drain pause"
    );

    let res = resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(filed(root), ids(15), "the resume sends only the rest");
}

/// Appends an event to the run journal from inside the stub, under the
/// journal's append lock, the way an `apb connector call` of the attempt
/// does. The engine tests have no `apb` binary to call through (the CLI
/// suite drives unknown sends through a real `apb connector call`); this
/// one case needs perl for the lock and says so when it is missing.
const APPEND: &str = r#"J="$APB_RUN_DIR/events.jsonl"
command -v perl >/dev/null || { echo 'this test needs perl' 1>&2; exit 99; }
add() { perl -e 'use Fcntl qw(:flock); open(my $w, ">>", $ARGV[0]) or die; flock($w, LOCK_EX) or die; open(my $r, "<", $ARGV[0]) or die; my $s = -1; while (<$r>) { $s = $1 if /^\{"seq":(\d+),/ } printf $w "{\"seq\":%d,\"ts\":1,%s}\n", $s + 1, $ARGV[1]; close $w;' "$J" "$1"; }
unknown_send() { add "\"type\":\"send_intent_recorded\",\"intent\":\"si-$I\",\"nonce\":\"n-$I\",\"node\":\"work\",\"item\":\"$I\",\"attempt\":$APB_ATTEMPT,\"connector\":\"smtp\",\"function\":\"send_email\",\"account\":\"main\",\"mode\":\"$1\",\"label\":\"mail\",\"pid\":0,\"deadline_ms\":0"; add "\"type\":\"send_intent_settled\",\"intent\":\"si-$I\",\"outcome\":\"unknown\",\"source\":\"response\",\"detail\":\"timeout\""; }
"#;

/// A supervised run driven on a thread the test joins, so nothing of the
/// drive outlives the test's environment: declared after the test's `Env`,
/// it drops first, and a test that panics before [`Supervised::finish`]
/// aborts the run and joins the driver on the way out.
struct Supervised {
    id: String,
    run_dir: std::path::PathBuf,
    driver: Option<std::thread::JoinHandle<()>>,
}

impl Supervised {
    /// Waits until the run reads `want`, then joins the driver.
    fn finish(mut self, root: &Path, want: RunStatus) {
        wait_status(root, &self.id, want);
        if let Some(d) = self.driver.take() {
            d.join().expect("the driver does not panic");
        }
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if let Some(d) = self.driver.take() {
            let _ = apb_engine::control::post_control(
                &self.run_dir,
                Control::Abort {
                    reason: "the test ended early".into(),
                    by: None,
                },
            );
            let _ = d.join();
        }
    }
}

fn supervised(root: &Path) -> Supervised {
    let prepared = prepare_supervised_background(
        root,
        "lst",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..RunOptions::default()
        },
    )
    .unwrap();
    let id = prepared.run_id().to_string();
    let run_dir = root.join(".apb/runs").join(&id);
    let root = root.to_path_buf();
    let driver = std::thread::spawn(move || {
        let _ = drive_prepared(&root, prepared);
    });
    Supervised {
        id,
        run_dir,
        driver: Some(driver),
    }
}

/// The `n`-th failed `node_finished` of `work` and the wake after it.
fn wait_failed(root: &Path, id: &str, n: usize) {
    poll_until(&format!("failure {n} of work and its wake"), || {
        let ev = read_all(&root.join(".apb/runs").join(id)).ok()?;
        let failed = work_finishes(&ev).iter().filter(|s| *s == "failed").count();
        let wakes = ev
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::WakeRaised { node, .. } if node == "work"),
            )
            .count();
        (failed >= n && wakes >= n).then_some(())
    })
}

fn retry(root: &Path, id: &str) {
    post_supervisor_command(
        root,
        id,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();
}

fn wait_status(root: &Path, id: &str, want: RunStatus) {
    poll_until(&format!("run status {want:?}"), || {
        let ev = read_all(&root.join(".apb/runs").join(id)).ok()?;
        (RunState::fold(&ev).run_status == want).then_some(())
    })
}

/// A `none` send with an unknown outcome: the item needs a look and is not
/// resent by a supervisor retry. Once a person settles the send, either
/// way, the next retry runs the item again: a send confirmed by a person
/// does not pass an item that never finished its work and check.
#[test]
fn an_unknown_none_send_is_never_resent_until_a_person_resolves_it() {
    for sent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let list = format!(
            "items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\non_needs_look: fail\n{CHECK}"
        );
        seed(root, &playbook(&list, "", ""), &[]);
        let hook = format!(
            "{APPEND}if [ \"$I\" = b ] && [ \"$N\" = 1 ]; then unknown_send none; echo 'send timed out' 1>&2; exit 1; fi"
        );
        let _env = Env::set(&stub(root, &hook), &[]);
        let run = supervised(root);
        let id = run.id.clone();
        wait_failed(root, &id, 1);
        let b = last_state(&events(root, &id), "b");
        assert_eq!(
            (b.state.as_str(), b.reason.as_deref()),
            ("needs_look", Some("unknown_send"))
        );

        retry(root, &id);
        wait_failed(root, &id, 2);
        assert_eq!(
            runs_of(root, "b"),
            1,
            "an unresolved unknown send is not resent"
        );

        let run_dir = root.join(".apb/runs").join(&id);
        apb_engine::connector::intent::resolve_by_person(&run_dir, "si-b", sent, None, "tester")
            .unwrap();
        retry(root, &id);
        run.finish(root, RunStatus::Succeeded);
        assert_eq!(runs_of(root, "b"), 2, "a settled send runs the item again");
        assert_eq!(runs_of(root, "a"), 1, "the passed item is kept");
        let b = last_state(&events(root, &id), "b");
        assert_eq!(
            (b.state.as_str(), b.note),
            ("passed", None),
            "its check passed it (resolved as sent: {sent})"
        );
    }
}

/// A supervisor retry of a failed list keeps the passed items and runs the
/// ones that need a look again.
#[test]
fn a_supervisor_retry_reruns_only_the_items_that_need_a_look() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\non_needs_look: fail\n{CHECK}"
    );
    seed(root, &playbook(&list, "", ""), &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    crate::list_run_test::plan(root, "b", Some(1), "1");
    let run = supervised(root);
    let id = run.id.clone();
    wait_failed(root, &id, 1);
    retry(root, &id);
    run.finish(root, RunStatus::Succeeded);
    assert_eq!((runs_of(root, "a"), runs_of(root, "b")), (1, 2));
    let ev = events(root, &id);
    assert!(
        ev.iter()
            .any(|e| matches!(&e.payload, EventPayload::ListStarted { reopened: true, .. }))
    );
    assert_eq!(last_state(&ev, "b").state, "passed");
}

/// A loop visit after a visit a supervisor retry reopened and finished is
/// a fresh visit: under `rerun: all` every item runs again.
#[test]
fn a_loop_visit_after_a_retried_visit_starts_afresh() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: [ {{ id: a }}, {{ id: b }} ] }}\nbatch: {{ size: 2, parallel: 1, retries: 0 }}\non_needs_look: fail\n{CHECK}"
    );
    seed(root, &crate::list_run_test::looped(&list), &[]);
    let _env = Env::set(&stub(root, ""), &[]);
    crate::list_run_test::plan(root, "b", Some(1), "1");
    let run = supervised(root);
    let id = run.id.clone();
    wait_failed(root, &id, 1);
    retry(root, &id);
    run.finish(root, RunStatus::Succeeded);
    assert_eq!(
        (runs_of(root, "a"), runs_of(root, "b")),
        (2, 3),
        "the loop visit ran both items again"
    );
    let ev = events(root, &id);
    let fresh = ev
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                EventPayload::ListStarted {
                    reopened: false,
                    ..
                }
            )
        })
        .count();
    assert_eq!(fresh, 2);
}

/// A quota failure of one item: the other items in flight finish, the node
/// gets no `node_finished`, and the resume continues the visit with only
/// that item.
#[test]
fn a_quota_pause_of_one_item_drains_the_rest_and_the_resume_continues() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cfg = tempfile::tempdir().unwrap();
    let list = format!(
        "items: {{ inline: [ {{ id: a }}, {{ id: b }}, {{ id: c }} ] }}\nbatch: {{ size: 3, parallel: 3, retries: 0 }}\n{CHECK}"
    );
    seed(root, &playbook(&list, "", ""), &[]);
    let quota =
        "429 FreeUsageLimitError: Rate limit exceeded. Please try again later. Retry-After: 1";
    let hook = format!("if [ \"$I\" = b ] && [ \"$N\" = 1 ]; then echo '{quota}' 1>&2; exit 1; fi");
    let cfg_path = cfg.path().to_string_lossy().into_owned();
    let _env = Env::set(
        &stub(root, &hook),
        &[
            ("APB_QUOTA_BACKOFF_MS", "600000,1200000"),
            ("APB_CONFIG_DIR", &cfg_path),
        ],
    );
    apb_core::run_origin::ensure_key().unwrap();
    let res = run(root, "lst", None, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Paused);
    let ev = events(root, &res.run_id);
    assert!(work_finishes(&ev).is_empty(), "no node_finished");
    assert_eq!(last_state(&ev, "b").state, "interrupted");
    assert_eq!(last_state(&ev, "a").state, "passed");
    assert_eq!(last_state(&ev, "c").state, "passed");

    let res = resume(root, &res.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        (runs_of(root, "a"), runs_of(root, "b"), runs_of(root, "c")),
        (1, 2, 1)
    );
}

/// A crash leaves an item attempt open: the next drive reaps it under its
/// item, and the resume runs only that item again.
#[test]
fn a_crashed_item_attempt_is_reaped_under_its_item_and_rerun_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let list = format!(
        "items: {{ inline: [ {{ id: a }}, {{ id: b }}, {{ id: c }} ] }}\nbatch: {{ size: 1, parallel: 1, retries: 0 }}\n{CHECK}"
    );
    seed(root, &playbook(&list, "", ""), &[]);
    let hook = r#"if [ "$I" = b ]; then while [ -f "$D/gate" ]; do sleep 0.05; done; fi"#;
    let _env = Env::set(&stub(root, hook), &[]);
    fs::write(stub_dir(root).join("gate"), "").unwrap();
    let rx = spawn(root);
    let id = run_id(root);
    poll_until("item b to start", || {
        stub_dir(root).join("started-b").is_file().then_some(())
    });
    request_pause(root, &id, &operator(PauseMode::Drain)).unwrap();
    fs::remove_file(stub_dir(root).join("gate")).unwrap();
    assert_eq!(joined(&rx).outcome, RunStatus::Paused);
    assert_eq!(runs_of(root, "c"), 0);

    // What a driver that died mid-item leaves behind: c admitted and its
    // attempt open under a pid no process can hold.
    let pid = u32::MAX;
    let run_dir = root.join(".apb/runs").join(&id);
    let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
    log.append(EventPayload::ListBatchStarted {
        node: "work".into(),
        batch: 9,
        items: vec!["c".into()],
    })
    .unwrap();
    log.append(EventPayload::AttemptStarted {
        node: "work".into(),
        item: Some("c".into()),
        attempt: 1,
        agent: "claude".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(pid),
        spawn_ms: None,
        model: None,
        workdir: None,
        transcript: None,
        token: None,
        isolation: None,
    })
    .unwrap();
    drop(log);

    let res = resume(root, &id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &id);
    let reaped = ev.iter().any(|e| {
        matches!(&e.payload,
            EventPayload::AttemptFinished { item: Some(i), status, .. }
                if i == "c" && status == "interrupted")
    });
    assert!(reaped, "the dead attempt is closed under item c");
    assert_eq!(
        (runs_of(root, "a"), runs_of(root, "b"), runs_of(root, "c")),
        (1, 1, 1)
    );
}
