//! Task 7 (b/c/d) plus the Task 6 carry-over fix: session capture, the
//! resume-vs-reprompt decision, downgrade journaling, and the visit-scoped
//! reprompt transcript.
//!
//! (b) `capture_session` against recorded fixture outputs (no live agents).
//! (c) A `resume` agent whose asking attempt captures a session is re-invoked
//!     with its resume flag and the answer as the follow-up; no transcript.
//! (d) A `resume` agent whose output carries no session downgrades to reprompt
//!     with a journaled `interaction_downgraded` action.
//! (loop) A bounded loop through an interactive node twice scopes each visit's
//!     reprompt transcript to that visit's Q&A.
//!
//! Bounded by construction: every stub advances only across drive-controlled
//! re-invocations (a per-invocation counter file), never on a timer, so a later
//! round can only appear after the prior answer is posted. Every wait is a
//! bounded poll whose panic message names what it waited on, and a `RunReaper`
//! built before the first panic point aborts and joins a still-parked drive.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::adapter::capture_session;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::question::post_answer;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const POLL_DEADLINE: Duration = Duration::from_secs(10);
const POLL_STEP: Duration = Duration::from_millis(10);

fn lock() -> MutexGuard<'static, ()> {
    common::env_lock()
}

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
        }
    }
}

struct RunReaper {
    root: PathBuf,
    run_id: String,
    handle: Option<thread::JoinHandle<()>>,
}
impl RunReaper {
    fn join(&mut self) {
        if let Some(h) = self.handle.take() {
            h.join().expect("drive thread joined");
        }
    }
}
impl Drop for RunReaper {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = apb_engine::run_cancel(&self.root, &self.run_id);
            let _ = h.join();
        }
    }
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

// --- (b) session capture from recorded fixtures ---

#[test]
fn capture_session_reads_claude_stream_json_session_id() {
    let raw = fixture("claude_session.jsonl");
    assert_eq!(
        capture_session("claude", &raw).as_deref(),
        Some("sess-abc123"),
        "claude stream-json output exposes session_id"
    );
    assert_eq!(
        capture_session("claude-code", &raw).as_deref(),
        Some("sess-abc123")
    );
}

#[test]
fn capture_session_none_when_no_session_id() {
    // A plain-text one-shot output carries no session id.
    let plain = "I finished the task.\nAll good.";
    assert_eq!(capture_session("claude", plain), None);
    // codex/opencode/hermes one-shot forms print plain final-answer text today,
    // so their capture yields None and they rely on the downgrade path.
    let codex = fixture("codex_plain.txt");
    assert_eq!(capture_session("codex", &codex), None);
    assert_eq!(capture_session("opencode", &codex), None);
    assert_eq!(capture_session("hermes", &codex), None);
    // An unknown agent never captures a session.
    assert_eq!(
        capture_session("unknown", &fixture("claude_session.jsonl")),
        None
    );
}

// --- shared drive scaffolding ---

fn make_stub(dir: &Path, body: &str) -> String {
    let path = dir.join("stub.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

fn set_env(stub: &str, home: &Path, cfg: &Path) {
    unsafe {
        std::env::set_var("APB_AGENT_CMD", stub);
        std::env::set_var("HOME", home);
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
}

/// Writes a global config that fixes `claude`'s invocation `interaction`
/// ceiling; `program` is left to APB_AGENT_CMD, so only the ceiling and argv
/// come from here.
fn write_config(cfg: &Path, interaction: &str) {
    let yaml = format!(
        "agents:\n  claude:\n    invocation:\n      argv: [\"-p\", \"{{prompt}}\", \"--model\", \"{{model}}\"]\n      interaction: {interaction}\n"
    );
    fs::write(cfg.join("config.yaml"), yaml).unwrap();
}

fn seed_profile(root: &Path, name: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    let yaml =
        format!("name: {name}\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n");
    fs::write(dir.join("profile.yaml"), yaml).unwrap();
    fs::write(dir.join("SOUL.md"), "role").unwrap();
}

/// One-node interactive playbook `iq` (schema 1).
fn seed_single(root: &Path) {
    init_project(root).unwrap();
    let src = "schema: 1\nid: iq\nname: IQ\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: ask, type: agent_task, prompt: \"ask something\", profile: arch, interactive: true }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: ask }\n  - { from: ask, to: done }\n";
    let dir = root.join(".apb/playbooks/iq/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/iq/current"), "1.0.0").unwrap();
}

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        thread::sleep(POLL_STEP);
    }
}

