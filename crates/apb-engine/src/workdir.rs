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
    /// The lock this guard took over was one the claimant run itself had
    /// parked across a `--keep-lock` pause.
    took_parked: bool,
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

    /// Keeps the lock across a `--keep-lock` pause (issue #200): the lock
    /// file names the paused run instead of a pid, so [`acquire_tree`] keeps
    /// refusing other write-runs while no process drives the run, and only
    /// that run's own resume takes the lock back (or its stop releases it).
    pub fn park(mut self, run_id: &str) -> Result<(), EngineError> {
        write_lock_text(&self.lock_path, &paused_owner(run_id))?;
        self.disarm();
        Ok(())
    }

    /// Whether the lock this guard took over was parked by the run it was
    /// taken for (a resume taking back its own `--keep-lock` lock).
    pub fn took_parked(&self) -> bool {
        self.took_parked
    }
}

/// The prefix of a lock file held by a paused run rather than a process.
const PAUSED_PREFIX: &str = "paused:";

fn paused_owner(run_id: &str) -> String {
    format!("{PAUSED_PREFIX}{run_id}")
}

/// Who a lock file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOwner {
    /// The process driving a write-run.
    Pid(u32),
    /// A run paused with `--keep-lock` (issue #200): no process, the run id.
    PausedRun(String),
}

/// The owner a lock file names, `None` for no lock or an unreadable one.
pub fn lock_owner(path: &Path) -> Option<LockOwner> {
    if !path.is_file() {
        return None;
    }
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let raw = raw.trim();
    if let Some(id) = raw.strip_prefix(PAUSED_PREFIX) {
        return apb_core::registry::is_safe_segment(id).then(|| LockOwner::PausedRun(id.into()));
    }
    match raw.parse::<u32>() {
        Ok(0) | Err(_) => None,
        Ok(pid) => Some(LockOwner::Pid(pid)),
    }
}

/// The `.apb` directory a lock file lives under (`.apb/workdir.lock` or
/// `.apb/locks/<tree>.lock`).
fn apb_dir_of(lock_path: &Path) -> Option<&Path> {
    let parent = lock_path.parent()?;
    match parent.file_name() {
        Some(name) if name == "locks" => parent.parent(),
        _ => Some(parent),
    }
}

/// Whether the paused run `run_id` still holds the lock at `lock_path`: its
/// run directory exists and the run is not over. A stopped or finished run
/// left a stale paused lock, which the next acquire takes.
fn paused_run_holds(lock_path: &Path, run_id: &str) -> bool {
    let Some(run_dir) = apb_dir_of(lock_path).map(|d| d.join("runs").join(run_id)) else {
        return false;
    };
    if !run_dir.is_dir() {
        return false;
    }
    crate::event::read_all_lossy_tail(&run_dir).is_ok_and(|events| {
        !crate::state::RunState::fold(&events)
            .run_status
            .is_terminal()
    })
}

/// Every lock of the workspace at `root` a paused run holds (issue #200), as
/// `(lock file, run id)`: what `apb doctor` lists.
pub fn paused_locks(root: &Path) -> Vec<(PathBuf, String)> {
    let mut files = vec![lock_path(root)];
    if let Ok(dir) = std::fs::read_dir(root.join(".apb/locks")) {
        files.extend(dir.flatten().map(|e| e.path()));
    }
    files
        .into_iter()
        .filter_map(|f| match lock_owner(&f) {
            Some(LockOwner::PausedRun(id)) => Some((f, id)),
            _ => None,
        })
        .collect()
}

