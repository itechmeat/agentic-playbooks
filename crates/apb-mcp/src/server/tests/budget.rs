//! The run budget on the playbook tools: a start whose budget asks and has
//! unestimated nodes is refused with `budget_needs_decision`, a decided one
//! starts and answers with its estimate and budget, and the plan of another
//! workspace and `playbook_get(estimate: true)` carry the estimate.

use super::*;

/// A playbook with one agent node, its `budget:` block and profile `main`.
const BUDGETED: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nbudget: { tokens: 1000 }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: done }\n";

fn seed_budgeted(root: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/bud/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), BUDGETED).unwrap();
    fs::write(root.join(".apb/playbooks/bud/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
}

fn start_args(budget: Option<BudgetArg>) -> PlaybookRunArgs {
    PlaybookRunArgs {
        id: "bud".into(),
        version: None,
        params: BTreeMap::new(),
        instruction: None,
        supervise: None,
        background: Some(true),
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        scope: None,
        continued_from: None,
        worktree: None,
        execution: None,
        budget,
        remember: None,
        notify: None,
    }
}

async fn start(server: &WfMcp, budget: Option<BudgetArg>) -> serde_json::Value {
    let res = server.playbook_run(Parameters(start_args(budget))).await;
    serde_json::from_str(&result_text(&res)).unwrap()
}

#[tokio::test]
async fn playbook_run_with_ask_and_unestimated_nodes_returns_budget_needs_decision() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_budgeted(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());
    let out = start(&server, None).await;
    assert_eq!(out["error"], "budget_needs_decision");
    assert_eq!(out["unestimated"], json!(["a"]));
    assert_eq!(out["options"], json!(["pause", "continue"]));
    assert_eq!(out["estimate"]["comparable"]["runs"], 0);
    assert!(
        !proj.path().join(".apb/runs").is_dir()
            || fs::read_dir(proj.path().join(".apb/runs"))
                .unwrap()
                .next()
                .is_none(),
        "a refused start creates no run"
    );
    let policy_only = start(
        &server,
        Some(BudgetArg {
            on_no_estimate: Some("sometimes".into()),
            ..BudgetArg::default()
        }),
    )
    .await;
    assert_eq!(policy_only["error"], "invalid_budget");
}

#[tokio::test]
async fn on_no_estimate_continue_starts_and_answer_carries_estimate_and_budget() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_budgeted(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());
    let out = start(
        &server,
        Some(BudgetArg {
            usd: Some(2.5),
            on_no_estimate: Some("continue".into()),
            ..BudgetArg::default()
        }),
    )
    .await;
    let run_id = out["run_id"].as_str().expect("the run started");
    let _reaper = driver_reaper::DriverReaper::new(proj.path(), run_id);
    assert_eq!(
        out["budget"]["limit"],
        json!({ "tokens": 1000, "usd": 2.5 })
    );
    assert_eq!(out["budget"]["on_no_estimate"], "continue");
    assert_eq!(out["budget"]["source"], "run");
    assert_eq!(out["estimate"]["unestimated"], json!(["a"]));
    let warnings = out["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap()
            .contains("no comparable attempt reported a cost")),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn playbook_get_estimate_true() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed_budgeted(proj.path());
    let server = WfMcp::new(proj.path().to_path_buf());
    let get = |estimate| PlaybookGetArgs {
        id: "bud".into(),
        version: None,
        workspace: None,
        detail: None,
        estimate,
    };
    let plain: serde_json::Value = serde_json::from_str(&result_text(
        &server.playbook_get(Parameters(get(None))).await,
    ))
    .unwrap();
    assert!(plain.get("estimate").is_none());
    let with: serde_json::Value = serde_json::from_str(&result_text(
        &server.playbook_get(Parameters(get(Some(true)))).await,
    ))
    .unwrap();
    let e = &with["estimate"];
    assert_eq!(e["playbook"], "bud");
    assert_eq!(e["execution"], "cli");
    assert_eq!(e["nodes"][0]["node"], "a");
    assert!(
        e["summary"]
            .as_str()
            .unwrap()
            .starts_with("estimate (0 comparable runs")
    );
}

