//! Task 4: drive parking on an interactive node's question (reprompt path).
//!
//! A stub agent prints the question marker (`<<<apb:question>>>`) followed by a
//! JSON question object and exits. Drive must park the node (no `NodeFinished`),
//! post exactly one `QuestionAsked`, raise a `WakeRaised`, and wait for an
//! answer through `answers.jsonl`; on the answer it emits `QuestionAnswered` and
//! re-invokes the node.
//!
//! Bounded by construction: the stub advances only across drive-controlled
//! re-invocations (a per-invocation counter file), never on a timer, so the
//! second question can only appear after the first answer is posted. Every wait
//! is a bounded poll whose panic message names what it waited on. A `RunReaper`
//! guard, built before the first panic point, aborts and joins a still-parked
//! drive so no orphaned thread survives a failed assertion.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::question::post_answer;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(10);

fn lock() -> MutexGuard<'static, ()> {
    common::env_lock()
}

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
        }
    }
}

/// Aborts and joins a still-parked drive on drop, so a failed assertion mid-run
/// never leaves an orphaned helper thread spinning on a torn-down tempdir.
struct RunReaper {
    root: PathBuf,
    run_id: String,
    handle: Option<thread::JoinHandle<()>>,
}
impl RunReaper {
    fn join(&mut self) {
        if let Some(h) = self.handle.take() {
            h.join().expect("drive thread joined");
        }
    }
}
impl Drop for RunReaper {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // Unpark the parked drive (post an abort), then join it.
            let _ = apb_engine::run_cancel(&self.root, &self.run_id);
            let _ = h.join();
        }
    }
}

fn make_stub(dir: &Path, body: &str) -> String {
    let path = dir.join("stub.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn set_env(stub: &str, home: &Path, cfg: &Path) {
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var("HOME", home);
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
}

fn seed_profile(root: &Path, name: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    let yaml =
        format!("name: {name}\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n");
    fs::write(dir.join("profile.yaml"), yaml).unwrap();
    fs::write(dir.join("SOUL.md"), "role").unwrap();
}

fn seed_playbook(root: &Path) {
    init_project(root).unwrap();
    let src = "schema: 1\nid: iq\nname: IQ\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: ask, type: agent_task, prompt: \"ask something\", profile: arch, interactive: true }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: ask }\n  - { from: ask, to: done }\n";
    let dir = root.join(".apb/playbooks/iq/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/iq/current"), "1.0.0").unwrap();
}

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        thread::sleep(POLL_STEP);
    }
}

fn latest_run_dir(root: &Path) -> PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

fn count_kind(events: &[Event], f: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().filter(|e| f(&e.payload)).count()
}

fn questions_asked(events: &[Event]) -> Vec<(String, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAsked {
                node,
                question,
                options,
            } if node == "ask" => Some((question.clone(), options.clone())),
            _ => None,
        })
        .collect()
}

fn spawn_run(root: &Path) -> (mpsc::Receiver<RunStatus>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let handle = thread::spawn(move || {
        let res = run(&root, "iq", None, RunOptions::default());
        let status = res.map(|r| r.outcome).unwrap_or(RunStatus::Failed);
        let _ = tx.send(status);
    });
    (rx, handle)
}

fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().to_string()
}

