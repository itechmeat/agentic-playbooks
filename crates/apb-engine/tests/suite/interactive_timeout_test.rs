//! Task 5: interactive question timeout semantics (spec
//! 2026-07-20-interactive-nodes). Mirrors `interactive_reprompt_test.rs`'s
//! harness (stub agent driven by a per-invocation counter file, `RunReaper`
//! built before the first panic point).
//!
//! `question_timeout_seconds: 1` is a REAL wall-clock second, not a stub - the
//! whole point of these tests is that drive's own clock fires the timeout, so
//! faking it would test nothing. 1s is the shortest value that still reliably
//! distinguishes "timeout fired" from "timeout did not fire" against the
//! engine's 50ms `AWAIT_CONTROL_POLL` park-loop granularity; every other wait
//! in this file is a bounded poll with a deadline-naming panic message.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
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

/// A linear playbook with an interactive `ask` node carrying
/// `question_timeout_seconds: 1` and, when `default_answer` is `Some`, a
/// `default_answer` field. A fallback edge to a failure finish node (mirrors
/// `agent_timeout_test.rs`) gives a clean terminal `RunStatus` instead of an
/// engine error when the node fails outright.
fn seed_playbook(root: &Path, default_answer: Option<&str>) {
    init_project(root).unwrap();
    let default_line = match default_answer {
        Some(ans) => format!(", default_answer: \"{ans}\""),
        None => String::new(),
    };
    let src = format!(
        "schema: 1\nid: iqt\nname: IQT\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: ask, type: agent_task, prompt: \"ask something\", profile: arch, interactive: true, question_timeout_seconds: 1{default_line} }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: ask }}\n  - {{ from: ask, to: done, condition: {{ type: node_status, node: ask, equals: success }} }}\n  - {{ from: ask, to: no, fallback: true }}\n"
    );
    let dir = root.join(".apb/playbooks/iqt/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/iqt/current"), "1.0.0").unwrap();
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

fn questions_asked(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAsked { node, question, .. } if node == "ask" => {
                Some(question.clone())
            }
            _ => None,
        })
        .collect()
}

fn questions_answered(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
            } if node == "ask" => Some((answer.clone(), answered_by.clone())),
            _ => None,
        })
        .collect()
}

fn spawn_run(root: &Path) -> (mpsc::Receiver<RunStatus>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let handle = thread::spawn(move || {
        let res = run(&root, "iqt", None, RunOptions::default());
        let status = res.map(|r| r.outcome).unwrap_or(RunStatus::Failed);
        let _ = tx.send(status);
    });
    (rx, handle)
}

fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().to_string()
}

/// Invocation 1 asks one question and exits (drive parks). Any later
/// invocation (only reachable once an answer arrives, human or timeout-
/// default) finishes successfully. Bounded by construction: the stub only
/// advances across drive-controlled re-invocations, never on a timer.
fn single_ask_body(counter: &Path) -> String {
    format!(
        "c=\"{}\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Which DB?\",\"options\":[]}}'\n  exit 0\nfi\necho done\nexit 0",
        counter.display()
    )
}

#[test]
fn expiry_with_default_answer_posts_timeout_answer_and_node_succeeds() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    seed_playbook(proj.path(), Some("proceed"));
    seed_profile(proj.path(), "arch");
    set_env(
        &make_stub(bin.path(), &single_ask_body(&counter)),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (!questions_asked(&events).is_empty()).then_some(())
    });

    // Never answer it ourselves: the engine's own `question_timeout_seconds:
    // 1` clock must post the default answer through the channel.
    poll_until("waiting for timeout QuestionAnswered", || {
        let events = read_all(&run_dir).ok()?;
        (!questions_answered(&events).is_empty()).then_some(())
    });
    let events = read_all(&run_dir).unwrap();
    let answered = questions_answered(&events);
    assert_eq!(answered.len(), 1, "exactly one QuestionAnswered expected");
    assert_eq!(
        answered[0],
        ("proceed".to_string(), "timeout".to_string()),
        "the default_answer must be posted with answered_by: timeout"
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
        "the interactive node must finish after the timeout-default answer"
    );
}

#[test]
fn expiry_without_default_answer_fails_the_node() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    seed_playbook(proj.path(), None);
    seed_profile(proj.path(), "arch");
    set_env(
        &make_stub(bin.path(), &single_ask_body(&counter)),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (!questions_asked(&events).is_empty()).then_some(())
    });

    // Never answer it: with no default_answer, the timeout must fail the
    // node outright rather than park forever.
    poll_until("NodeFinished for node ask", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .any(|e| {
                matches!(
                    &e.payload,
                    EventPayload::NodeFinished { node, .. } if node == "ask"
                )
            })
            .then_some(())
    });

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Failed);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    let finished = events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node,
            status,
            output,
            ..
        } if node == "ask" => Some((status.clone(), output.clone())),
        _ => None,
    });
    let (status, output) = finished.expect("ask must have a NodeFinished event");
    assert_eq!(status, "failed");
    assert!(
        output.contains("ask"),
        "output must name the node: {output}"
    );
    assert!(
        output.contains('1'),
        "output must name the timeout seconds: {output}"
    );
    assert!(
        output.contains("default_answer"),
        "output must name the missing default_answer: {output}"
    );
    assert_eq!(
        questions_answered(&events).len(),
        0,
        "no QuestionAnswered may appear when there is no default_answer"
    );
}
