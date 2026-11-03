use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::MutexGuard;

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

use crate::common;

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
            std::env::remove_var("APB_PAUSE_GRACE_MS");
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

#[test]
fn finish_with_prompt_stores_answer_as_output() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();

    init_project(proj.path()).unwrap();
    common::seed_profile(proj.path(), "writer", "claude", "haiku", &[]);
    let src = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults:\n  profile: writer\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success, prompt: \"compose the answer\" }\nedges:\n  - { from: s, to: f }\n";
    let vdir = proj.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), src).unwrap();
    fs::write(proj.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

    unsafe {
        // Sleep so the spawn-time attempt_started and the return-time
        // attempt_finished land in distinct milliseconds.
        std::env::set_var(
            "APB_AGENT_CMD",
            make_stub(bin.path(), "sleep 0.05\necho FINAL_ANSWER"),
        );
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    assert_eq!(
        state.outputs.get("f").map(|s| s.as_str()),
        Some("FINAL_ANSWER")
    );

    // The finish-answer path now journals its attempt at spawn time too: the
    // attempt_started carries the child pid and precedes attempt_finished (which
    // carries duration_ms) with a distinct, earlier timestamp.
    use apb_engine::event::EventPayload;
    let started = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "f"))
        .expect("attempt_started for finish node f");
    let finished = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::AttemptFinished { node, .. } if node == "f"))
        .expect("attempt_finished for finish node f");
    assert!(
        started.seq < finished.seq && started.ts < finished.ts,
        "finish attempt_started must precede attempt_finished (seq {} ts {} vs seq {} ts {})",
        started.seq,
        started.ts,
        finished.seq,
        finished.ts
    );
    let EventPayload::AttemptStarted { pid, .. } = &started.payload else {
        unreachable!("matched AttemptStarted above")
    };
    assert!(
        pid.is_some(),
        "finish attempt_started.pid must be Some at spawn"
    );
    let EventPayload::AttemptFinished { duration_ms, .. } = &finished.payload else {
        unreachable!("matched AttemptFinished above")
    };
    assert!(
        duration_ms.is_some(),
        "finish attempt_finished.duration_ms must be Some"
    );
}

// --- PR 21a: the composer's chain, its stops and its journal ---

use apb_engine::event::{Event, EventPayload, read_all};

/// A project whose finish node `f` composes with profile `writer`:
/// claude haiku, then the fallback claude opus.
fn seed_composer(root: &Path) {
    seed_composer_with(root, "");
}

/// [`seed_composer`] with `extra` added to the finish node's fields.
fn seed_composer_with(root: &Path, extra: &str) {
    init_project(root).unwrap();
    common::seed_profile(root, "writer", "claude", "haiku", &[("claude", "opus")]);
    let src = format!(
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults:\n  profile: writer\nnodes:\n  - {{ id: s, type: start }}\n  - {{ id: f, type: finish, outcome: success, prompt: \"compose the answer\"{extra} }}\nedges:\n  - {{ from: s, to: f }}\n"
    );
    let vdir = root.join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}

fn composer_env(stub: &str, home: &Path, cfg: &Path) {
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var("HOME", home);
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
}

fn starts_of(
    events: &[Event],
) -> Vec<(
    u64,
    Option<String>,
    apb_engine::execution_identity::ExecStep,
)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AttemptStarted {
                node, model, exec, ..
            } if node == "f" => Some((e.seq, model.clone(), exec.clone().expect("composer exec"))),
            _ => None,
        })
        .collect()
}

fn transitions(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::FallbackTriggered { .. }))
        .count()
}

fn finished_f(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "f"))
        .count()
}

/// Design 2.5 item 3: a CLI composer attempt journals the model apb asked
/// for and its chain position, and the fallback names the close that moved
/// the walk.
#[test]
fn the_closing_answer_journals_its_model_and_chain_position() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, home, cfg, bin) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    seed_composer(proj.path());
    composer_env(
        &make_stub(
            bin.path(),
            "for a in \"$@\"; do\n  if [ \"$a\" = opus ]; then echo ANSWER; exit 0; fi\ndone\necho boom 1>&2\nexit 1",
        ),
        home.path(),
        cfg.path(),
    );
    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&proj.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let starts = starts_of(&events);
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].1.as_deref(), Some("haiku"), "requested by apb");
    assert_eq!(starts[1].1.as_deref(), Some("opus"));
    assert_eq!(starts[0].2.step, 0);
    assert_eq!(starts[0].2.executed_by, "cli");
    assert_eq!(starts[0].2.entered, None);
    assert_eq!(starts[1].2.step, 1);
    assert_eq!(starts[0].2.binding, starts[1].2.binding);
    let entered = starts[1].2.entered.clone().expect("the fallback's entry");
    assert_eq!(entered.from_step, 0);
    assert_eq!(entered.closed_attempt, starts[0].0);
    assert_eq!(entered.reason.as_str(), "failed");
    let fb = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::FallbackTriggered {
                reason,
                executed_by,
                from,
                to,
                ..
            } => Some((
                reason.clone(),
                executed_by.clone(),
                from.clone(),
                to.clone(),
            )),
            _ => None,
        })
        .expect("one transition");
    assert_eq!(
        fb.0.map(|r| r.as_str().to_string()).as_deref(),
        Some("failed")
    );
    assert_eq!(fb.1.as_deref(), Some("cli"));
    assert_eq!((fb.2.as_str(), fb.3.as_str()), ("claude", "claude"));
}

