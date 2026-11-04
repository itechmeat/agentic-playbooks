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
        let dir = tempfile::Builder::new()
            .prefix(SANDBOX_PREFIX)
            .tempdir_in(parent)
            .expect("create sandbox config dir")
            .keep();
        // The run stamp key, in place before any test or spawned `apb`
        // looks for it: tests that stamp runs used to write it when absent,
        // and two writers at once (or an `apb` making its own) left runs
        // stamped with another key than the one read back.
        std::fs::write(dir.join("run-origin.key"), "k".repeat(64))
            .expect("write the run stamp key");
        dir
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
    // A suite run from inside an apb run (an agent step or a host task)
    // inherits its run id and task; the binary would then treat every start
    // as a nested one and refuse every person's decision.
    cmd.env_remove("APB_RUN_ID");
    cmd.env_remove("APB_HOST_TASK");
    cmd
}

/// `std::process` handle on the `apb` binary with a sandboxed global config dir.
pub fn apb_std() -> std::process::Command {
    let mut cmd = std::process::Command::new(apb_bin());
    cmd.env("APB_CONFIG_DIR", sandbox_config_dir());
    cmd.env_remove("APB_RUN_ID");
    cmd.env_remove("APB_HOST_TASK");
    cmd
}

/// `std::process` handle on a COPY of the `apb` binary at `exe`, with the same
/// sandboxed global config dir as [`apb_std`]. For tests about the binary
/// itself changing on disk under a running process (a reinstall).
pub fn apb_std_from(exe: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
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

/// Waits until the driver of run `run_id` let go of the busy lock of the
/// tree it works in.
///
/// A driver journals `run_finished` and only then makes its last
/// notification sweep, while it is still the run's one driver, before it
/// releases the lock (`apb_engine::scheduler::drive`). So a command that
/// needs the workdir right after the finish (a resume) can still find it
/// busy; a test waits here after it saw the finish, which is what a person
/// running the same commands one after another would never notice. Bounded.
pub fn wait_released(root: &Path, run_id: &str) {
    let run_dir = root.join(".apb/runs").join(run_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let events = apb_engine::event::read_all(&run_dir).unwrap_or_default();
        let tree = apb_engine::state::RunState::fold(&events)
            .worktree
            .map(PathBuf::from);
        if apb_engine::workdir::check_free_for(root, tree.as_deref(), run_id).is_ok() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the driver of {run_id} kept the workdir past 60s"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Kills every `apb __pause-waker` of run `run_id` in the project at
/// `root` and waits until each one is gone. A waker outlives every process
/// a test controls, like a detached driver. The root counts: run ids are
/// unique per project only, and two tests' runs started in the same
/// millisecond share one.
///
/// Waiting matters: a waker polls the journal, so one still alive when a
/// test rewrites the journal (a wake time moved into the past) resumes the
/// run itself, racing the command under test. The waker processes are
/// found on every unix the suite runs on (Linux `/proc`, `ps` elsewhere),
/// never skipped: a macOS run that found none left them alive.
#[cfg(unix)]
pub fn kill_wakers(root: &Path, run_id: &str) {
    let Ok(root) = std::fs::canonicalize(root) else {
        return;
    };
    let root = root.to_string_lossy().into_owned();
    let pids: Vec<u32> = process_args()
        .into_iter()
        .filter(|(_, args)| {
            let has = |s: &str| args.iter().any(|a| a == s);
            has("__pause-waker") && has(&root) && has(run_id)
        })
        .map(|(pid, _)| pid)
        .collect();
    for &pid in &pids {
        sig::kill_pid(pid);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for pid in pids {
        while sig::alive(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "waker {pid} of run {run_id} did not die"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// Every process the suite may see, as its pid and arguments: the exact
/// argv from `/proc` on Linux.
#[cfg(target_os = "linux")]
fn process_args() -> Vec<(u32, Vec<String>)> {
    let procs = std::fs::read_dir("/proc").expect("read /proc");
    procs
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
            let args = cmdline
                .split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            Some((pid, args))
        })
        .collect()
}

/// Every process the suite may see, as its pid and arguments: `ps` where
/// there is no `/proc` (macOS). The arguments are split on whitespace, which
/// the ones matched here (a canonical temp dir, a run id, a subcommand)
/// never contain. A `ps` that fails fails the test rather than kill nothing.
#[cfg(all(unix, not(target_os = "linux")))]
fn process_args() -> Vec<(u32, Vec<String>)> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=", "-o", "args="])
        .output()
        .expect("run ps");
    assert!(out.status.success(), "ps failed: {out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse::<u32>().ok()?;
            Some((pid, words.map(str::to_string).collect()))
        })
        .collect()
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

    /// Whether this guard is the one of the run at `run_dir`.
    pub fn guards(&self, run_dir: &Path) -> bool {
        self.run_dir == run_dir
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

/// Runs `apb <args>` on a fresh pseudo-terminal (util-linux `script`) in
/// `dir` with the config dir `cfg`, typing `answer` into the terminal for
/// the question it asks; returns the exit status and what the terminal
/// showed. `None` where `script` is missing (the caller skips).
#[cfg(target_os = "linux")]
pub fn on_a_terminal(
    dir: &Path,
    cfg: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
    answer: &[u8],
) -> Option<(i32, String)> {
    use std::io::Write as _;
    let script = ["/usr/bin/script", "/bin/script"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let line = std::iter::once(apb_bin().display().to_string())
        .chain(args.iter().map(|a| a.to_string()))
        .map(|a| quote(&a))
        .collect::<Vec<_>>()
        .join(" ");
    let mut cmd = std::process::Command::new(script);
    cmd.args(["-qec", &line, "/dev/null"])
        .current_dir(dir)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("APB_RUN_ID")
        .env_remove("APB_HOST_TASK")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().ok()?;
    // The answer waits in the terminal until the question reads it.
    child.stdin.take().unwrap().write_all(answer).unwrap();
    let out = child.wait_with_output().unwrap();
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Whether some process holds a `flock` on the file at `path`, read from
/// `/proc/locks` without touching the lock: a probe that took the lock
/// itself (even for an instant) could make a waker taking it then give up.
#[cfg(target_os = "linux")]
pub fn flock_held(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(ino) = std::fs::metadata(path).map(|m| m.ino()) else {
        return false;
    };
    let ino = format!(":{ino}");
    std::fs::read_to_string("/proc/locks").is_ok_and(|locks| {
        locks.lines().any(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            f.get(1) == Some(&"FLOCK") && f.get(5).is_some_and(|id| id.ends_with(&ino))
        })
    })
}