#[test]
fn ask_answer_finish_single_suspension() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    seed_playbook(proj.path());
    seed_profile(proj.path(), "arch");
    // Invocation 1: emit the marker plus a question and exit (drive parks).
    // Any later invocation (only reachable after an answer arrives) finishes.
    let body = format!(
        "c=\"{}\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Which DB?\",\"options\":[\"pg\",\"sqlite\"]}}'\n  exit 0\nfi\necho done\nexit 0",
        counter.display()
    );
    set_env(&make_stub(bin.path(), &body), home.path(), cfg.path());

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    // Drive parks: exactly one QuestionAsked for `ask`, and no NodeFinished yet.
    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (!questions_asked(&events).is_empty()).then_some(())
    });
    let events = read_all(&run_dir).unwrap();
    let asked = questions_asked(&events);
    assert_eq!(asked.len(), 1, "exactly one QuestionAsked expected");
    assert_eq!(asked[0].0, "Which DB?");
    assert_eq!(asked[0].1, vec!["pg".to_string(), "sqlite".to_string()]);
    assert_eq!(
        count_kind(
            &events,
            |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "ask")
        ),
        0,
        "the parked node must not have finished before the answer"
    );

    // Answer the question; drive must journal QuestionAnswered + a wake, then finish.
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();
    poll_until("QuestionAnswered for node ask", || {
        let events = read_all(&run_dir).ok()?;
        count_kind(
            &events,
            |p| matches!(p, EventPayload::QuestionAnswered { node, .. } if node == "ask"),
        )
        .ge(&1)
        .then_some(())
    });
    let events = read_all(&run_dir).unwrap();
    let answered: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
            } if node == "ask" => Some((answer.clone(), answered_by.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0], ("pg".to_string(), "human".to_string()));
    assert!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::WakeRaised { node, .. } if node == "ask"
        )) >= 1,
        "a wake must be raised when the question is asked"
    );

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "ask" && status == "succeeded"
        )),
        "the interactive node must finish after the answer"
    );
}

#[test]
fn two_questions_across_two_suspensions_count_based() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    seed_playbook(proj.path());
    seed_profile(proj.path(), "arch");
    // Invocation 1 asks Q1, invocation 2 (only reachable after Q1 is answered)
    // asks Q2, invocation 3 (only reachable after Q2 is answered) finishes.
    let body = format!(
        "c=\"{}\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Q1\",\"options\":[]}}'\n  exit 0\nfi\nif [ \"$n\" = \"2\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Q2\"}}'\n  exit 0\nfi\necho done\nexit 0",
        counter.display()
    );
    set_env(&make_stub(bin.path(), &body), home.path(), cfg.path());

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    // Q1 asked; Q2 must NOT be asked yet (bound by construction).
    poll_until("first QuestionAsked (Q1) for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (!questions_asked(&events).is_empty()).then_some(())
    });
    let asked = questions_asked(&read_all(&run_dir).unwrap());
    assert_eq!(asked.len(), 1, "only Q1 asked before it is answered");
    assert_eq!(asked[0].0, "Q1");

    post_answer(&run_dir, Some("ask"), "a1", "human").unwrap();

    // After answering Q1, the re-invocation asks Q2.
    poll_until("second QuestionAsked (Q2) for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (questions_asked(&events).len() >= 2).then_some(())
    });
    let asked = questions_asked(&read_all(&run_dir).unwrap());
    assert_eq!(asked.len(), 2, "exactly two questions asked");
    assert_eq!(asked[1].0, "Q2");

    post_answer(&run_dir, Some("ask"), "a2", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    // Exactly two of each, and strict interleave order: each question answered
    // before the next is asked.
    let events = read_all(&run_dir).unwrap();
    let mut ask_seqs = Vec::new();
    let mut ans_seqs = Vec::new();
    for e in &events {
        match &e.payload {
            EventPayload::QuestionAsked { node, .. } if node == "ask" => ask_seqs.push(e.seq),
            EventPayload::QuestionAnswered { node, .. } if node == "ask" => ans_seqs.push(e.seq),
            _ => {}
        }
    }
    assert_eq!(ask_seqs.len(), 2, "exactly two QuestionAsked");
    assert_eq!(ans_seqs.len(), 2, "exactly two QuestionAnswered");
    assert!(
        ask_seqs[0] < ans_seqs[0] && ans_seqs[0] < ask_seqs[1] && ask_seqs[1] < ans_seqs[1],
        "each question must be answered before the next is asked: asks={ask_seqs:?} answers={ans_seqs:?}"
    );
}