/// Releases every lock the paused run `run_id` holds (issue #200): its stop,
/// or a pause update without `--keep-lock`. A lock naming anything else is
/// left alone.
pub fn release_paused(root: &Path, run_id: &str) {
    for (file, id) in paused_locks(root) {
        if id == run_id {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Takes the busy lock of `tree` for the paused run `run_id` and parks it
/// under the run (a `--keep-lock` pause of a run no process drives). Refuses
/// with `WorkdirBusy` while another live write-run holds it.
pub fn park_for(root: &Path, tree: Option<&Path>, run_id: &str) -> Result<(), EngineError> {
    match acquire_at(tree_lock_path(root, tree), Some(run_id))? {
        Some(guard) => guard.park(run_id),
        None => Ok(()),
    }
}

impl Drop for WorkdirGuard {
    /// Removes the lock only while it still names this process: a lock
    /// another owner holds by now (a handover, a park, a new claimant after
    /// a stale steal) is theirs.
    fn drop(&mut self) {
        if self.armed && lock_owner(&self.lock_path) == Some(LockOwner::Pid(std::process::id())) {
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
    write_lock_text(lock_path, &pid.to_string())
}

fn write_lock_text(lock_path: &Path, text: &str) -> std::io::Result<()> {
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
    atomic_write_under(apb_dir, lock_path, text.as_bytes())
}

pub(crate) fn lock_holder(path: &Path) -> Option<u32> {
    match lock_owner(path)? {
        LockOwner::Pid(pid) => Some(pid),
        LockOwner::PausedRun(_) => None,
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
    acquire_at(tree_lock_path(root, tree), None)
}

/// [`acquire_tree`] for the resume of run `run_id`: a lock that run parked
/// across a `--keep-lock` pause (issue #200) is its own to take back.
pub fn acquire_tree_for(
    root: &Path,
    tree: Option<&Path>,
    allow_shared: bool,
    run_id: &str,
) -> Result<Option<WorkdirGuard>, EngineError> {
    if allow_shared {
        return Ok(None);
    }
    acquire_at(tree_lock_path(root, tree), Some(run_id))
}

/// Why the lock at `lock_path` is not free for `claimant`, or `None` when it
/// is (no lock, a stale one, or one `claimant` itself parked).
///
/// The hint names what the caller can do: a new run (`claimant` is `None`)
/// can take its own worktree or `--allow-shared-workdir`; an existing run
/// (its resume, or a `--keep-lock` pause of it) has neither option on any
/// surface, so it is told to wait for the other run or stop it.
fn busy_reason(lock_path: &Path, claimant: Option<&str>) -> Option<String> {
    let existing = claimant.is_some();
    match lock_owner(lock_path) {
        Some(LockOwner::Pid(pid)) if pid_alive(pid) => Some(if existing {
            format!(
                "another write-run holds the workdir (pid {pid}); try again once it finishes, or stop it first"
            )
        } else {
            format!(
                "another write-run holds the workdir (pid {pid}); give this run its own worktree or use --allow-shared-workdir"
            )
        }),
        Some(LockOwner::PausedRun(id))
            if claimant != Some(id.as_str()) && paused_run_holds(lock_path, &id) =>
        {
            Some(if existing {
                format!(
                    "the paused run `{id}` keeps the workdir locked (paused with --keep-lock); resume or stop it first"
                )
            } else {
                format!(
                    "the paused run `{id}` keeps the workdir locked (paused with --keep-lock); resume or stop it first, give this run its own worktree, or use --allow-shared-workdir"
                )
            })
        }
        _ => None,
    }
}

/// Read-only preflight of [`acquire_tree_for`]: refuses with `WorkdirBusy`
/// when resuming run `run_id` would find the busy lock of `tree` held by
/// another live run, without taking the lock. A resume that hands the run to
/// a detached driver calls it so the caller sees the refusal instead of a
/// driver that dies right after the resume reported success.
pub fn check_free_for(root: &Path, tree: Option<&Path>, run_id: &str) -> Result<(), EngineError> {
    match busy_reason(&tree_lock_path(root, tree), Some(run_id)) {
        Some(msg) => Err(EngineError::WorkdirBusy(msg)),
        None => Ok(()),
    }
}

fn acquire_at(
    lock_path: PathBuf,
    claimant: Option<&str>,
) -> Result<Option<WorkdirGuard>, EngineError> {
    // The check and the write are one step under a short mutex, so two
    // claimants of a free, stale or parked lock never both take it (the
    // loser's guard would later remove the winner's lock).
    // A mutex held by a live sibling past the short wait is a busy workdir,
    // so the queued and handover waits retry it like a held lock.
    let _taking = take_mutex(&lock_path).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock => EngineError::WorkdirBusy(e.to_string()),
        _ => EngineError::Io(e),
    })?;
    if let Some(msg) = busy_reason(&lock_path, claimant) {
        return Err(EngineError::WorkdirBusy(msg));
    }
    let took_parked = matches!(
        (lock_owner(&lock_path), claimant),
        (Some(LockOwner::PausedRun(id)), Some(c)) if id == c
    );
    // No lock, a stale one or the claimant's own parked one: overwrite it.
    write_lock(&lock_path, std::process::id())?;
    Ok(Some(WorkdirGuard {
        lock_path,
        armed: true,
        took_parked,
    }))
}

/// The mutex [`acquire_at`] holds while it checks and writes the lock at
/// `lock_path`: a sibling lock file next to it, held for microseconds. Never
/// creates the workspace's `.apb` and never goes through a symlinked
/// `locks` directory, like the lock write itself.
fn take_mutex(lock_path: &Path) -> std::io::Result<apb_core::fsutil::DirLock> {
    let parent = lock_path.parent().unwrap_or(lock_path);
    let apb_dir = apb_dir_of(lock_path).unwrap_or(parent);
    if !apb_dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("`{}` no longer exists", apb_dir.display()),
        ));
    }
    if parent
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(std::io::Error::other(format!(
            "`{}` is a symlink, refusing to write a lock through it",
            parent.display()
        )));
    }
    let name = lock_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    apb_core::fsutil::lock_dir(parent, &format!("{name}.take"))
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
                took_parked: false,
            }));
        }
        match acquire_at(lock_path.clone(), None) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(r: Result<impl std::fmt::Debug, EngineError>) -> String {
        match r {
            Err(EngineError::WorkdirBusy(msg)) => msg,
            other => panic!("expected WorkdirBusy, got {other:?}"),
        }
    }

    #[test]
    fn a_resume_is_not_told_about_options_only_a_new_run_has() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        apb_core::registry::init_project(root).unwrap();
        let _held = acquire_tree(root, None, false).unwrap();
        let start = busy(acquire_tree(root, None, false));
        assert!(start.contains("--allow-shared-workdir"), "{start}");
        let resume = busy(check_free_for(root, None, "r-1"));
        assert!(!resume.contains("--allow-shared-workdir"), "{resume}");
        assert!(!resume.contains("worktree"), "{resume}");
        assert!(resume.contains("stop it first"), "{resume}");
    }

    /// Two resumes of one run reclaiming the lock it parked: exactly one
    /// takes it, the other is told the workdir is busy.
    #[test]
    fn racing_claimants_of_a_parked_lock_take_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        apb_core::registry::init_project(root).unwrap();
        let lock = lock_path(root);
        for _ in 0..20 {
            write_lock_text(&lock, &paused_owner("r-1")).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (root, barrier) = (root.to_path_buf(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        acquire_tree_for(&root, None, false, "r-1")
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let taken = results.iter().filter(|r| matches!(r, Ok(Some(_)))).count();
            assert_eq!(taken, 1, "{results:?}");
            drop(results);
            assert_eq!(lock_owner(&lock), None, "the winner's guard released it");
        }
    }

    /// A guard whose lock names another owner by now leaves that lock alone.
    #[test]
    fn a_guard_never_removes_a_lock_it_no_longer_owns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        apb_core::registry::init_project(root).unwrap();
        let guard = acquire_tree(root, None, false).unwrap();
        let lock = lock_path(root);
        write_lock_text(&lock, &paused_owner("other")).unwrap();
        drop(guard);
        assert_eq!(
            lock_owner(&lock),
            Some(LockOwner::PausedRun("other".into()))
        );
    }
}
