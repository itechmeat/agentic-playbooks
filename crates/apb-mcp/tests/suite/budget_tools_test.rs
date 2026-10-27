//! The run budget through the MCP run tools: a run its budget paused shows
//! its budget in `run_status` and `run_wait` (with the instruction to ask the
//! person), refuses a plain `run_resume` with the structured
//! `budget_paused` answer, and resumes with a raise. A stub agent that
//! reports 200 tokens per attempt seeds one history run; no model is called.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use apb_core::budget::BudgetRequest;
use apb_engine::budget::BudgetResume;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use apb_mcp::tools;

use crate::common::env_lock;

const PLAYBOOK: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";

/// Restores the env vars this module sets, also when a test panics.
struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvGuard {
    fn set(vars: &[(&'static str, &Path)]) -> Self {
        let prior = vars
            .iter()
            .map(|(k, v)| {
                let old = std::env::var_os(k);
                // SAFETY: the caller holds the shared env lock.
                unsafe { std::env::set_var(k, v) };
                (*k, old)
            })
            .collect();
        EnvGuard(prior)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            // SAFETY: still under the env lock.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

struct Fixture {
    proj: tempfile::TempDir,
    _cfg: tempfile::TempDir,
    _env: EnvGuard,
    run_id: String,
}

impl Fixture {
    fn root(&self) -> &Path {
        self.proj.path()
    }
}

fn write_exec(path: &Path, body: &str) {
    use std::io::Write;
    let mut f = fs::File::create(path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f.sync_all().unwrap();
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

/// One history run, then a run with a 300-token budget that pauses before
/// `b` (each node spends 200).
fn budget_paused() -> Fixture {
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let root = proj.path();
    let out = root.join("out.json");
    fs::write(
        &out,
        serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "done\n\n```yaml\nstatus: success\nsummary: done\n```",
            "total_cost_usd": 0.01,
            "usage": {"input_tokens": 150, "output_tokens": 50}
        })
        .to_string(),
    )
    .unwrap();
    let agent = root.join("agent.sh");
    write_exec(
        &agent,
        &format!(
            "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
            out.display()
        ),
    );
    let env = EnvGuard::set(&[("APB_AGENT_CMD", &agent), ("APB_CONFIG_DIR", cfg.path())]);
    apb_core::run_origin::ensure_key().unwrap();
    apb_core::registry::init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/bud/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    fs::write(root.join(".apb/playbooks/bud/current"), "1.0.0").unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    let history = run(root, "bud", None, RunOptions::default()).unwrap();
    assert_eq!(history.outcome, RunStatus::Succeeded);
    let paused = run(
        root,
        "bud",
        None,
        RunOptions {
            budget: Some(BudgetRequest {
                tokens: Some(300),
                ..BudgetRequest::default()
            }),
            ..RunOptions::default()
        },
    )
    .unwrap();
    assert_eq!(paused.outcome, RunStatus::Paused);
    Fixture {
        proj,
        _cfg: cfg,
        _env: env,
        run_id: paused.run_id,
    }
}

#[test]
fn run_status_has_budget_block() {
    let _l = env_lock();
    let f = budget_paused();
    let out = tools::run_status(f.root(), &f.run_id).unwrap();
    let budget = &out["budget"];
    assert_eq!(budget["limit"]["tokens"], 300);
    assert_eq!(budget["tokens"]["spent"], 200.0);
    assert_eq!(budget["tokens"]["remaining"], 100.0);
    assert_eq!(budget["paused"]["node"], "b");
    assert_eq!(out["pause"]["cause"], "budget");
    assert_eq!(out["estimate"]["comparable"]["runs"], 1);
    assert!(
        out["estimate"]["summary"]
            .as_str()
            .unwrap()
            .contains("tokens")
    );
    assert!(out["estimate"].get("nodes").is_none());
}

#[test]
fn run_wait_on_budget_pause_carries_instruction() {
    let _l = env_lock();
    let f = budget_paused();
    let mut waiter = apb_engine::run_wait::RunWaiter::new(f.root(), &f.run_id).unwrap();
    let res = waiter.wait(Duration::from_secs(5)).unwrap();
    let out = tools::run_wait_result(f.root(), &f.run_id, &res).unwrap();
    assert_eq!(out["reason"], "stopped");
    assert_eq!(out["next"], tools::budget::BUDGET_PAUSE_NEXT);
    assert_eq!(out["budget"]["paused"]["node"], "b");
}

/// The tool layer under `run_resume`: a plain resume of a budget pause is
/// refused with the structured `budget_paused` answer, and a resume with a
/// raise passes the budget preflight and hands the run to a detached driver,
/// which journals the raise next to its `run_resumed` (the engine's tests
/// cover that journal; a person's confirmation is the server's rule, see
/// `server::tests::budget`).
#[test]
fn run_resume_refuses_a_plain_resume_and_detaches_one_with_a_raise() {
    let _l = env_lock();
    let f = budget_paused();
    let _reaper = crate::driver_reaper::DriverReaper::new(f.root(), &f.run_id);
    let plain = tools::run_resume(f.root(), &f.run_id, None, false, None).unwrap();
    assert_eq!(plain["error"], "budget_paused");
    assert_eq!(plain["budget"]["paused"]["node"], "b");
    assert!(plain["detail"].as_str().unwrap().contains("`b`"));

    let raise = BudgetResume {
        raise_tokens: Some(1000),
        ..BudgetResume::default()
    };
    let ack = tools::run_resume(f.root(), &f.run_id, None, false, Some(&raise)).unwrap();
    assert_eq!(ack["detached"], true, "{ack}");
    let sets = read_all(&f.root().join(".apb/runs").join(&f.run_id))
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.payload, EventPayload::BudgetSet { .. }))
        .count();
    assert_eq!(sets, 1, "the preflight journals nothing");
}
