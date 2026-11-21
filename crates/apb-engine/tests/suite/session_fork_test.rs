//! Session fork (`continue_session: { from, mode: fork }`): a node starts
//! from a copy of its source's agent session and never writes into the
//! source. A recording stub stands in for the claude CLI: it keeps one file
//! per session, appends to the session it resumes and, given
//! `--fork-session`, copies the resumed session to a new id first and
//! prints that id. So the tests read back the argv every attempt got and
//! the source session's bytes at the end.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventLog, EventPayload, read_all};
use apb_engine::pause::{PauseBy, PauseMode, PauseOutcome, PauseRequest, request_pause};
use apb_engine::scheduler::{RunOptions, RunResult, resume_by, run};
use apb_engine::state::RunStatus;

use crate::common;
use crate::token_economy_test::{Invocation, invocations, recording_stub, seed_playbook};

const DEADLINE: Duration = Duration::from_secs(30);

/// The help text of a claude CLI that lists every flag of the fork form, or
/// (`fork == false`) every flag but `--fork-session`.
fn help_text(fork: bool) -> &'static str {
    if fork {
        "  -p, --print\n  --output-format <format>\n  --model <model>\n  --resume <id>\n  --fork-session  When resuming, create a new session ID\n  --session-id <uuid>\n"
    } else {
        "  -p, --print\n  --output-format <format>\n  --model <model>\n  --resume <id>\n  --session-id <uuid>\n"
    }
}

/// The session keeping part of the stub, then `part`. Inside `part`,
/// `$sid` is the session this process runs in (empty when none), and
/// `ok`, `fail` print a claude result object reporting it, `bare` a
/// plain report that names no session.
fn fork_stub(stub_dir: &Path, help_fork: bool, part: &str) -> String {
    let sessions = stub_dir.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let help = help_text(help_fork);
    let body = r#"if [ "$1" = --help ]; then echo x >> '@DIR@/help-calls'; printf '%s' '@HELP@'; exit 0; fi
printf '%s' "$*" > '@DIR@/argv-'"$NODE-$N"
S='@SESSIONS@'
sid=""; resume=""; fork=0; prev=""
for a in "$@"; do
  case "$prev" in --session-id) sid="$a";; --resume) resume="$a";; esac
  if [ "$a" = --fork-session ]; then fork=1; fi
  prev="$a"
done
if [ -n "$resume" ]; then
  if [ "$fork" = 1 ]; then sid="fork-$NODE-$N"; cp "$S/$resume" "$S/$sid"; else sid="$resume"; fi
fi
if [ -n "$sid" ]; then printf '%s %s\n' "$NODE" "$N" >> "$S/$sid"; fi
ok() { printf '{"type":"result","is_error":false,"result":"\\n```yaml\\nstatus: success\\nsummary: ok\\n```\\n","session_id":"%s"}\n' "$sid"; }
fail() { printf '{"type":"result","is_error":false,"result":"\\n```yaml\\nstatus: failure\\nsummary: not yet\\n```\\n","session_id":"%s"}\n' "$sid"; }
bare() { printf '\n```yaml\nstatus: %s\nsummary: x\n```\n' "$1"; }
@PART@
"#
    .replace("@HELP@", help)
    .replace("@SESSIONS@", &sessions.to_string_lossy())
    .replace("@DIR@", &stub_dir.to_string_lossy())
    .replace("@PART@", part);
    recording_stub(stub_dir, &body)
}

/// `assess` then `implement`, the second forking the first's session.
/// `extra` is spliced into `implement`.
fn one_fork(extra: &str) -> String {
    format!(
        r#"schema: 2
id: fork
name: Fork
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: assess, type: agent_task, prompt: "Assess the change." }}
  - {{ id: implement, type: agent_task, prompt: "Implement what you assessed.", continue_session: {{ from: assess, mode: fork }}{extra} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: assess }}
  - {{ from: assess, to: implement }}
  - {{ from: implement, to: done }}
"#
    )
}

/// `assess`, then `left` and `right` both forking it, then the finish.
fn two_forks(left_extra: &str) -> String {
    format!(
        r#"schema: 2
id: forks
name: Forks
version: 1.0.0
defaults: {{ profile: main }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: assess, type: agent_task, prompt: "Assess the change." }}
  - {{ id: left, type: agent_task, prompt: "Try the left way.", continue_session: {{ from: assess, mode: fork }}{left_extra} }}
  - {{ id: right, type: agent_task, prompt: "Try the right way.", continue_session: {{ from: assess, mode: fork }} }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: assess }}
  - {{ from: assess, to: left }}
  - {{ from: assess, to: right }}
  - {{ from: left, to: done }}
  - {{ from: right, to: done }}
"#
    )
}

fn seed(root: &Path, id: &str, yaml: &str, agent: &str) {
    init_project(root).unwrap();
    seed_playbook(root, id, yaml);
    common::seed_profile(root, "main", agent, "haiku", &[]);
}

/// Runs `id` under `stub` with the infrastructure backoff shortened.
fn run_stub(root: &Path, id: &str, stub: &str) -> RunResult {
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", stub);
    let _backoff = common::EnvVar::set(apb_engine::failure_class::BACKOFF_ENV, "1,1");
    run(root, id, None, RunOptions::default()).unwrap()
}

