//! Task 9 (spec 2026-07-20-interactive-nodes): `apb answer <run> <text>` and
//! the waiting-on-question display in `apb runs` / `apb doctor --run <id>`.
//!
//! Fixtures are hand-built (playbook.yaml snapshot + questions.jsonl via
//! `apb_engine::question::post_question`) rather than driven through a real
//! agent run, exactly like `progress.rs`'s own interactive-node tests: the
//! interesting state (a pending question) is a channel-file fact, not
//! something a stub agent needs to actually produce here.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn apb() -> Command {
    Command::cargo_bin("apb").unwrap()
}

fn init(dir: &Path) {
    apb().arg("init").current_dir(dir).assert().success();
}

/// A single interactive `agent_task` node (`ask`), `answer_by` defaulting to
/// `human` (schema default), so the plain `apb answer` (which always posts
/// `answered_by: "human"`) is always accepted.
const INTERACTIVE_PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: hi, interactive: true }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: f }\n";

/// Two interactive nodes (`ask1`, `ask2`), both pending at once, for the
/// ambiguous-node test.
const TWO_INTERACTIVE_PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: ask1, type: agent_task, prompt: hi, interactive: true }\n  - { id: ask2, type: agent_task, prompt: hi, interactive: true }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask1 }\n  - { from: ask1, to: ask2 }\n  - { from: ask2, to: f }\n";

const RUN_STARTED: &str =
    "{\"seq\":0,\"ts\":1000,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\n";

fn run_dir(root: &Path, run_id: &str, playbook_yaml: &str) -> std::path::PathBuf {
    let d = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("playbook.yaml"), playbook_yaml).unwrap();
    fs::write(d.join("events.jsonl"), RUN_STARTED).unwrap();
    d
}

// (a) happy path: exactly one pending question, `--node` omitted.
#[test]
fn answer_posts_answered_by_human_when_node_is_omitted() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask", 1, "which way", Vec::new()).unwrap();

    apb()
        .args(["answer", "r1", "pg"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("answer posted for r1"));

    let answers = fs::read_to_string(rd.join("answers.jsonl")).unwrap();
    assert!(
        answers.contains("\"node\":\"ask\"")
            && answers.contains("\"answer\":\"pg\"")
            && answers.contains("\"answered_by\":\"human\""),
        "expected answers.jsonl to record the human answer, got:\n{answers}"
    );
}

// (a) `--node` targets the interactive node explicitly.
#[test]
fn answer_with_explicit_node_targets_that_node() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask", 1, "which way", Vec::new()).unwrap();

    apb()
        .args(["answer", "r1", "--node", "ask", "pg"])
        .current_dir(dir.path())
        .assert()
        .success();

    let answers = fs::read_to_string(rd.join("answers.jsonl")).unwrap();
    assert!(
        answers.contains("\"node\":\"ask\"") && answers.contains("\"answered_by\":\"human\""),
        "got:\n{answers}"
    );
}

// (b) no pending question at all: a clean, non-zero-exit error naming the
// run and stating there is no pending question.
#[test]
fn answer_fails_cleanly_when_no_question_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    run_dir(dir.path(), "r1", INTERACTIVE_PB);

    apb()
        .args(["answer", "r1", "x"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("r1"))
        .stderr(predicate::str::contains("no pending question"));
}

// (b) ambiguous: two nodes pending at once and no `--node` given must fail
// with a message listing the candidates.
#[test]
fn answer_with_ambiguous_pending_questions_lists_candidates() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", TWO_INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask1", 1, "q1", Vec::new()).unwrap();
    apb_engine::question::post_question(&rd, "ask2", 1, "q2", Vec::new()).unwrap();

    apb()
        .args(["answer", "r1", "x"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("ask1"))
        .stderr(predicate::str::contains("ask2"));
}

// (c) `apb runs` shows a waiting-on-question marker for a run parked on one.
#[test]
fn runs_command_shows_waiting_on_question() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask", 1, "which way, human", Vec::new()).unwrap();

    apb()
        .arg("runs")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("ask"))
        .stdout(predicate::str::contains("which way, human"));
}

// (c) `apb doctor --run <id>` lists a check flagging the pending question,
// naming both the node and the question text. A pending question is a
// normal wait state, not a blocking problem, so this must exit success -
// locked in explicitly so a future change cannot silently flip doctor to
// failure just because a question is pending.
#[test]
fn doctor_run_flags_the_pending_question() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask", 1, "which way, human", Vec::new()).unwrap();

    apb()
        .args(["doctor", "--run", "r1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("ask"))
        .stdout(predicate::str::contains("which way, human"));
}

// Fix round 1 (spec 2026-07-20-interactive-nodes, Security section): a
// question is agent-generated (untrusted) text. An embedded ANSI escape
// sequence must not survive into the terminal report as a live escape (the
// ESC byte, 0x1b, must be gone - a terminal would otherwise interpret it as
// an interpretation channel, e.g. changing colors or moving the cursor), an
// embedded `\r`/`\n` must not be able to forge extra report lines or wipe
// the current one, and a very long question must be truncated with "..."
// rather than dominating the report. Same fixture, both `apb runs` and
// `apb doctor --run` - both print sites must sanitize identically.
fn hostile_question() -> String {
    // ESC[31m ... ESC[0m (red-text ANSI), an embedded CR, an embedded LF,
    // then 500 chars of padding so truncation is exercised too.
    format!(
        "\x1b[31mred\x1b[0m question\r\nwith injected lines{}",
        "x".repeat(500)
    )
}

fn assert_sanitized(stdout: &str) {
    assert!(
        !stdout.contains('\x1b'),
        "an ESC byte (0x1b) must never reach the terminal report: {stdout:?}"
    );
    assert!(
        !stdout.contains('\r'),
        "an embedded CR must not survive into the report: {stdout:?}"
    );
    assert!(
        stdout.contains("..."),
        "a question padded past the cap must be truncated with '...': {stdout}"
    );
    // The sanitized question renders as plain text on the one line it was
    // printed on: no raw `\n` inside the question survives to start a new
    // line the caller did not print itself.
    let question_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("red") || l.contains("question"))
        .collect();
    assert_eq!(
        question_lines.len(),
        1,
        "the sanitized question must render on exactly one line, got:\n{stdout}"
    );
}

#[test]
fn runs_command_sanitizes_ansi_and_newlines_and_truncates() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask", 1, &hostile_question(), Vec::new()).unwrap();

    let out = apb().arg("runs").current_dir(dir.path()).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_sanitized(&stdout);
}

#[test]
fn doctor_run_sanitizes_ansi_and_newlines_and_truncates() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rd = run_dir(dir.path(), "r1", INTERACTIVE_PB);
    apb_engine::question::post_question(&rd, "ask", 1, &hostile_question(), Vec::new()).unwrap();

    let out = apb()
        .args(["doctor", "--run", "r1"])
        .current_dir(dir.path())
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_sanitized(&stdout);
}
