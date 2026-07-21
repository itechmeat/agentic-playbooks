//! Task 11: the live interactive transport for claude via the `apb __ask-server`
//! sidecar.
//!
//! - Injection: the spawned claude argv carries `--mcp-config` pointing at
//!   `apb __ask-server` with the run/node/attempt and the per-server timeout.
//! - Live observation: a stub that PRETENDS to be blocked (posts a question to
//!   the channel itself, then blocks until the answer file lands, then exits
//!   with a report) drives one long-lived attempt; drive - observing the
//!   channels on its own thread through the adapter's per-poll `on_tick` -
//!   journals `QuestionAsked` (+ a wake) and `QuestionAnswered`, and the attempt
//!   finishes with NO re-invocation (exactly one `attempt_started`).
//! - Node-timeout exclusion: a short `timeout_seconds` that WOULD fire during
//!   the open-question window does not, because the pending window is excluded.
//! - Downgrade: a `live`-configured non-claude agent journals
//!   `interaction_downgraded` and runs the marker/resume path instead.
//!
//! Bounded by construction: the stub blocks on the answer FILE, so it proceeds
//! only after the test posts the answer; its own loop is self-capped. Every
//! wait is a bounded poll whose panic message names what it waited on, and a
//! `RunReaper` built before the first panic point aborts and joins a still-live
//! drive so no orphaned thread survives a failed assertion.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::question::post_answer;
use apb_engine::scheduler::{RunOptions, resume, run};
use apb_engine::state::RunStatus;

use crate::common;

const POLL_DEADLINE: Duration = Duration::from_secs(15);
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

/// Aborts and joins a still-live drive on drop, so a failed assertion mid-run
/// never leaves an orphaned helper thread (and its stub child) spinning.
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

fn seed_profile(root: &Path, agent: &str) {
    let dir = root.join(".apb/profiles/arch");
    fs::create_dir_all(&dir).unwrap();
    let yaml = format!("name: arch\ndescription: d\nexecutor:\n  agent: {agent}\n  model: haiku\n");
    fs::write(dir.join("profile.yaml"), yaml).unwrap();
    fs::write(dir.join("SOUL.md"), "role").unwrap();
}

/// One-node interactive playbook `iq` (schema 1); `timeout` sets the `ask`
/// node's `timeout_seconds` (0 = omit).
fn seed_single(root: &Path, timeout: u64) {
    init_project(root).unwrap();
    let to = if timeout > 0 {
        format!(", timeout_seconds: {timeout}")
    } else {
        String::new()
    };
    let src = format!(
        "schema: 1\nid: iq\nname: IQ\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: ask, type: agent_task, prompt: \"ask something\", profile: arch, interactive: true{to} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: ask }}\n  - {{ from: ask, to: done }}\n"
    );
    let dir = root.join(".apb/playbooks/iq/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/iq/current"), "1.0.0").unwrap();
}