fn run_dir(root: &Path, res: &RunResult) -> PathBuf {
    root.join(".apb/runs").join(&res.run_id)
}

fn by_node<'a>(inv: &'a [Invocation], node: &str) -> Vec<&'a Invocation> {
    inv.iter().filter(|i| i.node == node).collect()
}

fn forks(i: &Invocation) -> bool {
    i.args.iter().any(|a| a == "--fork-session")
}

/// How often the stub printed its help. Kept in a file of its own: two
/// agents at once race on the recording stub's shared counter.
fn help_calls(stub_dir: &Path) -> usize {
    fs::read_to_string(stub_dir.join("help-calls")).map_or(0, |s| s.lines().count())
}

/// The argv of the `n`th process of `node`, as the stub wrote it.
fn argv(stub_dir: &Path, node: &str, n: u32) -> String {
    fs::read_to_string(stub_dir.join(format!("argv-{node}-{n}")))
        .unwrap_or_else(|e| panic!("{node} ran no process {n}: {e}"))
}

/// The source session's id (the one apb assigned `assess` at launch) and
/// its bytes now.
fn source(stub_dir: &Path) -> (String, String) {
    // The source's session is the one its last (successful) attempt ran in.
    let last = (1..)
        .take_while(|n| stub_dir.join(format!("argv-assess-{n}")).is_file())
        .last()
        .expect("assess ran");
    let first = argv(stub_dir, "assess", last);
    let sid = first
        .split(' ')
        .skip_while(|a| *a != "--session-id")
        .nth(1)
        .expect("assigned at launch")
        .to_string();
    let bytes = fs::read_to_string(stub_dir.join("sessions").join(&sid)).unwrap();
    (sid, bytes)
}

/// The argv contract a fork keeps, and what it leaves of the source: no
/// process of another node resumed the source session without the fork
/// flag (read from every argv the stub recorded), and the source session
/// file holds exactly the line its own last attempt wrote (the stub appends
/// to any session it resumes plainly, so a plain resume would show here).
fn assert_source_untouched(stub_dir: &Path) {
    let (sid, bytes) = source(stub_dir);
    let resume = format!("--resume {sid}");
    for e in fs::read_dir(stub_dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("argv-") || name.starts_with("argv-assess-") {
            continue;
        }
        let a = fs::read_to_string(e.path()).unwrap();
        assert!(
            !a.contains(&resume) || a.contains("--fork-session") || a.contains("--fork "),
            "{name} resumed the source without the fork flag: {a}"
        );
    }
    let last = (1..)
        .take_while(|n| stub_dir.join(format!("argv-assess-{n}")).is_file())
        .last()
        .unwrap();
    assert_eq!(
        bytes,
        format!("assess {last}\n"),
        "nothing but the source's own attempt wrote into it"
    );
}

/// One `session_handoff` event: node, warm, reason, mode.
type HandoffRow = (String, bool, Option<String>, Option<String>);

/// The `session_handoff` events of a journal.
fn handoffs(events: &[Event]) -> Vec<HandoffRow> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SessionHandoff {
                node,
                warm,
                reason,
                mode,
                ..
            } => Some((node.clone(), *warm, reason.clone(), mode.clone())),
            _ => None,
        })
        .collect()
}

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_run(root: &Path, id: &str) -> mpsc::Receiver<Result<RunResult, EngineError>> {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    let id = id.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, &id, None, RunOptions::default()));
    });
    rx
}

fn only_run_id(root: &Path) -> String {
    poll_until("the run directory", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .flatten()
            .find(|e| e.path().join("events.jsonl").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
    })
}

fn operator(mode: PauseMode) -> PauseRequest {
    PauseRequest {
        reason: Some("later".into()),
        by: Some(PauseBy::Operator),
        mode,
        ..PauseRequest::default()
    }
}

/// A stub whose first `implement` attempt waits on the file `gate`
/// after touching `agent_running`.
fn gated_stub(root: &Path) -> String {
    let d = root.display();
    fork_stub(
        root,
        true,
        &format!(
            r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then touch '{d}/agent_running'; while [ -f '{d}/gate' ]; do sleep 0.05; done; fi
ok"#
        ),
    )
}

/// Starts the one-fork run on a thread and pauses it `--now` while the
/// first `implement` attempt runs; returns the run id once paused.
fn paused_mid_fork(root: &Path) -> String {
    fs::write(root.join("gate"), "").unwrap();
    let rx = spawn_run(root, "fork");
    let run_id = only_run_id(root);
    poll_until("the fork attempt to start", || {
        root.join("agent_running").is_file().then_some(())
    });
    request_pause(root, &run_id, &operator(PauseMode::Now)).unwrap();
    let paused = rx
        .recv_timeout(DEADLINE)
        .expect("the drive returns")
        .expect("the drive must not fail");
    assert_eq!(paused.outcome, RunStatus::Paused);
    fs::remove_file(root.join("gate")).unwrap();
    run_id
}

