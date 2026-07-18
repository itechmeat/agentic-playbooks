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
