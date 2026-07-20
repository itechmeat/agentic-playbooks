//! Regression tests for the adapter's bounded waits.
//!
//! The adapter is the path every agent execution goes through, and it had two
//! ways to block forever:
//!
//! 1. After the agent process exited, `wait_with_output()` (headless) and
//!    `err_reader.join()` (ACP) read the pipes to EOF - but EOF is decided by
//!    whoever still holds the write ends, not by the process we waited for. A
//!    real agent spawns MCP servers and tool subprocesses; any one of them that
//!    outlives its parent keeps those fds open.
//! 2. In the ACP path, stdout reaching EOF was treated as proof the agent had
//!    exited, and the code went straight into an unbounded `child.wait()`. An
//!    agent can close stdout and keep running.
//!
//! Every test here fails (by hanging until the harness timeout) against the
//! pre-fix adapter, and none of them assert on timing alone: each also pins
//! down the OUTCOME the adapter must produce, because "returns quickly" is
//! satisfied just as well by a wrong answer.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use apb_core::config::Transport;
use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, ErrorClass};
use apb_engine::invocation::builtin;
use apb_engine::state::NodeStatus;

use crate::common;

/// How long a leftover descendant sleeps. Far longer than any bound under
/// test, so a wait that is not bounded shows up as a hang rather than as a
/// slow pass.
const LINGER_SECS: u32 = 300;