/// Two forks of one source run at the same time (each waits until it sees
/// the other started), each from its own copy, and the source session is
/// as its own node left it.
#[cfg(unix)]
#[test]
fn two_forks_run_in_parallel_and_leave_the_source_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "forks", &two_forks(""), "claude");
    let d = root.display();
    let stub = fork_stub(
        root,
        true,
        &format!(
            r#"case "$NODE" in left) other=right;; right) other=left;; *) other="";; esac
if [ -n "$other" ]; then
  touch '{d}/started-'"$NODE"; i=0
  while [ ! -f '{d}/started-'"$other" ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done
  if [ -f '{d}/started-'"$other" ]; then touch '{d}/overlap-'"$NODE"; fi
fi
ok"#
        ),
    );
    let res = run_stub(root, "forks", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert!(
        root.join("overlap-left").is_file(),
        "left never saw right run"
    );
    assert!(
        root.join("overlap-right").is_file(),
        "right never saw left run"
    );
    // Two processes at once race on the recording stub's shared counter, so
    // each fork is read back from the session and the argv it left.
    let (sid, _) = source(root);
    for node in ["left", "right"] {
        let fork = fs::read_to_string(root.join("sessions").join(format!("fork-{node}-1")))
            .unwrap_or_else(|e| panic!("{node} forked no session: {e}"));
        assert_eq!(fork, format!("assess 1\n{node} 1\n"), "{node}");
        let argv = argv(root, node, 1);
        assert!(
            argv.contains(&format!("--resume {sid} --fork-session")),
            "{node}: {argv}"
        );
    }
    assert_source_untouched(root);
}

/// A retry inside the attempt loop continues the session the fork
/// reported, without the fork flag.
#[cfg(unix)]
#[test]
fn a_retry_after_a_fork_continues_the_fork() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "claude");
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then fail; exit 0; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    let (sid, _) = source(root);
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 2);
    assert_eq!(implement[0].flag("--resume"), Some(sid.as_str()));
    assert!(forks(implement[0]));
    assert_eq!(implement[1].flag("--resume"), Some("fork-implement-1"));
    assert!(!forks(implement[1]), "{:?}", implement[1].args);
    assert_source_untouched(root);
}

/// A forked attempt that reports no session leaves the binding on the
/// source: the retry forks the source again, never resumes it plainly.
#[cfg(unix)]
#[test]
fn a_fork_that_reports_no_session_never_resumes_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "claude");
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then bare failure; exit 0; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    let (sid, _) = source(root);
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 2);
    for (i, attempt) in implement.iter().enumerate() {
        assert_eq!(attempt.flag("--resume"), Some(sid.as_str()), "attempt {i}");
        assert!(forks(attempt), "attempt {i}: {:?}", attempt.args);
    }
    assert_source_untouched(root);
}

/// A `retry_delay` re-entry forks the source again (the fork's own earlier
/// session is left behind), and the prompt carries the retry briefing.
#[cfg(unix)]
#[test]
fn a_delayed_retry_forks_the_source_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "fork",
        &one_fork(", max_retries: 1, retry_delay: { initial: 1s }"),
        "claude",
    );
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then echo 'the agent made a mistake' 1>&2; exit 1; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RetryDeferred { .. })),
        "the retry was deferred"
    );
    let inv = invocations(root);
    let (sid, _) = source(root);
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 2);
    assert_eq!(implement[1].flag("--resume"), Some(sid.as_str()));
    assert!(forks(implement[1]), "{:?}", implement[1].args);
    let prompt = implement[1].prompt();
    assert!(
        prompt.contains("What earlier attempts of this step did"),
        "{prompt}"
    );
    assert!(prompt.contains("Implement what you assessed."), "{prompt}");
    let warm: Vec<bool> = handoffs(&events).iter().map(|h| h.1).collect();
    assert_eq!(warm, vec![true, true], "one warm fork per entry");
    assert_source_untouched(root);
}

/// A deferral on a fallback step: both nodes run under one profile whose
/// primary always fails, so the source finished on the fallback; the entry
/// after the deferral forks on that step's executor (the one that matches
/// the source), warm, while the primary, another model, starts cold and is
/// never resumed.
#[cfg(unix)]
#[test]
fn a_delayed_retry_on_a_fallback_step_forks_on_that_step() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "fork",
        &one_fork(", profile: other, max_retries: 1, retry_delay: { initial: 1s }").replace(
            "prompt: \"Assess the change.\" }",
            "prompt: \"Assess the change.\", profile: other }",
        ),
        "claude",
    );
    common::seed_profile(root, "other", "claude", "opus", &[("claude", "haiku")]);
    let d = root.display();
    // The primary (opus) always fails; the fallback (haiku) fails once,
    // then works. Matched on the flag: the retry briefing names opus too.
    let stub = fork_stub(
        root,
        true,
        &format!(
            r#"case "$*" in *"--model opus"*) echo 'the agent made a mistake' 1>&2; exit 1;; esac
if [ "$NODE" = implement ]; then
  case "$*" in
    *) if [ ! -f '{d}/fb.marker' ]; then touch '{d}/fb.marker'; echo 'the agent made a mistake' 1>&2; exit 1; fi;;
  esac
fi
ok"#
        ),
    );
    let res = run_stub(root, "fork", &stub);
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded, "{events:#?}");
    let inv = invocations(root);
    let (sid, _) = source(root);
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 4, "two attempts per step");
    for attempt in &implement[..2] {
        assert_eq!(attempt.flag("--model"), Some("opus"));
        assert!(attempt.flag("--resume").is_none(), "{:?}", attempt.args);
    }
    assert_eq!(implement[2].flag("--model"), Some("haiku"));
    assert!(
        implement[2].flag("--resume").is_none(),
        "the fallback starts fresh in the entry decided for the primary"
    );
    assert_eq!(implement[3].flag("--model"), Some("haiku"));
    assert_eq!(implement[3].flag("--resume"), Some(sid.as_str()));
    assert!(forks(implement[3]), "{:?}", implement[3].args);
    let warm: Vec<bool> = handoffs(&events).iter().map(|h| h.1).collect();
    assert_eq!(warm.first(), Some(&false), "the primary is another model");
    assert_eq!(
        warm.last(),
        Some(&true),
        "the entry on the fallback is warm"
    );
    assert_source_untouched(root);
}