fn latest_run_dir(root: &Path) -> PathBuf {
    poll_until("run dir to appear", || {
        let runs = root.join(".apb/runs");
        let entry = fs::read_dir(&runs)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())?;
        Some(entry.path())
    })
}

fn run_id_of(run_dir: &Path) -> String {
    run_dir.file_name().unwrap().to_string_lossy().to_string()
}

fn count_kind(events: &[Event], f: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().filter(|e| f(&e.payload)).count()
}

fn questions_asked_count(events: &[Event], node: &str) -> usize {
    count_kind(
        events,
        |p| matches!(p, EventPayload::QuestionAsked { node: n, .. } if n == node),
    )
}

fn spawn_run(root: &Path, id: &str) -> (mpsc::Receiver<RunStatus>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let id = id.to_string();
    let handle = thread::spawn(move || {
        let res = run(&root, &id, None, RunOptions::default());
        let status = res.map(|r| r.outcome).unwrap_or(RunStatus::Failed);
        let _ = tx.send(status);
    });
    (rx, handle)
}

// --- (c) resume re-invocation uses the resume argv, no transcript ---

#[test]
fn resume_reinvokes_with_resume_flag_and_no_transcript() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    let argvfile = bin.path().join("argv");
    seed_single(proj.path());
    seed_profile(proj.path(), "arch");
    write_config(cfg.path(), "resume");
    // Invocation 1 emits a session id plus the marker+question (drive parks and
    // captures the session); the re-invocation (only reachable after the
    // answer) finishes. Every invocation records its full argv line.
    let body = format!(
        "c=\"{}\"\na=\"{}\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nprintf '%s\\n' \"$*\" >> \"$a\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '{{\"session_id\":\"sess-42\"}}'\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Which DB?\",\"options\":[\"pg\"]}}'\n  exit 0\nfi\necho done\nexit 0",
        counter.display(),
        argvfile.display()
    );
    set_env(&make_stub(bin.path(), &body), home.path(), cfg.path());

    let (rx, handle) = spawn_run(proj.path(), "iq");
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (questions_asked_count(&events, "ask") >= 1).then_some(())
    });
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    // The asking attempt's session id was journaled onto AttemptFinished.
    let events = read_all(&run_dir).unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::AttemptFinished { node, session: Some(s), .. }
                if node == "ask" && s == "sess-42"
        )),
        "the asking attempt must record its captured session id"
    );
    // No downgrade was journaled: resume with a captured session went straight
    // through.
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::SupervisorAction { action, .. } if action == "interaction_downgraded"
        )),
        0,
        "a captured session must resume without a downgrade"
    );

    let argv = fs::read_to_string(&argvfile).expect("argv recorded by the stub");
    assert!(
        argv.contains("--resume") && argv.contains("sess-42"),
        "the re-invocation must carry the resume flag and session id: {argv}"
    );
    assert!(
        !argv.contains("prior questions and answers"),
        "resume must NOT append the Q&A transcript: {argv}"
    );
}

// --- (a) resume invocation fails at runtime -> downgrade to reprompt ---

