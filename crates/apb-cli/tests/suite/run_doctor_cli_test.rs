//! Task 9: `apb doctor --run <id>`, the per-run doctor.
//!
//! The environment doctor answers "can this machine run playbooks"; this one
//! answers "what is wrong with THIS run", which is the question an operator
//! actually has when a run has been reading `running` for twenty minutes. It
//! is strictly read-only: it names the problem and repairs nothing.
//!
//! The journals here are hand-built because the interesting states cannot be
//! produced on demand by a real run: a *dead* attempt pid and a *stale*
//! driver.pid are, by definition, processes that are no longer there.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn apb() -> Command {
    Command::cargo_bin("apb").unwrap()
}

/// A pid no process can hold, so the liveness probe answers "no such process".
const DEAD_PID: u32 = u32::MAX;

fn init(dir: &Path) {
    apb().arg("init").current_dir(dir).assert().success();
}

fn run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    let d = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&d).unwrap();
    d
}

/// A wedged run: the attempt pid is dead, driver.pid names a dead process, and
/// one control entry sits past the persisted cursor, never applied. All three
/// must be named, and the run must exit non-zero.
#[test]
fn doctor_run_flags_a_wedged_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "wedged-1");

    fs::write(
        rd.join("events.jsonl"),
        format!(
            "{{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}}\n\
             {{\"seq\":1,\"ts\":2000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}}\n\
             {{\"seq\":2,\"ts\":3000,\"type\":\"attempt_started\",\"node\":\"a\",\"attempt\":1,\"agent\":\"stub\",\"pid\":{DEAD_PID}}}\n\
             {{\"seq\":3,\"ts\":4000,\"type\":\"supervisor_action\",\"action\":\"retry\",\"node\":\"a\",\"detail\":\"\"}}\n\
             {{\"seq\":4,\"ts\":5000,\"type\":\"supervisor_action\",\"action\":\"retry\",\"node\":\"a\",\"detail\":\"\"}}\n"
        ),
    )
    .unwrap();
    // A stale driver.pid: the sole reason `stop_run` would refuse to finalize.
    fs::write(rd.join("driver.pid"), DEAD_PID.to_string()).unwrap();
    // One control entry posted and never consumed (no control.cursor at all).
    fs::write(
        rd.join("control.jsonl"),
        "{\"seq\":0,\"cmd\":\"abort\",\"reason\":\"stop requested\"}\n",
    )
    .unwrap();

    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("wedged-1")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stdout(predicate::str::contains("[fail]"))
        .stdout(predicate::str::contains("driver"))
        .stdout(predicate::str::contains("attempt"))
        .stdout(predicate::str::contains("control"))
        .stdout(predicate::str::contains("[warn]"));
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(&DEAD_PID.to_string()),
        "the dead pid must be named so an operator can correlate it: {stdout}"
    );
    assert!(
        stdout.to_lowercase().contains("supervisor"),
        "the duplicate supervisor_action must be reported: {stdout}"
    );
}

/// A run that finished cleanly: no open attempts, no driver.pid, no control
/// backlog. Every check reports ok and the command exits zero.
#[test]
fn doctor_run_reports_all_ok_for_a_healthy_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "healthy-1");
    fs::write(
        rd.join("events.jsonl"),
        "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n\
         {\"seq\":1,\"ts\":2000,\"type\":\"node_started\",\"node\":\"a\",\"attempt\":1}\n\
         {\"seq\":2,\"ts\":3000,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n\
         {\"seq\":3,\"ts\":4000,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}\n",
    )
    .unwrap();

    let out = apb()
        .arg("doctor")
        .arg("--run")
        .arg("healthy-1")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"));
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("[fail]") && !stdout.contains("[warn]"),
        "a healthy completed run must print only ok checks: {stdout}"
    );
}

/// An unknown run is an error, not an empty ok report.
#[test]
fn doctor_run_rejects_an_unknown_run() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    apb()
        .arg("doctor")
        .arg("--run")
        .arg("ghost-1")
        .current_dir(dir.path())
        .assert()
        .failure();
}

/// The environment doctor is untouched: a bare `apb doctor` still runs the
/// whole-project checks.
#[test]
fn bare_doctor_still_checks_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    apb()
        .arg("doctor")
        .current_dir(dir.path())
        .assert()
        .stdout(predicate::str::contains("[ok]"));
}