/// One-node interactive playbook `iq` with `question_timeout_seconds` and an
/// optional `default_answer` on the `ask` node.
fn seed_single_q(root: &Path, q_timeout: u64, default_answer: Option<&str>) {
    init_project(root).unwrap();
    let da = match default_answer {
        Some(a) => format!(", default_answer: \"{a}\""),
        None => String::new(),
    };
    // Route a node SUCCESS to the success finish and a node FAILURE (a
    // no-default question timeout) to the failure finish, so the run outcome
    // reflects the node's fate.
    let src = format!(
        "schema: 1\nid: iq\nname: IQ\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: ask, type: agent_task, prompt: \"ask something\", profile: arch, interactive: true, question_timeout_seconds: {q_timeout}{da} }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: no, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: ask }}\n  - {{ from: ask, to: done, condition: {{ type: node_status, node: ask, equals: success }} }}\n  - {{ from: ask, to: no, fallback: true }}\n"
    );
    let dir = root.join(".apb/playbooks/iq/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), src).unwrap();
    fs::write(root.join(".apb/playbooks/iq/current"), "1.0.0").unwrap();
}

/// A stub claude that posts the question and blocks on the answer file; when an
/// answer for `ask` lands it extracts the answer text into `received` (the tool
/// result the agent "got") and exits. Self-capped so a missing answer cannot
/// hang.
fn answer_recording_stub_body(received: &Path) -> String {
    format!(
        "rd=\"$APB_RUN_DIR\"\n\
         printf '%s\\n' '{{\"seq\":0,\"node\":\"ask\",\"attempt\":1,\"question\":\"Which DB?\",\"options\":[]}}' >> \"$rd/questions.jsonl\"\n\
         i=0\n\
         while ! grep -q '\"node\":\"ask\"' \"$rd/answers.jsonl\" 2>/dev/null; do\n\
         i=$((i+1)); [ \"$i\" -gt 400 ] && break\n\
         sleep 0.05\n\
         done\n\
         grep '\"node\":\"ask\"' \"$rd/answers.jsonl\" | tail -1 | sed 's/.*\"answer\":\"\\([^\"]*\\)\".*/\\1/' > \"{}\"\n\
         echo 'proceeded on the default'\n\
         exit 0",
        received.display()
    )
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

fn spawn_run(root: &Path) -> (mpsc::Receiver<RunStatus>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let handle = thread::spawn(move || {
        let res = run(&root, "iq", None, RunOptions::default());
        let status = res.map(|r| r.outcome).unwrap_or(RunStatus::Failed);
        let _ = tx.send(status);
    });
    (rx, handle)
}

fn spawn_resume(root: &Path, run_id: &str) -> (mpsc::Receiver<RunStatus>, thread::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let run_id = run_id.to_string();
    let handle = thread::spawn(move || {
        let res = resume(&root, &run_id, None);
        let status = res.map(|r| r.outcome).unwrap_or(RunStatus::Failed);
        let _ = tx.send(status);
    });
    (rx, handle)
}

/// A stub claude that pretends to block in `ask_user`: it appends a question to
/// `$APB_RUN_DIR/questions.jsonl` (the same record the sidecar would post), then
/// blocks until an answer for node `ask` lands in `answers.jsonl`, then prints a
/// report and exits. `argv_file`, when set, records the full argv line so the
/// injection can be asserted. Self-capped so a missing answer cannot hang.
fn blocking_stub_body(argv_file: Option<&Path>) -> String {
    let record = match argv_file {
        Some(p) => format!("printf '%s\\n' \"$*\" >> \"{}\"\n", p.display()),
        None => String::new(),
    };
    format!(
        "{record}rd=\"$APB_RUN_DIR\"\n\
         printf '%s\\n' '{{\"seq\":0,\"node\":\"ask\",\"attempt\":1,\"question\":\"Which DB?\",\"options\":[\"pg\",\"sqlite\"]}}' >> \"$rd/questions.jsonl\"\n\
         i=0\n\
         while ! grep -q '\"node\":\"ask\"' \"$rd/answers.jsonl\" 2>/dev/null; do\n\
         i=$((i+1)); [ \"$i\" -gt 400 ] && break\n\
         sleep 0.05\n\
         done\n\
         echo 'picked the database from the user answer'\n\
         exit 0"
    )
}

// --- (a) injection reaches the spawned claude argv ---

#[test]
fn live_injection_reaches_the_spawned_argv() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let argvfile = bin.path().join("argv");
    seed_single(proj.path(), 0);
    seed_profile(proj.path(), "claude");
    set_env(
        &make_stub(bin.path(), &blocking_stub_body(Some(&argvfile))),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let run_id = run_id_of(&run_dir);
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id.clone(),
        handle: Some(handle),
    };

    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (count_kind(
            &events,
            |p| matches!(p, EventPayload::QuestionAsked { node, .. } if node == "ask"),
        ) >= 1)
            .then_some(())
    });
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let argv = fs::read_to_string(&argvfile).expect("argv recorded by the stub");
    assert!(
        argv.contains("--mcp-config"),
        "claude must be spawned with --mcp-config: {argv}"
    );
    assert!(
        argv.contains("__ask-server"),
        "the injection must point claude at the ask-server sidecar: {argv}"
    );
    assert!(
        argv.contains(&run_id) && argv.contains("\"ask\"") && argv.contains("\"--attempt\""),
        "the injection must carry the run/node/attempt: {argv}"
    );
    // No `question_timeout_seconds` on the node, so the large default timeout is
    // injected (about 28 h in ms) so the blocking call outlives the idle timer.
    assert!(
        argv.contains("100800000"),
        "the injection must carry the large default per-server timeout: {argv}"
    );
    // The marker was NOT injected on the live path (the live paragraph is used
    // instead), yet no downgrade happened.
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::SupervisorAction { action, .. } if action == "interaction_downgraded"
        )),
        0,
        "an injectable live node must not downgrade"
    );
}

