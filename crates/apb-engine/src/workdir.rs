use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use apb_core::fsutil::atomic_write;

use crate::error::EngineError;

/// How long a detached driver waits for the preparing process to finish
/// handing the workdir lock over before it reports the workdir busy. The
/// handover is a single atomic write issued right after the spawn, so this is
/// a generous bound on a sub-millisecond operation, not a real wait.
const HANDOVER_WAIT: Duration = Duration::from_secs(5);
const HANDOVER_STEP: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub struct WorkdirGuard {
    lock_path: PathBuf,
    /// Cleared by `disarm` when ownership of the lock file passes to another
    /// process: the guard then goes away without removing the lock, so the
    /// lock never lapses between the two owners.
    armed: bool,
}

impl WorkdirGuard {
    /// Stops this guard from removing the lock file when it is dropped. Only
    /// for a handover: the caller must have already written the new owner's
    /// pid into the lock file, otherwise the lock is leaked under a pid that
    /// is not driving anything.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Passes ownership of the workdir lock to process `pid` (the freshly
    /// spawned detached driver). The lock file is rewritten in place and this
    /// guard stops owning it, so there is no window in which the workdir is
    /// unlocked and a competing write-run could slip in.
    pub fn hand_over(mut self, pid: u32) -> Result<(), EngineError> {
        atomic_write(&self.lock_path, pid.to_string().as_bytes())?;
        self.disarm();
        Ok(())
    }
}

impl Drop for WorkdirGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.lock_path);
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    // unix: `kill -0 <pid>` exits successfully if the process exists.
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(".apb/workdir.lock")
}

fn lock_holder(path: &Path) -> Option<u32> {
    if !path.is_file() {
        return None;
    }
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    match raw.trim().parse::<u32>() {
        Ok(0) | Err(_) => None,
        Ok(pid) => Some(pid),
    }
}

pub fn acquire(root: &Path, allow_shared: bool) -> Result<Option<WorkdirGuard>, EngineError> {
    if allow_shared {
        return Ok(None);
    }
    let lock_path = lock_path(root);
    if let Some(pid) = lock_holder(&lock_path)
        && pid_alive(pid)
    {
        return Err(EngineError::WorkdirBusy(format!(
            "another write-run holds the workdir (pid {pid}); use worktree or --allow-shared-workdir"
        )));
    }
    // No lock, or a stale one - overwrite it.
    atomic_write(&lock_path, std::process::id().to_string().as_bytes())?;
    Ok(Some(WorkdirGuard {
        lock_path,
        armed: true,
    }))
}

/// Lock acquisition for a detached driver process (see
/// `scheduler::drive_run_from_dir`). The process that prepared the run holds
/// the workdir lock throughout preparation and hands it over by rewriting the
/// lock file with the driver's pid right after spawning it - so the driver can
/// reach this point either before or after that write lands:
///
///   * the lock already names US: the handover completed, adopt it;
///   * the lock names a live foreign pid: most likely the parent, still a few
///     microseconds away from the handover, so retry briefly rather than
///     failing a run that was legitimately handed to us;
///   * no lock, or a stale one: acquire normally (the parent died before it
///     could hand anything over).
pub fn acquire_handover(root: &Path) -> Result<Option<WorkdirGuard>, EngineError> {
    let lock_path = lock_path(root);
    let deadline = Instant::now() + HANDOVER_WAIT;
    loop {
        if lock_holder(&lock_path) == Some(std::process::id()) {
            return Ok(Some(WorkdirGuard {
                lock_path,
                armed: true,
            }));
        }
        match acquire(root, false) {
            Err(EngineError::WorkdirBusy(msg)) => {
                if Instant::now() >= deadline {
                    return Err(EngineError::WorkdirBusy(msg));
                }
                std::thread::sleep(HANDOVER_STEP);
            }
            other => return other,
        }
    }
}
