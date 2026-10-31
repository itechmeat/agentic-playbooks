//! Bounded agent output: an attempt keeps a bounded head and tail of what the
//! agent printed and capped log files, records what it lost on its
//! `attempt_finished`, and fails when the agent's final record itself was
//! cut. The limits come from the `output_limits` section of the global
//! config; a project config may only lower them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use apb_core::registry::init_project;
use apb_engine::capture::{AttemptOutputLoss, INCOMPLETE_RESULT};
use apb_engine::event::{EventPayload, OutputLoss, read_all};
use apb_engine::run_view::RunView;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const PLAYBOOK: &str = r#"
schema: 1
id: big
name: Big output
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do" }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: w }
  - { from: w, to: ok, condition: { type: node_status, node: w, equals: success } }
  - { from: w, to: no, fallback: true }
"#;

/// The tiny machine limits every test here runs under.
const GLOBAL_LIMITS: &str =
    "output_limits:\n  head_bytes: 1024\n  tail_bytes: 4096\n  log_file_bytes: 2048\n";

/// claude's final JSON result, with a success report and token usage.
fn final_record(reply_padding: usize) -> String {
    format!(
        r#"{{"type":"result","is_error":false,"result":"done{}\n\n```yaml\nstatus: success\nsummary: ok\n```","session_id":"s-1","usage":{{"input_tokens":5,"output_tokens":7}}}}"#,
        "y".repeat(reply_padding)
    )
}

/// An agent that prints `filler_lines` lines of 100 bytes and then `last`.
fn write_agent(root: &Path, filler_lines: usize, last: &str) -> String {
    let path = root.join("big-agent.sh");
    let line = "x".repeat(99);
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\ni=0\nwhile [ $i -lt {filler_lines} ]; do echo '{line}'; i=$((i+1)); done\ncat <<'EOF'\n{last}\nEOF\n"
        ),
    );
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

struct Fixture {
    _root: tempfile::TempDir,
    _cfg: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
    root: std::path::PathBuf,
}

