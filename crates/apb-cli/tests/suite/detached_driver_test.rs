//! Task 7: detached run drivers, end to end through the real `apb` binary.
//!
//! This is the only crate that can reach the shipped binary
//! (`CARGO_BIN_EXE_apb`), and the binary is exactly what
//! `apb_engine::driver::spawn_detached_driver` re-execs, so the "the run
//! outlives the process that started it" property is proven here and nowhere
//! else. Each scenario deliberately kills the launching process and then keeps
//! polling the run directory: a run that only completes because the parent
//! stayed alive would fail these tests.
//!
//! The stdio JSON-RPC plumbing (handshake, background reader thread, bounded
//! `recv_timeout` instead of a blocking read) follows `mcp_supervise_test.rs`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use apb_engine::event::{EventPayload, read_all};
use apb_engine::state::{RunState, RunStatus};

const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_STEP: Duration = Duration::from_millis(50);
/// How long a process gets to die after being SIGKILLed. SIGKILL cannot be
/// caught or ignored, so this is only ever reached when the signal did not
/// reach the process at all - which is exactly the failure that has to be
/// reported rather than waited out.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if start.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Every process signal and liveness check in this module is a syscall, not a
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
mod sig {
    /// SIGKILLs a single process.
    pub fn kill_pid(pid: u32) {
        // SAFETY: `kill` takes no pointers; an unknown pid is ESRCH.
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }

    /// SIGKILLs every process in the group led by `pid`.
    pub fn kill_group(pid: u32) {
        // SAFETY: as above; a negative pid addresses the process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
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

use sig::{alive, pgid_of};

/// `child.wait()` with a deadline, and a message naming what the wait was for.
///
/// The suite has no unbounded process wait left: every child it waits on dies
/// only because the test signalled it, so a signal that fails to land must
/// surface as a named failure rather than as a hang. `Child::wait` has no
/// timed form, hence the `try_wait` loop.
fn wait_with_deadline(child: &mut Child, budget: Duration, what: &str) {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out after {budget:?} waiting for: {what} (pid {} is still running)",
                    child.id()
                );
                std::thread::sleep(POLL_STEP);
            }
            Err(e) => panic!("wait failed while waiting for {what}: {e}"),
        }
    }
}

/// Kills the detached driver if a test bails out before the run finished.
/// Nothing else would: the driver deliberately outlives every process these
/// tests control, so a panicking `poll_until` would otherwise leave a live
/// `sleep` running against a tempdir that is about to be deleted. On the happy
/// path `driver.pid` is already gone and this is a no-op.
struct DriverReaper {
    run_dir: PathBuf,
}

impl Drop for DriverReaper {
    fn drop(&mut self) {
        if let Some(pid) = apb_engine::driver::read_driver_pid(&self.run_dir) {
            // The driver leads its own group, so this also takes down the
            // script it is running. Not waited on: the driver is not our
            // child (it was re-exec'd by another process), so there is no
            // handle to reap and nothing that could block here.
            sig::kill_group(pid);
            sig::kill_pid(pid);
        }
    }
}

fn finishes(run_dir: &Path) -> usize {
    read_all(run_dir)
        .unwrap_or_default()
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
        .count()
}

/// Waits until the run has journalled MORE than `already` terminal events and
/// returns the folded status. A resume adds a second `run_finished`, so the
/// baseline count is what tells a fresh finish from the one already on disk.
fn wait_for_outcome(run_dir: &Path, already: usize, what: &str) -> RunStatus {
    poll_until(what, || {
        let events = read_all(run_dir).ok()?;
        let n = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .count();
        if n <= already {
            return None;
        }
        Some(RunState::fold(&events).run_status)
    })
}

