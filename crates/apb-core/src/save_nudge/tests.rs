//! The save check over fixture sessions: no word lists anywhere, so the
//! requests here are in Greek; the structural signal and the model signal
//! decide alone.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::agents::claude::hooks::ClaudeStopHook;

/// "Every Monday prepare the sales report and send it to the team."
const GREEK_RECURRING: &str = "\u{039A}\u{03AC}\u{03B8}\u{03B5} \u{0394}\u{03B5}\u{03C5}\u{03C4}\u{03AD}\u{03C1}\u{03B1} \u{03B5}\u{03C4}\u{03BF}\u{03AF}\u{03BC}\u{03B1}\u{03C3}\u{03B5} \u{03C4}\u{03B7}\u{03BD} \u{03B1}\u{03BD}\u{03B1}\u{03C6}\u{03BF}\u{03C1}\u{03AC}";
/// "Hello, how are you?"
const GREEK_CHAT: &str = "\u{0393}\u{03B5}\u{03B9}\u{03B1} \u{03C3}\u{03BF}\u{03C5}, \u{03C4}\u{03B9} \u{03BA}\u{03AC}\u{03BD}\u{03B5}\u{03B9}\u{03C2};";

fn user(text: &str) -> Value {
    json!({ "type": "user", "cwd": "/p", "message": { "role": "user", "content": text } })
}

fn say(text: &str) -> Value {
    json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] } })
}

fn tool(name: &str, input: Value) -> Value {
    json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "tool_use", "id": "t", "name": name, "input": input }] } })
}

fn result() -> Value {
    json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": "ok" }] } })
}

fn bash(cmd: &str) -> Value {
    tool("Bash", json!({ "command": cmd }))
}

fn edit(path: &str) -> Value {
    tool("Edit", json!({ "file_path": path }))
}

/// A long, multi-step task: 8 steps of 5 kinds.
fn long_task() -> Vec<Value> {
    vec![
        bash("cargo test"),
        edit("/p/a.rs"),
        bash("git commit -m x"),
        tool("Read", json!({ "file_path": "/p/b.md" })),
        bash("git push"),
        edit("/p/c.ts"),
        tool("Grep", json!({ "pattern": "x" })),
        bash("cargo build"),
    ]
}

fn session(lines: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in lines {
        // Every call gets its result, as in a real session.
        out.extend(l.to_string().as_bytes());
        out.push(b'\n');
        if l["message"]["content"][0]["type"] == "tool_use" {
            out.extend(result().to_string().as_bytes());
            out.push(b'\n');
        }
    }
    out
}

struct Fixture {
    _tmp: tempfile::TempDir,
    transcript: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("s.jsonl");
        std::fs::write(&transcript, bytes).unwrap();
        let state = tmp.path().join("hooks");
        Fixture {
            _tmp: tmp,
            transcript,
            state,
        }
    }

    fn stdin(&self, session: &str, active: bool) -> Vec<u8> {
        json!({
            "session_id": session,
            "transcript_path": self.transcript,
            "cwd": "/p",
            "hook_event_name": "Stop",
            "stop_hook_active": active,
        })
        .to_string()
        .into_bytes()
    }

    fn run(&self, session: &str, active: bool, answer: Option<bool>) -> (Option<String>, usize) {
        let mut asked = 0;
        let mut ask = |_: &str, _: Option<&Path>| {
            asked += 1;
            answer
        };
        let out = run_stop_hook(
            &ClaudeStopHook,
            &self.stdin(session, active),
            &self.state,
            Thresholds::default(),
            &mut ask,
        );
        (out, asked)
    }
}

fn blocked(out: &Option<String>) -> bool {
    out.as_deref()
        .and_then(|o| serde_json::from_str::<Value>(o).ok())
        .is_some_and(|v| v["decision"] == "block" && v["reason"].is_string())
}

#[test]
fn a_short_chat_gets_no_reminder_and_no_question() {
    let f = Fixture::new(&session(&[user(GREEK_CHAT), say("fine")]));
    let (out, asked) = f.run("s", false, Some(true));
    assert_eq!(out, None);
    assert_eq!(asked, 0, "a task that did nothing is never asked about");
}

#[test]
fn a_long_multi_step_task_is_reminded_once_per_session() {
    let mut lines = vec![user(GREEK_CHAT)];
    lines.extend(long_task());
    lines.push(say("done"));
    let f = Fixture::new(&session(&lines));
    let (out, asked) = f.run("s", false, None);
    assert!(blocked(&out), "{out:?}");
    assert_eq!(asked, 0, "the structural signal needs no model");
    let reason: Value = serde_json::from_str(out.as_deref().unwrap()).unwrap();
    let reason = reason["reason"].as_str().unwrap();
    for phrase in [
        "playbook_catalog",
        "playbook_capture",
        "suggestion_dismiss",
        "tool search",
        "judge for yourself",
    ] {
        assert!(reason.contains(phrase), "{phrase}: {reason}");
    }
    assert!(!reason.contains('!') && !reason.contains('\u{2014}'));
    assert_eq!(f.run("s", false, None).0, None, "never twice");
    assert!(blocked(&f.run("other", false, None).0), "another session");
}