impl Fixture {
    /// A project with the playbook, the global limits, and `project_config`
    /// as its `.apb/config.yaml` when given.
    fn new(project_config: Option<&str>) -> Self {
        let lock = common::env_lock();
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        fs::write(cfg.path().join("config.yaml"), GLOBAL_LIMITS).unwrap();
        init_project(root.path()).unwrap();
        let dir = root.path().join(".apb/playbooks/big/1.0.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), PLAYBOOK).unwrap();
        fs::write(root.path().join(".apb/playbooks/big/current"), "1.0.0").unwrap();
        common::seed_main(root.path());
        if let Some(c) = project_config {
            fs::write(root.path().join(".apb/config.yaml"), c).unwrap();
        }
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        Fixture {
            root: root.path().to_path_buf(),
            _root: root,
            _cfg: cfg,
            _lock: lock,
        }
    }

    /// Runs the playbook under `agent`.
    fn run(&self, agent: &str) -> Ran {
        unsafe { std::env::set_var("APB_AGENT_CMD", agent) };
        let res = run(&self.root, "big", None, RunOptions::default());
        unsafe { std::env::remove_var("APB_AGENT_CMD") };
        let res = res.unwrap();
        let run_dir = self.root.join(".apb/runs").join(&res.run_id);
        let events = read_all(&run_dir).unwrap();
        let find = |pick: &dyn Fn(&EventPayload) -> bool| {
            events
                .iter()
                .find(|e| pick(&e.payload))
                .map(|e| e.payload.clone())
                .unwrap()
        };
        let finished = find(&|p| matches!(p, EventPayload::AttemptFinished { .. }));
        let EventPayload::NodeFinished { output, .. } =
            find(&|p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "w"))
        else {
            unreachable!()
        };
        Ran {
            outcome: res.outcome,
            finished,
            node_output: output,
            log: fs::read_to_string(run_dir.join("attempts/w-1/stdout.log")).unwrap(),
            losses: RunView::load(&run_dir, &res.run_id)
                .unwrap()
                .output_losses(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("APB_CONFIG_DIR") };
    }
}

/// What one run of the playbook left behind.
struct Ran {
    outcome: RunStatus,
    /// The attempt's `attempt_finished`.
    finished: EventPayload,
    /// The node's journaled output.
    node_output: String,
    /// The attempt's `stdout.log`.
    log: String,
    /// The run view's attempts that lost output.
    losses: Vec<AttemptOutputLoss>,
}

fn stdout_loss(finished: &EventPayload) -> &OutputLoss {
    let EventPayload::AttemptFinished {
        output_truncated, ..
    } = finished
    else {
        unreachable!()
    };
    output_truncated
        .iter()
        .find(|l| l.stream == "stdout")
        .expect("a stdout loss")
}

#[test]
fn output_over_the_limits_with_a_whole_final_record_succeeds_and_names_the_loss() {
    let fx = Fixture::new(None);
    let agent = write_agent(&fx.root, 200, &final_record(0));
    let ran = fx.run(&agent);

    assert_eq!(ran.outcome, RunStatus::Succeeded);
    let EventPayload::AttemptFinished {
        status,
        usage,
        transcript_incomplete,
        ..
    } = &ran.finished
    else {
        unreachable!()
    };
    assert_eq!(status, "succeeded");
    assert!(!transcript_incomplete);
    // The final record was read from the tail: its usage is recorded.
    assert_eq!(usage.as_ref().map(|u| u.output_tokens), Some(7));
    let loss = stdout_loss(&ran.finished);
    assert_eq!(loss.kept_bytes, 1024 + 4096);
    assert!(loss.dropped_bytes_at_least >= 200 * 100 - 5120);
    // The run view reports the same loss.
    assert_eq!(ran.losses.len(), 1);
    assert_eq!(&ran.losses[0].truncated[0], loss);
    // The log file holds its cap and one marker line, nothing more.
    let (kept, rest) = ran.log.split_at(2048);
    assert!(kept.bytes().all(|b| b == b'x' || b == b'\n'));
    assert!(
        rest.trim()
            .starts_with("[apb] output limit reached: 2048 bytes written"),
        "{rest}"
    );
}

#[test]
fn a_final_record_too_long_to_keep_fails_the_attempt_as_incomplete() {
    let fx = Fixture::new(None);
    // The record alone is longer than the tail, so no copy holds it whole.
    let agent = write_agent(&fx.root, 200, &final_record(8_000));
    let ran = fx.run(&agent);

    assert_eq!(ran.outcome, RunStatus::Failed);
    let EventPayload::AttemptFinished {
        status,
        transcript_incomplete,
        failure_kind,
        usage,
        ..
    } = &ran.finished
    else {
        unreachable!()
    };
    assert_eq!(status, "failed");
    assert!(transcript_incomplete);
    // Neither a quota nor a transport problem.
    assert_eq!(failure_kind.as_deref(), Some("agent"));
    assert_eq!(*usage, None);
    stdout_loss(&ran.finished);
    assert!(ran.losses[0].transcript_incomplete);
    assert!(
        ran.node_output.contains(INCOMPLETE_RESULT),
        "{}",
        ran.node_output
    );
}

#[test]
fn a_project_config_lowers_a_limit_but_cannot_raise_one() {
    let fx = Fixture::new(Some(
        "output_limits:\n  log_file_bytes: 100000000\n  head_bytes: 512\n",
    ));
    let agent = write_agent(&fx.root, 200, &final_record(0));
    let ran = fx.run(&agent);

    assert_eq!(ran.outcome, RunStatus::Succeeded);
    // The raise is ignored: the log is still capped at the machine's 2048.
    assert!(
        ran.log[2048..]
            .trim()
            .starts_with("[apb] output limit reached")
    );
    // The lowering applies: the head is 512 bytes.
    assert_eq!(stdout_loss(&ran.finished).kept_bytes, 512 + 4096);
}
