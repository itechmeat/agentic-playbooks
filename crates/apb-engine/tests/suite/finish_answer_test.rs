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
        std::env::set_var("APB_AGENT_CMD", make_stub(bin.path(), "echo FINAL_ANSWER"));
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
}
