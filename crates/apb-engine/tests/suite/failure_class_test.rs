//! Infrastructure failure classification end to end (spec 2026-08-05 section
//! 2.3, issue #71 item 2, issue #74 finding 2).
//!
//! Three properties, each bounded in wall clock:
//!   (a) a transient failure is retried on the SAME executor out of the
//!       infrastructure budget, so a node with no retries of its own still
//!       recovers, and the journal says why;
//!   (b) a budget failure is non-transient: no further attempt on this step,
//!       every later chain step on the same AGENT suppressed, a different agent
//!       still tried - and `fallback_triggered` finally names the models;
//!   (c) an abort posted while the engine is waiting out a backoff ends the run
//!       within a tick instead of at the end of the backoff.
//!
//! Every backoff here runs under `APB_INFRA_BACKOFF_MS`, so no test waits out
//! the real 5 s / 30 s policy.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::control::{Control, post_control};
use apb_engine::error::EngineError;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

use crate::common;

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(20);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn find_run_id(root: &Path, prefix: &str) -> String {
    poll_until(&format!("a run dir with prefix `{prefix}`"), || {
        let runs_dir = root.join(".apb/runs");
        if !runs_dir.is_dir() {
            return None;
        }
        fs::read_dir(&runs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|n| n.starts_with(prefix))
    })
}

fn executable(path: &Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    common::write_sync(path, body);
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// Sets `APB_AGENT_CMD` plus the backoff override for the lifetime of the
/// guard, under the one shared env lock, and restores both on drop (including
/// on panic). Same idiom as `control_liveness_test::AgentEnv`.
struct AgentEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl AgentEnv {
    fn set(prog: &str, backoff_ms: &str) -> Self {
        let lock = common::env_lock();
        unsafe {
            std::env::set_var("APB_AGENT_CMD", prog);
            std::env::set_var("APB_INFRA_BACKOFF_MS", backoff_ms);
        }
        Self { _lock: lock }
    }
}

impl Drop for AgentEnv {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_INFRA_BACKOFF_MS");
        }
    }
}

/// `max_retries` is deliberately ABSENT (so it resolves to 0): a node that is
/// allowed no retry of its own must still survive a transient infrastructure
/// failure, which is the whole point of a separate budget.
const WF: &str = r#"
schema: 1
id: PLAYBOOK_ID
name: Failure Class
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
"#;

fn seed(root: &Path, id: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF.replace("PLAYBOOK_ID", id)).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn attempt_records(run_dir: &Path, node: &str) -> Vec<(u32, String, Option<String>)> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AttemptFinished {
                node: n,
                attempt,
                status,
                failure_kind,
                ..
            } if n == node => Some((attempt, status, failure_kind)),
            _ => None,
        })
        .collect()
}

/// (a) A stub that fails twice with a transport-shaped message and then
/// succeeds. The node declares no retries, so without the infrastructure budget
/// it would fail on the first attempt.
#[cfg(unix)]
#[test]
fn a_transient_failure_is_retried_on_the_same_executor_without_a_node_retry() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "transientflow");
    common::seed_main(dir.path());

    let counter = dir.path().join("calls");
    let prog = executable(
        &dir.path().join("transient.sh"),
        &format!(
            "#!/bin/sh\nprintf x >> '{c}'\nn=$(wc -c < '{c}' | tr -d ' ')\nif [ \"$n\" -lt 3 ]; then echo 'API Error: 503 Service Unavailable' 1>&2; exit 1; fi\necho ok\n",
            c = counter.display()
        ),
    );
    let _env = AgentEnv::set(&prog, "20,20");
    let res = run(dir.path(), "transientflow", None, RunOptions::default()).unwrap();

    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "two transient failures must be absorbed by the infrastructure budget"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    let attempts = attempt_records(&run_dir, "work");
    assert_eq!(
        attempts.len(),
        3,
        "expected three attempts (two transient, one success), got {attempts:?}"
    );
    assert_eq!(
        attempts[0],
        (1, "failed".to_string(), Some("transient".to_string())),
        "the first attempt must be journaled failed and classified transient"
    );
    assert_eq!(
        attempts[1],
        (2, "failed".to_string(), Some("transient".to_string())),
        "the second attempt must be journaled failed and classified transient"
    );
    assert_eq!(attempts[2].1, "succeeded");
    assert_eq!(
        attempts[2].2, None,
        "a successful attempt carries no failure_kind"
    );

    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "an infrastructure retry must NOT consume the node's retry budget (no retry_started)"
    );
    let infra: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction {
                action,
                node,
                detail,
            } if action == "infra_retry" && node.as_deref() == Some("work") => Some(detail.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        infra.len(),
        2,
        "each infrastructure retry must be observable in the attempt timeline, got {infra:?}"
    );
    assert!(
        infra[0].contains("transient") && infra[0].contains("20"),
        "the marker must name the classification and the backoff it waited: {}",
        infra[0]
    );
}

