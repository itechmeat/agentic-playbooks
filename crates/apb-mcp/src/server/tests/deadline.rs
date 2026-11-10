//! The run deadline on MCP `run_resume`: a run past its deadline resumes
//! only with `deadline` (more time from now), a plain resume is refused
//! `deadline_passed`, and a duration that does not parse is refused
//! `deadline_invalid` before anything else.

use super::*;

const SLOW: &str = "schema: 2\nid: dl\nname: Deadline\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: script, script: \"scripts/s.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: done }\n";

/// A run of `dl` its 500 ms deadline ended during a slow script.
fn expired_run(root: &Path) -> String {
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/dl/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), SLOW).unwrap();
    fs::write(vdir.join("scripts/s.sh"), "sleep 3\necho ok\n").unwrap();
    fs::write(root.join(".apb/playbooks/dl/current"), "1.0.0").unwrap();
    let res = apb_engine::scheduler::run(
        root,
        "dl",
        None,
        apb_engine::RunOptions {
            deadline_ms: Some(500),
            ..apb_engine::RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(res.outcome, apb_engine::state::RunStatus::Aborted);
    res.run_id
}

fn resume_args(run_id: &str, deadline: Option<&str>) -> RunResumeArgs {
    RunResumeArgs {
        deadline: deadline.map(str::to_string),
        run_id: run_id.into(),
        from_node: Some("s".into()),
        retry_now: false,
        node: None,
        allow_environment_drift: false,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        workspace: None,
        budget: None,
        confirm_budget: None,
        launch_version: None,
        launch_choice: None,
        confirm_launch: None,
        external_data: None,
    }
}

#[test]
fn run_resume_deadline_grants_time() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let run_id = expired_run(proj.path());
    let _reaper = driver_reaper::DriverReaper::new(proj.path(), &run_id);
    let server = WfMcp::new(proj.path().to_path_buf());
    let call = |deadline: Option<&str>| {
        result_text(&server.run_resume_for(resume_args(&run_id, deadline), None))
    };
    let plain = call(None);
    assert!(plain.contains("deadline_passed"), "{plain}");
    let bad: serde_json::Value = serde_json::from_str(&call(Some("soon"))).unwrap();
    assert_eq!(bad["policy"], "deadline_invalid", "{bad}");
    let granted: serde_json::Value = serde_json::from_str(&call(Some("10m"))).unwrap();
    assert_eq!(granted["detached"], true, "{granted}");
}