#[tokio::test]
async fn prepare_run_plan_has_estimate() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, _b, b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let plan = prepared_token(&server, &b_id).await;
    let e = &plan["plan"]["estimate"];
    assert_eq!(e["playbook"], "noagent");
    assert_eq!(e["comparable"]["runs"], 0);
    assert_eq!(e["unestimated"], json!([]));
    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

/// A run of `bud` its unanswered `ask` paused before `a` (no history): no
/// agent ever ran.
fn ask_paused(root: &Path) -> String {
    apb_core::run_origin::ensure_key().unwrap();
    seed_budgeted(root);
    let res =
        apb_engine::scheduler::run(root, "bud", None, apb_engine::RunOptions::default()).unwrap();
    assert_eq!(res.outcome, apb_engine::state::RunStatus::Paused);
    res.run_id
}

fn resume_args(run_id: &str, confirm_budget: Option<String>) -> RunResumeArgs {
    RunResumeArgs {
        run_id: run_id.into(),
        from_node: None,
        allow_environment_drift: false,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        workspace: None,
        budget: Some(BudgetResumeArg {
            on_no_estimate: Some("continue".into()),
            ..BudgetResumeArg::default()
        }),
        confirm_budget,
    }
}

fn events_len(root: &Path, run_id: &str) -> usize {
    apb_engine::event::read_all(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .len()
}

#[test]
fn a_budget_change_over_mcp_needs_the_persons_confirmation() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let run_id = ask_paused(proj.path());
    let _reaper = driver_reaper::DriverReaper::new(proj.path(), &run_id);
    let server = WfMcp::new(proj.path().to_path_buf());
    let before = events_len(proj.path(), &run_id);
    let call = |nonce: Option<String>| -> serde_json::Value {
        let res = server.run_resume_for(resume_args(&run_id, nonce), None);
        serde_json::from_str(&result_text(&res)).unwrap()
    };
    let first = call(None);
    assert_eq!(first["error"], "budget_change_requires_confirmation");
    assert_eq!(first["change"], "on_no_estimate continue");
    let nonce = first["budget_nonce"].as_str().unwrap().to_string();
    assert_eq!(call(Some("budget-stale".into()))["error"], first["error"]);
    assert_eq!(events_len(proj.path(), &run_id), before, "nothing applied");
    // The echoed nonce passes the surface rule: the resume goes ahead.
    let confirmed = call(Some(nonce));
    assert_eq!(confirmed["detached"], true, "{confirmed}");
}

#[test]
fn a_run_a_supervisor_agent_watches_refuses_budget_changes_over_mcp() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let run_id = ask_paused(proj.path());
    let dir = proj.path().join(".apb/runs").join(&run_id);
    let mut cfg = apb_engine::run_config::read_run_config(&dir).unwrap();
    cfg.mode = apb_engine::RunMode::AgentSupervised;
    apb_engine::run_config::write_run_config(&dir, &cfg).unwrap();
    let server = WfMcp::new(proj.path().to_path_buf());
    let res = server.run_resume_for(resume_args(&run_id, None), None);
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    assert_eq!(out["error"], "budget_change_refused");
}

/// The refusal code of a budget change on run `run_id` after `damage`
/// broke its run directory.
fn refusal_after(damage: impl FnOnce(&Path)) -> serde_json::Value {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let run_id = ask_paused(proj.path());
    damage(&proj.path().join(".apb/runs").join(&run_id));
    let server = WfMcp::new(proj.path().to_path_buf());
    let res = server.run_resume_for(resume_args(&run_id, None), None);
    let out: serde_json::Value = serde_json::from_str(&result_text(&res)).unwrap();
    out["error"].clone()
}

#[test]
fn a_budget_change_is_refused_when_the_run_config_does_not_read() {
    let code = refusal_after(|dir| std::fs::write(dir.join("run.yaml"), "mode: [").unwrap());
    assert_eq!(code, "budget_change_refused");
}

#[test]
fn a_budget_change_is_refused_when_the_journal_does_not_read() {
    let code = refusal_after(|dir| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("events.jsonl"))
            .unwrap();
        f.write_all(b"not a journal line\n").unwrap();
    });
    assert_eq!(code, "budget_change_refused");
}