fn stub(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

fn headless(program: String) -> ClaudeAdapter {
    ClaudeAdapter {
        program,
        spec: builtin("claude").unwrap(),
    }
}

fn acp(program: String) -> ClaudeAdapter {
    let mut spec = builtin("claude").unwrap();
    spec.transport = Transport::Acp;
    ClaudeAdapter { program, spec }
}

fn task<'a>(
    workdir: &'a Path,
    policy: &'a apb_engine::adapter::ConnectorEnvPolicy,
) -> AgentTask<'a> {
    AgentTask {
        prompt: "go",
        model: "haiku",
        workdir,
        timeout: None,
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        connector_policy: policy,
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only performs the existence/permission check.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// SIGKILLs a pid and its group, so no test can leak a 300-second `sleep`
/// whatever path it takes out of the test body.
struct Reaper(i32);

impl Drop for Reaper {
    fn drop(&mut self) {
        if self.0 > 0 {
            // SAFETY: as above; a negative pid addresses the process group.
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
}

/// Reads the pid the stub recorded, waiting briefly for the file to appear.
fn recorded_pid(path: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = fs::read_to_string(path)
            && let Ok(pid) = s.trim().parse::<i32>()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "stub never recorded its descendant pid at {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until_dead(pid: i32, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what} (pid {pid}) to die"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// --- 1. a daemonized grandchild holding the pipes -----------------------------

// The headless path. The stub backgrounds a long `sleep` - which inherits the
// agent's stdout and stderr - then prints its report and exits. The agent
// process is therefore gone while its pipes are still held open by a process
// the adapter never spawned and does not know about.
//
// Pre-fix, `child.wait_with_output()` read those pipes to EOF and blocked for
// the full LINGER_SECS. Post-fix the adapter SIGKILLs the agent's process
// group first, which is what makes EOF arrive.
//
// The correct outcome is SUCCESS, not a timeout: the agent did its job and
// printed a complete report before exiting. The leftover descendant is noise,
// and failing the node over it would turn a working agent into a flaky one.
// Reaching the drain budget at all is the abnormal case (test 2 below).
#[test]
fn headless_agent_leaving_a_grandchild_on_the_pipes_completes_promptly() {
    // APB_AGENT_EXIT_GRACE_MS / APB_AGENT_DRAIN_BUDGET_MS are process-global,
    // so a test that overrides them would otherwise change the budgets under
    // any adapter test running concurrently. All of them take the lock.
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let policy = Default::default();
    let pidfile = dir.path().join("grandchild.pid");
    let ad = headless(stub(
        dir.path(),
        "linger-headless.sh",
        &format!("sleep {LINGER_SECS} &\necho $! > grandchild.pid\necho pong"),
    ));

    let started = Instant::now();
    let report = ad.run(&task(dir.path(), &policy)).unwrap();
    let elapsed = started.elapsed();

    let gc = recorded_pid(&pidfile);
    let _reaper = Reaper(gc);

    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(
        report.summary, "pong",
        "the agent's output must still be collected in full"
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "the adapter blocked for {elapsed:?} on pipes held by a descendant; \
         LINGER_SECS is {LINGER_SECS}s, so an unbounded read shows up here"
    );
    wait_until_dead(gc, "the grandchild that inherited the agent's pipes");
}

// The ACP path has the same exposure through its stderr drain, which used to be
// an unbounded `err_reader.join()`.
#[test]
fn acp_agent_leaving_a_grandchild_on_the_pipes_completes_promptly() {
    // APB_AGENT_EXIT_GRACE_MS / APB_AGENT_DRAIN_BUDGET_MS are process-global,
    // so a test that overrides them would otherwise change the budgets under
    // any adapter test running concurrently. All of them take the lock.
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let policy = Default::default();
    let pidfile = dir.path().join("grandchild.pid");
    let ad = acp(stub(
        dir.path(),
        "linger-acp.sh",
        &format!(
            "sleep {LINGER_SECS} &\necho $! > grandchild.pid\n\
             echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done ok\"}}'"
        ),
    ));

    let started = Instant::now();
    let report = ad.run(&task(dir.path(), &policy)).unwrap();
    let elapsed = started.elapsed();

    let gc = recorded_pid(&pidfile);
    let _reaper = Reaper(gc);

    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(report.summary, "done ok");
    assert!(
        elapsed < Duration::from_secs(60),
        "the ACP stderr drain blocked for {elapsed:?} on a descendant's pipes"
    );
    wait_until_dead(gc, "the grandchild that inherited the agent's pipes");
}

// --- 2. stdout EOF is not proof the agent exited ------------------------------

// The stub streams its result, closes stdout outright (`exec 1>&-`), and only
// then does a couple of seconds of "work" before exiting cleanly.
//
// The property is that the adapter waits for the real exit. It is asserted from
// BOTH sides: the call must not return before the stub's post-EOF work is done
// (so EOF was not mistaken for exit), and it must still report success with the
// streamed result (so waiting did not cost the output).
#[test]
fn acp_stdout_eof_is_not_treated_as_the_agent_having_exited() {
    // APB_AGENT_EXIT_GRACE_MS / APB_AGENT_DRAIN_BUDGET_MS are process-global,
    // so a test that overrides them would otherwise change the budgets under
    // any adapter test running concurrently. All of them take the lock.
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let policy = Default::default();
    let ad = acp(stub(
        dir.path(),
        "eof-then-work.sh",
        "echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done ok\"}'\n\
         exec 1>&-\n\
         sleep 3\n\
         exit 0",
    ));

    let started = Instant::now();
    let report = ad.run(&task(dir.path(), &policy)).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(report.summary, "done ok");
    assert!(
        elapsed >= Duration::from_secs(2),
        "returned after only {elapsed:?}: the adapter treated stdout EOF as the \
         agent having exited instead of waiting for the process to actually end"
    );
}

// ... and when such an agent never exits, the wait is bounded and the tree is
// torn down. The grace period is lowered through APB_AGENT_EXIT_GRACE_MS so the
// test does not sit through the 5-minute production default; the default is
// deliberately generous because its job is only to make an infinite wait
// finite, never to cap honest work (an agent's working time is governed by the
// node's own timeout_seconds, inside the streaming loop this bound sits after).
#[test]
fn acp_agent_that_closes_stdout_and_never_exits_is_bounded_and_killed() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let policy = Default::default();
    let ad = acp(stub(
        dir.path(),
        "eof-then-wedge.sh",
        &format!(
            "echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done ok\"}}'\n\
             exec 1>&-\n\
             sleep {LINGER_SECS}"
        ),
    ));

    unsafe {
        std::env::set_var("APB_AGENT_EXIT_GRACE_MS", "800");
    }
    let spawned = std::sync::Mutex::new(0u32);
    let started = Instant::now();
    let err = ad
        .run_cancellable(
            &task(dir.path(), &policy),
            &AtomicBool::new(false),
            Some(&|pid| {
                *spawned.lock().unwrap() = pid;
            }),
        )
        .unwrap_err();
    let elapsed = started.elapsed();
    unsafe {
        std::env::remove_var("APB_AGENT_EXIT_GRACE_MS");
    }

    let pid = *spawned.lock().unwrap() as i32;
    let _reaper = Reaper(pid);

    assert!(
        matches!(err.0, ErrorClass::Timeout),
        "a wedged agent must fail as a timeout, got {err:?}"
    );
    assert!(
        err.1.contains("stdout"),
        "the error must say what the adapter was waiting for, got: {}",
        err.1
    );
    assert!(
        elapsed < Duration::from_secs(60),
        "the wait was not bounded: {elapsed:?} against an 800ms grace period"
    );
    wait_until_dead(pid, "the wedged agent, whose tree must be killed");
}
