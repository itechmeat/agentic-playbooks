//! The one spawn of a node agent's process, for the headless and the
//! streaming transports alike: the command (wrapped in the isolation
//! sandbox when the attempt has one), its environment
//! ([`super::spawn_env::prepare`], the sandbox overlay last), its own
//! process group, and the spawn-time callback.

use std::ffi::OsString;
use std::process::{Child, Command, Stdio};

use super::failure::ErrorClass;
use super::spawn_env::{self, SpawnTarget};

/// Spawns `program argv` in `workdir` for `target` with `stdin`, stdout and
/// stderr piped, and calls `on_spawn` with the pid and the spawn's own
/// milliseconds.
///
/// Without a sandbox the process leads its own process group
/// (`process_group(0)`). In a sandbox, bwrap runs the agent and apb calls
/// `setsid()` before exec instead: a new session with no controlling
/// terminal, whose process group still equals the pid, so the group kill and
/// the graceful stop reach the whole tree either way.
pub(super) fn spawn_agent(
    program: &str,
    argv: &[String],
    stdin: Stdio,
    workdir: &std::path::Path,
    target: &SpawnTarget,
    on_spawn: Option<&dyn Fn(u32, u64)>,
) -> Result<Child, (ErrorClass, String)> {
    let sandbox = target.env.sandbox.as_deref();
    let mut cmd = match sandbox {
        Some(plan) => {
            let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
            plan.command(program.as_ref(), &args)
        }
        None => {
            let mut c = Command::new(program);
            c.args(argv).current_dir(workdir);
            c
        }
    };
    cmd.stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The process env, the connector scrub, the run context, the agent
    // home, zcode's provider config, the status file and the sandbox
    // overlay, in one place for every form (start and resume) of every
    // transport.
    spawn_env::prepare(&mut cmd, target)?;
    #[cfg(unix)]
    crate::proc::own_group(&mut cmd, sandbox.is_some());
    let started = std::time::Instant::now();
    let child = spawn_retrying(&mut cmd).map_err(|e| {
        // In a sandbox the process apb starts is bwrap: name it, so a
        // missing or broken bwrap is not taken for a missing agent.
        let msg = match sandbox {
            Some(plan) => format!(
                "spawn `{program}` in the sandbox (`{}`) failed: {e}",
                plan.bwrap.display()
            ),
            None => format!("spawn `{program}` failed: {e}"),
        };
        (ErrorClass::ProcessExit, msg)
    })?;
    // Attempt journaling (spawn-time): the process exists, so record it now
    // (pid = child.id()) before any of its work runs.
    if let Some(cb) = on_spawn {
        cb(child.id(), started.elapsed().as_millis() as u64);
    }
    Ok(child)
}

/// Spawns `cmd`, retrying briefly while the executable is busy (ETXTBSY,
/// right after it was written).
fn spawn_retrying(cmd: &mut Command) -> std::io::Result<Child> {
    for _ in 0..20 {
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e),
        }
    }
    cmd.spawn()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A process started in its own session still takes the whole tree
    /// down with a group kill: its grandchildren are in its group.
    #[test]
    fn a_group_kill_reaches_the_grandchildren_of_a_new_session() {
        let t = tempfile::tempdir().unwrap();
        let pidfile = t.path().join("grandchild");
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!(
                "sleep 30 & echo $! > '{}'; wait",
                pidfile.display()
            ))
            .stdout(Stdio::null());
        crate::proc::own_group(&mut cmd, true);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id() as i32;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::fs::read_to_string(&pidfile).map_or(true, |s| s.trim().is_empty()) {
            assert!(std::time::Instant::now() < deadline, "no grandchild");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: plain syscalls on processes this test started.
        unsafe {
            assert_eq!(libc::getsid(pid), pid, "a new session");
            assert_eq!(
                libc::getpgid(grandchild),
                pid,
                "the grandchild is in the group"
            );
            assert_eq!(libc::kill(-pid, libc::SIGKILL), 0);
        }
        child.wait().unwrap();
        let gone = (0..200).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            // SAFETY: probing a pid this test started.
            let dead = unsafe { libc::kill(grandchild, 0) != 0 };
            dead || std::fs::read_to_string(format!("/proc/{grandchild}/stat"))
                .is_ok_and(|s| s.contains(") Z "))
        });
        assert!(gone, "the grandchild survived the group kill");
    }
}