// --- (b) live observation end to end, single attempt, timeout excluded ---

#[test]
fn live_observation_journals_round_and_excludes_pending_from_timeout() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    // A short node timeout that WOULD fire while the question sits unanswered if
    // the pending window were not excluded from the clock (timing IS the subject
    // for this exclusion; the honest budget is the node's `timeout_seconds`).
    seed_single(proj.path(), 1);
    seed_profile(proj.path(), "claude");
    set_env(
        &make_stub(bin.path(), &blocking_stub_body(None)),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    // Drive observes the sidecar-posted question and journals it (+ a wake)
    // while the agent is still blocked (no answer yet).
    poll_until("QuestionAsked for node ask", || {
        let events = read_all(&run_dir).ok()?;
        (count_kind(&events, |p| {
            matches!(p, EventPayload::QuestionAsked { node, question, .. }
                if node == "ask" && question == "Which DB?")
        }) >= 1)
            .then_some(())
    });
    let events = read_all(&run_dir).unwrap();
    assert!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::WakeRaised { node, .. } if node == "ask"
        )) >= 1,
        "a wake must be raised when the live question appears"
    );
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::QuestionAnswered { node, .. } if node == "ask"
        )),
        0,
        "no answer yet, so no QuestionAnswered"
    );

    // Hold the question OPEN past the node timeout (1s) before answering. Without
    // the pending-window exclusion the node would be killed as timed out here.
    thread::sleep(Duration::from_millis(1500));
    let mid = read_all(&run_dir).unwrap();
    assert_eq!(
        count_kind(&mid, |p| matches!(
            p,
            EventPayload::NodeFinished { node, .. } if node == "ask"
        )),
        0,
        "the node must not finish (least of all time out) while the question is open"
    );

    // Answer; the agent unblocks and finishes in the SAME attempt.
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    // Exactly one QuestionAsked and one QuestionAnswered, answered_by human.
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::QuestionAsked { node, .. } if node == "ask"
        )),
        1,
        "exactly one live question"
    );
    let answered: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
            } if node == "ask" => Some((answer.clone(), answered_by.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(answered, vec![("pg".to_string(), "human".to_string())]);
    // Exactly one attempt for the node: the live attempt was NOT re-invoked.
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::AttemptStarted { node, .. } if node == "ask"
        )),
        1,
        "the live attempt must not be re-invoked (Q&A resolves in-attempt)"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "ask" && status == "succeeded"
        )),
        "the node must succeed after the live answer"
    );
}

// --- (c) live-configured non-claude agent downgrades ---

/// Config agent `livex` with `interaction: live` - not claude, so injection is
/// unavailable and the node downgrades.
fn write_livex_config(cfg: &Path) {
    let yaml = "agents:\n  livex:\n    invocation:\n      argv: [\"-p\", \"{prompt}\", \"--model\", \"{model}\"]\n      interaction: live\n";
    fs::write(cfg.join("config.yaml"), yaml).unwrap();
}

#[test]
fn live_configured_non_claude_agent_downgrades() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    seed_single(proj.path(), 0);
    seed_profile(proj.path(), "livex");
    write_livex_config(cfg.path());
    // The stub just finishes normally (no question): the downgrade is journaled
    // at the first-attempt guard, before the attempt runs, regardless.
    set_env(
        &make_stub(bin.path(), "echo done\nexit 0"),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    let downgrade = events.iter().find_map(|e| match &e.payload {
        EventPayload::SupervisorAction {
            action,
            node,
            detail,
        } if action == "interaction_downgraded" => Some((node.clone(), detail.clone())),
        _ => None,
    });
    let (node, detail) = downgrade.expect("a live->resume downgrade must be journaled");
    assert_eq!(node.as_deref(), Some("ask"));
    assert!(
        detail.contains("claude") && detail.contains("livex"),
        "the downgrade reason must name the claude requirement and the agent: {detail}"
    );
}

