use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::fsutil::atomic_write_under;

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
        write_lock(&self.lock_path, pid)?;
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

/// Where the workdir lock of the execution root lives. `pub(crate)` so
/// `run_doctor` can report the lock holder without a second copy of the path
/// convention.
pub(crate) fn lock_path(root: &Path) -> PathBuf {
    root.join(".apb/workdir.lock")
}

/// Where the busy lock of the tree a run works in lives (issue #67 item 8).
///
/// The lock belongs to the checkout the tree is part of, not to the exact
/// directory: a tree in the execution root's own checkout (the root itself or
/// any directory in it) shares the historical `.apb/workdir.lock`, because its
/// files are the root's files, while a separate git worktree gets its own lock
/// under the execution root's `.apb/locks/`, named by a digest of its
/// canonical top level. So two runs over different git worktrees of one
/// project never contend, and two runs whose trees overlap always do. The
/// lock lives in the execution root, so a worktree never grows an `.apb` of
/// its own. Outside git, a directory inside the execution root counts as the
/// root and any other directory as its own tree.
pub fn tree_lock_path(root: &Path, tree: Option<&Path>) -> PathBuf {
    let Some(tree) = tree else {
        return lock_path(root);
    };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let (root_c, tree_c) = (canon(root), canon(tree));
    let identity = match (
        git_path(&tree_c, "--show-toplevel"),
        git_path(&root_c, "--show-toplevel"),
    ) {
        (Some(tree_top), Some(root_top)) if tree_top == root_top => root_c.clone(),
        (Some(tree_top), _) => tree_top,
        (None, _) if tree_c.starts_with(&root_c) => root_c.clone(),
        (None, _) => tree_c,
    };
    if identity == root_c {
        return lock_path(root);
    }
    let digest = apb_core::content::sha256_hex(identity.to_string_lossy().as_bytes());
    let short: String = digest
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect();
    root.join(".apb/locks").join(format!("tree-{short}.lock"))
}

/// An absolute, canonical path git reports for `dir` (`--show-toplevel`,
/// `--git-common-dir`); `None` outside git or when git is unavailable. Only
/// `rev-parse` runs, which reads no index and runs no hooks.
pub(crate) fn git_path(dir: &Path, what: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", what])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    PathBuf::from(path).canonicalize().ok()
}

/// Writes the lock naming `pid`, inside the workspace's existing `.apb`: a
/// driver of a deleted workspace must not re-create it (a tree lock's
/// `.apb/locks` may be created, `.apb` itself never).
fn write_lock(lock_path: &Path, pid: u32) -> std::io::Result<()> {
    let parent = lock_path.parent().unwrap_or(lock_path);
    let apb_dir = match parent.file_name() {
        Some(name) if name == "locks" => {
            // A `locks` that is a symlink (say, committed into a cloned
            // repository) would carry the lock file out of the project.
            if parent
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(std::io::Error::other(format!(
                    "`{}` is a symlink, refusing to write a lock through it",
                    parent.display()
                )));
            }
            parent.parent().unwrap_or(parent)
        }
        _ => parent,
    };
    atomic_write_under(apb_dir, lock_path, pid.to_string().as_bytes())
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

/// Takes the execution root's workdir lock. See [`acquire_tree`].
pub fn acquire(root: &Path, allow_shared: bool) -> Result<Option<WorkdirGuard>, EngineError> {
    acquire_tree(root, None, allow_shared)
}

/// Takes the busy lock of `tree` (the execution root when `None`), refusing
/// with `WorkdirBusy` while a live process holds it.
pub fn acquire_tree(
    root: &Path,
    tree: Option<&Path>,
    allow_shared: bool,
) -> Result<Option<WorkdirGuard>, EngineError> {
    if allow_shared {
        return Ok(None);
    }
    acquire_at(tree_lock_path(root, tree))
}

fn acquire_at(lock_path: PathBuf) -> Result<Option<WorkdirGuard>, EngineError> {
    if let Some(pid) = lock_holder(&lock_path)
        && pid_alive(pid)
    {
        return Err(EngineError::WorkdirBusy(format!(
            "another write-run holds the workdir (pid {pid}); give this run its own worktree or use --allow-shared-workdir"
        )));
    }
    // No lock, or a stale one - overwrite it.
    write_lock(&lock_path, std::process::id())?;
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
    acquire_handover_within(root, None, HANDOVER_WAIT)
}

/// `acquire_handover` with a caller-chosen ceiling, never shorter than
/// `HANDOVER_WAIT`. A detached driver of a QUEUED run has two waits stacked on
/// top of each other: the handover race with its parent, and the unrelated
/// write-run that made the run queue in the first place. Both are the same
/// poll, so they get one deadline rather than two.
pub fn acquire_handover_within(
    root: &Path,
    tree: Option<&Path>,
    wait: Duration,
) -> Result<Option<WorkdirGuard>, EngineError> {
    wait_for_workdir(
        tree_lock_path(root, tree),
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
    acquire_queued_tree(root, None, wait, stopped)
}

/// [`acquire_queued`] for the busy lock of `tree` (the execution root when
/// `None`).
pub fn acquire_queued_tree(
    root: &Path,
    tree: Option<&Path>,
    wait: Duration,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Option<WorkdirGuard>, EngineError> {
    wait_for_workdir(
        tree_lock_path(root, tree),
        wait,
        QUEUE_STEP,
        LockWait::Queue { stopped },
    )
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
    lock_path: PathBuf,
    wait: Duration,
    step: Duration,
    mut mode: LockWait<'_>,
) -> Result<Option<WorkdirGuard>, EngineError> {
    let deadline = Instant::now() + wait;
    loop {
        if matches!(mode, LockWait::Handover) && lock_holder(&lock_path) == Some(std::process::id())
        {
            return Ok(Some(WorkdirGuard {
                lock_path,
                armed: true,
            }));
        }
        match acquire_at(lock_path.clone()) {
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
