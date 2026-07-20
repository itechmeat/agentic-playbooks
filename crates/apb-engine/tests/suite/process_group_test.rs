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

#[test]
fn timeout_kills_whole_process_tree() {
    let bindir = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let prog = write_spawning_agent(bindir.path());
    let ad = ClaudeAdapter {
        program: prog,
        spec: builtin("claude").unwrap(),
    };

    let err = ad
        .run(&AgentTask {
            prompt: "go",
            model: "haiku",
            workdir: work.path(),
            timeout: Some(Duration::from_secs(1)),
            stream_log: None,
            soul: None,
            grant_autonomy: false,
            connector_policy: &Default::default(),
            interactive: false,
            node: "test",
            agent: "claude",
        })
        .unwrap_err();
    assert!(matches!(err.0, ErrorClass::Timeout), "got: {err:?}");

    let pid = fs::read_to_string(work.path().join("grandchild.pid"))
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
        "grandchild pid {pid} survived timeout: process tree not killed"
    );
}
