//! Shared test-only utilities for the apb-cli integration binaries (the
//! consolidated `../main.rs` suite and `../init_interactive_test.rs`).
//!
//! Every spawn of the `apb` binary goes through [`apb`] or [`apb_std`], which
//! point `APB_CONFIG_DIR` at a sandbox. Without it the child resolves the
//! global config dir from `XDG_CONFIG_HOME`/`HOME` - the developer's REAL
//! `~/.config/apb` - and every command run inside a seeded tempdir
//! auto-registers that tempdir in the real `projects.json` (plus writes
//! `state/agents-detect.json` there). CI never noticed: auto-registration is
//! off whenever `CI` is set. A test that needs its own config dir still sets
//! `APB_CONFIG_DIR` itself; the later `.env()` wins.
//!
//! The sandbox is one directory per test process, not per command: several
//! tests run a sequence of commands that share global state, exactly as they
//! used to share the real config dir. `real_config_guard_test.rs` fails the suite
//! if a test spawns the binary any other way.

#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Path of the `apb` binary under test. Only for handing to code that spawns
/// it itself (e.g. a script run by an apb child, which inherits that child's
/// sandboxed env); tests spawn it through [`apb`] / [`apb_std`].
pub fn apb_bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_apb"))
}

/// Prefix of the per-process sandbox directories under `CARGO_TARGET_TMPDIR`.
const SANDBOX_PREFIX: &str = "apb-cli-test-config-";

/// The per-process global config dir every spawned `apb` uses. Created on
/// first use under cargo's `target/tmp`, never under the real home. A static
/// is never dropped, so sandboxes left by earlier runs are pruned here once
/// they are an hour old (a live suite's own dir is younger than that).
pub fn sandbox_config_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let parent = Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(parent).expect("create CARGO_TARGET_TMPDIR");
        prune_stale_sandboxes(parent);
        tempfile::Builder::new()
            .prefix(SANDBOX_PREFIX)
            .tempdir_in(parent)
            .expect("create sandbox config dir")
            .keep()
    })
}

fn prune_stale_sandboxes(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(3600);
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(SANDBOX_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// `assert_cmd` handle on the `apb` binary with a sandboxed global config dir.
pub fn apb() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::new(apb_bin());
    cmd.env("APB_CONFIG_DIR", sandbox_config_dir());
    cmd
}

/// `std::process` handle on the `apb` binary with a sandboxed global config dir.
pub fn apb_std() -> std::process::Command {
    let mut cmd = std::process::Command::new(apb_bin());
    cmd.env("APB_CONFIG_DIR", sandbox_config_dir());
    cmd
}

/// Every process signal and liveness check in these tests is a syscall, not a
/// `kill`/`ps` subprocess.
///
/// That is not tidiness. `Command::new("kill").arg("-9").arg("-<pgid>")` is
/// accepted by BSD kill (macOS, where this suite passed) but rejected by
/// procps-ng kill (Linux, and so CI), which hands the leading `-` of the
/// operand to getopt and errors out as if it were an unknown option. The
/// signal was then never delivered, the status of the spawned `kill` was
/// discarded, and the unbounded `child.wait()` that followed blocked forever:
/// the CI job burned 30 minutes on a test whose own 60s poll ceiling was never
/// reached, because control never got that far. `apb_engine::proc::run_capture`
/// and `apb_core::detect` moved off the same subprocess form for the same
/// reason. A syscall has no argument-parsing layer to disagree about.
#[cfg(unix)]
pub mod sig {
    /// SIGKILLs a single process.
    ///
    /// Validated for the same reason `kill_group` is, and it is not academic:
    /// `RunGuard` calls both on a pid it parsed out of `driver.pid`. A
    /// `driver.pid` holding `4294967295` narrows to `-1`, and
    /// `kill(-1, SIGKILL)` is "every process I may signal" - this test suite
    /// would end the developer's session.
    ///
    /// `> 0` here, where `kill_group` needs `> 1`: the single-pid form does
    /// not negate its argument, so pid 1 is just init and merely EPERMs. Only
    /// 0 ("my own process group") and the values that narrow negative have to
    /// go.
    pub fn kill_pid(pid: u32) {
        let Some(pid) = single_target(pid) else {
            return;
        };
        // SAFETY: `kill` takes no pointers; `pid` is a validated positive pid,
        // never a wildcard, and an unknown pid is ESRCH.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }

    /// The `kill(2)` argument naming a single process, or `None` when `pid`
    /// cannot name one.
    fn single_target(pid: u32) -> Option<i32> {
        match i32::try_from(pid) {
            Ok(p) if p > 0 => Some(p),
            _ => None,
        }
    }

    /// SIGKILLs every process in the group led by `pid`.
    ///
    /// Refuses anything that cannot lead an addressable group. The group form
    /// negates its argument, so pid 1 becomes `kill(-1, SIGKILL)` - "every
    /// process I may signal" - pid 0 targets our own group, and a pid above
    /// `i32::MAX` narrows negative and then lands on a small unrelated pid.
    /// `RunGuard` feeds this a pid parsed out of `driver.pid`, and a
    /// signal target that came from a file gets validated. Mirrors
    /// `apb_engine::proc::group_target`.
    pub fn kill_group(pid: u32) {
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        if pid <= 1 {
            return;
        }
        // SAFETY: as above; a validated negative pid addresses the group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }

    /// The process-group id of `pid`, or `None` once the process is gone.
    pub fn pgid_of(pid: u32) -> Option<u32> {
        // SAFETY: `getpgid` takes no pointers and reports ESRCH as -1.
        let pgid = unsafe { libc::getpgid(pid as i32) };
        (pgid >= 0).then_some(pgid as u32)
    }

    /// Whether `pid` still exists (a zombie counts as existing, which is the
    /// point of the reaping assertions in this module).
    pub fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 performs the permission and existence checks
        // without delivering anything.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}

/// Kills a detached run's driver when a test ends, however it ends. The
/// driver deliberately outlives every process a test controls, so an assert
/// that fails before the run finished would otherwise leave it running (a
/// gate waits forever). Declare it as soon as the run id is known, before any
/// fallible step, and after the tempdir so it drops first. On the happy path
/// `driver.pid` is already gone and this is a no-op.
pub struct RunGuard {
    run_dir: PathBuf,
}

impl RunGuard {
    pub fn new(root: &Path, run_id: &str) -> Self {
        Self {
            run_dir: root.join(".apb/runs").join(run_id),
        }
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = apb_engine::driver::read_driver_pid(&self.run_dir) {
            // The driver leads its own group, so this also takes down the
            // script it is running. Not waited on: the driver is not our
            // child, so there is no handle to reap and nothing to block on.
            sig::kill_group(pid);
            sig::kill_pid(pid);
        }
    }
}
