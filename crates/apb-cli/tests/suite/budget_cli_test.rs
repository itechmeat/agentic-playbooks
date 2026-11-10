//! The run budget through the real `apb` binary: `apb run --estimate`
//! prints the estimate and starts nothing; a run whose token budget no
//! longer covers the next node pauses, `apb wait` exits 4 with the raise
//! flags, a plain `apb resume` exits 2, and `--raise-budget-tokens`
//! finishes it. A stub agent reports 200 tokens per attempt; no model is
//! called.

use std::fs;
use std::path::Path;

use predicates::prelude::*;

use crate::common::{RunGuard, apb};

pub(crate) const PLAYBOOK: &str = "schema: 2\nid: bud\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nbudget: { tokens: 5000, on_no_estimate: continue }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";

/// Writes the playbook `bud` at `version`, profile `main` (SOUL `soul`)
/// and the stub agent; returns the stub's path.
pub(crate) fn seed(root: &Path, version: &str, soul: &str) -> String {
    if !root.join(".apb").is_dir() {
        apb().arg("init").current_dir(root).assert().success();
    }
    let vdir = root.join(".apb/playbooks/bud").join(version);
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        PLAYBOOK.replace("version: 1.0.0", &format!("version: {version}")),
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/bud/current"), version).unwrap();
    let pdir = root.join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), soul).unwrap();
    let out = root.join("out.json");
    fs::write(
        &out,
        serde_json::json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": "done\n\n```yaml\nstatus: success\nsummary: done\n```",
            "usage": {"input_tokens": 150, "output_tokens": 50}
        })
        .to_string(),
    )
    .unwrap();
    let stub = root.join("agent.sh");
    if !stub.exists() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let mut f = fs::File::create(&stub).unwrap();
        write!(
            f,
            "#!/bin/sh\n[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{}'\n",
            out.display()
        )
        .unwrap();
        f.sync_all().unwrap();
        drop(f);
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    stub.to_string_lossy().into_owned()
}

/// One finished run of `bud` on the stub, the history an estimate reads.
pub(crate) fn history_run(root: &Path, stub: &str) {
    apb()
        .args(["run", "bud"])
        .env("APB_AGENT_CMD", stub)
        .current_dir(root)
        .assert()
        .success();
}

fn run_ids(root: &Path) -> usize {
    fs::read_dir(root.join(".apb/runs"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[test]
fn run_estimate_flag_prints_and_creates_no_run_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    apb()
        .args(["run", "bud", "--estimate"])
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "estimate (0 comparable runs of bud 1.0.0, cli):",
        ))
        .stdout(predicate::str::contains(
            "not estimated: a (no history), b (no history)",
        ))
        .stdout(predicate::str::contains(
            "budget: 5000 tokens, on no estimate: continue",
        ));
    assert_eq!(run_ids(root), 0);
}

/// A run with a 300-token budget after one history run: `a` (200) fits,
/// `b` no longer does. Returns its id.
fn paused_run(root: &Path, stub: &str) -> String {
    history_run(root, stub);
    let out = apb()
        .args(["run", "bud", "--detach", "--budget-tokens", "300"])
        .env("APB_AGENT_CMD", stub)
        .current_dir(root)
        .assert()
        .success()
        .stderr(predicate::str::contains("budget: 300 tokens"))
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string()
}

#[test]
fn budget_tokens_pauses_and_wait_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let run_id = paused_run(root, &stub);
    let _guard = RunGuard::new(root, &run_id);
    apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .current_dir(root)
        .assert()
        .code(4)
        .stdout(predicate::str::contains("paused before `b`"))
        .stdout(predicate::str::contains("--raise-budget-tokens"));
    apb()
        .args(["runs", &run_id])
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "tokens 200 of 300 spent, 100 left",
        ));
}

#[test]
fn resume_without_budget_decision_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let run_id = paused_run(root, &stub);
    let _guard = RunGuard::new(root, &run_id);
    apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .current_dir(root)
        .assert()
        .code(4);
    apb()
        .args(["resume", &run_id])
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .assert()
        .code(2)
        .stderr(predicate::str::contains("budget_paused"))
        .stderr(predicate::str::contains("--over-budget-once"));
}