#[test]
fn resume_runtime_failure_downgrades_and_completes_via_reprompt() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    let argvfile = bin.path().join("argv");
    seed_single(proj.path());
    seed_profile(proj.path(), "arch");
    write_config(cfg.path(), "resume");
    // Any invocation whose argv carries `--resume` fails (the resume form is the
    // suspect); a fresh (non-resume) invocation succeeds. So: inv1 (non-resume,
    // n=1) captures a session and asks; the resume attempt (argv has --resume)
    // exits nonzero -> runtime downgrade; the reprompt re-run (non-resume, n=2)
    // finishes. Every invocation records its full argv line.
    let body = format!(
        "a=\"{}\"\nc=\"{}\"\nprintf '%s\\n' \"$*\" >> \"$a\"\ncase \"$*\" in\n  *--resume*) echo boom 1>&2; exit 1 ;;\nesac\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '{{\"session_id\":\"sess-99\"}}'\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Which DB?\"}}'\n  exit 0\nfi\necho done\nexit 0",
        argvfile.display(),
        counter.display()
    );
    set_env(&make_stub(bin.path(), &body), home.path(), cfg.path());

    let (rx, handle) = spawn_run(proj.path(), "iq");
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (questions_asked_count(&events, "ask") >= 1).then_some(())
    });
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    // Exactly one downgrade was journaled (no ping-pong), naming the resume
    // failure.
    let downgrades: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction { action, detail, .. }
                if action == "interaction_downgraded" =>
            {
                Some(detail.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        downgrades.len(),
        1,
        "exactly one downgrade per failed round (no transport ping-pong): {downgrades:?}"
    );
    assert!(
        downgrades[0].contains("resume"),
        "the downgrade reason must name the resume failure: {}",
        downgrades[0]
    );
    // The reprompt re-run carried the transcript, and the node succeeded.
    let argv = fs::read_to_string(&argvfile).expect("argv recorded by the stub");
    assert!(
        argv.contains("--resume"),
        "the failed resume attempt must have run: {argv}"
    );
    assert!(
        argv.contains("## prior questions and answers") && argv.contains("A: pg"),
        "the downgrade must complete via a reprompt transcript: {argv}"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "ask" && status == "succeeded"
        )),
        "the node must succeed via the reprompt after the resume failure"
    );
}

// --- (d) no session captured -> downgrade to reprompt ---

#[test]
fn resume_without_session_downgrades_to_reprompt() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    let promptfile = bin.path().join("prompts");
    seed_single(proj.path());
    seed_profile(proj.path(), "arch");
    write_config(cfg.path(), "resume");
    // Invocation 1 asks a question but prints NO session id; capture fails, so
    // the answer round must downgrade to reprompt (transcript appended). Each
    // invocation records its prompt argument ($2 = the `-p` value).
    let body = format!(
        "c=\"{}\"\np=\"{}\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nprintf '%s\\n' \"$2\" >> \"$p\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"Which DB?\"}}'\n  exit 0\nfi\necho done\nexit 0",
        counter.display(),
        promptfile.display()
    );
    set_env(&make_stub(bin.path(), &body), home.path(), cfg.path());

    let (rx, handle) = spawn_run(proj.path(), "iq");
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (questions_asked_count(&events, "ask") >= 1).then_some(())
    });
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    // The downgrade is journaled, and its reason names the missing session.
    let downgrade = events.iter().find_map(|e| match &e.payload {
        EventPayload::SupervisorAction {
            action,
            node,
            detail,
        } if action == "interaction_downgraded" => Some((node.clone(), detail.clone())),
        _ => None,
    });
    let (node, detail) = downgrade.expect("a downgrade must be journaled");
    assert_eq!(node.as_deref(), Some("ask"));
    assert!(
        detail.contains("session"),
        "the downgrade reason must name the missing session: {detail}"
    );
    // The reprompt path appended the transcript.
    let prompts = fs::read_to_string(&promptfile).expect("prompts recorded by the stub");
    assert!(
        prompts.contains("## prior questions and answers") && prompts.contains("A: pg"),
        "the downgrade must fall back to the reprompt transcript: {prompts}"
    );
}

// --- (loop) the reprompt transcript is scoped to the current visit ---