/// A run whose single script node sleeps, so the run is provably still in
/// flight when the launching process is killed. `SLEEP` seconds is long enough
/// to make the kill land mid-run and short enough to keep the suite quick.
fn slowscript_yaml(id: &str, sleep_seconds: u32) -> (String, String) {
    (
        format!(
            r#"
schema: 1
id: {id}
name: Slow Script
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - {{ id: work, type: script, script: "scripts/work.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: work }}
  - {{ from: work, to: done }}
"#
        ),
        format!("#!/bin/sh\nsleep {sleep_seconds}\n"),
    )
}

fn seed(root: &Path, id: &str, playbook: &str, script: &str) {
    Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), playbook).unwrap();
    fs::write(vdir.join("scripts/work.sh"), script).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

// Scenario 1: the hidden `__drive-run` subcommand re-opens a run that another
// process prepared and drives it to completion. The parent here does nothing
// but prepare; every byte the child needs comes out of `runs/<id>`.
#[test]
fn drive_run_subcommand_completes_a_run_prepared_by_another_process() {
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("driveme", 1);
    seed(dir.path(), "driveme", &yaml, &script);

    let prepared = apb_engine::prepare_supervised_background(
        dir.path(),
        "driveme",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    // Release the workdir lock the way a parent that failed to spawn would;
    // the child then takes it itself.
    drop(prepared);

    let out = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("__drive-run")
        .arg("--root")
        .arg(dir.path())
        .arg("--run-id")
        .arg(&run_id)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "__drive-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let _reaper = DriverReaper {
        run_dir: run_dir.clone(),
    };
    let status = wait_for_outcome(&run_dir, 0, "the driven run to reach a terminal event");
    assert_eq!(status, RunStatus::Succeeded);
}

// Scenario 2: a background `playbook_run` started over MCP survives its
// launcher's whole PROCESS GROUP being killed. This is the production incident
// the task exists for, and the group is the part that matters: a host that
// tears down its subtree with `kill(-pgid)`, or a closed terminal SIGHUPing
// its foreground group, reaches every process that shares the launcher's
// group. A driver that merely has its own pid but inherits that group dies
// right along with the launcher, leaving the run no safer than the in-process
// thread it replaced - so this test signals the group, not the pid.
#[test]
fn mcp_background_run_survives_a_group_kill_of_the_mcp_process() {
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("bgsurvive", 3);
    seed(dir.path(), "bgsurvive", &yaml, &script);

    // `apb mcp` leads its own group, so the group kill below cannot reach the
    // test runner itself.
    let mut mcp = McpSession::start(dir.path());
    let body = mcp.call(
        2,
        r#"{"name":"playbook_run","arguments":{"id":"bgsurvive","background":true,"acknowledge_untrusted":true}}"#,
    );
    let run_id = body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no run_id in playbook_run response: {body}"))
        .to_string();

    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let _reaper = DriverReaper {
        run_dir: run_dir.clone(),
    };

    // The run is still in flight (the script sleeps 3s): the driver must be a
    // process of its own, not a thread of the MCP server.
    let driver_pid = poll_until("driver.pid to name the detached driver process", || {
        apb_engine::driver::read_driver_pid(&run_dir)
    });
    assert_ne!(
        driver_pid,
        std::process::id(),
        "the driver must not be this test process"
    );
    assert_ne!(
        driver_pid,
        mcp.pid(),
        "the driver must be a separate process from `apb mcp`, not a thread inside it"
    );

    // ... and in its own process group, which is what makes the group kill
    // below survivable.
    let mcp_pgid = pgid_of(mcp.pid()).expect("mcp process group");
    let driver_pgid = pgid_of(driver_pid).expect("driver process group");
    assert_eq!(
        driver_pgid, driver_pid,
        "the driver must lead its own process group"
    );
    assert_ne!(
        driver_pgid, mcp_pgid,
        "the driver must not share its launcher's process group"
    );

    // Kill the launcher's entire group, mid-run.
    mcp.kill_group();
    poll_until(
        "the MCP process to actually die from the group kill",
        || {
            if alive(mcp.pid()) { None } else { Some(()) }
        },
    );

    let status = wait_for_outcome(
        &run_dir,
        0,
        "the detached run to finish after its launcher's process group was killed",
    );
    assert_eq!(
        status,
        RunStatus::Succeeded,
        "the run must complete on its own after its launcher's group was killed"
    );
}

// Scenario 2b: the contract of `spawn_driver_at` (and so of
// `spawn_detached_driver`, which is the same function with `current_exe()`)
// asserted directly, since every other scenario reaches it indirectly through
// the MCP server. Three promises: the returned pid is the driver's own pid -
// the SAME one that lands in `driver.pid` and that liveness checks read - the
// driver leads its own process group, and it completes the run with the caller
// doing nothing but wait.
#[test]
fn spawn_driver_at_returns_the_driver_pid_and_drives_the_run_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("spawnme", 2);
    seed(dir.path(), "spawnme", &yaml, &script);

    let prepared = apb_engine::prepare_supervised_background(
        dir.path(),
        "spawnme",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    let pid = apb_engine::driver::spawn_driver_at(
        Path::new(env!("CARGO_BIN_EXE_apb")),
        dir.path(),
        &run_id,
        None,
        false,
    )
    .unwrap();
    // Hand the lock across exactly as `start_detached` does, so the driver
    // adopts it rather than waiting the handover window out.
    prepared.hand_over_workdir_lock(pid).unwrap();

    let _reaper = DriverReaper {
        run_dir: run_dir.clone(),
    };

    // The returned pid is the driver itself: whatever it publishes as
    // `driver.pid` must be the very pid we were handed, or the workdir
    // handover and every downstream liveness check are aimed at the wrong
    // process.
    let published = poll_until("the driver to publish driver.pid", || {
        apb_engine::driver::read_driver_pid(&run_dir)
    });
    assert_eq!(
        published, pid,
        "spawn_driver_at must return the pid that ends up in driver.pid"
    );
    assert_eq!(
        pgid_of(pid),
        Some(pid),
        "the driver must lead its own process group"
    );

    // The caller drives nothing - it only waits.
    let status = wait_for_outcome(&run_dir, 0, "the spawned driver to finish the run alone");
    assert_eq!(status, RunStatus::Succeeded);
}

// Scenario 2c: a driver killed mid-run must read as DEAD. The launcher reaps
// its driver handles, so a killed driver's pid is released instead of lingering
// as a zombie - and `kill -0`, which is how liveness is checked here and in
// `workdir`, succeeds for a zombie. Without reaping, a driver that was
// SIGKILLed mid-run would read as alive for the rest of the launcher's
// session, and the stuck run it left behind could never be recognised as
// recoverable - which is exactly the signal Tasks 8 and 9 build on.
#[test]
fn a_killed_driver_is_reaped_and_stops_reading_as_alive() {
    let dir = tempfile::tempdir().unwrap();
    // Long enough that the driver is certainly still running when killed.
    let (yaml, script) = slowscript_yaml("reapme", 30);
    seed(dir.path(), "reapme", &yaml, &script);

    let prepared = apb_engine::prepare_supervised_background(
        dir.path(),
        "reapme",
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // This test process is the launcher, so it owns the reaper.
    let pid = apb_engine::driver::spawn_driver_at(
        Path::new(env!("CARGO_BIN_EXE_apb")),
        dir.path(),
        &run_id,
        None,
        false,
    )
    .unwrap();
    prepared.hand_over_workdir_lock(pid).unwrap();

    poll_until("the driver to start driving", || {
        apb_engine::driver::read_driver_pid(&run_dir)
    });
    assert!(alive(pid), "the driver should be alive before we kill it");

    sig::kill_pid(pid);

    // Unreaped, the pid stays a zombie and signal 0 keeps succeeding forever.
    poll_until(
        "the killed driver's pid to be reaped and stop reading as alive",
        || if alive(pid) { None } else { Some(()) },
    );

    // And it left the evidence Tasks 8/9 need: a driver.pid naming a pid that
    // is provably gone, on a run that never finished.
    assert_eq!(
        apb_engine::driver::read_driver_pid(&run_dir),
        Some(pid),
        "a killed driver leaves its driver.pid behind - that is the stale marker"
    );
    assert_eq!(
        finishes(&run_dir),
        0,
        "the killed run must not have finished"
    );
}

// Scenario 2d: a stop issued in the SPAWN WINDOW must not be discarded.
//
// `driver.pid` used to be written by the child, inside `drive`, so between the
// spawn returning and the child getting through a full exec (easily 100ms)
// nothing named the driver. A `run_stop` landing there saw no driver, took the
// dead-run branch, finalized the run with `RunAborted` and advanced the control
// cursor past its own Abort - and the child then started, saw neither the Abort
// (both its watcher and its top-of-loop scan begin at that advanced cursor) nor
// any reason to stop, and executed the whole run past its terminal event. An
// agent that gets a run_id from `playbook_run` and immediately calls `run_stop`
// hits exactly this window. The parent now publishes the pid before it returns,
// so the stop sees a live driver and the driver applies the abort itself.
#[test]
fn a_stop_in_the_driver_spawn_window_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    // Long enough that a run which ignored the stop would still be sleeping
    // well past the assertions below.
    let (yaml, script) = slowscript_yaml("stopwindow", 30);
    seed(dir.path(), "stopwindow", &yaml, &script);

    // The real production path: `playbook_run` with background:true goes
    // through `hand_to_detached_driver`, and the tool call returns the instant
    // that function does - which is precisely the window under test.
    let mut mcp = McpSession::start(dir.path());
    let body = mcp.call(
        2,
        r#"{"name":"playbook_run","arguments":{"id":"stopwindow","background":true,"acknowledge_untrusted":true}}"#,
    );
    let run_id = body["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no run_id in playbook_run response: {body}"))
        .to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let _reaper = DriverReaper {
        run_dir: run_dir.clone(),
    };

    // No polling: whoever holds the run_id holds it the moment the call
    // returned, and by then the run must already name its driver.
    let pid = apb_engine::driver::read_driver_pid(&run_dir).expect(
        "the spawning parent must publish driver.pid before it returns the run_id, or a stop \
         issued right here sees no driver and finalizes a run that is about to execute",
    );

    // No polling, no waiting for the child to come up: the stop lands in the
    // window on purpose.
    let outcome = apb_engine::stop_run(dir.path(), &run_id).unwrap();
    assert_eq!(
        outcome,
        apb_engine::StopOutcome::SignaledLiveDriver,
        "the driver was spawned before the stop, so the stop must signal it rather than declare the run dead"
    );

    // Not `wait_for_outcome`: an aborted run journals `RunAborted`, not
    // `RunFinished`, so the terminal event to wait for is the abort itself.
    let status = poll_until("the stopped run to reach a terminal event", || {
        let events = read_all(&run_dir).ok()?;
        let folded = RunState::fold(&events).run_status;
        matches!(
            folded,
            RunStatus::Aborted | RunStatus::Succeeded | RunStatus::Failed
        )
        .then_some(folded)
    });
    assert_eq!(
        status,
        RunStatus::Aborted,
        "a run stopped in the spawn window must end aborted"
    );

    // And it must really have STOPPED: nothing may be journalled after the
    // terminal event, and the sleeping script must never have completed.
    poll_until("the driver process to exit", || {
        if alive(pid) { None } else { Some(()) }
    });
    let events = read_all(&run_dir).unwrap();
    let aborts = events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
        .count();
    assert_eq!(aborts, 1, "the abort must be applied exactly once");
    let terminal_at = events
        .iter()
        .position(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
        .expect("a RunAborted");
    assert_eq!(
        terminal_at,
        events.len() - 1,
        "nothing may be written after the run was finalized, got {:?}",
        events[terminal_at + 1..]
            .iter()
            .map(|e| &e.payload)
            .collect::<Vec<_>>()
    );
    assert!(
        !events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "work" && status == "succeeded"
        )),
        "the stopped run must not have completed its sleeping node"
    );
}

