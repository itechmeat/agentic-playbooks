//! The detached `apb __pause-waker` processes a run leaves behind when no
//! driver holds it: the waker of a pause with an `until` and of a released
//! run ([`crate::pause::waker`]), and the notify waker
//! ([`crate::notify::retry`]). This module starts them and keeps one alive
//! per wait.
//!
//! Each waker holds a lease for its whole life: an OS lock (`flock` on
//! unix) on a file under `runs/<id>/wakers/` named for what it waits for
//! (`pause-<seq>`, `release-<seq>`, `notify`, `deadline`). The OS drops the lock when
//! the process dies, so a lease outlives no waker. A second waker for the
//! same wait finds the lease held and exits at once, `apb resume --all`
//! spawns none while one is held, and its own resume of a due wait waits
//! for nobody: it leaves a held one to its waker. So a quota pause is
//! probed once, and two drivers never race for one wake. Lease files are
//! never removed (removing one while another process opens it could hand
//! two wakers one wait); they are empty and as many as the run's waits.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What a waker waits for: the name of its lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakerFor {
    /// The `run_paused` at this journal sequence (its `until`).
    Pause(u64),
    /// The `drive_released` at this journal sequence.
    Release(u64),
    /// The retry of the run's last notifications.
    Notify,
    /// The follow-up due after the `run_finished` at this journal sequence.
    FollowUp(u64),
    /// The run's deadline while no driver holds it: one waker per run,
    /// whatever parks it (`crate::run_deadline::run_deadline_waker`).
    Deadline,
}

impl WakerFor {
    fn lease_name(self) -> String {
        match self {
            WakerFor::Pause(seq) => format!("pause-{seq}"),
            WakerFor::Release(seq) => format!("release-{seq}"),
            WakerFor::Notify => "notify".into(),
            WakerFor::FollowUp(seq) => format!("follow-up-{seq}"),
            WakerFor::Deadline => "deadline".into(),
        }
    }

    /// The arguments of `apb __pause-waker` after the run id.
    fn args(self) -> Vec<String> {
        match self {
            WakerFor::Pause(seq) => vec!["--seq".into(), seq.to_string()],
            WakerFor::Release(seq) => vec!["--release".into(), seq.to_string()],
            WakerFor::Notify => vec!["--notify".into()],
            WakerFor::FollowUp(seq) => vec!["--follow-up".into(), seq.to_string()],
            WakerFor::Deadline => vec!["--deadline".into()],
        }
    }

    /// What a waker that could not start leaves to a person.
    fn what(self) -> &'static str {
        match self {
            WakerFor::Pause(_) | WakerFor::Release(_) => "it waits until a set time",
            WakerFor::Notify => "a notification waits for a retry",
            WakerFor::FollowUp(_) => "a follow-up check waits for its time",
            WakerFor::Deadline => "a run's deadline waits for its time",
        }
    }

    /// What a person runs instead when its waker could not start.
    fn instead(self, run_id: &str) -> String {
        match self {
            WakerFor::Pause(_) | WakerFor::Release(_) => {
                format!("`apb resume {run_id}` or `apb resume --all` picks it up")
            }
            WakerFor::Notify | WakerFor::FollowUp(_) => "`apb resume --all` picks it up".into(),
            WakerFor::Deadline => "`apb resume --all` ends it".into(),
        }
    }
}

fn lease_path(run_dir: &Path, wait: WakerFor) -> PathBuf {
    run_dir.join("wakers").join(wait.lease_name())
}

/// Opens (creating) the lease file of `wait` under `run_dir`, which an
/// agent step may write: a `wakers` entry that is not a real directory, a
/// lease that is not a regular file (a fifo fails at once, it never blocks
/// the opener) and a link anywhere below `run_dir` are refused. The lease
/// is opened relative to the `wakers` directory it checked
/// ([`apb_core::fsutil::open_append_private_in`]), so a `wakers` swapped
/// for a link meanwhile is never gone through.
fn open_lease(run_dir: &Path, wait: WakerFor) -> std::io::Result<File> {
    let dir = run_dir.join("wakers");
    apb_core::process_mode::guard_write(&dir)?;
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(std::io::Error::other(
                "the run's wakers entry is not a directory",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            apb_core::fsutil::create_dir_under(run_dir, &dir)?;
        }
        Err(e) => return Err(e),
    }
    apb_core::fsutil::open_append_private_in(&dir, &wait.lease_name())
}

