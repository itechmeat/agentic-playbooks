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
use std::process::{Child, ChildStdin, Stdio};
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

use crate::common::RunGuard;
use crate::common::sig::{self, alive, pgid_of};

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

/// Waits until the detached driver has started the run's node: by then it has
/// exec'd and claimed `driver.pid` itself.
fn wait_until_driving(run_dir: &Path) {
    poll_until("the driver to start the node", || {
        read_all(run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::NodeStarted { .. }))
            .then_some(())
    });
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
    crate::common::apb_std()
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
    let _guard = RunGuard::new(dir.path(), &run_id);
    // Release the workdir lock the way a parent that failed to spawn would;
    // the child then takes it itself.
    drop(prepared);

    let out = crate::common::apb_std()
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
    let run_id = mcp.run_background("bgsurvive");

    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let _guard = RunGuard::new(dir.path(), &run_id);

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

    // The pid the launcher published is the driver's own: once the child is
    // driving (and has claimed `driver.pid` itself) the file still names it.
    // A wrapper process in between would leave the child's pid here instead,
    // and the workdir handover and every liveness check would aim at the
    // wrong process.
    wait_until_driving(&run_dir);
    assert_eq!(
        apb_engine::driver::read_driver_pid(&run_dir),
        Some(driver_pid),
        "the published pid must be the pid of the process that drives the run"
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

// Scenario 2c: a driver killed mid-run must read as DEAD. The launcher (the
// long-lived `apb mcp`) reaps its driver handles, so a killed driver's pid is
// released instead of lingering as a zombie - and `kill -0`, which is how
// liveness is checked here and in `workdir`, succeeds for a zombie. Without
// reaping, a driver that was SIGKILLed mid-run would read as alive for the
// rest of the launcher's session, and the stuck run it left behind could never
// be recognised as recoverable - which is exactly the signal Tasks 8 and 9
// build on.
#[test]
fn a_killed_driver_is_reaped_and_stops_reading_as_alive() {
    let dir = tempfile::tempdir().unwrap();
    // Long enough that the driver is certainly still running when killed.
    let (yaml, script) = slowscript_yaml("reapme", 30);
    seed(dir.path(), "reapme", &yaml, &script);

    let mut mcp = McpSession::start(dir.path());
    let run_id = mcp.run_background("reapme");
    let _guard = RunGuard::new(dir.path(), &run_id);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let pid = apb_engine::driver::read_driver_pid(&run_dir).expect("driver.pid");
    wait_until_driving(&run_dir);
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
    let run_id = mcp.run_background("stopwindow");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let _guard = RunGuard::new(dir.path(), &run_id);

    // No polling: whoever holds the run_id holds it the moment the call
    // returned, and by then the run must already name its driver.
    let pid = apb_engine::driver::read_driver_pid(&run_dir).expect(
        "the spawning parent must publish driver.pid before it returns the run_id, or a stop \
         issued right here sees no driver and finalizes a run that is about to execute",
    );

    // The premise, asserted rather than assumed: the run must still be in
    // flight at the instant the stop is issued. The node sleeps 30s, so this
    // holds by construction, and pinning it here is what makes the outcome
    // check below unambiguous - see the note on `AlreadyTerminal`.
    let before = RunState::fold(&read_all(&run_dir).unwrap()).run_status;
    assert_eq!(
        before,
        RunStatus::Running,
        "the premise of this test is a run still in flight when the stop lands; \
         it was already {before:?}, so the spawn window was never exercised"
    );

    // No polling, no waiting for the child to come up: the stop lands in the
    // window on purpose.
    let outcome = apb_engine::stop_run(dir.path(), &run_id).unwrap();
    // What must NEVER happen is `FinalizedDeadRun`: that is the defect this
    // test exists for, the stop looking at a run whose driver was just spawned,
    // concluding nothing is driving it, and writing the terminal event itself.
    //
    // Both other outcomes mean the driver owned the abort, which is the
    // property:
    //   * `SignaledLiveDriver` - the stop saw the driver and left the terminal
    //     event to it. This is what macOS reports.
    //   * `AlreadyTerminal` - the driver was quicker than the stop. `stop_run`
    //     posts the Abort BEFORE it probes for a driver, so on a fast host the
    //     driver can exec, read the Abort at the top of its drive loop, write
    //     `RunAborted`, drop `driver.pid` and exit inside that gap; the probe
    //     then finds no pid and the re-read finds the run terminal. Linux CI
    //     reports this. It is the driver applying the stop itself, which is
    //     precisely what the fix was for.
    //
    // The assertion above is what keeps this honest: `AlreadyTerminal` would
    // ALSO be returned by `stop_run`'s entry check for a run that was terminal
    // before the stop ran at all, and that would mean the premise had
    // collapsed. Proving the run was `Running` one statement earlier rules out
    // most of that, but not quite all of it: a driver that fails to exec and
    // journals a terminal FAILURE in the same window would also land here with
    // the premise quietly gone. What excludes that is downstream - the
    // `poll_until` below waits for any terminal state, but the `assert_eq!`
    // straight after it requires that state to be `Aborted`, and the check
    // after that requires exactly one `RunAborted` in the journal. A driver
    // that never ran could not have aborted anything, so a collapsed premise
    // fails there rather than passing quietly. Those assertions, not this one,
    // are what make the outcome check safe to relax.
    assert!(
        matches!(
            outcome,
            apb_engine::StopOutcome::SignaledLiveDriver | apb_engine::StopOutcome::AlreadyTerminal
        ),
        "the driver was spawned before the stop, so the stop must leave the terminal event to \
         it, not declare the run dead and finalize it: got {outcome:?}"
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

// Scenario 2e: a run started from the dashboard survives the dashboard.
//
// The dashboard used to drive its runs on a thread of its own process, while
// the CLI and MCP hand theirs to a detached driver. Every dashboard restart
// (and on a dev box the service restarts on every `apb` reinstall) took each
// run it had started down with it, leaving a `running` journal that nothing
// would ever finish. Kill the dashboard mid-run: the run must still finish.
#[test]
fn a_dashboard_run_survives_the_dashboard_being_killed() {
    let cfg = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("dashsurvive", 3);
    seed(dir.path(), "dashsurvive", &yaml, &script);
    // Register the project in this test's own registry, the way any `apb`
    // command run inside it does, so the global dashboard can address it.
    let listed = crate::common::apb_std()
        .arg("list")
        .current_dir(dir.path())
        .env("APB_CONFIG_DIR", cfg.path())
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .unwrap();
    assert!(listed.status.success(), "apb list failed: {listed:?}");
    let workspace = fs::read_to_string(dir.path().join(".apb/workspace.local"))
        .expect("the project was registered")
        .trim()
        .to_string();

    let mut dashboard = Dashboard::start(cfg.path());
    let run_id = dashboard.start_run("dashsurvive", &workspace);
    let _guard = RunGuard::new(dir.path(), &run_id);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    wait_until_driving(&run_dir);

    dashboard.kill();

    let status = wait_for_outcome(
        &run_dir,
        0,
        "the dashboard's run to finish after the dashboard was killed",
    );
    assert_eq!(status, RunStatus::Succeeded);
}

/// A global `apb dashboard` on a free loopback port, killed on drop.
struct Dashboard {
    child: Child,
    port: u16,
}

impl Dashboard {
    fn start(config_dir: &Path) -> Self {
        // A port probed free can be taken by another test's listener before
        // the dashboard binds it; the dashboard then exits on the bind error,
        // and a fresh port is tried. Three attempts bound it.
        for _ in 0..3 {
            if let Some(dashboard) = Self::try_start(config_dir) {
                return dashboard;
            }
        }
        panic!("the dashboard failed to bind a free port three times");
    }

    fn try_start(config_dir: &Path) -> Option<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = crate::common::apb_std()
            .args(["dashboard", "--no-open", "--port", &port.to_string()])
            .env("APB_CONFIG_DIR", config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        // The guard exists before the first wait that can panic.
        let mut dashboard = Self { child, port };
        // Readiness is this child's own claim on the port, not a bare connect:
        // under a full suite a connect can reach a stranger that took the port
        // (the ConnectionReset this test used to flake on). The dashboard
        // writes `serve.lock` only after its bind succeeded, naming its pid
        // and port.
        let lock = config_dir.join("serve.lock");
        let bound = poll_until("the dashboard to bind its port or exit", || {
            if let Ok(Some(_)) = dashboard.child.try_wait() {
                return Some(false);
            }
            let raw = fs::read(&lock).ok()?;
            let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
            (v["pid"] == pid && v["port"] == port).then_some(true)
        });
        if !bound {
            // Already reaped by `try_wait`, which the drop checks first.
            return None;
        }
        poll_until("the dashboard to accept connections", || {
            std::net::TcpStream::connect(("127.0.0.1", port)).ok()
        });
        Some(dashboard)
    }

    /// `POST /api/playbooks/{id}/run` and the run id it answers with.
    fn start_run(&mut self, id: &str, workspace: &str) -> String {
        let mut conn = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            conn,
            "POST /api/playbooks/{id}/run?workspace={workspace} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
            self.port
        )
        .unwrap();
        let mut response = String::new();
        std::io::Read::read_to_string(&mut conn, &mut response).unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(body)
            .unwrap_or_else(|_| panic!("the run start answered: {response}"));
        json["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no run_id in: {response}"))
            .to_string()
    }

    fn kill(&mut self) {
        sig::kill_pid(self.child.id());
        wait_with_deadline(&mut self.child, REAP_DEADLINE, "the dashboard to die");
    }
}

impl Drop for Dashboard {
    /// Bounded, like `McpSession`'s: this also runs while unwinding.
    fn drop(&mut self) {
        // A child already reaped (killed by the test, or exited on its own)
        // must not be signalled again: its pid may belong to another process
        // by now. `try_wait` answers from the cached status then.
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        sig::kill_pid(self.child.id());
        let deadline = Instant::now() + REAP_DEADLINE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(POLL_STEP),
            }
        }
    }
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

    let out = crate::common::apb_std()
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
    let _guard = RunGuard::new(dir.path(), &run_id);

    let mut mcp = McpSession::start(dir.path());
    let started = Instant::now();
    // The rewritten snapshot is not an approved digest, so the resume needs
    // the acknowledge a start would.
    let body = mcp.call(
        2,
        &format!(
            r#"{{"name":"run_resume","arguments":{{"run_id":"{run_id}","from_node":"work","acknowledge_untrusted":true}}}}"#
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

/// Copies a run directory tree (a run shipped inside a repository is exactly
/// such a copy under a new name).
fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

// MCP `run_resume` holds a resume to the same consent as a start: a run
// directory apb did not create here (one that came with the repository) is
// refused even with an acknowledge, and a run whose snapshot is not approved
// needs the acknowledge.
#[test]
fn mcp_run_resume_refuses_a_foreign_run_and_an_unapproved_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("resumegate", 0);
    seed(dir.path(), "resumegate", &yaml, &script);
    let out = crate::common::apb_std()
        .arg("run")
        .arg("resumegate")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let run_id = stdout
        .split_whitespace()
        .find(|w| w.starts_with("resumegate-"))
        .unwrap_or_else(|| panic!("no run id in `apb run` output: {stdout}"))
        .to_string();
    let runs = dir.path().join(".apb/runs");
    copy_dir(&runs.join(&run_id), &runs.join("shipped-1"));
    let _guards = (
        RunGuard::new(dir.path(), &run_id),
        RunGuard::new(dir.path(), "shipped-1"),
    );

    let mut mcp = McpSession::start(dir.path());
    let foreign = mcp.call(
        2,
        r#"{"name":"run_resume","arguments":{"run_id":"shipped-1","from_node":"work","acknowledge_untrusted":true}}"#,
    );
    assert_eq!(
        foreign["policy_refusal"]["policy"], "run_not_created_locally",
        "got: {foreign}"
    );
    let unapproved = mcp.call(
        3,
        &format!(
            r#"{{"name":"run_resume","arguments":{{"run_id":"{run_id}","from_node":"work"}}}}"#
        ),
    );
    assert_eq!(
        unapproved["policy_refusal"]["policy"], "untrusted_requires_acknowledge",
        "got: {unapproved}"
    );
    mcp.kill();
}

// A long-running `apb mcp` (an agent session's MCP server) keeps starting
// background runs after `apb` is reinstalled under it. Reinstalling replaces
// the file, so the running process's own executable reads as deleted, and a
// driver re-exec'd from that path used to fail to spawn.
#[test]
fn mcp_background_run_starts_after_the_binary_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("afterreinstall", 0);
    seed(dir.path(), "afterreinstall", &yaml, &script);

    let bin_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let exe = bin_dir.path().join("apb");
    fs::copy(crate::common::apb_bin(), &exe).unwrap();
    let mut mcp = McpSession::start_with(dir.path(), crate::common::apb_std_from(&exe));

    // The reinstall: a new file at the same path, as `cargo install` does.
    fs::remove_file(&exe).unwrap();
    fs::copy(crate::common::apb_bin(), &exe).unwrap();

    let run_id = mcp.run_background("afterreinstall");
    let _guard = RunGuard::new(dir.path(), &run_id);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    let status = wait_for_outcome(&run_dir, 0, "the run started after the reinstall to finish");
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
        Self::start_with(root, crate::common::apb_std())
    }

    fn start_with(root: &Path, mut cmd: std::process::Command) -> Self {
        cmd.arg("mcp")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Its own process group, so a test can kill the launcher's whole
            // group without also signalling the cargo test runner.
            .process_group(0);
        // A binary this test just copied can read as busy (ETXTBSY) while a
        // process another test forked still holds the copy's write handle.
        let mut child = apb_core::fsutil::spawn_when_not_busy(&mut cmd).unwrap();
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

    /// Starts playbook `id` as a background run and returns its run id.
    fn run_background(&mut self, id: &str) -> String {
        let body = self.call(
            2,
            &format!(
                r#"{{"name":"playbook_run","arguments":{{"id":"{id}","background":true,"acknowledge_untrusted":true}}}}"#
            ),
        );
        body["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no run_id in playbook_run response: {body}"))
            .to_string()
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
        // A child already reaped must not be signalled again (its pid may be
        // reused); `try_wait` answers from the cached status then.
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
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

/// Prepares a run of `id` the way a launcher does and releases its workdir
/// lock, leaving a run directory a `__drive-run` can pick up.
fn prepare_released(root: &Path, id: &str) -> String {
    let prepared = apb_engine::prepare_supervised_background(
        root,
        id,
        None,
        apb_engine::RunOptions::default(),
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    drop(prepared);
    run_id
}

// Issue #139 F13: a driver whose workspace is deleted mid-node stops after the
// node without re-creating the run directory (context.md, outputs, the control
// cursor were all written with create_dir_all).
#[test]
fn a_driver_does_not_recreate_a_workspace_deleted_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let (yaml, script) = slowscript_yaml("midrun", 2);
    seed(dir.path(), "midrun", &yaml, &script);
    let run_id = prepare_released(dir.path(), "midrun");
    let _guard = RunGuard::new(dir.path(), &run_id);
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    let mut driver = crate::common::apb_std()
        .arg("__drive-run")
        .arg("--root")
        .arg(dir.path())
        .arg("--run-id")
        .arg(&run_id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until_driving(&run_dir);
    fs::remove_dir_all(dir.path()).unwrap();
    wait_with_deadline(&mut driver, POLL_DEADLINE, "the driver to stop");

    assert!(
        !dir.path().exists(),
        "the driver re-created the deleted workspace: {:?}",
        walk(dir.path())
    );
}

/// Every path under `root`, for a failure message.
fn walk(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&p) {
            for e in rd.flatten() {
                stack.push(e.path());
            }
        }
        out.push(p);
    }
    out
}