// Scenario 3: `run_resume` acknowledges immediately and the resumed run then
// completes without the caller. The resumed node sleeps 10s, so an ack that
// arrives in well under 5s can only mean the drive was handed to another
// process; killing the MCP server right after the ack then proves the run does
// not depend on it.
#[test]
fn mcp_run_resume_acks_immediately_and_the_run_completes_detached() {
    let dir = tempfile::tempdir().unwrap();
    // A quick first run, so there is a finished run on disk to resume into.
    let (yaml, script) = slowscript_yaml("resumeme", 0);
    seed(dir.path(), "resumeme", &yaml, &script);

    let out = Command::new(env!("CARGO_BIN_EXE_apb"))
        .arg("run")
        .arg("resumeme")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the seeded first run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .split_whitespace()
        .find(|w| w.starts_with("resumeme-"))
        .unwrap_or_else(|| panic!("no run id in `apb run` output: {stdout}"))
        .to_string();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // The resumed attempt takes a long time. Scripts execute from the run's
    // own snapshot, so rewriting it here is what the resumed node will run.
    fs::write(run_dir.join("scripts/work.sh"), "#!/bin/sh\nsleep 10\n").unwrap();
    let before = finishes(&run_dir);
    let _reaper = DriverReaper {
        run_dir: run_dir.clone(),
    };

    let mut mcp = McpSession::start(dir.path());
    let started = Instant::now();
    let body = mcp.call(
        2,
        &format!(
            r#"{{"name":"run_resume","arguments":{{"run_id":"{run_id}","from_node":"work"}}}}"#
        ),
    );
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "run_resume must ack immediately, but blocked for {elapsed:?} (the resumed node sleeps 10s)"
    );
    assert_eq!(body["run_id"].as_str(), Some(run_id.as_str()));
    assert_eq!(body["resumed_from"].as_str(), Some("work"));
    assert_eq!(body["reason"].as_str(), Some("explicit_from_node"));
    assert_eq!(body["detached"].as_bool(), Some(true));

    // And the run really does proceed without the caller.
    mcp.kill();
    let status = wait_for_outcome(
        &run_dir,
        before,
        "the resumed run to finish after the MCP process was killed",
    );
    assert_eq!(status, RunStatus::Succeeded);
}