/// A waker's lease on one wait of one run, held until dropped.
#[derive(Debug)]
pub struct Lease {
    file: File,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl Lease {
    /// Takes the lease of `wait` on the run at `run_dir`; `None` while
    /// another process holds it. An error when the lease file cannot be
    /// opened at all.
    pub fn try_take(run_dir: &Path, wait: WakerFor) -> std::io::Result<Option<Lease>> {
        let file = open_lease(run_dir, wait)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Lease { file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Whether a live waker holds the lease of `wait` now. A lease that
    /// cannot be read counts as free: a waker spawned then exits at once if
    /// one does hold it.
    pub fn held(run_dir: &Path, wait: WakerFor) -> bool {
        let path = lease_path(run_dir, wait);
        if !path.is_file() {
            return false;
        }
        matches!(Self::try_take(run_dir, wait), Ok(None))
    }
}

/// Spawns the detached waker for `wait` of run `run_id`, unless a live one
/// already holds its lease. Best effort: a waker that cannot start leaves
/// an ordinary paused or released run, which `apb resume` or `apb resume
/// --all` continues, and a finished run whose notification `apb resume
/// --all` sweeps. Only the `apb` binary can serve as the waker, so a host
/// process that is not `apb` (a test binary) spawns nothing. Returns the
/// pid of the waker spawned.
pub fn spawn(root: &Path, run_id: &str, wait: WakerFor) -> Option<u32> {
    let child = start(root, run_id, wait, Stdio::null())?;
    let pid = child.id();
    reap(child);
    Some(pid)
}

/// Waits for `child` to exit on a thread of its own.
fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Starts `apb __pause-waker` for `wait` of run `run_id` (see [`spawn`]),
/// its stdout going to `stdout`.
fn start(root: &Path, run_id: &str, wait: WakerFor, stdout: Stdio) -> Option<std::process::Child> {
    // A read-only or scenario process starts no waker and opens no lease.
    apb_core::process_mode::guard_program_start().ok()?;
    if !apb_core::registry::is_safe_segment(run_id)
        || Lease::held(&root.join(".apb/runs").join(run_id), wait)
    {
        return None;
    }
    let exe = apb_core::fsutil::reexec_exe().ok()?;
    if exe.file_stem().and_then(|s| s.to_str()) != Some("apb") {
        return None;
    }
    let root = std::fs::canonicalize(root).ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("__pause-waker")
        .arg("--root")
        .arg(&root)
        .arg("--run-id")
        .arg(run_id)
        .args(wait.args())
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null());
    // Its own process group, like the detached driver: tearing down the
    // launcher's group must not take the waker with it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    match apb_core::fsutil::spawn_when_not_busy(&mut cmd) {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!(
                "apb: warning: run `{run_id}`: {}, but its waker could not start ({e}); {}",
                wait.what(),
                wait.instead(run_id)
            );
            None
        }
    }
}

/// What a waker writes on its stdout once it holds its lease
/// ([`acknowledge_held`]).
const HELD: &[u8] = b"held\n";

/// How long [`spawn_held`] waits for the waker it started to hold its
/// lease.
pub const HELD_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether a waker holds the lease of `wait` of run `run_id` once this
/// returns: a live one already did, or the waker this call starts
/// acknowledged it ([`acknowledge_held`]) within a bounded wait. A spawned
/// process is never taken for a held lease: a waker that cannot open its
/// lease (a fifo or a link at its path) exits without acknowledging. A
/// waker that ended without acknowledging because another one holds the
/// wait counts as held. `Err` says why nothing holds it.
pub fn spawn_held(root: &Path, run_id: &str, wait: WakerFor) -> Result<(), String> {
    start_held(root, run_id, wait).settle()
}

