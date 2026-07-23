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
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use apb_core::fsutil::atomic_write;

/// File inside `runs/<id>` naming the OS process currently driving the run.
pub const DRIVER_PID_FILE: &str = "driver.pid";

/// File inside a parent-driven child run naming the parent run id that is
/// driving it in-process (issue #45 finding 10). Written instead of
/// `driver.pid` so doctor/status never treat the parent's process as a
/// dedicated (and therefore "stale") driver of the child.
pub const DRIVEN_BY_FILE: &str = "driven_by";

pub fn driver_pid_path(run_dir: &Path) -> PathBuf {
    run_dir.join(DRIVER_PID_FILE)
}

pub fn driven_by_path(run_dir: &Path) -> PathBuf {
    run_dir.join(DRIVEN_BY_FILE)
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

/// The parent run id recorded as driving this child in-process, or `None`
/// when the run is not currently nested under a parent drive.
pub fn read_driven_by(run_dir: &Path) -> Option<String> {
    let s = std::fs::read_to_string(driven_by_path(run_dir)).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Publishes `pid` as the process driving this run, from the process that just
/// SPAWNED that driver rather than from the driver itself.
///
/// `DriverPidGuard::claim` runs inside the child, and a full exec easily takes
/// a hundred milliseconds to get there. For that whole window `driver.pid` did
/// not exist, so `liveness::driver_is_live` reported no driver and a `stop_run`
/// landing in it took the dead-run branch: it finalized a run whose driver was
/// only just starting, and the child then executed the whole run past its own
/// terminal `RunAborted`. A caller that gets a run_id back and stops the run
/// immediately (an agent doing `playbook_run` then `run_stop`) hits exactly
/// that window. The parent already knows the child's pid - it hands the workdir
/// lock over by it - so it publishes the pid at the same point, and the child's
/// own guard simply adopts the file (it writes the identical value and still
/// removes it on a clean exit).
///
/// Best effort, like `claim`: a run that is genuinely under way must not fail
/// because its pid could not be published.
pub fn publish_driver_pid(run_dir: &Path, pid: u32) {
    let path = driver_pid_path(run_dir);
    if let Err(e) = atomic_write(&path, pid.to_string().as_bytes()) {
        eprintln!(
            "apb: warning: could not write {}: a stop issued before the driver starts may finalize this run as dead: {e}",
            path.display()
        );
    }
}

/// Owns `runs/<id>/driver.pid` for the lifetime of one drive call: written
/// (atomically, 0600) when the drive starts, removed when it returns, whatever
/// the outcome. Every drive invocation takes one - the CLI's synchronous run,
/// the in-process background thread, and the detached child alike - so the
/// file always names the process that is really doing the work.
///
/// Best effort by design: failing to publish the pid must not abort a run that
/// is otherwise fine, so write and removal errors are not fatal. They are not
/// silent either - see `claim`.
pub(crate) struct DriverPidGuard {
    path: PathBuf,
}

impl DriverPidGuard {
    /// Adopts the file when one is already there rather than requiring to
    /// create it: a detached driver's parent publishes the child's pid before
    /// the child gets here (`publish_driver_pid`), and that value is the pid
    /// this write puts back. Ownership - and so the removal on exit - moves to
    /// the drive either way.
    pub(crate) fn claim(run_dir: &Path) -> Self {
        let path = driver_pid_path(run_dir);
        if let Err(e) = atomic_write(&path, std::process::id().to_string().as_bytes()) {
            // A live drive with no driver.pid is the one state in which every
            // liveness consumer is wrong in the dangerous direction:
            // `stop_run` sees no driver and finalizes a run that is still
            // working, and `doctor --run` reports no drive in progress. The
            // failure must not abort the run, but an operator staring at a
            // spurious finalize needs to be able to correlate it with its
            // cause. apb-engine has no tracing facility, so this is an
            // eprintln, matching `stop::abort_children` and the progress
            // warnings.
            eprintln!(
                "apb: warning: could not write {}: this drive is invisible to liveness checks and may be finalized as dead: {e}",
                path.display()
            );
        }
        Self { path }
    }
}

impl Drop for DriverPidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Owns `runs/<id>/driven_by` for the lifetime of a nested in-process drive
/// of a sub-playbook child (issue #45 finding 10).
///
/// A child run is driven on the same OS process as its parent; publishing
/// that process as `driver.pid` of the child makes doctor/status treat the
/// parent's detached argv (`__drive-run --run-id <parent>`) as a stale claim
/// for the child. Instead we record the parent run id and let liveness follow
/// the parent's drive.
///
/// Also clears any leftover `driver.pid` so a transitional file from an older
/// build cannot keep failing the child as "stale pid file".
pub(crate) struct NestedDriveGuard {
    driven_by: PathBuf,
    driver_pid: PathBuf,
}

impl NestedDriveGuard {
    pub(crate) fn claim(run_dir: &Path, parent_run_id: &str) -> Self {
        let driven_by = driven_by_path(run_dir);
        let driver_pid = driver_pid_path(run_dir);
        // Drop a misleading parent-process claim if one is present.
        let _ = std::fs::remove_file(&driver_pid);
        if let Err(e) = atomic_write(&driven_by, parent_run_id.as_bytes()) {
            eprintln!(
                "apb: warning: could not write {}: this nested child is invisible to parent-driven liveness checks: {e}",
                driven_by.display()
            );
        }
        Self {
            driven_by,
            driver_pid,
        }
    }
}

impl Drop for NestedDriveGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.driven_by);
        // Defensive: never leave a driver.pid behind for a nested drive.
        let _ = std::fs::remove_file(&self.driver_pid);
    }
}

