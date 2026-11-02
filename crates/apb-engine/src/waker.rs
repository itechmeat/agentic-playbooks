//! The detached `apb __pause-waker` processes a run leaves behind when no
//! driver holds it: the waker of a pause with an `until` and of a released
//! run ([`crate::pause::waker`]), and the notify waker
//! ([`crate::notify::retry`]). This module starts them and keeps one alive
//! per wait.
//!
//! Each waker holds a lease for its whole life: an OS lock (`flock` on
//! unix) on a file under `runs/<id>/wakers/` named for what it waits for
//! (`pause-<seq>`, `release-<seq>`, `notify`). The OS drops the lock when
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
}

impl WakerFor {
    fn lease_name(self) -> String {
        match self {
            WakerFor::Pause(seq) => format!("pause-{seq}"),
            WakerFor::Release(seq) => format!("release-{seq}"),
            WakerFor::Notify => "notify".into(),
        }
    }

    /// The arguments of `apb __pause-waker` after the run id.
    fn args(self) -> Vec<String> {
        match self {
            WakerFor::Pause(seq) => vec!["--seq".into(), seq.to_string()],
            WakerFor::Release(seq) => vec!["--release".into(), seq.to_string()],
            WakerFor::Notify => vec!["--notify".into()],
        }
    }

    /// What a waker that could not start leaves to a person.
    fn what(self) -> &'static str {
        match self {
            WakerFor::Pause(_) | WakerFor::Release(_) => "it waits until a set time",
            WakerFor::Notify => "a notification waits for a retry",
        }
    }

    /// What a person runs instead when its waker could not start.
    fn instead(self, run_id: &str) -> String {
        match self {
            WakerFor::Pause(_) | WakerFor::Release(_) => {
                format!("`apb resume {run_id}` or `apb resume --all` picks it up")
            }
            WakerFor::Notify => "`apb resume --all` picks it up".into(),
        }
    }
}

fn lease_path(run_dir: &Path, wait: WakerFor) -> PathBuf {
    run_dir.join("wakers").join(wait.lease_name())
}

/// Opens (creating) the lease file at `path` under `run_dir`, which an agent
/// step may write: a `wakers` entry that is not a real directory and a lease
/// that is not a regular file are refused, and no link is followed.
fn open_lease(run_dir: &Path, path: &Path) -> std::io::Result<File> {
    apb_core::process_mode::guard()?;
    let dir = run_dir.join("wakers");
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
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other("a lease file is a link"));
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("a lease file is not a regular file"));
    }
    apb_core::fsutil::narrow_open(&file);
    Ok(file)
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
        let path = lease_path(run_dir, wait);
        let file = open_lease(run_dir, &path)?;
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
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own process group, like the detached driver: tearing down the
    // launcher's group must not take the waker with it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    match apb_core::fsutil::spawn_when_not_busy(&mut cmd) {
        Ok(mut child) => {
            let pid = child.id();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Some(pid)
        }
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

/// Spawns the detached notify waker of run `run_id` (`apb __pause-waker
/// --notify`), which retries the notifications its drive's last sweep
/// could not send (`crate::notify::retry`).
pub fn spawn_notify_waker(root: &Path, run_id: &str) -> Option<u32> {
    spawn(root, run_id, WakerFor::Notify)
}

#[cfg(test)]
mod tests {
    use super::*;

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