// --- (d) question timeout WITH default_answer: engine posts it ---

#[test]
fn live_question_timeout_posts_default_answer() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let received = bin.path().join("received");
    // question_timeout_seconds: 1 with a default; drive must post it as a
    // "timeout" answer (timing IS the subject; the honest budget is 1s).
    seed_single_q(proj.path(), 1, Some("proceed"));
    seed_profile(proj.path(), "claude");
    set_env(
        &make_stub(bin.path(), &answer_recording_stub_body(&received)),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    // Never answer from the test: the engine's question timeout must post the
    // default_answer on its own.
    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    let answered: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
            } if node == "ask" => Some((answer.clone(), answered_by.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        answered,
        vec![("proceed".to_string(), "timeout".to_string())],
        "the default answer must be journaled as a timeout answer"
    );
    // The agent (via the channel) received the default answer text.
    let got = fs::read_to_string(&received).expect("stub recorded the answer it got");
    assert_eq!(
        got.trim(),
        "proceed",
        "the blocked agent must receive the default answer text: {got}"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "ask" && status == "succeeded"
        )),
        "the node must succeed after the default answer"
    );
}

// --- (e) question timeout WITHOUT default_answer: attempt fails ---

#[test]
fn live_question_timeout_without_default_fails_the_attempt() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    // question_timeout_seconds: 1, no default: the open question times out, the
    // agent is torn down, and the attempt fails with the Task 5 wording.
    seed_single_q(proj.path(), 1, None);
    seed_profile(proj.path(), "claude");
    set_env(
        &make_stub(bin.path(), &blocking_stub_body(None)),
        home.path(),
        cfg.path(),
    );

    let (rx, handle) = spawn_run(proj.path());
    let run_dir = latest_run_dir(proj.path());
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: run_id_of(&run_dir),
        handle: Some(handle),
    };

    // Never answer: the timeout with no default must fail the node, and the
    // playbook routes that failure to the failure finish.
    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("run finished within deadline");
    assert_eq!(status, RunStatus::Failed);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    let finished = events.iter().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node,
            status,
            output,
            ..
        } if node == "ask" => Some((status.clone(), output.clone())),
        _ => None,
    });
    let (status, output) = finished.expect("the node must finish");
    assert_eq!(status, "failed", "the timed-out node must fail");
    assert!(
        output.contains("ask")
            && output.contains("timed out")
            && output.contains("no default_answer"),
        "the failure must name the node and the question timeout: {output}"
    );
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::QuestionAnswered { node, .. } if node == "ask"
        )),
        0,
        "no answer was posted, so no QuestionAnswered may appear"
    );
    // Exactly one attempt: a question timeout does not re-invoke the agent.
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::AttemptStarted { node, .. } if node == "ask"
        )),
        1,
        "the timed-out live attempt must not be re-invoked"
    );
}

// --- (f) live crash-recovery: an orphaned channel question is adopted, never
//         raced by a fresh live attempt (final-fix-wave Finding 1) ---

/// A stub claude that records each invocation (one line per spawn) to `counter`.
/// Before any answer exists it posts the sidecar-style question to
/// `questions.jsonl` and exits (the setup run's single spawn). Once an answer
/// for `ask` has landed it just reports and exits - the reprompt re-invocation
/// that consumes the adopted question's answer.
fn crash_recovery_stub_body(counter: &Path) -> String {
    format!(
        "printf '%s\\n' invoked >> \"{}\"\n\
         rd=\"$APB_RUN_DIR\"\n\
         if grep -q '\"node\":\"ask\"' \"$rd/answers.jsonl\" 2>/dev/null; then\n\
         echo 'finished with the answer'\n\
         exit 0\n\
         fi\n\
         printf '%s\\n' '{{\"seq\":0,\"node\":\"ask\",\"attempt\":1,\"question\":\"Which DB?\",\"options\":[\"pg\",\"sqlite\"]}}' >> \"$rd/questions.jsonl\"\n\
         exit 0",
        counter.display()
    )
}

