//! End-to-end tests for the `apb __ask-server` live-question sidecar (spec
//! 2026-07-20-interactive-nodes, Task 10).
//!
//! These spawn the built `apb` binary and drive a real MCP `initialize` +
//! `tools/call ask_user` handshake over its stdio pipes (newline-delimited
//! JSON-RPC, as `rmcp::transport::stdio()` requires). They live in the CLI
//! crate because only the package declaring the `apb` bin gets
//! `CARGO_BIN_EXE_apb`. The answer-matching rule itself is unit-tested in
//! apb-mcp (`ask_server_test.rs`).
//!
//! Every wait is bounded and names what it waits for; the child is reaped on
//! every path by an RAII guard built before the first assertion. No bare
//! sleeps: the 60 s progress cadence is shortened to fire on every poll via
//! `APB_ASK_PROGRESS_SECS=0`, so the keep-alive path is asserted in
//! milliseconds rather than a real minute.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use apb_engine::{post_answer, read_questions_after};

/// Reaps the sidecar child on every exit path (guideline: build the reaper
/// before the first thing that can panic). `kill` + `wait` on an
/// already-exited child are harmless.
struct Reaper(Option<Child>);

impl Reaper {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("child already reaped")
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Spawns `apb __ask-server` for a fresh tempdir run and returns the reaper,
/// its stdin, a line channel from its stdout, and the run directory. The
/// progress cadence is shortened to fire on every poll.
fn spawn_sidecar(
    node: &str,
) -> (
    Reaper,
    ChildStdin,
    Receiver<String>,
    tempfile::TempDir,
    String,
) {
    let tmp = tempfile::tempdir().unwrap();
    let run = "runx";
    let run_dir = tmp.path().join(run);
    std::fs::create_dir_all(&run_dir).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_apb"))
        .args([
            "__ask-server",
            "--run",
            run,
            "--node",
            node,
            "--attempt",
            "1",
        ])
        .env("APB_RUN_DIR", &run_dir)
        .env("APB_ASK_PROGRESS_SECS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    (
        Reaper(Some(child)),
        stdin,
        rx,
        tmp,
        run_dir.to_string_lossy().into_owned(),
    )
}

/// Drains lines until one satisfies `pred` or the deadline elapses; panics
/// naming `what` on timeout.
fn wait_for_line(rx: &Receiver<String>, what: &str, mut pred: impl FnMut(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_default();
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                if pred(&line) {
                    return line;
                }
            }
            Err(_) => panic!("timed out after 15s waiting for: {what}"),
        }
    }
}

fn handshake(stdin: &mut ChildStdin, rx: &Receiver<String>) {
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    writeln!(stdin, "{init}").unwrap();
    stdin.flush().unwrap();
    wait_for_line(rx, "the initialize response", |l| {
        l.contains("\"id\":1") && l.contains("protocolVersion")
    });
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();
}

#[test]
fn ask_user_posts_question_emits_progress_and_returns_the_answer() {
    let (mut reaper, mut stdin, rx, _tmp, run_dir) = spawn_sidecar("ask");
    let run_dir = std::path::PathBuf::from(run_dir);

    handshake(&mut stdin, &rx);

    // Call ask_user with a progressToken so the keep-alive path is exercised.
    let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask_user","arguments":{"question":"Which DB?","options":["pg","sqlite"]},"_meta":{"progressToken":"p1"}}}"#;
    writeln!(stdin, "{call}").unwrap();
    stdin.flush().unwrap();

    // The question must reach the channel (bounded), proving post_question ran.
    let q_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let qs = read_questions_after(&run_dir, None).unwrap();
        if qs
            .iter()
            .any(|q| q.node == "ask" && q.question == "Which DB?")
        {
            break;
        }
        if Instant::now() > q_deadline {
            panic!(
                "timed out after 15s waiting for ask_user to post the question to questions.jsonl"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // With the cadence shortened to every poll, a progress notification for our
    // token must arrive while no answer exists yet (proves the 60 s keep-alive
    // path without waiting a real minute).
    wait_for_line(&rx, "a notifications/progress for token p1", |l| {
        l.contains("notifications/progress") && l.contains("p1")
    });

    // Now answer, and the tool call must return the answer text.
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();

    let result = wait_for_line(&rx, "the ask_user tool result", |l| {
        l.contains("\"id\":2") && l.contains("result")
    });
    assert!(
        result.contains("pg"),
        "ask_user must return the posted answer text, got: {result}"
    );

    // Explicit reap before the guard, so a failure here still leaves the guard
    // to clean up.
    let _ = reaper.child().kill();
    let _ = reaper.child().wait();
}

#[test]
fn sidecar_exits_when_stdin_closes() {
    let (mut reaper, stdin, rx, _tmp, _run_dir) = spawn_sidecar("ask");
    // A pipe reader keeps draining so the child never blocks writing; the
    // handshake is not needed - closing stdin (EOF) alone must end the serve
    // loop and exit the process.
    drop(rx);
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match reaper.child().try_wait().unwrap() {
            Some(status) => {
                assert!(
                    status.success() || status.code().is_some(),
                    "sidecar exited abnormally on stdin close: {status:?}"
                );
                return;
            }
            None => {
                if Instant::now() > deadline {
                    panic!(
                        "timed out after 15s waiting for the sidecar to exit after stdin closed"
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn missing_run_dir_exits_nonzero_naming_the_var() {
    let out = Command::new(env!("CARGO_BIN_EXE_apb"))
        .args([
            "__ask-server",
            "--run",
            "runx",
            "--node",
            "ask",
            "--attempt",
            "1",
        ])
        .env_remove("APB_RUN_DIR")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "must exit non-zero without APB_RUN_DIR"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("APB_RUN_DIR"),
        "stderr must name APB_RUN_DIR, got: {stderr}"
    );
}

#[test]
fn mismatched_run_dir_basename_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    // basename is `other`, but --run says `runx`: a mis-injected sidecar.
    let dir = tmp.path().join("other");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_apb"))
        .args([
            "__ask-server",
            "--run",
            "runx",
            "--node",
            "ask",
            "--attempt",
            "1",
        ])
        .env("APB_RUN_DIR", &dir)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "must exit non-zero on basename mismatch"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("APB_RUN_DIR"),
        "stderr must name APB_RUN_DIR, got: {stderr}"
    );
}
