use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the stdout/stderr reader threads get to deliver what they read
/// once the script process is gone. Not a work budget: by then the pipes are
/// normally at EOF already and the value arrives immediately. It bounds the
/// one case the group kill cannot cover - a descendant that left the process
/// group while still holding the inherited pipe.
const DRAIN_BUDGET: Duration = Duration::from_secs(5);

/// Stable prefix of the note appended to stderr when the stdout drain expired.
/// Public so callers and tests can recognise the condition without matching
/// the whole sentence.
pub const DRAIN_LOST_MARKER: &str = "apb: script stdout was not collected";

fn drain_lost_marker() -> String {
    format!(
        "{DRAIN_LOST_MARKER}: a descendant outlived the script still holding its stdout open, \
         so the pipe never reached EOF within the {}s drain budget. The captured stdout is \
         EMPTY, not truncated. A script that backgrounds a long-lived helper should redirect \
         its output (`... >/dev/null 2>&1 &`).",
        DRAIN_BUDGET.as_secs()
    )
}

/// The `kill(2)` argument that addresses the process group led by `pid`, or
/// `None` when `pid` cannot lead an addressable one.
///
/// Pure and separately tested, because getting this wrong is the worst bug
/// this codebase could ship. To `kill(2)` the out-of-range values are not
/// errors, they are WILDCARDS, and the group form negates its argument, so
/// three classes of input have to be refused:
///
///   * `pid` 0 negates to 0, which means "my own process group" - suicide.
///   * `pid` 1 negates to -1, which means "every process I may signal". This
///     is the catastrophic one: a group kill aimed at init kills the user's
///     entire session.
///   * `pid` above `i32::MAX` narrows to a negative number first, so negating
///     it yields a small POSITIVE pid and the signal lands on an unrelated
///     process (`u32::MAX` becomes 1, i.e. init).
///
/// Every caller today passes `Child::id()` from a handle we own and cannot
/// reach any of these. The check exists so that a caller who one day passes a
/// pid parsed from `driver.pid` or `workdir.lock` - both of which are just
/// files, and a corrupt or hostile one is not far-fetched - gets a no-op
/// instead of a catastrophe.
fn group_target(pid: u32) -> Option<i32> {
    match i32::try_from(pid) {
        Ok(p) if p > 1 => Some(-p),
        _ => None,
    }
}

