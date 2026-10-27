//! The run budget through the real `apb` binary: `apb run --estimate`
//! prints the estimate and starts nothing; a run whose token budget no
//! longer covers the next node pauses, `apb wait` exits 4 with the raise
//! flags, a plain `apb resume` exits 2, and `--raise-budget-tokens`
//! finishes it. A stub agent reports 200 tokens per attempt; no model is
//! called.

use std::fs;
use std::path::Path;

use predicates::prelude::*;

use crate::common::{RunGuard, apb};

pub(crate) const PLAYBOOK: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nbudget: { tokens: 5000, on_no_estimate: continue }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";

/// Writes the playbook `bud` at `version`, profile `main` (SOUL `soul`)
/// and the stub agent; returns the stub's path.
pub(crate) fn seed(root: &Path, version: &str, soul: &str) -> String {
    if !root.join(".apb").is_dir() {
        apb().arg("init").current_dir(root).assert().success();
    }
    let vdir = root.join(".apb/playbooks/bud").join(version);
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        PLAYBOOK.replace("version: 1.0.0", &format!("version: {version}")),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/bud/current"), version).unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), soul).unwrap();
    let out = root.join("out.json");
    fs::write(
        &out,
        serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "done\n\n```yaml\nstatus: success\nsummary: done\n```",
            "usage": {"input_tokens": 150, "output_tokens": 50}
        })
        .to_string(),
    )
    .unwrap();
    let stub = root.join("agent.sh");
    if !stub.exists() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let mut f = fs::File::create(&stub).unwrap();
        write!(
            f,
            "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
            out.display()
        )
        .unwrap();
        f.sync_all().unwrap();
        drop(f);
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    stub.to_string_lossy().into_owned()
}

/// One finished run of `bud` on the stub, the history an estimate reads.
pub(crate) fn history_run(root: &Path, stub: &str) {
    apb()
        .args(["run", "bud"])
        .env("APB_AGENT_CMD", stub)
        .current_dir(root)
        .assert()
        .success();
}

fn run_ids(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[test]
fn run_estimate_flag_prints_and_creates_no_run_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    apb()
        .args(["run", "bud", "--estimate"])
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "estimate (0 comparable runs of bud 1.0.0, cli):",
        ))
        .stdout(predicate::str::contains(
            "not estimated: a (no history), b (no history)",
        ))
        .stdout(predicate::str::contains(
            "budget: 5000 tokens, on no estimate: continue",
        ));
    assert_eq!(run_ids(root), 0);
}

/// A run with a 300-token budget after one history run: `a` (200) fits,
/// `b` no longer does. Returns its id.
fn paused_run(root: &Path, stub: &str) -> String {
    history_run(root, stub);
    let out = apb()
        .args(["run", "bud", "--detach", "--budget-tokens", "300"])
        .env("APB_AGENT_CMD", stub)
        .current_dir(root)
        .assert()
        .success()
        .stderr(predicate::str::contains("budget: 300 tokens"))
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string()
}

#[test]
fn budget_tokens_pauses_and_wait_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let run_id = paused_run(root, &stub);
    let _guard = RunGuard::new(root, &run_id);
    apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .current_dir(root)
        .assert()
        .code(4)
        .stdout(predicate::str::contains("paused before `b`"))
        .stdout(predicate::str::contains("--raise-budget-tokens"));
    apb()
        .args(["runs", &run_id])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "tokens 200 of 300 spent, 100 left",
        ));
}

#[test]
fn resume_without_budget_decision_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let run_id = paused_run(root, &stub);
    let _guard = RunGuard::new(root, &run_id);
    apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .current_dir(root)
        .assert()
        .code(4);
    apb()
        .args(["resume", &run_id])
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("budget_paused"))
        .stderr(predicate::str::contains("--over-budget-once"));
}

#[test]
fn raise_budget_tokens_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let run_id = paused_run(root, &stub);
    let _guard = RunGuard::new(root, &run_id);
    apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .current_dir(root)
        .assert()
        .code(4);
    apb()
        .args(["resume", &run_id, "--raise-budget-tokens", "1k"])
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains("finished: succeeded"));
}
