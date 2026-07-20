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
