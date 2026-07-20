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
            #[cfg(unix)]
            {
                let pid = child.id() as i32;
                let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
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
    let stdout = rx_out
        .recv_timeout(DRAIN_BUDGET)
        .unwrap_or_default()
        .trim()
        .to_string();
    let stderr = rx_err
        .recv_timeout(DRAIN_BUDGET)
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(Captured {
        status,
        stdout,
        stderr,
        cancelled,
    })
}