// --- minimal stdio MCP client -------------------------------------------------

/// A live `apb mcp` child spoken to over stdio, with the initialize handshake
/// already done. `call` issues one `tools/call` and returns the tool's own
/// (double-encoded) JSON body.
struct McpSession {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
}

impl McpSession {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_apb"))
            .arg("mcp")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so a test can kill the launcher's whole
            // group without also signalling the cargo test runner.
            .process_group(0)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(line.clone()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05","capabilities":{{}},"clientInfo":{{"name":"test","version":"0"}}}}}}"#
        )
        .unwrap();
        stdin.flush().unwrap();
        rx.recv_timeout(Duration::from_secs(20))
            .expect("no response to initialize");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        stdin.flush().unwrap();

        Self { child, stdin, rx }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn call(&mut self, id: u32, params: &str) -> serde_json::Value {
        writeln!(
            self.stdin,
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{params}}}"#
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let line = self
            .rx
            .recv_timeout(Duration::from_secs(20))
            .expect("no response to tools/call");
        assert!(
            !line.contains("\"isError\":true"),
            "tools/call returned an error: {line}"
        );
        let outer: serde_json::Value = serde_json::from_str(&line).expect("json-rpc response");
        let text = outer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool body in: {line}"));
        serde_json::from_str(text).expect("tool body json")
    }

    fn kill(&mut self) {
        sig::kill_pid(self.child.id());
        wait_with_deadline(
            &mut self.child,
            REAP_DEADLINE,
            "the `apb mcp` process to die",
        );
    }

    /// Kills the launcher's entire process group, the way a host tears down a
    /// subtree. Anything that inherited this group dies with it.
    ///
    /// `apb mcp` never exits on its own - it is a stdio server, and this
    /// struct still holds its stdin open - so the group signal is the only
    /// thing that can end it. A signal that fails to land therefore has to
    /// fail loudly here, which is what the deadline is for.
    fn kill_group(&mut self) {
        sig::kill_group(self.child.id());
        wait_with_deadline(
            &mut self.child,
            REAP_DEADLINE,
            "the `apb mcp` process to die from the kill of its process group",
        );
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        // Never leave an `apb mcp` child behind when a test fails early. Drop
        // runs during unwinding too, so this must not be able to hang: a
        // second panic while panicking aborts the process, and an unbounded
        // wait here would bury the original failure under a hang instead.
        sig::kill_pid(self.child.id());
        let deadline = Instant::now() + REAP_DEADLINE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(POLL_STEP),
            }
        }
        eprintln!(
            "warning: `apb mcp` (pid {}) did not die within {REAP_DEADLINE:?} of SIGKILL",
            self.child.id()
        );
    }
}