/// Rewrites the run's journal to keep only through the FIRST `AttemptStarted`
/// for `ask`, simulating a crash in the exact window this fix covers: the
/// sidecar already posted to `questions.jsonl`, but drive never journaled the
/// matching `QuestionAsked`. The folded node is left Interrupted (open attempt,
/// no finish), which `plan_resume` restarts.
fn crash_after_attempt_started(run_dir: &Path) {
    let events = read_all(run_dir).unwrap();
    let cut = events
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::AttemptStarted { node, .. } if node == "ask"),
        )
        .expect("the setup run must have started an attempt for `ask`");
    let mut buf = String::new();
    for e in &events[..=cut] {
        buf.push_str(&serde_json::to_string(e).unwrap());
        buf.push('\n');
    }
    fs::write(run_dir.join("events.jsonl"), buf).unwrap();
}

fn count_lines(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

#[test]
fn live_crash_recovery_adopts_orphaned_channel_question() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let counter = bin.path().join("invocations");
    seed_single(proj.path(), 0);
    seed_profile(proj.path(), "claude");
    set_env(
        &make_stub(bin.path(), &crash_recovery_stub_body(&counter)),
        home.path(),
        cfg.path(),
    );

    // Phase 1 (setup): a real run establishes the run dir + manifest and lets
    // the stub post its question to questions.jsonl exactly as the live sidecar
    // would. The stub exits immediately, so the run terminates fast.
    let res = run(proj.path(), "iq", None, RunOptions::default()).expect("setup run drove");
    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    assert_eq!(
        count_lines(&run_dir.join("questions.jsonl")),
        1,
        "the setup run must leave exactly one channel question for `ask`"
    );
    assert_eq!(
        count_lines(&counter),
        1,
        "the setup run spawns the stub exactly once"
    );

    // Sculpt the crash shape: drop everything from QuestionAsked onward so the
    // journal knows nothing about the question the sidecar already posted, then
    // clear the spawn counter so any resume-time spawn is unambiguously new.
    crash_after_attempt_started(&run_dir);
    let _ = fs::remove_file(run_dir.join("answers.jsonl"));
    fs::remove_file(&counter).unwrap();

    // Phase 2 (resume): the guard must adopt the orphan (journal one
    // QuestionAsked + a wake) and park, WITHOUT spawning a fresh live attempt.
    let (rx, handle) = spawn_resume(proj.path(), &res.run_id);
    let mut reaper = RunReaper {
        root: proj.path().to_path_buf(),
        run_id: res.run_id.clone(),
        handle: Some(handle),
    };

    poll_until("adopted QuestionAsked for node ask on resume", || {
        let events = read_all(&run_dir).ok()?;
        (count_kind(&events, |p| {
            matches!(p, EventPayload::QuestionAsked { node, question, .. }
                if node == "ask" && question == "Which DB?")
        }) >= 1)
            .then_some(())
    });

    // The recovery journaled the question from the orphaned channel entry - it
    // did NOT spawn a fresh live attempt (which would have recorded a spawn and
    // could have raced the orphan's answer).
    assert_eq!(
        count_lines(&counter),
        0,
        "the crash-recovery guard must not spawn a fresh live attempt"
    );
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::QuestionAsked { node, .. } if node == "ask"
        )),
        1,
        "exactly one QuestionAsked, adopted from the orphaned channel entry"
    );
    assert!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::WakeRaised { node, .. } if node == "ask"
        )) >= 1,
        "a wake must be raised for the adopted question"
    );
    assert_eq!(
        count_kind(&events, |p| matches!(
            p,
            EventPayload::QuestionAnswered { node, .. } if node == "ask"
        )),
        0,
        "no answer posted yet, so the run stays parked (no QuestionAnswered)"
    );

    // Answer the adopted question; the node completes via the reprompt path
    // (one re-invocation) with that answer.
    post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();
    let status = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("resume finished within deadline");
    assert_eq!(status, RunStatus::Succeeded);
    reaper.join();

    let events = read_all(&run_dir).unwrap();
    let answered: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::QuestionAnswered {
                node,
                answer,
                answered_by,
            } if node == "ask" => Some((answer.clone(), answered_by.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        answered,
        vec![("pg".to_string(), "human".to_string())],
        "the adopted question is answered exactly once, by the human"
    );
    assert_eq!(
        count_lines(&counter),
        1,
        "exactly one re-invocation delivered the answer via the reprompt path"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, status, .. } if node == "ask" && status == "succeeded"
        )),
        "the node must succeed after the answer is delivered"
    );
}
