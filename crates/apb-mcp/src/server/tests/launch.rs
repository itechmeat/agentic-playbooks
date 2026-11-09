//! The launch policy on the MCP surface: a refused start carries the
//! `confirm_launch` or `launch_by_person` start action with its nonce and
//! fixed version and choice, a confirmed retry runs and `run_status` shows
//! the authorization, an acknowledgement of trust is no launch
//! confirmation, a run step never confirms, and an agent's save never
//! loosens the policy.

use super::*;

fn seed(root: &Path, decl: &str) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!("schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nagent_launch: {decl}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
}

fn start(confirm: Option<&Value>) -> PlaybookRunArgs {
    let arg = |k: &str| confirm.and_then(|c| c[k].as_str()).map(str::to_string);
    PlaybookRunArgs {
        id: "quick".into(),
        version: None,
        params: BTreeMap::new(),
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
        remember: None,
        notify: None,
        confirm_launch: arg("launch_nonce"),
        launch_version: arg("launch_version"),
        launch_choice: arg("launch_choice"),
        unattended: None,
        external_data: None,
    }
}

fn json_of(res: &CallToolResult) -> Value {
    let text = result_text(res);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn action<'a>(out: &'a Value, code: &str) -> Option<&'a Value> {
    out["start_actions"]
        .as_array()?
        .iter()
        .find(|a| a["code"] == code)
}

#[tokio::test]
async fn an_ask_playbook_is_refused_with_its_action_and_a_confirmed_retry_runs() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "ask");
    let server = WfMcp::new(proj.path().to_path_buf());

    // `acknowledge_untrusted` answers trust only.
    let out = json_of(&server.playbook_run(Parameters(start(None))).await);
    let refusal = &out["policy_refusal"];
    assert_eq!(refusal["policy"], "launch_requires_confirmation", "{out}");
    // MCP serializes the start actions generically: the new code reaches
    // the host by its wire name, with every argument the retry passes.
    let a = action(&out, "confirm_launch").expect("a confirm_launch action");
    let names: Vec<&str> = a["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["confirm_launch", "launch_version", "launch_choice"]);
    assert!(
        a["principals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "host")
    );

    let ran = json_of(&server.playbook_run(Parameters(start(Some(refusal)))).await);
    let run_id = ran["run_id"]
        .as_str()
        .expect("the confirmed retry runs")
        .to_string();
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id,
                workspace: None,
            }))
            .await,
    );
    assert_eq!(status["launch"]["confirmation"], "host_relayed", "{status}");
    assert_eq!(status["launch"]["policy"], "ask");
    let shown = serde_json::to_string(&status).unwrap();
    assert!(
        !shown.contains(refusal["launch_nonce"].as_str().unwrap()),
        "no surface shows a nonce"
    );
}

#[tokio::test]
async fn a_deny_playbook_is_a_persons_launch_only() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "deny");
    let server = WfMcp::new(proj.path().to_path_buf());
    let out = json_of(&server.playbook_run(Parameters(start(None))).await);
    assert_eq!(
        out["policy_refusal"]["policy"], "launch_requires_person",
        "{out}"
    );
    assert!(action(&out, "launch_by_person").is_some(), "{out}");
    assert!(action(&out, "confirm_launch").is_none());
}

#[tokio::test]
async fn a_run_step_never_confirms_a_launch() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "ask");
    let server = WfMcp::new(proj.path().to_path_buf());
    let out = json_of(&server.playbook_run(Parameters(start(None))).await);
    let refusal = out["policy_refusal"].clone();
    for marker in ["APB_RUN_ID", "APB_HOST_TASK"] {
        // SAFETY: under CROSS_ENV_LOCK, like every env mutation here.
        unsafe { std::env::set_var(marker, "parent-1") };
        let res = json_of(&server.playbook_run(Parameters(start(Some(&refusal)))).await);
        unsafe { std::env::remove_var(marker) };
        assert_eq!(
            res["policy_refusal"]["policy"], "launch_refused_in_run_step",
            "{marker}: {res}"
        );
    }
}

