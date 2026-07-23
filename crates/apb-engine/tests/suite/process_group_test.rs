use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use apb_engine::adapter::{AgentAdapter, AgentTask, ClaudeAdapter, ErrorClass};
use apb_engine::invocation::builtin;

// An agent that spawns a background descendant (grandchild), records its pid in
// the working directory, and sleeps. We check that the timeout tears down the WHOLE
// process group, not just the direct child: the grandchild must be dead after kill.
fn write_spawning_agent(dir: &Path) -> String {
    let path = dir.join("spawner.sh");
    // sleep 30 & - a background grandchild; its pid goes to grandchild.pid; then the agent itself sleeps.
    fs::write(
        &path,
        "#!/bin/sh\nsleep 30 &\necho $! > grandchild.pid\nsleep 30\n",
    )
    .unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn pid_alive(pid: &str) -> bool {
    // kill -0 <pid>: code 0 if the process is alive, nonzero otherwise.
    Command::new("kill")
        .arg("-0")
        .arg(pid)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn assert_grandchild_dead(work: &Path) {
    let pid = fs::read_to_string(work.join("grandchild.pid"))
        .expect("agent should have recorded a grandchild pid");
    let pid = pid.trim();
    assert!(!pid.is_empty(), "empty grandchild pid");

    // Give SIGKILL time to propagate through the group and get reaped (the grandchild
    // becomes an orphan adopted by init/launchd). Poll for up to ~1s.
    let started = Instant::now();
    let mut alive = true;
    while started.elapsed() < Duration::from_secs(1) {
        if !pid_alive(pid) {
            alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !alive,
        "grandchild pid {pid} survived tear-down: process tree not killed"
    );
}

fn agent_task<'a>(
    work: &'a Path,
    timeout: Option<Duration>,
    policy: &'a apb_engine::adapter::ConnectorEnvPolicy,
) -> AgentTask<'a> {
    AgentTask {
        prompt: "go",
        model: "haiku",
        workdir: work,
        timeout,
        stream_log: None,
        soul: None,
        grant_autonomy: false,
        connector_policy: policy,
        interactive: false,
        node: "test",
        agent: "claude",
    }
}

#[test]
fn timeout_kills_whole_process_tree() {
    let bindir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let prog = write_spawning_agent(bindir.path());
    let ad = ClaudeAdapter {
        program: prog,
        spec: builtin("claude").unwrap(),
    };
    let policy = Default::default();

    let err = ad
        .run(&agent_task(
            work.path(),
            Some(Duration::from_secs(1)),
            &policy,
        ))
        .unwrap_err();
    assert!(matches!(err.0, ErrorClass::Timeout), "got: {err:?}");
    assert_grandchild_dead(work.path());
}

/// Issue 45 finding 11: cancel and timeout share `kill_process_tree`. A
/// mid-run cancel flag must tear down the whole attempt process group the same
/// way a node timeout does, so no grandchild of the cancelled attempt survives.
#[test]
fn cancel_kills_whole_process_tree() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    let bindir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let prog = write_spawning_agent(bindir.path());
    let ad = ClaudeAdapter {
        program: prog,
        spec: builtin("claude").unwrap(),
    };
    let policy = Default::default();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::clone(&cancel);
    let pid_file = work.path().join("grandchild.pid");

    // Wait until the agent has written grandchild.pid, then cancel. Without
    // that barrier a fast cancel could race the spawn and the assertion would
    // have no pid to check.
    thread::spawn(move || {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if pid_file.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        cancel_flag.store(true, Ordering::Relaxed);
    });

    let err = ad
        .run_cancellable(
            &agent_task(work.path(), None, &policy),
            &cancel,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(err.0, ErrorClass::Transport) && err.1.contains("cancelled"),
        "got: {err:?}"
    );
    assert_grandchild_dead(work.path());
}