/// A `--now` pause mid-attempt, then a resume: the resumed attempt forks the
/// source again, never the interrupted fork's session.
#[cfg(unix)]
#[test]
fn a_pause_and_resume_forks_the_source_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(""), "claude");
    let stub = gated_stub(root);
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let _grace = common::EnvVar::set("APB_PAUSE_GRACE_MS", "300");
    let run_id = paused_mid_fork(root);
    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    let (sid, _) = source(root);
    let implement = by_node(&inv, "implement");
    assert_eq!(implement.len(), 2, "the paused attempt and the resumed one");
    assert_eq!(implement[1].flag("--resume"), Some(sid.as_str()));
    assert!(forks(implement[1]), "{:?}", implement[1].args);
    assert_source_untouched(root);
}

/// A driver that resumed the run and died mid-attempt, then `apb resume`:
/// the attempt after the reaped one forks the source again.
#[cfg(unix)]
#[test]
fn a_driver_death_forks_the_source_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(""), "claude");
    let stub = gated_stub(root);
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let _grace = common::EnvVar::set("APB_PAUSE_GRACE_MS", "300");
    let run_id = paused_mid_fork(root);
    // The journal a driver that resumed the run and died mid-attempt
    // leaves: an attempt whose process is gone.
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = gone.id();
    gone.wait().unwrap();
    let rd = root.join(".apb/runs").join(&run_id);
    let mut log = EventLog::open(&rd).unwrap();
    log.append(EventPayload::resumed(
        "implement",
        Some(PauseBy::Operator),
        false,
    ))
    .unwrap();
    log.append(EventPayload::AttemptStarted {
        item: None,
        node: "implement".into(),
        attempt: 2,
        agent: "claude".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(dead_pid),
        spawn_ms: None,
        model: Some("haiku".into()),
        workdir: None,
        transcript: None,
        token: None,
        isolation: None,
        exec: None,
        cloud: None,
    })
    .unwrap();
    drop(log);
    let PauseOutcome::Journaled { .. } =
        request_pause(root, &run_id, &operator(PauseMode::Drain)).unwrap()
    else {
        panic!("a run nothing drives is paused on the spot");
    };
    let res = resume_by(root, &run_id, None, false, PauseBy::Operator).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    let (sid, _) = source(root);
    let implement = by_node(&inv, "implement");
    assert_eq!(
        implement.len(),
        2,
        "the paused attempt and the one after the death"
    );
    assert_eq!(implement[1].flag("--resume"), Some(sid.as_str()));
    assert!(forks(implement[1]), "{:?}", implement[1].args);
    assert_source_untouched(root);
}

/// codex has no fork form: the node starts cold with the reason, never
/// falls back to a plain continue.
#[cfg(unix)]
#[test]
fn a_fork_on_codex_starts_cold_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(""), "codex");
    // Any help probe is counted where `help_calls` reads it, so the zero
    // below fails when the engine probes codex.
    let body = r#"for a in "$@"; do if [ "$a" = --help ]; then echo x >> '@D@/help-calls'; exit 0; fi; done
echo 'session id: 0199aa00-1111-7222-8333-444455556666' 1>&2; printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'"#
        .replace("@D@", &root.display().to_string());
    let stub = recording_stub(root, &body);
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    assert!(!by_node(&inv, "implement")[0].resumed());
    assert_eq!(help_calls(root), 0, "nothing to probe without a fork form");
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(
        handoffs(&events),
        vec![(
            "implement".to_string(),
            false,
            Some("agent `codex` cannot fork a session".to_string()),
            Some("fork".to_string()),
        )]
    );
}

/// An installed claude whose help does not list `--fork-session` is never
/// handed the flag: the node starts cold with the reason.
#[cfg(unix)]
#[test]
fn a_fork_on_a_cli_without_the_flag_starts_cold() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(""), "claude");
    let stub = fork_stub(root, false, "ok");
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    let implement = by_node(&inv, "implement");
    assert!(
        implement[0].flag("--resume").is_none(),
        "{:?}",
        implement[0].args
    );
    assert!(!forks(implement[0]));
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(
        handoffs(&events),
        vec![(
            "implement".to_string(),
            false,
            Some("the installed claude CLI does not list --fork-session in its help".to_string()),
            Some("fork".to_string()),
        )]
    );
    assert_source_untouched(root);
}