/// A waker [`start_held`] started, whose acknowledgment
/// [`HeldWaker::settle`] waits for: a caller re-arming many waits starts
/// them all first, then settles each, so the waits overlap and the whole
/// pass waits about [`HELD_WAIT`] at most, not that much per run.
pub struct HeldWaker {
    run_dir: std::path::PathBuf,
    wait: WakerFor,
    state: HeldState,
}

enum HeldState {
    /// Nothing was started: a live waker held the lease, or none could
    /// start.
    NotStarted,
    /// Started at `since`; its acknowledgment arrives on `ack`.
    Started {
        child: std::process::Child,
        ack: std::sync::mpsc::Receiver<bool>,
        since: std::time::Instant,
        limit: std::time::Duration,
    },
}

/// Starts the waker of [`spawn_held`] without waiting for it.
pub fn start_held(root: &Path, run_id: &str, wait: WakerFor) -> HeldWaker {
    let run_dir = root.join(".apb/runs").join(run_id);
    let state = match start(root, run_id, wait, Stdio::piped()) {
        Some(child) => HeldState::waiting(child, HELD_WAIT),
        None => HeldState::NotStarted,
    };
    HeldWaker {
        run_dir,
        wait,
        state,
    }
}

impl HeldState {
    /// `child`'s acknowledgment read on a thread of its own, awaited for
    /// `limit` from now.
    fn waiting(mut child: std::process::Child, limit: std::time::Duration) -> Self {
        let out = child.stdout.take();
        let (tx, ack) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut got = [0u8; HELD.len()];
            let acked = out.is_some_and(|mut o| o.read_exact(&mut got).is_ok() && got == HELD);
            let _ = tx.send(acked);
        });
        HeldState::Started {
            child,
            ack,
            since: std::time::Instant::now(),
            limit,
        }
    }
}

impl HeldWaker {
    /// [`spawn_held`]'s answer: waits for the acknowledgment until its
    /// limit counted from the start, not from this call.
    pub fn settle(self) -> Result<(), String> {
        let HeldState::Started {
            child,
            ack,
            since,
            limit,
        } = self.state
        else {
            return if Lease::held(&self.run_dir, self.wait) {
                Ok(())
            } else {
                Err("no waker could start for it".into())
            };
        };
        let answer = ack.recv_timeout(limit.saturating_sub(since.elapsed()));
        reap(child);
        match answer {
            Ok(true) => Ok(()),
            // Its stdout closed unacknowledged: it ended, and only a wait
            // another waker holds is still served.
            Ok(false) if Lease::held(&self.run_dir, self.wait) => Ok(()),
            Ok(false) => Err("its waker ended before it held the wait's lease".into()),
            Err(_) => Err(format!(
                "its waker did not hold the wait's lease within {}s",
                limit.as_secs()
            )),
        }
    }
}

/// Whether this process is an `apb __pause-waker` that acknowledges its
/// lease on its stdout ([`acknowledge_on_hold`]).
static ACKNOWLEDGES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Makes this process acknowledge the lease it takes as a waker
/// ([`acknowledge_held`]). Only `apb __pause-waker` calls it: a process
/// that runs a waker's body in-process keeps its stdout.
pub fn acknowledge_on_hold() {
    ACKNOWLEDGES.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Tells the process that started this waker ([`spawn_held`]) that it holds
/// its lease, once, then points stdout at the null device, so nothing a
/// waker writes later reaches a pipe nobody reads. Nothing unless
/// [`acknowledge_on_hold`] was called.
pub(crate) fn acknowledge_held() {
    use std::io::Write as _;
    if !ACKNOWLEDGES.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(HELD).and_then(|()| out.flush());
    #[cfg(unix)]
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        use std::os::fd::AsRawFd as _;
        // SAFETY: both descriptors are open; dup2 replaces fd 1 atomically.
        unsafe {
            libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO);
        }
    }
}