#[test]
fn a_short_task_is_reminded_only_when_the_model_says_recurring() {
    let lines = [
        user(GREEK_RECURRING),
        edit("/p/report.md"),
        bash("git commit -m r"),
    ];
    let f = Fixture::new(&session(&lines));
    // No model, or a failed one: no signal.
    assert_eq!(f.run("a", false, None), (None, 1));
    assert_eq!(f.run("b", false, Some(false)).0, None);
    // The model says recurring: a reminder.
    let (out, asked) = f.run("c", false, Some(true));
    assert!(blocked(&out), "{out:?}");
    assert_eq!(asked, 1);
    let reason: Value = serde_json::from_str(out.as_deref().unwrap()).unwrap();
    assert!(
        reason["reason"]
            .as_str()
            .unwrap()
            .contains("decision model")
    );
    // An answer is cached per session and request.
    assert_eq!(f.run("b", false, Some(true)), (None, 0));
}

#[test]
fn a_session_that_used_a_playbook_tool_is_left_alone() {
    let mut lines = vec![
        user("first"),
        tool("mcp__agentic-playbooks__playbook_catalog", json!({})),
        user(GREEK_RECURRING),
    ];
    lines.extend(long_task());
    let f = Fixture::new(&session(&lines));
    assert_eq!(f.run("s", false, Some(true)), (None, 0));
}

#[test]
fn a_continued_turn_is_never_blocked_again() {
    let mut lines = vec![user(GREEK_CHAT)];
    lines.extend(long_task());
    let f = Fixture::new(&session(&lines));
    assert_eq!(f.run("s", true, Some(true)), (None, 0));
}

#[test]
fn a_broken_transcript_or_event_is_silent() {
    let f = Fixture::new(b"\x00\xffnot json\n{\"type\":\n");
    assert_eq!(f.run("s", false, Some(true)), (None, 0));
    let mut ask = |_: &str, _: Option<&Path>| Some(true);
    assert_eq!(
        run_stop_hook(
            &ClaudeStopHook,
            b"garbage",
            &f.state,
            Thresholds::default(),
            &mut ask
        ),
        None
    );
    let missing = Fixture::new(b"");
    std::fs::remove_file(&missing.transcript).unwrap();
    assert_eq!(missing.run("s", false, Some(true)), (None, 0));
}

#[test]
fn only_the_steps_since_the_last_request_count() {
    let mut lines = vec![user("one")];
    lines.extend(long_task());
    lines.push(user(GREEK_CHAT));
    lines.push(bash("git status"));
    let task = task::read(crate::discover::HistoryAgent::ClaudeCode, &session(&lines));
    assert_eq!(task.steps, ["git:status"]);
    assert_eq!(task.request.as_deref(), Some(GREEK_CHAT));
    assert_eq!(
        decide(&task, Thresholds::default(), &mut |_| Some(true)),
        Verdict::Quiet("no_acting_step")
    );
}

#[test]
fn a_tail_without_a_request_has_no_last_task() {
    // The last request lies before the tail: the steps seen belong to no
    // known task, so they never add up to a reminder.
    let task = task::read(
        crate::discover::HistoryAgent::ClaudeCode,
        &session(&long_task()),
    );
    assert_eq!(task.request, None);
    assert_eq!(
        decide(&task, Thresholds::default(), &mut |_| Some(true)),
        Verdict::Quiet("no_acting_step")
    );
}

#[test]
fn a_session_reminded_while_the_model_was_asked_is_not_reminded_again() {
    let lines = [
        user(GREEK_RECURRING),
        edit("/p/report.md"),
        bash("git commit -m r"),
    ];
    let f = Fixture::new(&session(&lines));
    // Another hook run of the same session reminds it while this one
    // waits for the model.
    let mut ask = |_: &str, _: Option<&Path>| {
        state::update(
            &f.state,
            &state::session_key("s"),
            crate::clock::now_ms_u64(),
            |e| e.nudged = true,
        )
        .unwrap();
        Some(true)
    };
    let out = run_stop_hook(
        &ClaudeStopHook,
        &f.stdin("s", false),
        &f.state,
        Thresholds::default(),
        &mut ask,
    );
    assert_eq!(out, None);
}

#[cfg(unix)]
#[test]
fn a_transcript_that_is_a_link_is_not_read() {
    let mut lines = vec![user(GREEK_CHAT)];
    lines.extend(long_task());
    let f = Fixture::new(&session(&lines));
    let link = f.transcript.with_extension("link");
    std::os::unix::fs::symlink(&f.transcript, &link).unwrap();
    assert!(task::tail(&f.transcript).is_ok());
    assert!(task::tail(&link).is_err());
}

#[cfg(unix)]
#[test]
fn a_transcript_that_is_a_fifo_fails_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let fifo = tmp.path().join("t.jsonl");
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(task::tail(&fifo).is_err());
    });
    let failed = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("opening a FIFO blocked");
    assert!(failed);
}
