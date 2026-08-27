use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::fsutil::atomic_write;

use crate::error::EngineError;
use crate::liveness::pid_alive;

/// How long a detached driver waits for the preparing process to finish
/// handing the workdir lock over before it reports the workdir busy. The
/// handover is a single atomic write issued right after the spawn, so this is
/// a generous bound on a sub-millisecond operation, not a real wait.
const HANDOVER_WAIT: Duration = Duration::from_secs(5);
const HANDOVER_STEP: Duration = Duration::from_millis(20);

/// Poll interval for a run sitting in the workdir queue. Far coarser than
/// `HANDOVER_STEP`, because the wait it paces is a whole other run finishing
/// (seconds to minutes), not a sub-millisecond handover.
const QUEUE_STEP: Duration = Duration::from_millis(250);

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

/// Where the workdir lock lives. `pub(crate)` so `run_doctor` can report the
/// lock holder without a second copy of the path convention.
pub(crate) fn lock_path(root: &Path) -> PathBuf {
    root.join(".apb/workdir.lock")
}

pub(crate) fn lock_holder(path: &Path) -> Option<u32> {
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
    acquire_handover_within(root, HANDOVER_WAIT)
}

/// `acquire_handover` with a caller-chosen ceiling, never shorter than
/// `HANDOVER_WAIT`. A detached driver of a QUEUED run has two waits stacked on
/// top of each other: the handover race with its parent, and the unrelated
/// write-run that made the run queue in the first place. Both are the same
/// poll, so they get one deadline rather than two.
pub fn acquire_handover_within(
    root: &Path,
    wait: Duration,
) -> Result<Option<WorkdirGuard>, EngineError> {
    wait_for_workdir(
        root,
        wait.max(HANDOVER_WAIT),
        HANDOVER_STEP,
        LockWait::Handover,
    )
}

/// Waits for an unrelated write-run to release the workdir, up to `wait`.
///
/// This is what turns "the workdir is busy" from a refusal into a queue: a run
/// that was ADMITTED (its directory, snapshot and parameters are already on
/// disk) parks here until the holder finishes, instead of the start being
/// rejected and the caller's event evaporating with it. `stopped` is polled on
/// every miss so a queued run can still be cancelled while it waits.
///
/// Deliberately NOT the handover poll: `acquire_handover` adopts a lock file
/// that names this process, which is right for a detached driver (one run per
/// process) and wrong here (a server process drives several runs on threads,
/// so "our pid" is a lock some other run of ours is holding).
pub fn acquire_queued(
    root: &Path,
    wait: Duration,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Option<WorkdirGuard>, EngineError> {
    wait_for_workdir(root, wait, QUEUE_STEP, LockWait::Queue { stopped })
}

/// Which of the two waits `wait_for_workdir` is performing. They differ in
/// whether a lock naming this process is ours to adopt, and in what a
/// give-up reads like.
enum LockWait<'a> {
    /// A detached driver adopting the lock its parent rewrote to its pid.
    Handover,
    /// An admitted run waiting out another write-run.
    Queue {
        stopped: &'a mut dyn FnMut() -> bool,
    },
}

/// The poll shared by both waits: retry `acquire` every `step` until it
/// succeeds, `wait` elapses, or (queue only) the run is stopped.
fn wait_for_workdir(
    root: &Path,
    wait: Duration,
    step: Duration,
    mut mode: LockWait<'_>,
) -> Result<Option<WorkdirGuard>, EngineError> {
    let lock_path = lock_path(root);
    let deadline = Instant::now() + wait;
    loop {
        if matches!(mode, LockWait::Handover) && lock_holder(&lock_path) == Some(std::process::id())
        {
            return Ok(Some(WorkdirGuard {
                lock_path,
                armed: true,
            }));
        }
        match acquire(root, false) {
            Err(EngineError::WorkdirBusy(msg)) => {
                if let LockWait::Queue { stopped } = &mut mode
                    && (*stopped)()
                {
                    return Err(EngineError::WorkdirBusy(format!(
                        "{msg}; the queued run was stopped before the workdir freed"
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(match &mode {
                        LockWait::Handover => EngineError::WorkdirBusy(msg),
                        LockWait::Queue { .. } => EngineError::WorkdirBusy(format!(
                            "{msg}; gave up after {}s in the workdir queue",
                            wait.as_secs()
                        )),
                    });
                }
                std::thread::sleep(step);
            }
            other => return other,
        }
    }
}