#[tokio::test]
async fn an_agents_save_never_loosens_the_launch_policy() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    let yaml = |decl: &str| {
        format!(
            "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\n{decl}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
        )
    };
    crate::tools::playbook_create(root, "quick", &yaml("agent_launch: deny\n")).unwrap();
    let e = crate::tools::playbook_update(root, "quick", &yaml("")).unwrap_err();
    assert!(e.to_string().contains("launch_policy_loosened"), "{e}");
    assert!(e.to_string().contains("dashboard editor"), "{e}");
    crate::tools::playbook_delete(root, "quick").unwrap();
    let e = crate::tools::playbook_create(root, "quick", &yaml("agent_launch: ask\n")).unwrap_err();
    assert!(e.to_string().contains("launch_policy_loosened"), "{e}");
}

// --- Every other MCP launch tool, through its real handler ---
//
// Each tool passes its own launch kind and the MCP initiator to the one
// launch check: an `ask` refusal carries its nonce and start actions, the
// confirmed retry runs and records how it was launched, a `deny` refusal is
// a person's only (no nonce), and a run step never confirms. The policy is
// set by the machine's `policy.yaml`, so one playbook serves every case.

/// Sets the machine's default launch policy.
fn machine_launch(policy: &str) {
    let dir = std::env::var_os("APB_CONFIG_DIR").expect("a sandboxed config dir");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        Path::new(&dir).join("policy.yaml"),
        format!("launch:\n  agent: {policy}\n"),
    )
    .unwrap();
}

/// The launch record the run's manifest holds.
fn launch_of(root: &Path, run_id: &str) -> apb_core::launch::LaunchRecord {
    apb_engine::manifest::read(&root.join(".apb/runs").join(run_id))
        .unwrap()
        .and_then(|m| m.launch)
        .unwrap_or_else(|| panic!("run `{run_id}` records its launch"))
}

/// Asserts an `ask` refusal of `kind`: its nonce, its version and choice,
/// and the `confirm_launch` start action. Returns the refusal.
fn assert_ask(out: &Value, kind: &str) -> Value {
    let refusal = out["policy_refusal"].clone();
    assert_eq!(refusal["policy"], "launch_requires_confirmation", "{out}");
    assert_eq!(refusal["launch_kind"], kind, "{out}");
    assert!(refusal["launch_nonce"].as_str().is_some(), "{out}");
    assert!(refusal["launch_version"].as_str().is_some(), "{out}");
    assert!(refusal["launch_choice"].as_str().is_some(), "{out}");
    assert!(action(out, "confirm_launch").is_some(), "{out}");
    refusal
}

/// Asserts a `deny` refusal of `kind`: a person's launch, no nonce.
fn assert_deny(out: &Value, kind: &str) {
    let refusal = &out["policy_refusal"];
    assert_eq!(refusal["policy"], "launch_requires_person", "{out}");
    assert_eq!(refusal["launch_kind"], kind, "{out}");
    assert_eq!(refusal["launch_by_person"], true, "{out}");
    assert!(refusal.get("launch_nonce").is_none(), "{out}");
    assert!(action(out, "launch_by_person").is_some(), "{out}");
    assert!(action(out, "confirm_launch").is_none(), "{out}");
}