#[test]
fn raise_budget_tokens_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let run_id = paused_run(root, &stub);
    let _guard = RunGuard::new(root, &run_id);
    apb()
        .args(["wait", &run_id, "--timeout", "60"])
        .current_dir(root)
        .assert()
        .code(4);
    apb()
        .args(["resume", &run_id, "--raise-budget-tokens", "1k"])
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .assert()
        .success()
        .stdout(predicate::str::contains("finished: succeeded"));
}

/// Runs `apb <args>` in `root` on a pseudo-terminal that is the child's
/// controlling terminal (the budget question reads `/dev/tty`), answering
/// `answer` once `prompt` appeared. Returns the exit code and everything
/// the child wrote.
#[cfg(unix)]
fn run_on_tty(
    root: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    prompt: &str,
    answer: &str,
) -> (Option<i32>, String) {
    use std::io::{Read as _, Write as _};
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let (mut master, mut slave): (libc::c_int, libc::c_int) = (0, 0);
    let mut winsize = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let winsize_ptr = std::ptr::addr_of_mut!(winsize);
    // SAFETY: openpty writes two descriptors this function then owns.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winsize_ptr,
        )
    };
    assert_eq!(rc, 0, "openpty failed");
    // SAFETY: both descriptors are fresh and owned here.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut cmd = crate::common::apb_std();
    cmd.args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::from(slave.try_clone().unwrap()))
        .stdout(std::process::Stdio::from(slave.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(slave.try_clone().unwrap()));
    for (k, v) in env {
        cmd.env(k, v);
    }
    // SAFETY: only async-signal-safe calls between fork and exec: a new
    // session, then the pty on stdin as its controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    drop(slave);
    let seen = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&seen);
    let mut reader = std::fs::File::from(master.try_clone().unwrap());
    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    let mut writer = std::fs::File::from(master.try_clone().unwrap());
    let limit = Instant::now() + Duration::from_secs(60);
    let mut answered = false;
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if !answered && seen.lock().unwrap().contains(prompt) {
            writer.write_all(answer.as_bytes()).unwrap();
            writer.flush().unwrap();
            answered = true;
        }
        if Instant::now() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no exit; terminal so far:\n{}", seen.lock().unwrap());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // A detached driver keeps the terminal open, so the reader never sees
    // its end: what the child wrote is read after a short settle, and the
    // reader thread is left to end with the terminal.
    std::thread::sleep(Duration::from_millis(300));
    drop(pump);
    let text = seen.lock().unwrap().clone();
    assert!(answered, "the question was never asked:\n{text}");
    (status.code(), text)
}

/// The answer a person gives to the `on_no_estimate: ask` question is not
/// part of a keyed start's request: the request is captured from the
/// arguments before the question, so the same arguments repeated without
/// a terminal answer the run the first start made, not
/// `start_key_conflict`.
#[cfg(unix)]
#[test]
fn a_budget_answer_does_not_change_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = seed(root, "1.0.0", "");
    let vdir = root.join(".apb/playbooks/bud/1.0.0");
    fs::write(
        vdir.join("playbook.yaml"),
        PLAYBOOK.replace("on_no_estimate: continue", "on_no_estimate: ask"),
    )
    .unwrap();
    let args = ["run", "bud", "--detach", "--start-key", "budget-answer-1"];
    let (code, text) = run_on_tty(
        root,
        &args,
        &[("APB_AGENT_CMD", &stub)],
        "pause before such a node or continue? [p/c]",
        "p\n",
    );
    assert_eq!(code, Some(0), "{text}");
    let run_id = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("run started: "))
        .expect("run id")
        .trim()
        .to_string();
    let _guard = RunGuard::new(root, &run_id);
    let again = apb()
        .args(args)
        .env("APB_AGENT_CMD", &stub)
        .current_dir(root)
        .output()
        .unwrap();
    let (out, err) = (
        String::from_utf8_lossy(&again.stdout),
        String::from_utf8_lossy(&again.stderr),
    );
    assert_eq!(again.status.code(), Some(0), "{out} {err}");
    assert!(
        out.contains(&format!("run already started: {run_id} (start key)")),
        "{out} {err}"
    );
    assert_eq!(run_ids(root), 1);
}