/// Two forks and a re-entry of one of them in one process: the help is
/// probed once.
#[cfg(unix)]
#[test]
fn the_help_probe_runs_once_per_process_and_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "forks",
        &two_forks(", max_retries: 1, retry_delay: { initial: 1s }"),
        "claude",
    );
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = left ] && [ "$N" = 1 ]; then echo 'the agent made a mistake' 1>&2; exit 1; fi
ok"#,
    );
    let res = run_stub(root, "forks", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    // Two agents at once race on the recording stub's shared counter, so
    // the argv is read from the files the stub keeps per node.
    let (sid, _) = source(root);
    let forked = format!("--resume {sid} --fork-session");
    for (node, n) in [("left", 1), ("left", 2), ("right", 1)] {
        let a = argv(root, node, n);
        assert!(a.contains(&forked), "{node} {n}: {a}");
    }
    assert_eq!(help_calls(root), 1, "one probe per process and binary");
    assert_source_untouched(root);
}

/// The stub replaced at the same path by one whose help lacks the flag: the
/// next fork probes again and starts cold.
#[cfg(unix)]
#[test]
fn a_replaced_cli_at_the_same_path_is_probed_again() {
    let stubs = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    seed(first.path(), "fork", &one_fork(""), "claude");
    seed(second.path(), "fork", &one_fork(""), "claude");
    let stub = fork_stub(stubs.path(), true, "ok");
    let res = run_stub(first.path(), "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(first.path(), &res)).unwrap();
    assert!(handoffs(&events)[0].1, "the first binary forks warm");

    let replaced = fork_stub(stubs.path(), false, "ok");
    assert_eq!(replaced, stub, "the same path");
    let res = run_stub(second.path(), "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(second.path(), &res)).unwrap();
    let h = handoffs(&events);
    assert!(!h[0].1, "{h:?}");
    assert_eq!(
        h[0].2.as_deref(),
        Some("the installed claude CLI does not list --fork-session in its help")
    );
    assert_eq!(help_calls(stubs.path()), 2);
}

/// The handoff of a fork is journaled with `mode: fork`.
#[cfg(unix)]
#[test]
fn the_handoff_journals_mode_fork() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(""), "claude");
    let stub = fork_stub(root, true, "ok");
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    assert_eq!(
        handoffs(&events),
        vec![(
            "implement".to_string(),
            true,
            None,
            Some("fork".to_string())
        )]
    );
    let journal = fs::read_to_string(run_dir(root, &res).join("events.jsonl")).unwrap();
    assert!(journal.contains(r#""mode":"fork""#), "{journal}");
}

/// A fork that reports its source's own session id did not fork: it is
/// never continued. The handoff journals the reason, and the retry starts
/// a fresh agent instead of resuming the reported (source) session.
#[cfg(unix)]
#[test]
fn a_fork_that_reports_the_source_session_is_never_continued() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "claude");
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then sid="$resume"; fail; exit 0; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let (sid, _) = source(root);
    let second = argv(root, "implement", 2);
    assert!(
        !second.contains("--resume"),
        "the retry starts fresh: {second}"
    );
    let events = read_all(&run_dir(root, &res)).unwrap();
    let h = handoffs(&events);
    assert_eq!(h.len(), 2, "{h:?}");
    assert!(h[0].1, "the entry forked warm");
    assert_eq!(
        (h[1].1, h[1].2.as_deref(), h[1].3.as_deref()),
        (
            false,
            Some(
                "the fork reported the source's own session id, so it did not fork: the next attempt starts a fresh agent"
            ),
            Some("fork")
        )
    );
    assert!(!second.contains(&sid));
}

/// A fork hands the source's whole conversation to the node: a node under
/// another profile on the same agent and model starts cold with the reason,
/// and its CLI's help is never probed (the probe runs last).
#[cfg(unix)]
#[test]
fn a_fork_under_another_profile_starts_cold_without_a_probe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", profile: twin"), "claude");
    common::seed_profile(root, "twin", "claude", "haiku", &[]);
    fs::write(root.join(".apb/profiles/twin/SOUL.md"), "Another role.").unwrap();
    let stub = fork_stub(root, true, "ok");
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    let h = handoffs(&events);
    assert!(!h[0].1, "{h:?}");
    assert!(
        h[0].2.as_deref().is_some_and(
            |r| r.contains("ran under profile project/main, this node runs under project/twin")
        ),
        "{h:?}"
    );
    assert_eq!(
        help_calls(root),
        0,
        "no probe for a node that does not fork"
    );
    assert!(!argv(root, "implement", 1).contains("--resume"));
}

