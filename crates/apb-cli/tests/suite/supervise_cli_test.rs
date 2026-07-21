//! CLI smoke test for `apb run --supervise` (Phase 4c, Task 5, Step 1).
//!
//! The live cycle of a background agent against a real `claude` is not
//! reproducible in a headless CI environment (it needs a real coding agent) -
//! only a manual check covers that (see docs/tasks.md and
//! `.superpowers/sdd/task-5-report.md`). This test is an automated proxy: it
//! drives exactly the same CLI path (`apb run <id> --supervise`), but swaps
//! the executed agent via `APB_AGENT_CMD` for a stub that just records its
//! own invocation and exits immediately. The playbook is built without
//! agent_task nodes (only start -> prompt -> finish), so the run itself
//! deterministically reaches succeeded without any live agent involved - the
//! stub is only needed to confirm that the engine actually tried to spawn the
//! background supervisor.
//!
//! Modeled on `run_cli_test.rs` (assert_cmd/cargo_bin("apb"), seeding a
//! temporary `.apb` project) and on
//! `crates/apb-engine/tests/background_supervisor_test.rs` (the same agent
//! stub, the same poll_until technique instead of a fixed-duration sleep).

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// APB_AGENT_CMD is process-wide environment, shared across all cargo test
// threads; other tests in this binary don't touch it, but we keep the same
// serialize-guard as in the engine tests (background_supervisor_test.rs,
// supervised_drive_test.rs), in case of future neighbors in this file/binary.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(50);

fn playbook() -> Command {
    Command::cargo_bin("apb").unwrap()
}

// A playbook without agent_task: start -> prompt -> finish, the run finishes
// on its own. `supervisor.executor` (and `defaults.executor`) are set so
// that the background agent spawn in run_background finds a resolvable
// executor - it's exactly this spawn that APB_AGENT_CMD replaces.
const SUPERVISED_NOAGENT: &str = r#"
schema: 1
id: svnoagent
name: Supervised No Agent
version: 1.0.0
defaults:
  profile: main
supervisor:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    playbook()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    let vdir = dir.path().join(".apb/playbooks/svnoagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), SUPERVISED_NOAGENT).unwrap();
    fs::write(dir.path().join(".apb/playbooks/svnoagent/current"), "1.0.0").unwrap();
    let pdir = dir.path().join(".apb/profiles/main");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    dir
}

