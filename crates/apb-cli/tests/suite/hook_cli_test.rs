//! `apb hook stop` end to end: Claude Code's Stop hook JSON on stdin, a
//! fixture transcript, and the answer on stdout; exit 0 whatever happens.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{Value, json};

fn line(v: Value) -> String {
    format!("{v}\n")
}

fn tool(name: &str, input: Value) -> String {
    line(
        json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "t", "name": name, "input": input }] } }),
    ) + &line(
        json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "t", "content": "ok" }] } }),
    )
}

/// A long task after a request in Greek ("Prepare the release").
fn long_session() -> String {
    let mut s = line(
        json!({ "type": "user", "cwd": "/p", "message": { "content": "\u{0395}\u{03C4}\u{03BF}\u{03AF}\u{03BC}\u{03B1}\u{03C3}\u{03B5} \u{03C4}\u{03B7}\u{03BD} \u{03AD}\u{03BA}\u{03B4}\u{03BF}\u{03C3}\u{03B7}" } }),
    );
    for (name, input) in [
        ("Bash", json!({ "command": "cargo test" })),
        ("Edit", json!({ "file_path": "/p/a.rs" })),
        ("Bash", json!({ "command": "git commit -m x" })),
        ("Read", json!({ "file_path": "/p/b.md" })),
        ("Bash", json!({ "command": "git push" })),
        ("Edit", json!({ "file_path": "/p/c.ts" })),
        ("Grep", json!({ "pattern": "x" })),
        ("Bash", json!({ "command": "cargo build" })),
    ] {
        s += &tool(name, input);
    }
    s
}

struct Hook {
    dir: tempfile::TempDir,
}

impl Hook {
    fn new(transcript: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("s.jsonl"), transcript).unwrap();
        Hook { dir }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn transcript(&self) -> PathBuf {
        self.dir.path().join("s.jsonl")
    }

    /// Runs the hook with `stdin` and `env`; its stdout and exit code.
    fn run(&self, stdin: &[u8], env: &[(&str, &str)]) -> (String, Option<i32>) {
        let mut cmd = crate::common::apb_std();
        cmd.args(["hook", "stop"])
            .current_dir(self.dir.path())
            .env("APB_CONFIG_DIR", self.config())
            .env("HOME", self.dir.path())
            .env_remove("APB_SAVE_NUDGE")
            .env_remove("APB_SAVE_NUDGE_MIN_STEPS")
            .env_remove("APB_SAVE_NUDGE_MIN_KINDS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        (String::from_utf8(out.stdout).unwrap(), out.status.code())
    }

    fn event(&self, session: &str, active: bool) -> Vec<u8> {
        event(session, &self.transcript(), self.dir.path(), active)
    }
}

fn event(session: &str, transcript: &Path, cwd: &Path, active: bool) -> Vec<u8> {
    json!({
        "session_id": session,
        "transcript_path": transcript,
        "cwd": cwd,
        "hook_event_name": "Stop",
        "stop_hook_active": active,
    })
    .to_string()
    .into_bytes()
}

#[test]
fn a_long_task_is_reminded_once_and_never_inside_a_run_step() {
    let h = Hook::new(&long_session());
    // Inside a run step, or switched off: nothing, and nothing recorded.
    assert_eq!(
        h.run(&h.event("s1", false), &[("APB_RUN_ID", "r1")]),
        (String::new(), Some(0))
    );
    assert_eq!(
        h.run(&h.event("s1", false), &[("APB_SAVE_NUDGE", "off")]),
        (String::new(), Some(0))
    );
    // A higher bound than the task: nothing.
    assert_eq!(
        h.run(&h.event("s1", false), &[("APB_SAVE_NUDGE_MIN_STEPS", "50")]),
        (String::new(), Some(0))
    );
    assert_eq!(
        h.run(&h.event("s1", true), &[]),
        (String::new(), Some(0)),
        "a continued turn"
    );
    assert!(
        !h.config().join("hooks/save-nudge.json").exists(),
        "a quiet run records nothing"
    );

    let (out, code) = h.run(&h.event("s1", false), &[]);
    assert_eq!(code, Some(0));
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["decision"], "block");
    assert!(v["reason"].as_str().unwrap().contains("playbook_catalog"));
    let state = std::fs::read_to_string(h.config().join("hooks/save-nudge.json")).unwrap();
    assert!(!state.contains("s1"), "the session id is kept hashed only");

    assert_eq!(
        h.run(&h.event("s1", false), &[]),
        (String::new(), Some(0)),
        "once per session"
    );
}

#[test]
fn broken_input_is_silent_and_exits_zero() {
    let h = Hook::new("\u{0}not json at all\n");
    assert_eq!(h.run(&h.event("s", false), &[]), (String::new(), Some(0)));
    assert_eq!(h.run(b"{ not json", &[]), (String::new(), Some(0)));
    assert_eq!(h.run(b"", &[]), (String::new(), Some(0)));
    let missing = event(
        "s",
        &h.dir.path().join("missing.jsonl"),
        h.dir.path(),
        false,
    );
    assert_eq!(h.run(&missing, &[]), (String::new(), Some(0)));
}

/// The host stopped reading (it timed out, or closed the pipe): the
/// reminder cannot be written, and the hook still exits 0.
#[test]
fn a_closed_stdout_still_exits_zero() {
    let h = Hook::new(&long_session());
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = crate::common::apb_std()
        .args(["hook", "stop"])
        .current_dir(h.dir.path())
        .env("APB_CONFIG_DIR", h.config())
        .env("HOME", h.dir.path())
        .env_remove("APB_SAVE_NUDGE")
        .stdin(Stdio::piped())
        .stdout(writer)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&h.event("s1", false))
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(0));
}

/// A host event this apb does not know (a newer settings file, an older
/// binary) is no failure of the host's turn.
#[test]
fn an_unknown_hook_event_exits_zero_and_prints_nothing() {
    let out = crate::common::apb_std()
        .args(["hook", "pre-compact", "--extra"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
}