/// The opencode fork form end to end: the node resumes the source with
/// `--session <id> --fork`, the CLI starts from a copy, the source session
/// is as its own attempt left it, and a retry continues the fork's own
/// session with no fork flag.
#[cfg(unix)]
#[test]
fn an_opencode_fork_starts_from_a_copy_and_a_retry_continues_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "opencode");
    let s = root.join("sessions");
    fs::create_dir_all(&s).unwrap();
    let d = root.display();
    let body = r#"if [ "$1" = run ] && [ "$2" = --help ]; then echo x >> '@D@/help-calls'; printf '%s\n' '  --session <id>' '  --fork  Fork the session before continuing' '  --format <f>' '  -m, --model <m>' '  --title <t>'; exit 0; fi
if [ "$1" = session ]; then printf '[{"id":"ses_src","title":"%s"}]' "$(cat '@D@/title')"; exit 0; fi
printf '%s' "$*" > '@D@/argv-'"$NODE-$N"
S='@D@/sessions'
sid=""; resume=""; fork=0; prev=""
for a in "$@"; do
  case "$prev" in --session) resume="$a";; --title) printf '%s' "$a" > '@D@/title'; sid=ses_src;; esac
  if [ "$a" = --fork ]; then fork=1; fi
  prev="$a"
done
if [ -n "$resume" ]; then
  if [ "$fork" = 1 ]; then sid="fork-$NODE-$N"; cp "$S/$resume" "$S/$sid"; else sid="$resume"; fi
fi
if [ -n "$sid" ]; then printf '%s %s\n' "$NODE" "$N" >> "$S/$sid"; fi
printf '{"type":"step_start","sessionID":"%s"}\n' "$sid"
if [ "$NODE" = implement ] && [ "$N" = 1 ]; then printf '\n```yaml\nstatus: failure\nsummary: not yet\n```\n'; exit 0; fi
printf '\n```yaml\nstatus: success\nsummary: ok\n```\n'
"#
    .replace("@D@", &d.to_string());
    let stub = recording_stub(root, &body);
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let first = argv(root, "implement", 1);
    assert!(first.contains("--session ses_src --fork"), "{first}");
    let second = argv(root, "implement", 2);
    assert!(second.contains("--session fork-implement-1"), "{second}");
    assert!(
        !second.contains("--fork"),
        "the retry continues the fork: {second}"
    );
    let events = read_all(&run_dir(root, &res)).unwrap();
    let h = handoffs(&events);
    assert_eq!((h[0].1, h[0].3.as_deref()), (true, Some("fork")), "{h:?}");
    let src = fs::read_to_string(s.join("ses_src")).unwrap();
    assert_eq!(src, "assess 1\n", "the source holds only its own attempt");
    let fork = fs::read_to_string(s.join("fork-implement-1")).unwrap();
    assert_eq!(
        fork, "assess 1\nimplement 1\nimplement 2\n",
        "the fork started from a copy of the source and the retry continued it"
    );
    assert_eq!(help_calls(root), 1);
}

/// A forked attempt that fails but printed its new session id: the retry
/// inside the attempt loop continues that fork, never the source.
#[cfg(unix)]
#[test]
fn a_failed_fork_that_printed_its_session_is_continued() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "claude");
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then printf '{"type":"result","is_error":true,"result":"broke","session_id":"%s"}\n' "$sid"; echo 'the agent made a mistake' 1>&2; exit 1; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let second = argv(root, "implement", 2);
    assert!(second.contains("--resume fork-implement-1"), "{second}");
    assert!(!second.contains("--fork-session"), "{second}");
    assert_source_untouched(root);
}

/// The source ran under one profile and was rebound to the fork's profile
/// after it ran: the fork compares the binding the source's attempt
/// recorded when it ran, not its binding now, so it starts cold and names
/// both profiles. `assess` succeeds under `main`, the run holds at `hold`,
/// `assess` is rebound to `twin` (same agent and model, another role prompt),
/// then `implement` under `twin` must not fork `main`'s conversation.
#[cfg(unix)]
#[test]
fn a_source_rebound_after_it_ran_is_not_forked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: fork
name: Fork
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: assess, type: agent_task, prompt: "Assess the change." }
  - { id: hold, type: agent_task, prompt: "Hold." }
  - { id: implement, type: agent_task, prompt: "Implement.", profile: twin, continue_session: { from: assess, mode: fork } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: assess }
  - { from: assess, to: hold }
  - { from: hold, to: implement }
  - { from: implement, to: done }
"#;
    seed(root, "fork", yaml, "claude");
    common::seed_profile(root, "twin", "claude", "haiku", &[]);
    fs::write(root.join(".apb/profiles/twin/SOUL.md"), "Another role.").unwrap();
    let d = root.display().to_string();
    let part = r#"if [ "$NODE" = hold ]; then touch '@D@/holding'; while [ -f '@D@/hold' ]; do sleep 0.05; done; fi