/// Claim for the duration of one `drive` call: either an owned `driver.pid`
/// (top-level run) or a parent-driven `driven_by` marker (sub-playbook child).
///
/// Held only so the inner guard's `Drop` runs; nothing reads the payload.
#[allow(dead_code)]
pub(crate) enum DriveClaim {
    Owned(DriverPidGuard),
    Nested(NestedDriveGuard),
}

impl DriveClaim {
    /// Top-level runs own `driver.pid`; nested children with a parent record
    /// `driven_by` instead (issue #45 finding 10).
    pub(crate) fn claim(run_dir: &Path, parent_run: Option<&str>) -> Self {
        match parent_run {
            Some(parent) => Self::Nested(NestedDriveGuard::claim(run_dir, parent)),
            None => Self::Owned(DriverPidGuard::claim(run_dir)),
        }
    }
}

/// Re-execs this binary as `apb __drive-run ...` in a separate, detached OS
/// process and returns its pid. The child drives the run at `runs/<run_id>` to
/// completion on its own and outlives us.
///
/// `resume` selects the resume path (`--resume`, honouring `from_node` through
/// the normal resume planner) over re-opening a freshly prepared run.
/// `allow_environment_drift` is forwarded to the resume path as
/// `--allow-environment-drift` so a resume that the caller already cleared for
/// drift writes its `EnvironmentDriftAccepted` events instead of refusing.
pub fn spawn_detached_driver(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    resume: bool,
    allow_environment_drift: bool,
) -> io::Result<u32> {
    let exe = std::env::current_exe()?;
    spawn_driver_at(
        &exe,
        root,
        run_id,
        from_node,
        resume,
        allow_environment_drift,
    )
}

/// `spawn_detached_driver` against an explicitly named driver binary, for
/// callers that know where `apb` lives rather than being it. Production code
/// wants `spawn_detached_driver`; this exists because `current_exe()` inside a
/// test binary is the test harness, so the detached path can only be exercised
/// end-to-end by naming the real binary.
pub fn spawn_driver_at(
    exe: &Path,
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    resume: bool,
    allow_environment_drift: bool,
) -> io::Result<u32> {
    // The child gets an absolute root: it starts from a different working
    // directory context and must not have to guess what a relative path meant
    // to the parent.
    let root = std::fs::canonicalize(root)?;

    let mut cmd = Command::new(exe);
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
        if allow_environment_drift {
            cmd.arg("--allow-environment-drift");
        }
    }
    cmd.current_dir(&root);
    // Null stdio: the child must not hold the parent's pipes open (a chat host
    // waiting on our stdout would hang) and has nowhere to write anyway - the
    // run's own journal is its output.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The driver leads its OWN process group (pgid == its pid), the same idiom
    // as `proc::run_capture` and `adapter::spawn_in_group`. Without this it
    // inherits the launcher's group, and every way a host tears down a subtree
    // at once - `kill(-pgid)`, a closed terminal SIGHUPing its foreground
    // group - takes the driver down with the launcher. That is precisely the
    // incident this whole mechanism exists to prevent, so inheriting the
    // group would leave the run no safer than the in-process thread it
    // replaced.
    #[cfg(unix)]
    cmd.process_group(0);

    let child = cmd.spawn()?;
    let pid = child.id();
    reap_in_background(child);
    Ok(pid)
}

/// Waits on a driver handle from a throwaway thread, purely to reap it.
///
/// The driver stays our child until it exits, and an unwaited-for dead child
/// is a zombie: its pid is never released, so `kill -0` keeps succeeding for
/// it. `apb mcp` is long-lived and starts one driver per background run, so
/// those zombies would accumulate - and, far worse, a driver killed mid-run
/// would read as ALIVE for the rest of the session, which is exactly the
/// liveness signal `driver.pid` is supposed to provide (Tasks 8 and 9 decide
/// "is this run recoverable" from it). Reaping keeps that signal honest.
///
/// The thread costs a blocked `waitpid` and nothing else, and it is not what
/// keeps the run alive: if this process dies, the thread dies with it and the
/// driver is simply reparented to init, which reaps it instead.
fn reap_in_background(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}
