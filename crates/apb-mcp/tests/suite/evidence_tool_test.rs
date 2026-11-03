//! `run_evidence_read` in host mode: the host task's prompt and environment
//! carry the attempt's evidence session, the host reads through the tool,
//! and the claims it writes are checked against what it read. A read after
//! the attempt ended is refused.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::execution::{ExecutionMode, ExecutionRequest};
use apb_core::registry::init_project;
use apb_core::step_env::StepCaller;
use apb_engine::evidence::Verdict;
use apb_engine::host_task::{self, PendingHostTask, SubmitRequest, SubmitStatus};
use apb_engine::scheduler::{RunOptions, run_background};
use apb_engine::state::{RunState, RunStatus};
use apb_mcp::tools::evidence::{ReadArgs, run_evidence_read};
use serde_json::{Value, json};

const PLAYBOOK: &str = r#"schema: 2
id: ev
name: ev
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: collect, type: script, script: scripts/collect.sh, runner: sh, results: [ { path: mail.jsonl, format: records } ] }
  - { id: read, type: agent_task, prompt: "Summarize the mail.", evidence: { from: [collect], claims: required } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: collect }
  - { from: collect, to: read }
  - { from: read, to: done }
"#;

const MAIL: &str = r#"{"apb_records": 1, "complete": true, "source": "inbox"}
{"id": "m1", "kind": "mail", "text": "The invoice for March was paid on time by Acme."}
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let v = root.join(".apb/playbooks/ev/1.0.0");
    fs::create_dir_all(v.join("scripts")).unwrap();
    fs::write(v.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(
        v.join("scripts/collect.sh"),
        format!("cat > mail.jsonl <<'APB_EOF'\n{MAIL}APB_EOF\necho ok\n"),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/ev/current"), "1.0.0").unwrap();
    let p = root.join(".apb/profiles/main");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(p.join("SOUL.md"), "").unwrap();
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn args(op: &str) -> ReadArgs {
    ReadArgs {
        op: op.into(),
        ..ReadArgs::default()
    }
}

#[test]
fn a_host_task_reads_its_session_through_the_tool_and_its_claims_are_checked() {
    let _sandbox = crate::common::config_sandbox();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let run_id = run_background(
        root,
        "ev",
        None,
        RunOptions {
            execution: ExecutionRequest {
                mode: Some(ExecutionMode::Host),
                host_session: true,
                client: Some("test-host".into()),
                inherited: false,
            },
            ..Default::default()
        },
    )
    .unwrap();
    let task: PendingHostTask = poll("the host task of `read`", || {
        host_task::pending_for_run(root, &run_id)
            .ok()?
            .into_iter()
            .find(|t| t.node == "read")
    });
    let session = task
        .env
        .get(apb_engine::evidence::SESSION_ENV)
        .expect("the session in the task's environment")
        .clone();
    assert!(task.prompt.contains("## evidence"), "{}", task.prompt);
    assert!(
        task.prompt.contains(&session),
        "the prompt names the session"
    );
    assert!(task.prompt.contains("run_evidence_read"));

    let step = StepCaller {
        run_id: Some(run_id.clone()),
        host_task: None,
    };
    let outline = run_evidence_read(root, &session, &args("outline"), &step).unwrap();
    let view = outline["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ref"].as_str().unwrap().trim_start_matches("n:"))
        .find(|v| v.contains("mail.jsonl"))
        .expect("the package view")
        .to_string();
    let page = run_evidence_read(
        root,
        &session,
        &ReadArgs {
            view: Some(view),
            ..args("page")
        },
        &step,
    )
    .unwrap();
    let m1 = page["items"][0]["ref"].as_str().unwrap().to_string();
    assert_eq!(page["items"][0]["citable"], true);
    assert_eq!(page["session"], json!(session));
    assert!(page["read_chars"].as_u64().unwrap() > 0);
    // (A step of another run is refused is owned by the engine boundary,
    // `a_closed_session_and_another_runs_step_are_refused`.)

    let status_file = task.env.get("APB_STATUS_FILE").unwrap();
    fs::write(
        status_file,
        json!({"status": "success", "outputs": {"claims": [
            {"id": "c1", "text": "The March invoice was paid on time.", "cites": [{"ref": m1, "span": [4, 30]}]}
        ]}})
        .to_string(),
    )
    .unwrap();
    host_task::submit_to_run(
        root,
        &run_id,
        SubmitRequest {
            task_id: task.task_id.clone(),
            status: SubmitStatus::Succeeded,
            output: "The March invoice was paid.\n\n```yaml\nstatus: success\nsummary: ok\n```"
                .into(),
            usage: None,
            note: None,
            submitted_by: "host".into(),
            client: Some("test-host".into()),
            model: None,
            findings: None,
            warnings: None,
        },
    )
    .unwrap();
    let run_dir = root.join(".apb/runs").join(&run_id);
    let status = poll("the run to end", || {
        let events = apb_engine::event::read_all(&run_dir).ok()?;
        let st = RunState::fold(&events).run_status;
        st.is_terminal().then_some(st)
    });
    assert_eq!(status, RunStatus::Succeeded);
    // The read after the attempt ended is refused.
    let e = run_evidence_read(root, &session, &args("outline"), &step).unwrap_err();
    assert!(e.to_string().contains("is closed"), "{e}");
    // `run_results` shows the claim with its verdict, wording and fragment.
    let results = apb_mcp::tools::results::run_results(root, &run_id, Some("read")).unwrap();
    let claim: &Value = &results["evidence"][0]["claims"][0];
    assert_eq!(
        claim["verdict"],
        json!(Verdict::StructurallyValid),
        "{results}"
    );
    assert_eq!(claim["cites"][0]["fragment"], "invoice for March was paid");
    assert_eq!(
        claim["wording"],
        apb_engine::evidence::STRUCTURALLY_VALID_WORDING
    );
}