/// A loop that re-enters the interactive `ask` node twice via a bounded back
/// edge (schema 2). `tick` is a plain prompt node so the loop can cycle without
/// another agent invocation.
fn seed_loop(root: &Path) {
    init_project(root).unwrap();
    let src = "schema: 2\nid: iloop\nname: ILoop\nversion: 1.0.0\ndefaults: { profile: arch }\nnodes:\n  - { id: start, type: start }\n  - { id: ask, type: agent_task, prompt: \"ask something\", interactive: true }\n  - { id: tick, type: prompt, prompt: \"tick\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: ask }\n  - { from: ask, to: tick }\n  - { from: tick, to: ask, max_traversals: 1 }\n  - { from: tick, to: done, fallback: true }\n";
    let dir = root.join(".apb/playbooks/iloop/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/iloop/current"), "1.0.0").unwrap();
}

#[test]
fn reprompt_transcript_is_scoped_to_the_current_visit() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("count");
    let pf = bin.path().join("prompt");
    seed_loop(proj.path());
    seed_profile(proj.path(), "arch");
    write_config(cfg.path(), "reprompt");
    // Visit 1: inv1 asks "v1q", inv2 finishes. Visit 2: inv3 asks "v2q", inv4
    // finishes. Each invocation records its prompt ($2) to `prompt.<n>`.
    let body = format!(
        "c=\"{}\"\npf=\"{}\"\nn=$(cat \"$c\" 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > \"$c\"\nprintf '%s\\n' \"$2\" > \"$pf.$n\"\nif [ \"$n\" = \"1\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"v1q\"}}'\n  exit 0\nfi\nif [ \"$n\" = \"3\" ]; then\n  printf '%s\\n' '<<<apb:question>>>'\n  printf '%s\\n' '{{\"question\":\"v2q\"}}'\n  exit 0\nfi\necho done\nexit 0",
        counter.display(),
        pf.display()
    );
    set_env(&make_stub(bin.path(), &body), home.path(), cfg.path());

    let (rx, handle) = spawn_run(proj.path(), "iloop");
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    // Answer visit 1's question.
    poll_until("first QuestionAsked (v1q)", || {
        let events = read_all(&run_dir).ok()?;
        (questions_asked_count(&events, "ask") >= 1).then_some(())
    });
    post_answer(&run_dir, Some("ask"), "v1ans", "human").unwrap();

    // Answer visit 2's question (only asked after v1 is answered and the loop
    // re-enters).
    poll_until("second QuestionAsked (v2q)", || {
        let events = read_all(&run_dir).ok()?;
        (questions_asked_count(&events, "ask") >= 2).then_some(())
    });
    post_answer(&run_dir, Some("ask"), "v2ans", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    // The visit-2 re-invocation (invocation 4) must carry ONLY visit 2's Q&A.
    let v2 = fs::read_to_string(pf.with_extension("4")).expect("prompt.4 written by the stub");
    assert!(
        v2.contains("## prior questions and answers"),
        "visit-2 re-invocation must carry a transcript: {v2}"
    );
    assert!(
        v2.contains("Q: v2q") && v2.contains("A: v2ans"),
        "visit-2 transcript must contain visit-2's Q&A: {v2}"
    );
    assert!(
        !v2.contains("v1q") && !v2.contains("v1ans"),
        "visit-2 transcript must NOT replay visit-1's round: {v2}"
    );
}

/// grok and cursor both emit JSON session ids only under their JSON output
/// modes; under the plain-text one-shot form used by the built-in invocation
/// they yield `None` and rely on the resume -> reprompt downgrade, exactly like
/// codex/opencode/hermes.
#[test]
fn capture_session_reads_grok_and_cursor_json_session_ids() {
    let grok = r#"{"type":"result","session_id":"5f0a1b2c-3d4e-4f50-8a9b-0c1d2e3f4a5b"}"#;
    assert_eq!(
        capture_session("grok", grok).as_deref(),
        Some("5f0a1b2c-3d4e-4f50-8a9b-0c1d2e3f4a5b")
    );

    let cursor = r#"{"type":"result","chatId":"cursor-chat-42"}"#;
    assert_eq!(
        capture_session("cursor", cursor).as_deref(),
        Some("cursor-chat-42")
    );

    let plain = "just the final answer text\n";
    assert_eq!(capture_session("grok", plain), None);
    assert_eq!(capture_session("cursor", plain), None);
}