/// (b) A budget failure. The chain is claude-code/haiku -> claude-code/sonnet ->
/// claude/opus: the middle step is a DIFFERENT model on the SAME agent, which
/// the pre-existing `(agent, model)` sameness guard happily walks into and which
/// a spend limit dooms. The third step is a different agent, so it must still be
/// tried. The stub succeeds only for the third step (it keys on the model in
/// argv, the only chain difference visible to a stub).
#[cfg(unix)]
#[test]
fn a_budget_failure_skips_same_agent_fallback_steps_and_names_the_models() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "budgetflow");
    common::seed_profile(
        dir.path(),
        "main",
        "claude-code",
        "haiku",
        &[("claude-code", "sonnet"), ("claude", "opus")],
    );

    let prog = executable(
        &dir.path().join("budget.sh"),
        "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = opus ]; then echo ok; exit 0; fi\ndone\necho 'Your credit balance is too low to access the API' 1>&2\nexit 1\n",
    );
    let _env = AgentEnv::set(&prog, "20,20");
    let res = run(dir.path(), "budgetflow", None, RunOptions::default()).unwrap();

    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a different agent may still work, so cross-agent fallback must stay allowed"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    let attempts = attempt_records(&run_dir, "work");
    assert_eq!(
        attempts.len(),
        2,
        "expected exactly two attempts (the doomed same-agent step must be skipped), got {attempts:?}"
    );
    assert_eq!(
        attempts[0],
        (1, "failed".to_string(), Some("budget".to_string())),
        "the spend-limit attempt must be journaled failed and classified budget"
    );
    assert_eq!(attempts[1].1, "succeeded");

    let fallbacks: Vec<(String, String, Option<String>, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::FallbackTriggered {
                from,
                to,
                from_model,
                to_model,
                ..
            } => Some((
                from.clone(),
                to.clone(),
                from_model.clone(),
                to_model.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        fallbacks.len(),
        1,
        "only the cross-agent fallback may be triggered, got {fallbacks:?}"
    );
    assert_eq!(
        fallbacks[0],
        (
            "claude-code".to_string(),
            "claude".to_string(),
            Some("haiku".to_string()),
            Some("opus".to_string())
        ),
        "fallback_triggered must name both models (issue #74 finding 2)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "a non-transient failure must not consume a retry on the failing step"
    );
}

/// The boundary ruling recorded in the spec section 2.2 addendum: a
/// `require_verdict` node whose attempt is killed on its deadline is labeled
/// `interrupted`, and that shape must now get the bounded infrastructure retry
/// on the SAME executor. Before Task 6 it broke straight to the fallback chain,
/// so a node with no fallbacks (this playbook) failed after exactly one attempt.
///
/// The node's own `max_retries` is absent (0), so recovery here can only come
/// from the infrastructure budget.
#[cfg(unix)]
#[test]
fn a_required_verdict_lost_to_a_deadline_kill_retries_the_same_executor() {
    const WF_VERDICT_TIMEOUT: &str = r#"
schema: 1
id: verdicttimeoutflow
name: Verdict Timeout
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do", require_verdict: true, timeout_seconds: 1 }
  - { id: done, type: finish, outcome: success }
  - { id: failed, type: finish, outcome: failure }
edges:
  - { from: start, to: work }
  - { from: work, to: done, condition: { type: node_status, node: work, equals: success } }
  - { from: work, to: failed, condition: { type: node_status, node: work, equals: failure } }
"#;
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/verdicttimeoutflow/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_VERDICT_TIMEOUT).unwrap();
    fs::write(
        dir.path().join(".apb/playbooks/verdicttimeoutflow/current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(dir.path());

    // First invocation wedges past the node's 1 s deadline and is killed without
    // writing a verdict; the second records one and exits.
    let seen = dir.path().join("seen");
    let prog = executable(
        &dir.path().join("wedge_once.sh"),
        &format!(
            "#!/bin/sh\nif [ -f '{s}' ]; then printf '{{\"status\":\"success\",\"outputs\":\"recovered\"}}' > \"$APB_STATUS_FILE\"; echo done; exit 0; fi\ntouch '{s}'\nsleep 30\n",
            s = seen.display()
        ),
    );
    let _env = AgentEnv::set(&prog, "20,20");
    let res = run(
        dir.path(),
        "verdicttimeoutflow",
        None,
        RunOptions::default(),
    )
    .unwrap();

    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "a deadline kill under require_verdict must retry the chosen executor, not give up"
    );
    let run_dir = dir.path().join(".apb/runs").join(&res.run_id);
    let attempts = attempt_records(&run_dir, "work");
    assert_eq!(
        attempts.len(),
        2,
        "expected the killed attempt plus one infrastructure retry, got {attempts:?}"
    );
    assert_eq!(
        attempts[0],
        (1, "interrupted".to_string(), Some("transient".to_string())),
        "the killed attempt keeps its interrupted label AND is classified transient"
    );
    assert_eq!(attempts[1].1, "succeeded");
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "the recovery must come from the infrastructure budget, not from a node retry"
    );
}

/// (c) An abort posted while the engine is waiting out a backoff. The backoff is
/// set to 30 s, the whole assertion is bounded at 10 s, and the abort must land
/// far under the backoff: the wait is a tick-poll loop, not a sleep.
#[cfg(unix)]
#[test]
fn an_abort_during_a_backoff_ends_the_run_promptly() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "abortbackoffflow");
    common::seed_main(dir.path());

    let prog = executable(
        &dir.path().join("always_transient.sh"),
        "#!/bin/sh\necho 'read ECONNRESET' 1>&2\nexit 1\n",
    );
    let _env = AgentEnv::set(&prog, "30000,30000");

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "abortbackoffflow", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "abortbackoffflow-");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    // Wait until the engine is genuinely inside the backoff.
    poll_until("the engine to enter an infrastructure backoff", || {
        read_all(&run_dir).ok()?.into_iter().find(|e| {
            matches!(&e.payload, EventPayload::SupervisorAction { action, .. }
                if action == "infra_retry")
        })
    });

    let posted = Instant::now();
    post_control(
        &run_dir,
        Control::Abort {
            reason: "stop during backoff".into(),
        },
    )
    .unwrap();
    let res = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("the drive must return after the abort")
        .unwrap();
    let landed = posted.elapsed();

    assert_eq!(res.outcome, RunStatus::Aborted);
    assert!(
        landed < Duration::from_secs(5),
        "the abort took {landed:?} to land, i.e. the backoff is a blocking sleep rather than a tick-poll loop"
    );
}