/// Spawns the detached notify waker of run `run_id` (`apb __pause-waker
/// --notify`), which retries the notifications its drive's last sweep
/// could not send (`crate::notify::retry`).
pub fn spawn_notify_waker(root: &Path, run_id: &str) -> Option<u32> {
    spawn(root, run_id, WakerFor::Notify)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wakers started side by side that never acknowledge: each is settled
    /// against its own limit counted from its start, so settling them one
    /// after the other waits about one limit in all, not one per waker.
    #[cfg(unix)]
    #[test]
    fn wakers_started_together_are_awaited_together() {
        let dir = tempfile::tempdir().unwrap();
        let limit = std::time::Duration::from_millis(400);
        let silent = || {
            let child = std::process::Command::new("sleep")
                .arg("1")
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            HeldWaker {
                run_dir: dir.path().to_path_buf(),
                wait: WakerFor::Notify,
                state: HeldState::waiting(child, limit),
            }
        };
        let started: Vec<HeldWaker> = (0..4).map(|_| silent()).collect();
        let pids: Vec<u32> = started
            .iter()
            .filter_map(|w| match &w.state {
                HeldState::Started { child, .. } => Some(child.id()),
                HeldState::NotStarted => None,
            })
            .collect();
        let begin = std::time::Instant::now();
        for w in started {
            let why = w.settle().unwrap_err();
            assert!(why.contains("did not hold"), "{why}");
        }
        let took = begin.elapsed();
        assert!(
            took < limit * 2,
            "four silent wakers took {took:?}, about one limit each"
        );
        // The children end on their own and are reaped; none outlives the
        // test. A signal 0 finds a pid until it is reaped, on every unix.
        let exists = |p: &u32| {
            let pid = libc::pid_t::try_from(*p).unwrap();
            // SAFETY: signal 0 only checks that the process exists.
            unsafe { libc::kill(pid, 0) == 0 }
        };
        let gone = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while pids.iter().any(exists) {
            assert!(
                std::time::Instant::now() < gone,
                "a child outlived its waker"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// One lease per wait: a second taker is refused while the first holds
    /// it, other waits are independent, and dropping it frees it.
    #[test]
    fn a_lease_is_held_by_one_waker_per_wait() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path();
        assert!(!Lease::held(run, WakerFor::Release(4)));
        let first = Lease::try_take(run, WakerFor::Release(4)).unwrap();
        assert!(first.is_some());
        assert!(Lease::held(run, WakerFor::Release(4)));
        assert!(
            Lease::try_take(run, WakerFor::Release(4))
                .unwrap()
                .is_none()
        );
        assert!(!Lease::held(run, WakerFor::Release(5)));
        assert!(!Lease::held(run, WakerFor::Pause(4)));
        assert!(!Lease::held(run, WakerFor::Notify));
        drop(first);
        assert!(!Lease::held(run, WakerFor::Release(4)));
        assert!(
            Lease::try_take(run, WakerFor::Release(4))
                .unwrap()
                .is_some()
        );
    }

    /// The run directory may be written by an agent step: a `wakers` entry
    /// or a lease file that is a link is refused, and nothing is created or
    /// re-moded where it points.
    #[cfg(unix)]
    #[test]
    fn a_linked_lease_path_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::os::unix::fs::symlink(outside.path(), run.join("wakers")).unwrap();
        assert!(Lease::try_take(&run, WakerFor::Notify).is_err());
        assert!(!outside.path().join("notify").exists());

        std::fs::remove_file(run.join("wakers")).unwrap();
        std::fs::create_dir(run.join("wakers")).unwrap();
        let target = outside.path().join("victim");
        std::fs::write(&target, "x").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&target, run.join("wakers/notify")).unwrap();
        assert!(Lease::try_take(&run, WakerFor::Notify).is_err());
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "the target keeps its mode");
    }
}