/// Sets each run-step marker in turn around `call` and asserts the step
/// refusal of the confirmed retry.
async fn assert_step_refused<F, Fut>(what: &str, call: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Value>,
{
    for marker in ["APB_RUN_ID", "APB_HOST_TASK"] {
        // SAFETY: under CROSS_ENV_LOCK, like every env mutation here.
        unsafe { std::env::set_var(marker, "parent-1") };
        let out = call().await;
        unsafe { std::env::remove_var(marker) };
        assert_eq!(
            out["policy_refusal"]["policy"], "launch_refused_in_run_step",
            "{what} {marker}: {out}"
        );
    }
}

fn plan_args(token: &str, confirm: Option<&Value>) -> PlaybookExecutePlanArgs {
    let arg = |k: &str| confirm.and_then(|c| c[k].as_str()).map(str::to_string);
    PlaybookExecutePlanArgs {
        plan_token: token.to_string(),
        acknowledge_untrusted: None,
        confirm_irreversible: None,
        budget: None,
        confirm_launch: arg("launch_nonce"),
        launch_version: arg("launch_version"),
        launch_choice: arg("launch_choice"),
        external_data: None,
    }
}

#[tokio::test]
async fn playbook_execute_plan_goes_through_the_launch_check() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = tempfile::tempdir().unwrap();
    let (a, b, b_id) = setup_two(cfg.path());
    let server = WfMcp::new(a.path().to_path_buf());
    let token = prepared_token(&server, &b_id).await["plan_token"]
        .as_str()
        .unwrap()
        .to_string();
    let execute = |confirm: Option<Value>| {
        let args = plan_args(&token, confirm.as_ref());
        let server = &server;
        async move { json_of(&server.playbook_execute_plan(Parameters(args)).await) }
    };

    machine_launch("deny");
    assert_deny(&execute(None).await, "execute_plan");

    machine_launch("ask");
    let refusal = assert_ask(&execute(None).await, "execute_plan");
    assert_step_refused("playbook_execute_plan", || execute(Some(refusal.clone()))).await;
    // A refusal never burns the plan: the confirmed retry runs in B.
    let out = execute(Some(refusal.clone())).await;
    let run_id = out["run_ref"]["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the confirmed retry runs: {out}"))
        .to_string();
    let rec = launch_of(b.path(), &run_id);
    assert_eq!(rec.kind, "execute_plan");
    assert_eq!(rec.initiator, "agent_mcp");
    assert_eq!(rec.confirmation.as_deref(), Some("host_relayed"));
    assert_eq!(rec.policy, apb_core::launch::LaunchPolicy::Ask);

    // SAFETY: under CROSS_ENV_LOCK.
    unsafe { std::env::remove_var("APB_CONFIG_DIR") };
}

#[tokio::test]
async fn playbook_run_with_continued_from_goes_through_the_launch_check() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "allow");
    let server = WfMcp::new(proj.path().to_path_buf());
    // The predecessor, started while agents may launch it freely.
    let first = json_of(&server.playbook_run(Parameters(start(None))).await);
    let pred = first["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_string();
    let cont = |confirm: Option<Value>| {
        let args = PlaybookRunArgs {
            continued_from: Some(pred.clone()),
            ..start(confirm.as_ref())
        };
        let server = &server;
        async move { json_of(&server.playbook_run(Parameters(args)).await) }
    };

    machine_launch("deny");
    let out = cont(None).await;
    assert_deny(&out, "continue_from");
    assert_eq!(out["policy_refusal"]["run_id"], pred.as_str(), "{out}");

    machine_launch("ask");
    let refusal = assert_ask(&cont(None).await, "continue_from");
    assert_step_refused("playbook_run continued_from", || {
        cont(Some(refusal.clone()))
    })
    .await;
    // The nonce of a plain start does not confirm the continue.
    let plain = json_of(&server.playbook_run(Parameters(start(None))).await);
    let plain = assert_ask(&plain, "start");
    assert_eq!(
        cont(Some(plain)).await["policy_refusal"]["policy"],
        "launch_requires_confirmation"
    );
    let out = cont(Some(refusal)).await;
    let run_id = out["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the confirmed retry runs: {out}"))
        .to_string();
    let rec = launch_of(proj.path(), &run_id);
    assert_eq!(rec.kind, "continue_from");
    assert_eq!(rec.initiator, "agent_mcp");
    assert_eq!(rec.confirmation.as_deref(), Some("host_relayed"));
}

fn resume_args(run_id: &str, confirm: Option<&Value>) -> RunResumeArgs {
    let arg = |k: &str| confirm.and_then(|c| c[k].as_str()).map(str::to_string);
    RunResumeArgs {
        run_id: run_id.to_string(),
        from_node: Some("note".into()),
        retry_now: false,
        node: None,
        allow_environment_drift: false,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        workspace: None,
        budget: None,
        confirm_budget: None,
        confirm_launch: arg("launch_nonce"),
        launch_version: arg("launch_version"),
        launch_choice: arg("launch_choice"),
        external_data: None,
    }
}

