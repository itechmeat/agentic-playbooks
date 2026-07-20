//! Detached run drivers: the OS process that actually drives a run, and the
//! `runs/<id>/driver.pid` file that names it.
//!
//! A run started from a chat session used to be driven on a thread inside the
//! process that started it (`apb mcp`). When that process died - a host
//! session killed under memory pressure, a closed terminal - it took every
//! in-flight agent attempt with it. The fix is the same trick the CLI's
//! `apb run --supervise` already used: re-exec our own binary as a separate,
//! fully detached process which re-opens the prepared run from `runs/<id>` and
//! drives it alone. The parent is then free to exit at any moment.
//!
//! Splitting the spawn helper out into the engine (rather than leaving it in
//! `apb-cli`) is what lets `apb-mcp` use it too: mcp cannot depend on cli, but
//! both depend on the engine.
//!
//! Anti-TOCTOU note: the child re-opens an ALREADY prepared run. The policy
//! gate, permit verification and the immutable manifest snapshot all happen in
//! the parent before the spawn, and the child reads that snapshot - it never
//! re-resolves live profile or skill files, so the posture is unchanged.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use apb_core::fsutil::atomic_write;

/// File inside `runs/<id>` naming the OS process currently driving the run.
pub const DRIVER_PID_FILE: &str = "driver.pid";

pub fn driver_pid_path(run_dir: &Path) -> PathBuf {
    run_dir.join(DRIVER_PID_FILE)
}

/// The pid recorded as driving this run, or `None` when no drive is in
/// progress (the file is written when a drive starts and removed when it
/// ends). A pid here is a claim, not proof of life: callers that need
/// liveness check the pid themselves.
pub fn read_driver_pid(run_dir: &Path) -> Option<u32> {
    std::fs::read_to_string(driver_pid_path(run_dir))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Owns `runs/<id>/driver.pid` for the lifetime of one drive call: written
/// (atomically, 0600) when the drive starts, removed when it returns, whatever
/// the outcome. Every drive invocation takes one - the CLI's synchronous run,
/// the in-process background thread, and the detached child alike - so the
/// file always names the process that is really doing the work.
///
/// Best effort by design: failing to publish the pid must not abort a run that
/// is otherwise fine, so write and removal errors are ignored.
pub(crate) struct DriverPidGuard {
    path: PathBuf,
}

impl DriverPidGuard {
    pub(crate) fn claim(run_dir: &Path) -> Self {
        let path = driver_pid_path(run_dir);
        let _ = atomic_write(&path, std::process::id().to_string().as_bytes());
        Self { path }
    }
}

impl Drop for DriverPidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Re-execs this binary as `apb __drive-run ...` in a separate, detached OS
/// process and returns its pid. The child drives the run at `runs/<run_id>` to
/// completion on its own; we do not wait for it, so it is reparented to init
/// when we exit - which is the entire point.
///
/// `resume` selects the resume path (`--resume`, honouring `from_node` through
/// the normal resume planner) over re-opening a freshly prepared run.
pub fn spawn_detached_driver(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    resume: bool,
) -> io::Result<u32> {
    let exe = std::env::current_exe()?;
    // The child gets an absolute root: it starts from a different working
    // directory context and must not have to guess what a relative path meant
    // to the parent.
    let root = std::fs::canonicalize(root)?;

    let mut cmd = Command::new(&exe);
    cmd.arg("__drive-run")
        .arg("--root")
        .arg(&root)
        .arg("--run-id")
        .arg(run_id);
    if let Some(node) = from_node {
        cmd.arg("--from-node").arg(node);
    }
    if resume {
        cmd.arg("--resume");
    }
    cmd.current_dir(&root);
    // Null stdio: the child must not hold the parent's pipes open (a chat host
    // waiting on our stdout would hang) and has nowhere to write anyway - the
    // run's own journal is its output.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd.spawn()?;
    let pid = child.id();
    // Deliberately not waited on: dropping the handle orphans the process,
    // which is what lets it outlive us (the same trick as
    // `spawn_detached_supervised` and `ClaudeAdapter::spawn_supervisor`).
    drop(child);
    Ok(pid)
}