ok"#
    .replace("@D@", &d);
    let stub = fork_stub(root, true, &part);
    fs::write(root.join("hold"), "").unwrap();
    let (tx, rx) = mpsc::channel();
    {
        let (r, s) = (root.to_path_buf(), stub.clone());
        std::thread::spawn(move || {
            let _ = tx.send(run_stub(&r, "fork", &s));
        });
    }
    let start = Instant::now();
    while !root.join("holding").exists() {
        assert!(start.elapsed() < DEADLINE, "hold never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let run_id = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .file_name()
        .to_string_lossy()
        .into_owned();
    let (_, _, bundle) = apb_core::profile_store::compute_bundle(
        root,
        apb_core::profile_store::PlaybookOrigin::Project,
        &apb_core::profile::QualifiedProfileRef {
            name: "twin".into(),
            scope: apb_core::profile::ProfileScope::Auto,
        },
    )
    .unwrap();
    apb_engine::scheduler::post_supervisor_command(
        root,
        &run_id,
        apb_engine::control::Control::Rebind {
            node: "assess".into(),
            profile: "twin".into(),
            scope: apb_core::profile::ProfileScope::Auto,
            bundle,
            reason: None,
        },
    )
    .unwrap();
    fs::remove_file(root.join("hold")).unwrap();
    let res = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    let rebound = events
        .iter()
        .find(|e| matches!(&e.payload,
            EventPayload::ProfileRebound { node, profile, .. } if node == "assess" && profile == "project/twin"))
        .map(|e| e.seq)
        .expect("the source was rebound after it ran");
    let handoff = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::SessionHandoff { node, .. } if node == "implement"))
        .map(|e| e.seq)
        .expect("the fork's handoff");
    // Only a rebind applied before the decision tells the binding the
    // source recorded from its binding now.
    assert!(
        rebound < handoff,
        "the rebind came before the fork's handoff"
    );
    let h: Vec<_> = handoffs(&events)
        .into_iter()
        .filter(|r| r.0 == "implement")
        .collect();
    assert!(!h[0].1, "{h:?}");
    assert!(
        h[0].2.as_deref().is_some_and(
            |r| r.contains("ran under profile project/main, this node runs under project/twin")
        ),
        "{h:?}"
    );
    assert!(!argv(root, "implement", 1).contains("--resume"));
}

/// The Codex sequence, with no timing dependence: the source ran under
/// `main`; the target, under `twin` (same agent and model, another role
/// prompt), fails and defers its retry; the run is paused during the delay,
/// the rebind of the target to `main` is queued while nothing drives it, and
/// the resume releases the ten-minute delay through the retry-now path and
/// applies the rebind before the deferred retry runs. That retry still
/// runs on its pinned binding (`twin`), so the fork is compared against that
/// binding and starts cold.
#[cfg(unix)]
#[test]
fn a_deferred_retry_rebound_during_its_delay_is_not_forked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "fork",
        &one_fork(", profile: twin, max_retries: 1, retry_delay: { initial: 10m }"),
        "claude",
    );
    common::seed_profile(root, "twin", "claude", "haiku", &[]);
    fs::write(root.join(".apb/profiles/twin/SOUL.md"), "Another role.").unwrap();
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then echo 'the agent made a mistake' 1>&2; exit 1; fi
ok"#,
    );
    let _lock = common::env_lock();
    let _cmd = common::EnvVar::set("APB_AGENT_CMD", &stub);
    let _backoff = common::EnvVar::set(apb_engine::failure_class::BACKOFF_ENV, "1,1");
    let (tx, rx) = mpsc::channel();
    {
        let r = root.to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "fork", None, RunOptions::default()));
        });
    }
    // A ten-minute delay releases the run: the drive returns with the retry
    // deferred and nothing driving it, so nothing below races a driver.
    let released = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    let run_id = released.run_id.clone();
    assert!(
        read_all(&run_dir(root, &released))
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RetryDeferred { .. })),
        "the retry was deferred"
    );
    let req = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(root, &run_id, &req).unwrap();
    let (_, _, bundle) = apb_core::profile_store::compute_bundle(
        root,
        apb_core::profile_store::PlaybookOrigin::Project,
        &apb_core::profile::QualifiedProfileRef {
            name: "main".into(),
            scope: apb_core::profile::ProfileScope::Auto,
        },
    )
    .unwrap();
    apb_engine::scheduler::post_supervisor_command(
        root,
        &run_id,
        apb_engine::control::Control::Rebind {
            node: "implement".into(),
            profile: "main".into(),
            scope: apb_core::profile::ProfileScope::Auto,
            bundle,
            reason: None,
        },
    )
    .unwrap();
    // The ten-minute delay is released through the retry-now path, never
    // waited out: nothing in this test depends on timing.
    let req = apb_engine::scheduler::ResumeRequest {
        retry_now: Some(apb_engine::scheduler::RetryNow {
            node: Some("implement"),
            by: apb_engine::scheduler::deferred::ExpediteBy::Operator,
        }),
        ..apb_engine::scheduler::ResumeRequest::operator()
    };
    let res = apb_engine::resume_req(root, &run_id, &req).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir(root, &res)).unwrap();
    let rebound = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::ProfileRebound { node, .. } if node == "implement"))
        .map(|e| e.seq)
        .expect("the target was rebound");
    let retry = events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::SessionHandoff { node, .. } if node == "implement"))
        .expect("the deferred entry's handoff");
    assert!(
        rebound < retry.seq,
        "the rebind came before the deferred retry"
    );
    let EventPayload::SessionHandoff { warm, reason, .. } = &retry.payload else {
        unreachable!()
    };
    assert!(!warm, "the deferred entry starts cold");
    assert!(
        reason
            .as_deref()
            .is_some_and(|r| r.contains("this node runs under project/twin")),
        "{reason:?}"
    );
    let inv = invocations(root);
    assert!(
        by_node(&inv, "implement").iter().all(|i| !forks(i)),
        "no attempt forked"
    );
}

