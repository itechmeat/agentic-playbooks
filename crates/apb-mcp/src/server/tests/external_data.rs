//! External data runs on the MCP start tools: a flagged `playbook_run`
//! writes the flag into the run and `run_status` and `runs_list` show it,
//! `remember` with the flag (or with an inheriting `continued_from`) is
//! refused before a run directory exists with the memory file unchanged, a
//! trial carries the flag, a resume may not change it, a step of a flagged
//! run may not read or store run memory, and the read-only server lists no
//! start tool.

use super::*;

const QUICK: &str = "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nparams:\n  - { name: customer, type: text, default: none }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

fn seed(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), QUICK).unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
}

fn args(external_data: Option<bool>, remember: Option<bool>) -> PlaybookRunArgs {
    PlaybookRunArgs {
        id: "quick".into(),
        version: None,
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        instruction: None,
        supervise: None,
        background: None,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        scope: None,
        continued_from: None,
        worktree: None,
        execution: None,
        budget: None,
        remember,
        notify: None,
        launch_version: None,
        launch_choice: None,
        unattended: None,
        confirm_launch: None,
        external_data,
    }
}

fn json_of(res: &CallToolResult) -> serde_json::Value {
    serde_json::from_str(&result_text(res)).unwrap()
}

fn run_count(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|rd| rd.count())
        .unwrap_or(0)
}

fn memory_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.join(".apb/memory")];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn a_flagged_start_is_flagged_and_shown_and_remember_is_refused() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    seed(root);
    let server = WfMcp::new(root.to_path_buf());

    // Remember with the flag: refused before anything is written.
    let memory = memory_files(root);
    let out = json_of(
        &server
            .playbook_run(Parameters(args(Some(true), Some(true))))
            .await,
    );
    assert_eq!(out["error"], "external_data_remember_refused", "{out}");
    assert_eq!(run_count(root), 0);
    assert_eq!(memory_files(root), memory);

    // A flagged start: the manifest and the journal carry the flag, and
    // the run surfaces show it.
    let out = json_of(
        &server
            .playbook_run(Parameters(args(Some(true), None)))
            .await,
    );
    let run_id = out["run_id"].as_str().unwrap().to_string();
    let dir = root.join(".apb/runs").join(&run_id);
    let m = apb_engine::manifest::read(&dir).unwrap().unwrap();
    assert!(m.external_data);
    assert!(apb_engine::external_data::is_flagged(&dir));
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: run_id.clone(),
                workspace: None,
            }))
            .await,
    );
    assert_eq!(status["external_data"], true, "{status}");
    assert!(
        !status["execution"].to_string().contains("+external"),
        "{status}"
    );

    // An inheriting successor that asks to remember is refused too; one
    // that clears the flag is refused as immutable.
    let mut inherit = args(None, Some(true));
    inherit.continued_from = Some(run_id.clone());
    let out = json_of(&server.playbook_run(Parameters(inherit)).await);
    assert_eq!(out["error"], "external_data_remember_refused", "{out}");
    let mut clear = args(Some(false), None);
    clear.continued_from = Some(run_id.clone());
    let out = json_of(&server.playbook_run(Parameters(clear)).await);
    assert_eq!(out["error"], "external_data_immutable", "{out}");
    assert_eq!(run_count(root), 1);
    assert_eq!(memory_files(root), memory);

    // An unflagged run carries no field on its status.
    let out = json_of(&server.playbook_run(Parameters(args(None, None))).await);
    let plain = out["run_id"].as_str().unwrap().to_string();
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id: plain,
                workspace: None,
            }))
            .await,
    );
    assert!(status.get("external_data").is_none(), "{status}");

    // A resume may repeat the run's value, never change it; the journal
    // stays as it was.
    let journal = fs::read(dir.join("events.jsonl")).unwrap();
    let resume = |flag: Option<bool>| RunResumeArgs {
        run_id: run_id.clone(),
        from_node: None,
        retry_now: false,
        node: None,
        allow_environment_drift: false,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        workspace: None,
        budget: None,
        confirm_budget: None,
        confirm_launch: None,
        launch_version: None,
        launch_choice: None,
        external_data: flag,
    };
    let out = json_of(&server.run_resume_for(resume(Some(false)), None));
    assert_eq!(out["error"], "external_data_immutable", "{out}");
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), journal);

    // A step of the flagged run may not read or store run memory.
    let step = apb_core::step_env::StepCaller {
        run_id: Some(run_id.clone()),
        ..Default::default()
    };
    let target = crate::tools::memory::MemoryTarget {
        id: "quick",
        scope: "project",
        version: None,
    };
    for action in [
        crate::tools::memory::MemoryAction::Get,
        crate::tools::memory::MemoryAction::Set(apb_core::run_memory::RememberInput {
            params: Some(BTreeMap::from([(
                "customer".to_string(),
                "acme".to_string(),
            )])),
            instruction: None,
        }),
    ] {
        let err = crate::tools::memory::playbook_memory_as(root, &target, action, &step)
            .unwrap_err()
            .to_string();
        assert!(err.contains("external_data_run"), "{err}");
        assert_eq!(memory_files(root), memory);
    }
}

