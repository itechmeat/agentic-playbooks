//! Billed use on the MCP surface (GUARDRAILS.md "Billed use"): a start that
//! would run billed agent work is refused with the `confirm_launch` start
//! action and its nonce, the host's retry with that nonce after asking the
//! person runs and records the confirmation, and a run step never
//! confirms. codex's basis comes from `auth.json` in `$CODEX_HOME`; a stub
//! stands in for the binary.

use super::*;

/// Sets variables for the test's life, under `CROSS_ENV_LOCK`, and puts
/// the prior values back.
struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Env {
    fn set(vars: &[(&'static str, Option<&std::ffi::OsStr>)]) -> Self {
        let mut prior = Vec::new();
        for (k, v) in vars {
            prior.push((*k, std::env::var_os(k)));
            // SAFETY: under CROSS_ENV_LOCK, like every env mutation here.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        Env(prior)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in self.0.drain(..).rev() {
            // SAFETY: under CROSS_ENV_LOCK.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// A project with playbook `quick` whose `work` node runs the codex
/// profile `worker`, a codex home holding an api key login, and the stub.
fn seed(root: &Path, codex: &Path) {
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/quick/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: quick\nname: Quick\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, profile: worker, prompt: go }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/quick/current"), "1.0.0").unwrap();
    let prof = root.join(".apb/profiles/worker");
    fs::create_dir_all(&prof).unwrap();
    fs::write(
        prof.join("profile.yaml"),
        "name: worker\nexecutor:\n  agent: codex\n  model: gpt-6-sol\n",
    )
    .unwrap();
    fs::write(prof.join("SOUL.md"), "").unwrap();
    fs::write(codex.join("auth.json"), r#"{"OPENAI_API_KEY":"k"}"#).unwrap();
    fs::write(
        root.join("reply.jsonl"),
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}\n",
    )
    .unwrap();
    let stub = root.join("agent.sh");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
            root.join("reply.jsonl").display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
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
    }
}

fn json_of(res: &CallToolResult) -> Value {
    let text = result_text(res);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

#[tokio::test]
async fn billed_work_starts_only_with_the_nonce_the_host_relays_and_never_from_a_step() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    seed(proj.path(), codex.path());
    let stub = proj.path().join("agent.sh");
    let _env = Env::set(&[
        ("APB_TEST_BILLING_DEFAULT", None),
        ("CODEX_API_KEY", None),
        ("OPENAI_API_KEY", None),
        ("OPENAI_BASE_URL", None),
        ("CODEX_HOME", Some(codex.path().as_os_str())),
        ("APB_AGENT_CMD", Some(stub.as_os_str())),
    ]);
    let server = WfMcp::new(proj.path().to_path_buf());

    let out = json_of(&server.playbook_run(Parameters(start(None))).await);
    let refusal = out["policy_refusal"].clone();
    assert_eq!(refusal["policy"], "billing_requires_confirmation", "{out}");
    assert_eq!(
        refusal["billing"]["held"],
        json!(["codex: billed"]),
        "{out}"
    );
    let action = out["start_actions"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["code"] == "confirm_launch"))
        .unwrap_or_else(|| panic!("a confirm_launch action: {out}"));
    assert!(
        action["reason"]
            .as_str()
            .is_some_and(|w| w.contains("billed")),
        "{action}"
    );
    let runs = |root: &Path| {
        fs::read_dir(root.join(".apb/runs"))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    assert_eq!(runs(proj.path()), 0, "nothing started");

    for marker in ["APB_RUN_ID", "APB_HOST_TASK"] {
        let _step = Env::set(&[(marker, Some(std::ffi::OsStr::new("parent-1")))]);
        let res = json_of(&server.playbook_run(Parameters(start(Some(&refusal)))).await);
        assert_eq!(
            res["policy_refusal"]["policy"], "billing_refused_in_run_step",
            "{marker}: {res}"
        );
    }

    let ran = json_of(&server.playbook_run(Parameters(start(Some(&refusal)))).await);
    let run_id = ran["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the confirmed retry runs: {ran}"))
        .to_string();
    let status = json_of(
        &server
            .run_status(Parameters(RunRefArgs {
                run_id,
                workspace: None,
            }))
            .await,
    );
    let billing = &status["launch"]["billing"];
    assert_eq!(billing["confirmation"], "host_relayed", "{status}");
    assert_eq!(
        billing["confirmed"],
        json!([{"agent": apb_core::agents::codex::ID, "basis": "billed"}]),
        "{status}"
    );
}

/// A login whose basis apb cannot read (no codex login at all, as on a
/// fresh CI runner) is held like billed work under the default `ask`: the
/// MCP start asks the person through the host, it never proceeds silently.
#[tokio::test]
async fn an_unknown_basis_asks_before_an_mcp_start() {
    let _l = CROSS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _cfg = ConfigDirGuard::new();
    let proj = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    seed(proj.path(), codex.path());
    fs::remove_file(codex.path().join("auth.json")).unwrap();
    let stub = proj.path().join("agent.sh");
    let _env = Env::set(&[
        ("APB_TEST_BILLING_DEFAULT", None),
        ("CODEX_API_KEY", None),
        ("OPENAI_API_KEY", None),
        ("OPENAI_BASE_URL", None),
        ("CODEX_HOME", Some(codex.path().as_os_str())),
        ("APB_AGENT_CMD", Some(stub.as_os_str())),
    ]);
    let server = WfMcp::new(proj.path().to_path_buf());
    let out = json_of(&server.playbook_run(Parameters(start(None))).await);
    let refusal = &out["policy_refusal"];
    assert_eq!(refusal["policy"], "billing_requires_confirmation", "{out}");
    assert_eq!(
        refusal["billing"]["held"],
        json!(["codex: unknown"]),
        "{out}"
    );
    assert!(out["run_id"].is_null(), "nothing started: {out}");
    assert!(
        !proj.path().join(".apb/runs").is_dir()
            || fs::read_dir(proj.path().join(".apb/runs"))
                .unwrap()
                .next()
                .is_none()
    );
}