/// A fork that reports its source's own session id leaves nothing of the
/// source in its attempt: the session transcript is copied only after the
/// reported id is checked, so the source's record is never copied in.
#[cfg(unix)]
#[test]
fn a_fork_that_reports_the_source_session_copies_no_source_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "claude");
    let d = root.display().to_string();
    // claude keeps its session records under `projects/` of its config
    // directory: the stub puts the source's there under its id.
    let part = r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then mkdir -p '@D@/claude/projects/p'; cp "$S/$resume" '@D@/claude/projects/p/'"$resume.jsonl"; sid="$resume"; fail; exit 0; fi
ok"#
    .replace("@D@", &d);
    let stub = fork_stub(root, true, &part);
    let res = {
        let _lock = common::env_lock();
        let _cmd = common::EnvVar::set("APB_AGENT_CMD", stub);
        let _claude = common::EnvVar::set("CLAUDE_CONFIG_DIR", format!("{d}/claude"));
        let _backoff = common::EnvVar::set(apb_engine::failure_class::BACKOFF_ENV, "1,1");
        run(root, "fork", None, RunOptions::default()).unwrap()
    };
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let (sid, _) = source(root);
    assert!(
        root.join(format!("claude/projects/p/{sid}.jsonl"))
            .is_file(),
        "the source's record was where a copy would find it"
    );
    let run_dir = run_dir(root, &res);
    let first = read_all(&run_dir)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::AttemptStarted {
                node,
                attempt: 1,
                transcript,
                ..
            } if node == "implement" => transcript,
            _ => None,
        })
        .expect("the first fork attempt names its transcript");
    let first = run_dir.join(first);
    assert!(first.join("stdout.log").is_file(), "{}", first.display());
    assert!(
        !first.join("session.jsonl").exists(),
        "the source's session was copied into the fork's attempt"
    );
}

/// A fork that wrote a success verdict to its status file and then exited
/// abnormally still succeeds with the session it reported: the attempt
/// journals that session, so a later node continues the fork, warm.
#[cfg(unix)]
#[test]
fn a_fork_success_from_the_status_file_is_continued_by_a_later_node() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = r#"schema: 2
id: fork
name: Fork
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: assess, type: agent_task, prompt: "Assess the change." }
  - { id: implement, type: agent_task, prompt: "Implement.", continue_session: { from: assess, mode: fork } }
  - { id: finalize, type: agent_task, prompt: "Finalize.", continue_session: implement }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: assess }
  - { from: assess, to: implement }
  - { from: implement, to: finalize }
  - { from: finalize, to: done }
"#;
    seed(root, "fork", yaml, "claude");
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ]; then printf '{"status":"success","outputs":"done"}' > "$APB_STATUS_FILE"; printf '{"type":"result","is_error":true,"result":"broke","session_id":"%s"}\n' "$sid"; echo 'the agent made a mistake' 1>&2; exit 1; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let inv = invocations(root);
    assert_eq!(by_node(&inv, "implement").len(), 1, "the verdict decided");
    let events = read_all(&run_dir(root, &res)).unwrap();
    let journaled = events.iter().find_map(|e| match &e.payload {
        EventPayload::AttemptFinished {
            node,
            status,
            session,
            ..
        } if node == "implement" && status == "succeeded" => Some(session.clone()),
        _ => None,
    });
    assert_eq!(journaled, Some(Some("fork-implement-1".to_string())));
    let h: Vec<_> = handoffs(&events)
        .into_iter()
        .filter(|r| r.0 == "finalize")
        .collect();
    assert_eq!(h.len(), 1, "{h:?}");
    assert!(h[0].1, "the later node continues the fork warm: {h:?}");
    let finalize = argv(root, "finalize", 1);
    assert!(finalize.contains("--resume fork-implement-1"), "{finalize}");
    assert!(!finalize.contains("--fork-session"), "{finalize}");
    assert_source_untouched(root);
}

/// A failed fork whose one result object is larger than 64 KiB, with its
/// session id last: the whole record is read, so the retry continues the
/// fork instead of forking the source again.
#[cfg(unix)]
#[test]
fn a_failed_fork_with_a_large_result_is_continued() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "fork", &one_fork(", max_retries: 1"), "claude");
    let stub = fork_stub(
        root,
        true,
        r#"if [ "$NODE" = implement ] && [ "$N" = 1 ]; then big=$(head -c 100000 /dev/zero | tr '\0' a); printf '{"type":"result","is_error":true,"result":"%s","session_id":"%s"}\n' "$big" "$sid"; echo 'the agent made a mistake' 1>&2; exit 1; fi
ok"#,
    );
    let res = run_stub(root, "fork", &stub);
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let second = argv(root, "implement", 2);
    assert!(second.contains("--resume fork-implement-1"), "{second}");
    assert!(!second.contains("--fork-session"), "{second}");
    assert_source_untouched(root);
}