#[tokio::test]
async fn a_flagged_trial_is_flagged() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    seed(root);
    let server = WfMcp::new(root.to_path_buf());
    let out = server.playbook_trial_for(
        Parameters(PlaybookTrialArgs {
            id: "quick".into(),
            version: None,
            params: BTreeMap::new(),
            instruction: None,
            scope: None,
            confirm_launch: None,
            launch_version: None,
            launch_choice: None,
            external_data: Some(true),
        }),
        None,
    );
    let out = json_of(&out);
    let run_id = out["run_id"].as_str().unwrap_or_else(|| panic!("{out}"));
    assert!(apb_engine::external_data::is_flagged(
        &root.join(".apb/runs").join(run_id)
    ));
}

/// `playbook_execute_plan` with the flag prepares a flagged run in the
/// target workspace: its manifest and its `run_started` carry the flag.
#[tokio::test]
async fn a_flagged_plan_execution_is_flagged() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, b, b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let execute = |flag: Option<bool>| {
        let server = &server;
        let b_id = &b_id;
        async move {
            let plan = prepared_token(server, b_id).await;
            let out = json_of(
                &server
                    .playbook_execute_plan(Parameters(PlaybookExecutePlanArgs {
                        plan_token: plan["plan_token"].as_str().unwrap().to_string(),
                        acknowledge_untrusted: None,
                        confirm_irreversible: None,
                        budget: None,
                        launch_version: None,
                        launch_choice: None,
                        confirm_launch: None,
                        external_data: flag,
                    }))
                    .await,
            );
            out["run_ref"]["run_id"]
                .as_str()
                .unwrap_or_else(|| panic!("{out}"))
                .to_string()
        }
    };
    let flagged = execute(Some(true)).await;
    let plain = execute(None).await;
    let runs = b.path().join(".apb/runs");
    let m = apb_engine::manifest::read(&runs.join(&flagged))
        .unwrap()
        .unwrap();
    assert!(m.external_data);
    assert!(matches!(
        apb_engine::event::read_all(&runs.join(&flagged)).unwrap()[0].payload,
        apb_engine::event::EventPayload::RunStarted {
            external_data: true,
            ..
        }
    ));
    assert!(!apb_engine::external_data::is_flagged(&runs.join(&plain)));
    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

/// The read-only server keeps refusing every start tool: none is listed.
#[test]
fn the_read_only_server_lists_no_start_tool() {
    let listed: Vec<String> = WfMcp::for_read_only(PathBuf::from("."))
        .tool_router
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for start in [
        "playbook_run",
        "playbook_trial",
        "playbook_execute_plan",
        "playbook_scenario_run",
        "run_resume",
    ] {
        assert!(!listed.iter().any(|n| n == start), "{start}");
    }
}
