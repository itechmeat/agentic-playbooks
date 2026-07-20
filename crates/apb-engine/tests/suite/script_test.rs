use apb_engine::script::run_script;
use apb_engine::state::NodeStatus;
use std::fs;
use std::time::Duration;

fn write_script(dir: &std::path::Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

#[test]
fn sh_script_success_captures_stdout() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/ok.sh", "echo hello-script");
    let r = run_script(
        ver.path(),
        work.path(),
        "scripts/ok.sh",
        "sh",
        Some(Duration::from_secs(10)),
        None,
    )
    .unwrap();
    assert_eq!(r.status, NodeStatus::Succeeded);
    assert_eq!(r.stdout, "hello-script");
}

#[test]
fn sh_script_nonzero_is_failed() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/bad.sh", "echo oops; exit 2");
    let r = run_script(
        ver.path(),
        work.path(),
        "scripts/bad.sh",
        "sh",
        Some(Duration::from_secs(10)),
        None,
    )
    .unwrap();
    assert_eq!(r.status, NodeStatus::Failed);
}

#[test]
fn sh_script_timeout_is_timed_out() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/slow.sh", "sleep 5");
    let start = std::time::Instant::now();
    let r = run_script(
        ver.path(),
        work.path(),
        "scripts/slow.sh",
        "sh",
        Some(Duration::from_millis(300)),
        None,
    )
    .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(r.status, NodeStatus::TimedOut);
    // The timeout must fire quickly, not after the script itself would have
    // finished on its own sleep 5 - otherwise the timeout guarantee is not proven.
    assert!(elapsed < Duration::from_secs(3), "elapsed = {elapsed:?}");
}

// A script that backgrounds a process which inherits its stdout, and then
// exits. The captured-output drain has to be bounded, because EOF on that pipe
// belongs to the background process, not to the script: before the bound,
// `run_capture` finished its `try_wait` loop the moment the script exited and
// then sat in an unbounded `rx_out.recv()` for as long as the background
// process chose to live. A script node that did this hung its run forever.
//
// Note what is deliberately NOT done here. The agent adapter and the detect
// probe both SIGKILL the process group before draining, which releases the
// pipes and rescues the output. `run_capture` must not: a script node that
// starts a long-lived helper (`npm run dev >/dev/null 2>&1 &`) is a legitimate
// pattern that works today precisely because nothing kills the group, and a
// group kill would break it. So the bound is the whole fix here, and the
// documented cost is that a script backgrounding a process WITHOUT redirecting
// its output loses the captured stdout instead of hanging.
//
// This test asserts the bound, the surviving daemon, and the empty stdout that
// is the cost. The marker that makes that cost visible to an operator is
// asserted in the test below, at the `run_capture` layer, because
// `ScriptResult` has no stderr field to carry it up to here.
#[test]
fn script_backgrounding_a_process_on_its_stdout_is_bounded_and_spares_the_daemon() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(
        ver.path(),
        "scripts/daemon.sh",
        "sleep 300 &\necho $! > daemon.pid\necho started",
    );

    let started = std::time::Instant::now();
    // No timeout: this is the configuration in which the old unbounded recv
    // could never be preempted by anything.
    let r = run_script(
        ver.path(),
        work.path(),
        "scripts/daemon.sh",
        "sh",
        None,
        None,
    )
    .unwrap();
    let elapsed = started.elapsed();

    let pid: i32 = fs::read_to_string(work.path().join("daemon.pid"))
        .expect("script should have recorded the background pid")
        .trim()
        .parse()
        .expect("background pid");

    // SAFETY: signal 0 only performs the existence check.
    let daemon_survived = unsafe { libc::kill(pid, 0) } == 0;
    // SAFETY: reap it either way so the test cannot leak a 300-second sleep.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }

    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the output drain was not bounded: {elapsed:?} against a background process sleeping 300s"
    );
    assert_eq!(r.status, NodeStatus::Succeeded);
    assert!(
        daemon_survived,
        "run_capture must not kill the script's process group: backgrounding a \
         long-lived helper is a supported pattern"
    );

    // The cost of sparing the daemon, pinned rather than described. The drain
    // expires with the pipe still open, so stdout comes back EMPTY - not
    // truncated, and `echo started` is nowhere in it - while the node is still
    // Succeeded. Pinning it here is what makes the loss a decision rather than
    // an accident; the marker that reports it is asserted one layer down, in
    // `an_expired_drain_says_so_instead_of_losing_stdout_silently`, because
    // `ScriptResult` has no stderr field to carry it.
    assert_eq!(
        r.stdout, "",
        "an expired drain yields no stdout at all, not partial output"
    );
}

// The same situation one layer down, where the evidence lives. `run_capture`
// is where the drain budget is, so it is where the loss can be named: a
// Succeeded node whose entire stdout silently became "" is indistinguishable
// from a script that printed nothing, and a downstream condition reading that
// output would quietly see the wrong thing.
#[test]
fn an_expired_drain_says_so_instead_of_losing_stdout_silently() {
    let work = tempfile::tempdir().unwrap();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg("sleep 300 &\necho $! > daemon.pid\necho started")
        .current_dir(work.path());

    let captured = apb_engine::proc::run_capture(cmd, None, None).unwrap();

    let pid: i32 = fs::read_to_string(work.path().join("daemon.pid"))
        .expect("script should have recorded the background pid")
        .trim()
        .parse()
        .expect("background pid");
    // SAFETY: `kill` takes no pointers; reap it so the test leaks nothing.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }

    assert_eq!(captured.stdout, "", "the drain expired, so stdout is empty");
    assert!(
        captured
            .stderr
            .contains(apb_engine::proc::DRAIN_LOST_MARKER),
        "the lost stdout must be explained on stderr, got: {:?}",
        captured.stderr
    );
    assert!(
        captured.status.is_some_and(|s| s.success()),
        "the script itself succeeded; only its output was lost"
    );
}
