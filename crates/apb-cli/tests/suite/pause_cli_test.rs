//! Issue #200: the operator surfaces of a pause through the real `apb`
//! binary: `apb pause`, the pause marker of `apb runs` (list and detail),
//! `apb wait` exit 4 with the reason, and `apb resume` before `until`.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::common::RunGuard;

const DEADLINE: Duration = Duration::from_secs(60);

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

const PLAYBOOK: &str = r#"
schema: 1
id: pausable
name: Pausable
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: s, type: script, script: "scripts/s.sh", runner: sh }
  - { id: t, type: script, script: "scripts/t.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: s }
  - { from: s, to: t }
  - { from: t, to: done }
"#;

fn seed(root: &Path) {
    crate::common::apb_std()
        .arg("init")
        .current_dir(root)
        .output()
        .unwrap();
    let vdir = root.join(".apb/playbooks/pausable/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
    let d = root.display();
    fs::write(
        vdir.join("scripts/s.sh"),
        format!("touch '{d}/s_running'\nwhile [ -f '{d}/slow' ]; do sleep 0.05; done\necho s\n"),
    )
    .unwrap();
    fs::write(vdir.join("scripts/t.sh"), "echo t\n").unwrap();
    fs::write(root.join(".apb/playbooks/pausable/current"), "1.0.0").unwrap();
}

fn apb(root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = crate::common::apb_std()
        .args(args)
        .env("APB_WAKER_POLL_MS", "50")
        .current_dir(root)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn pause_runs_wait_and_resume_report_the_pause() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    fs::write(root.join("slow"), "").unwrap();
    let (code, stdout, stderr) = apb(root, &["run", "pausable", "--detach"]);
    assert_eq!(code, 0, "{stdout} {stderr}");
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let _guard = RunGuard::new(root, &run_id);
    poll("the script to start", || {
        root.join("s_running").is_file().then_some(())
    });

    // Bad arguments are refused before anything is posted.
    let (code, _, err) = apb(root, &["pause", &run_id, "--until", "someday"]);
    assert_eq!(code, 2);
    assert!(err.contains("until"), "{err}");
    let (code, _, _) = apb(root, &["pause", &run_id, "--drain", "--now"]);
    assert_ne!(code, 0, "--drain and --now exclude each other");

    let (code, out, err) = apb(
        root,
        &["pause", &run_id, "--reason", "lunch", "--until", "1h"],
    );
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("pausing"), "{out}");
    assert!(out.contains("resumes on its own at"), "{out}");

    // While the script still runs, the run reads running with a pausing marker.
    let (_, list, _) = apb(root, &["runs"]);
    let line = list.lines().find(|l| l.contains(&run_id)).expect("a row");
    assert!(
        line.split('\t').nth(2) == Some("running") && line.contains("pausing (lunch, by operator"),
        "{line}"
    );

    fs::remove_file(root.join("slow")).unwrap();
    poll("the run to read paused", || {
        let (_, list, _) = apb(root, &["runs"]);
        list.lines()
            .any(|l| l.split('\t').nth(2) == Some("paused"))
            .then_some(())
    });
    let (_, list, _) = apb(root, &["runs"]);
    let line = list.lines().find(|l| l.contains(&run_id)).unwrap();
    assert!(
        line.contains("paused (lunch, by operator, since ") && line.contains(", until "),
        "{line}"
    );
    let (_, detail, _) = apb(root, &["runs", &run_id]);
    assert!(detail.contains("  paused (lunch, by operator"), "{detail}");

    // `apb wait` returns exit 4 with the pause details.
    let (code, out, _) = apb(root, &["wait", &run_id, "--timeout", "30"]);
    assert_eq!(code, 4, "{out}");
    assert!(
        out.contains("lunch") && out.contains("by operator"),
        "{out}"
    );
    assert!(out.contains("until "), "{out}");

    // Resuming before `until` says so, and the run finishes.
    let (code, out, err) = apb(root, &["resume", &run_id]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("resuming early"), "{out}");
    assert!(out.contains("finished: succeeded"), "{out}");
}

/// A resume while another live run holds the busy lock of the tree fails
/// with `workdir busy` and leaves the run paused: no `run_resumed` lands in
/// the journal, so the run does not read running with a dead driver.
#[test]
fn resume_with_a_busy_workdir_fails_and_the_run_stays_paused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    fs::write(root.join("slow"), "").unwrap();
    let (code, stdout, stderr) = apb(root, &["run", "pausable", "--detach"]);
    assert_eq!(code, 0, "{stdout} {stderr}");
    let run_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let _guard = RunGuard::new(root, &run_id);
    poll("the script to start", || {
        root.join("s_running").is_file().then_some(())
    });
    let (code, out, err) = apb(root, &["pause", &run_id, "--reason", "busy"]);
    assert_eq!(code, 0, "{out} {err}");
    fs::remove_file(root.join("slow")).unwrap();
    poll("the run to read paused", || {
        let (_, list, _) = apb(root, &["runs"]);
        list.lines()
            .any(|l| l.contains(&run_id) && l.split('\t').nth(2) == Some("paused"))
            .then_some(())
    });
    // The driver released its lock on pause; a live process (this test)
    // takes it, as another write-run would.
    let lock = root.join(".apb/workdir.lock");
    poll("the paused driver to release the lock", || {
        (!lock.is_file()).then_some(())
    });
    fs::write(&lock, std::process::id().to_string()).unwrap();

    let (code, out, err) = apb(root, &["resume", &run_id]);
    assert_eq!(code, 2, "{out} {err}");
    assert!(err.contains("workdir busy"), "{err}");
    let journal = fs::read_to_string(root.join(".apb/runs").join(&run_id).join("events.jsonl"))
        .unwrap_or_default();
    assert!(!journal.contains("\"run_resumed\""), "{journal}");
    let (_, list, _) = apb(root, &["runs"]);
    let line = list.lines().find(|l| l.contains(&run_id)).unwrap();
    assert_eq!(line.split('\t').nth(2), Some("paused"), "{line}");

    // Once the lock frees, the same resume goes through.
    fs::remove_file(&lock).unwrap();
    let (code, out, err) = apb(root, &["resume", &run_id]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(out.contains("finished: succeeded"), "{out}");
}