#[tokio::test]
async fn run_resume_of_a_never_authorized_run_goes_through_the_launch_check() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    apb_core::run_origin::ensure_key().unwrap();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    seed_noagent_run(root);
    approve_version(root, "noagent", "1.0.0");
    // A run started past every launch surface, as an older apb did: its
    // manifest holds no authorization.
    let res = apb_engine::run(root, "noagent", None, apb_engine::RunOptions::default()).unwrap();
    let run_id = res.run_id;
    let run_dir = root.join(".apb/runs").join(&run_id);
    assert!(apb_engine::gate::launch::run_authorization(&run_dir).is_none());
    let server = WfMcp::new(root.to_path_buf());
    let resume = |confirm: Option<&Value>| {
        json_of(&server.run_resume_for(resume_args(&run_id, confirm), None))
    };

    machine_launch("deny");
    let out = resume(None);
    assert_deny(&out, "resume");
    assert_eq!(out["policy_refusal"]["run_id"], run_id.as_str(), "{out}");

    machine_launch("ask");
    let refusal = assert_ask(&resume(None), "resume");
    assert_step_refused("run_resume", || {
        let out = resume(Some(&refusal));
        async move { out }
    })
    .await;
    // The confirmed retry passes the launch check and the grant's own
    // verification before the detached driver is spawned (the driver that
    // journals the record is the real binary, proven in the CLI suite).
    let out = resume(Some(&refusal));
    assert!(
        out.get("policy_refusal").is_none(),
        "the confirmed retry resumes: {out}"
    );
    assert_eq!(out["detached"], true, "{out}");
}

fn trial_args(confirm: Option<&Value>) -> PlaybookTrialArgs {
    let arg = |k: &str| confirm.and_then(|c| c[k].as_str()).map(str::to_string);
    PlaybookTrialArgs {
        id: "quick".into(),
        version: None,
        params: BTreeMap::new(),
        instruction: None,
        scope: None,
        confirm_launch: arg("launch_nonce"),
        launch_version: arg("launch_version"),
        launch_choice: arg("launch_choice"),
        external_data: None,
    }
}

#[tokio::test]
async fn playbook_trial_goes_through_the_launch_check() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    seed(proj.path(), "allow");
    let server = WfMcp::new(proj.path().to_path_buf());
    let trial = |confirm: Option<&Value>| {
        json_of(&server.playbook_trial_for(Parameters(trial_args(confirm)), None))
    };

    machine_launch("deny");
    assert_deny(&trial(None), "trial");

    machine_launch("ask");
    let refusal = assert_ask(&trial(None), "trial");
    assert_step_refused("playbook_trial", || {
        let out = trial(Some(&refusal));
        async move { out }
    })
    .await;
    let out = trial(Some(&refusal));
    let run_id = out["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the confirmed retry runs: {out}"))
        .to_string();
    let rec = launch_of(proj.path(), &run_id);
    assert_eq!(rec.kind, "trial");
    assert_eq!(rec.initiator, "agent_mcp");
    assert_eq!(rec.confirmation.as_deref(), Some("host_relayed"));
}

/// The trial nonce covers the root's profile bundle: a SOUL changed after
/// the person was asked does not match the old nonce, and the refusal
/// carries a fresh one.
#[tokio::test]
async fn a_trial_nonce_does_not_cover_a_changed_root_profile() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nagent_launch: ask\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: x, profile: main }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: done }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "a role\n").unwrap();
    let server = WfMcp::new(root.to_path_buf());
    let trial = |confirm: Option<&Value>| {
        json_of(&server.playbook_trial_for(Parameters(trial_args(confirm)), None))
    };
    let refusal = assert_ask(&trial(None), "trial");
    fs::write(pdir.join("SOUL.md"), "a changed role\n").unwrap();
    let again = assert_ask(&trial(Some(&refusal)), "trial");
    assert_ne!(
        again["launch_nonce"], refusal["launch_nonce"],
        "a fresh nonce"
    );
}