/// The composer's stub: sleeps while `slow` exists, else answers.
fn slow_composer(bin: &Path) -> String {
    let d = bin.display();
    make_stub(
        bin,
        &format!(
            "touch '{d}/composing'\nwhile [ -f '{d}/slow' ]; do sleep 0.05; done\necho ANSWER"
        ),
    )
}

fn spawn_run(root: &Path) -> std::sync::mpsc::Receiver<apb_engine::scheduler::RunResult> {
    let (tx, rx) = std::sync::mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        if let Ok(r) = run(&root, "p", None, RunOptions::default()) {
            let _ = tx.send(r);
        }
    });
    rx
}

fn wait_for(path: &Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !path.is_file() {
        assert!(
            std::time::Instant::now() < deadline,
            "{path:?} never appeared"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    String::new()
}

fn run_id_of(root: &Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(id) = fs::read_dir(root.join(".apb/runs"))
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
        {
            return id;
        }
        assert!(std::time::Instant::now() < deadline, "no run dir");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Card A4 and plan row A11: a `--now` pause while the closing answer is
/// composed ends the composer's walk. No transition, no `node_finished`,
/// so no result is settled; the resume composes again from the first step
/// and settles once.
#[test]
fn paused_composer_resumes_and_snapshots_once() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let _l = lock();
    let _g = EnvGuard;
    let (proj, home, cfg, bin) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let root = proj.path();
    seed_composer_with(root, ", results: [ { path: summary.md } ]");
    composer_env(&slow_composer(bin.path()), home.path(), cfg.path());
    unsafe { std::env::set_var("APB_PAUSE_GRACE_MS", "200") };
    fs::write(bin.path().join("slow"), "").unwrap();
    let rx = spawn_run(root);
    wait_for(&bin.path().join("composing"));
    let run_id = run_id_of(root);
    request_pause(
        root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Now,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the drive returns");
    unsafe { std::env::remove_var("APB_PAUSE_GRACE_MS") };
    assert_eq!(res.outcome, RunStatus::Paused);
    let run_dir = root.join(".apb/runs").join(&run_id);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(transitions(&events), 0, "no transition after a pause");
    assert_eq!(
        finished_f(&events),
        0,
        "no node_finished for a paused composer"
    );
    assert_eq!(starts_of(&events).len(), 1, "no fallback attempt");

    fs::remove_file(bin.path().join("slow")).unwrap();
    let res = apb_engine::scheduler::resume(root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let starts = starts_of(&events);
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].2.step, 0, "the resume composes on the first step");
    assert_eq!(starts[1].2.entered, None);
    assert_eq!(transitions(&events), 0);
    assert_eq!(finished_f(&events), 1, "the resumed composer settles once");
    let stamps: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, result, .. } if node == "f" => result.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(stamps.len(), 1, "one snapshot: {stamps:?}");
    assert_eq!(
        fs::read_dir(run_dir.join("results/snapshots"))
            .unwrap()
            .count(),
        1,
        "the pause stored no snapshot"
    );
}

/// Card A4: an abort while the closing answer is composed ends the walk:
/// no transition, no fallback attempt, no `node_finished`.
#[test]
fn an_abort_during_the_closing_answer_moves_on_to_no_fallback() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, home, cfg, bin) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let root = proj.path();
    seed_composer(root);
    composer_env(&slow_composer(bin.path()), home.path(), cfg.path());
    fs::write(bin.path().join("slow"), "").unwrap();
    let rx = spawn_run(root);
    wait_for(&bin.path().join("composing"));
    let run_id = run_id_of(root);
    apb_engine::stop::stop_run(root, &run_id).unwrap();
    let res = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the drive returns");
    fs::remove_file(bin.path().join("slow")).unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
    let events = read_all(&root.join(".apb/runs").join(&run_id)).unwrap();
    assert_eq!(transitions(&events), 0, "no transition after an abort");
    assert_eq!(starts_of(&events).len(), 1, "no fallback attempt");
    assert_eq!(finished_f(&events), 0);
}