/// SIGKILLs every process in the group led by `pid` (pgid == pid, which is why
/// everything spawned for teardown uses `process_group(0)`).
///
/// Safe to call after the leader has been reaped: a pgid is not recycled while
/// any process remains in the group, and an empty group is a harmless ESRCH.
pub(crate) fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    if let Some(target) = group_target(pid) {
        // SAFETY: `kill` takes no pointers and is async-signal-safe; `target`
        // is a validated negative group id, never a wildcard.
        unsafe {
            libc::kill(target, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

pub struct Captured {
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    /// The process was killed due to the cancellation signal (`cancel`), not
    /// a timeout/exit code. Lets the caller distinguish a join:any
    /// cancellation from a real timeout.
    pub cancelled: bool,
}

/// Runs the command, capturing stdout/stderr. If the timeout is exceeded OR
/// `cancel` is set, kills the whole process group and returns status = None
/// (with `cancelled = true` in the cancellation case). With no timeout and no
/// cancellation, waits for completion.
pub fn run_capture(
    mut cmd: Command,
    timeout: Option<Duration>,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<Captured> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Make the child process the leader of its own process group (pgid ==
    // pid), so that on timeout the whole group can be killed (shell
    // pipelines, background jobs `&`, forks), not just the direct child.
    cmd.process_group(0);
    let mut child = cmd.spawn()?;

    // Read the streams on separate threads so we don't hit pipe buffers filling up.
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        let _ = out_pipe.read_to_string(&mut s);
        let _ = tx_out.send(s);
    });
    thread::spawn(move || {
        let mut s = String::new();
        let _ = err_pipe.read_to_string(&mut s);
        let _ = tx_err.send(s);
    });

    let start = Instant::now();
    let mut cancelled = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let timed_out = timeout.is_some_and(|limit| start.elapsed() >= limit);
        let was_cancelled = cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        if timed_out || was_cancelled {
            // Kill the whole process group (a negative pid signals the
            // group), not just the direct child - otherwise grandchild
            // processes that inherited the stdout/stderr pipes keep the
            // channel open and the recv() below blocks forever.
            // Cancellation (another join:any branch won) likewise tears
            // down the whole script tree, with no leaked side effects.
            //
            // libc::kill is used directly instead of spawning a `kill`
            // subprocess: the syscall is immediate and avoids arg-parsing
            // ambiguity of `kill -KILL -<pgid>` across implementations,
            // which on some Linux setups failed to reach the grandchild
            // and left the stdout pipe open until natural exit.
            kill_process_group(child.id());
            // Also the leader directly, through the handle rather than by pid:
            // if `process_group(0)` did not take, the group kill above missed
            // it, and the `wait` below would then block on a live child.
            let _ = child.kill();
            let _ = child.wait();
            cancelled = was_cancelled;
            break None;
        }
        thread::sleep(Duration::from_millis(50));
    };

    // Bounded, not `recv()`. The group kill above normally guarantees EOF, but
    // it only reaches the group: a script that calls `setsid` (or otherwise
    // leaves the group) and keeps the inherited stdout fd open would hold
    // these reader threads open forever, and an unbounded `recv` would hand
    // that hang straight to the drive loop. The reader threads are abandoned
    // in that case, which costs a blocked thread and nothing else.
    let (stdout, stdout_lost) = match rx_out.recv_timeout(DRAIN_BUDGET) {
        Ok(s) => (s.trim().to_string(), false),
        Err(_) => (String::new(), true),
    };
    let mut stderr = rx_err
        .recv_timeout(DRAIN_BUDGET)
        .unwrap_or_default()
        .trim()
        .to_string();
    // An expired drain yields EMPTY stdout, not truncated stdout, and the node
    // is still reported Succeeded - so without this the entire output of a
    // script vanishes with nothing anywhere saying why, and a downstream
    // condition reading that output silently sees "".
    //
    // Said twice, on purpose, because neither channel alone reaches everyone:
    //
    //   * appended to `Captured::stderr`, which is the honest place for it and
    //     is what any consumer of this function can test and surface. Today's
    //     only consumer, `script::run_script`, happens to drop stderr on the
    //     floor when it builds `ScriptResult` - so this alone would be
    //     invisible for script nodes, which is exactly why the eprintln below
    //     exists rather than instead of it;
    //   * an eprintln, the mechanism this crate already uses for operator
    //     warnings with no tracing facility (`stop::abort_children`, the
    //     progress and driver-pid warnings). Visible for a foreground
    //     `apb run`. Honest limitation: a DETACHED driver runs with stderr on
    //     /dev/null, so there it is lost too, and closing that gap properly
    //     means giving `ScriptResult` a stderr channel and threading it
    //     through the node-execution signature.
    if stdout_lost {
        let marker = drain_lost_marker();
        eprintln!("apb: warning: {marker}");
        if !stderr.is_empty() {
            stderr.push('\n');
        }
        stderr.push_str(&marker);
    }
    Ok(Captured {
        status,
        stdout,
        stderr,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::group_target;

    /// The signal-target rule, tested as pure arithmetic.
    ///
    /// Deliberately NOT by calling `kill_process_group` and observing what
    /// survives. The whole point of this rule is that the rejected inputs are
    /// wildcards: exercising the unguarded path would send SIGKILL to the test
    /// runner's own process group (pid 0) or to every process the developer
    /// owns (pid 1). A test must never be one missing `if` away from ending
    /// the user's session, so the decision is separated from the syscall and
    /// only the decision is tested.
    #[test]
    fn a_group_target_is_never_a_wildcard() {
        // 0 negates to 0: "my own process group".
        assert_eq!(group_target(0), None);
        // 1 negates to -1: "every process I may signal". The catastrophic one.
        assert_eq!(group_target(1), None);
        // Above i32::MAX the value narrows negative first, so negating it
        // yields a small positive pid and the signal lands on an unrelated
        // process. u32::MAX would otherwise become +1, i.e. init.
        assert_eq!(group_target(u32::MAX), None);
        assert_eq!(group_target((i32::MAX as u32) + 1), None);

        // Real pids address their own group, and nothing else.
        assert_eq!(group_target(2), Some(-2));
        assert_eq!(group_target(4321), Some(-4321));
        assert_eq!(group_target(i32::MAX as u32), Some(-i32::MAX));
    }

    /// Every accepted target is strictly negative, which is the property the
    /// group form depends on: a non-negative argument would address a single
    /// process or a wildcard instead of a group.
    #[test]
    fn every_accepted_group_target_is_negative() {
        for pid in [2u32, 3, 99, 1000, 65_535, 4_194_304, i32::MAX as u32] {
            let target = group_target(pid).expect("a real pid must be addressable");
            assert!(
                target < 0,
                "pid {pid} produced non-negative target {target}"
            );
            assert_eq!(target, -(pid as i32));
        }
    }
}