/// Agent stub: appends the received arguments to a marker file and exits
/// immediately with success - it does not spawn a live agent (`claude`), it
/// only records that the engine really tried to spawn it.
fn agent_stub(dir: &Path, marker_file: &Path) -> String {
    let path = dir.join("agent_stub.sh");
    let body = format!(
        "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo '---end---'; }} >> '{}'\nexit 0\n",
        marker_file.display()
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

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

/// Extracts run_id from stdout of the form "supervised run started: <run_id>"
/// (see run_cmd in crates/apb-cli/src/main.rs). If the output format ever
/// changes, the fallback path is the single directory under `.apb/runs/`,
/// because this test creates exactly one run.
fn extract_run_id(stdout: &str, runs_dir: &Path) -> String {
    if let Some(idx) = stdout.find("started: ") {
        let rest = &stdout[idx + "started: ".len()..];
        let run_id: String = rest.trim_end().lines().next().unwrap_or("").to_string();
        if !run_id.is_empty() {
            return run_id;
        }
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(runs_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one run dir under {}",
        runs_dir.display()
    );
    entries
        .pop()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

#[test]
fn supervise_flag_spawns_stub_agent_and_run_finishes() {
    let dir = seeded();

    let marker_file = dir.path().join("agent_invocation.txt");
    let stub = agent_stub(dir.path(), &marker_file);

    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let assert = playbook()
        .args(["run", "svnoagent", "--supervise"])
        .current_dir(dir.path())
        .env("APB_AGENT_CMD", &stub)
        .assert()
        .success()
        .stdout(predicate::str::contains("supervised run started:"));

    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    let runs_dir = dir.path().join(".apb/runs");
    let run_id = extract_run_id(&stdout, &runs_dir);
    let run_dir = runs_dir.join(&run_id);

    // --supervise is non-blocking: the run itself proceeds in a separate
    // detached child process, and the CLI itself has already exited by this
    // point - we wait for the terminal event in events.jsonl separately,
    // with a bounded timeout.
    let events_path = run_dir.join("events.jsonl");
    let events_text = poll_until(
        "events.jsonl to contain a terminal run_finished event",
        || {
            let text = fs::read_to_string(&events_path).ok()?;
            if text.contains("\"type\":\"run_finished\"") {
                Some(text)
            } else {
                None
            }
        },
    );
    assert!(
        events_text.contains("\"outcome\":\"succeeded\""),
        "playbook without agent_task nodes must finish successfully:\n{events_text}"
    );

    // The agent stub was actually invoked - the engine tried to spawn a
    // background agent in a separate child process (supervisor.executor
    // resolves, the spawn happens during prepare_supervised_background,
    // verified via poll).
    let marker_text = poll_until("agent stub invocation marker to appear", || {
        let text = fs::read_to_string(&marker_file).ok()?;
        if text.is_empty() { None } else { Some(text) }
    });
    assert!(
        marker_text.contains("---end---"),
        "stub must have been invoked at least once:\n{marker_text}"
    );

    // The supervisor session is persisted on disk for this run.
    let session_path = run_dir.join("supervisor").join("session.json");
    let session_text = poll_until("supervisor/session.json to appear", || {
        fs::read_to_string(&session_path).ok()
    });
    // Only an irreversible token fingerprint (sha256:...) is written to
    // disk, not the token itself: the secret is not stored at rest.
    assert!(
        session_text.contains("\"token_hash\"") && session_text.contains("sha256:"),
        "session.json must persist a token hash, not the raw token:\n{session_text}"
    );
    assert!(
        !session_text.contains("\"token\":"),
        "raw supervisor token must never be persisted on disk:\n{session_text}"
    );

    drop(_env);
}

/// `--supervise --continued-from` must survive the detached spawn boundary and
/// establish the same predecessor/successor links as the autonomous path.
#[test]
fn supervise_continued_from_establishes_lineage() {
    let dir = seeded();

    let marker_file = dir.path().join("agent_invocation.txt");
    let stub = agent_stub(dir.path(), &marker_file);

    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Predecessor: a normal supervised run that finishes on its own.
    let assert = playbook()
        .args(["run", "svnoagent", "--supervise"])
        .current_dir(dir.path())
        .env("APB_AGENT_CMD", &stub)
        .assert()
        .success()
        .stdout(predicate::str::contains("supervised run started:"));
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let runs_dir = dir.path().join(".apb/runs");
    let first_id = extract_run_id(&stdout, &runs_dir);
    let first_dir = runs_dir.join(&first_id);
    poll_until("predecessor run_finished", || {
        let text = fs::read_to_string(first_dir.join("events.jsonl")).ok()?;
        if text.contains("\"type\":\"run_finished\"") {
            Some(())
        } else {
            None
        }
    });

    // Successor: supervised retry linked via --continued-from.
    let assert = playbook()
        .args([
            "run",
            "svnoagent",
            "--supervise",
            "--continued-from",
            &first_id,
        ])
        .current_dir(dir.path())
        .env("APB_AGENT_CMD", &stub)
        .assert()
        .success()
        .stdout(predicate::str::contains("supervised run started:"));
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let second_id = extract_run_id(&stdout, &runs_dir);
    assert_ne!(first_id, second_id, "successor must be a distinct run");
    let second_dir = runs_dir.join(&second_id);
    poll_until("successor run_finished", || {
        let text = fs::read_to_string(second_dir.join("events.jsonl")).ok()?;
        if text.contains("\"type\":\"run_finished\"") {
            Some(())
        } else {
            None
        }
    });

    let pred_cfg = apb_engine::run_config::read_run_config(&first_dir).unwrap();
    let succ_cfg = apb_engine::run_config::read_run_config(&second_dir).unwrap();
    assert_eq!(pred_cfg.superseded_by.as_deref(), Some(second_id.as_str()));
    assert_eq!(succ_cfg.continued_from.as_deref(), Some(first_id.as_str()));

    drop(_env);
}

/// Supervised prepare rejects an unknown continued_from predecessor the same
/// way the autonomous path does (handshake surfaces the engine error).
#[test]
fn supervise_continued_from_rejects_unknown_predecessor() {
    let dir = seeded();

    let marker_file = dir.path().join("agent_invocation.txt");
    let stub = agent_stub(dir.path(), &marker_file);

    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    playbook()
        .args([
            "run",
            "svnoagent",
            "--supervise",
            "--continued-from",
            "ghost-1",
        ])
        .current_dir(dir.path())
        .env("APB_AGENT_CMD", &stub)
        .assert()
        .failure()
        .stderr(predicate::str::contains("run `ghost-1`"));

    drop(_env);
}
