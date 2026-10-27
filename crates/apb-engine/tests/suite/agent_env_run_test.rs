//! A profile's `process_env: allowlist` through whole runs: what each
//! attempt's agent was given and what the journal says about it, across a
//! fallback onto another agent and a pause and resume, and every other
//! agent a run spawns (a judge's emulation, a finish answer, the
//! background supervisor, a quota probe, the context summarizer). The stub agent
//! records the NAMES of its environment per call; no value is asserted or
//! printed.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::pause::{
    PauseBy, PauseMode, PauseRequest, WakeProbe, probe_before_wake, request_pause,
};
use apb_engine::scheduler::{RunMode, RunOptions, resume, run, run_background};
use apb_engine::state::{RunState, RunStatus};

use crate::common::{self, EnvVar};

const OPERATOR_CANARY: &str = "APB_TEST_OPERATOR_SECRET";
const CLAUDE_CANARY: &str = "ANTHROPIC_APB_RUN_CANARY";
const CODEX_CANARY: &str = "CODEX_APB_RUN_CANARY";
const DEADLINE: Duration = Duration::from_secs(20);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One agent_task node `work` bound to the default profile.
const WORK: &str = "schema: 1\nid: envpb\nname: Env\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"do\", max_retries: 0 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

/// A project with playbook `envpb` (by default [`WORK`]) whose nodes bind
/// profile `main` (claude-code, then the given fallbacks) carrying
/// `process_env`.
struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(process_env: &str) -> Self {
        Self::with(WORK, &[("codex", "m")], process_env)
    }

    fn with(playbook: &str, fallbacks: &[(&str, &str)], process_env: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_project(root).unwrap();
        let vdir = root.join(".apb/playbooks/envpb/1.0.0");
        fs::create_dir_all(&vdir).unwrap();
        fs::write(vdir.join("playbook.yaml"), playbook).unwrap();
        fs::write(root.join(".apb/playbooks/envpb/current"), "1.0.0").unwrap();
        common::seed_profile(root, "main", "claude-code", "haiku", fallbacks);
        let profile = root.join(".apb/profiles/main/profile.yaml");
        let mut yaml = fs::read_to_string(&profile).unwrap();
        yaml.push_str(process_env);
        fs::write(&profile, yaml).unwrap();
        Project { dir }
    }

    /// The env every test of this file runs under: the stub agent, HOME,
    /// CODEX_HOME and the config dir in temp dirs, the canaries set.
    fn env(&self, agent: String, home: &Path, cfg: &Path) -> Vec<EnvVar> {
        vec![
            EnvVar::set("APB_AGENT_CMD", agent),
            EnvVar::set("HOME", home),
            EnvVar::set("CODEX_HOME", home.join(".codex")),
            EnvVar::set("APB_CONFIG_DIR", cfg),
            EnvVar::set(OPERATOR_CANARY, "1"),
            EnvVar::set(CLAUDE_CANARY, "1"),
            EnvVar::set(CODEX_CANARY, "1"),
        ]
    }

    /// A stub agent: numbers its calls, writes the `env -0` of call N to
    /// `env-N`, waits while the file `slow` exists, then answers. A call
    /// listed in `fail_calls` prints its text on stderr and exits 1. An
    /// emulated decision gets a one-question answer; any other call prints
    /// 300 bytes, enough to trip a small context limit.
    fn agent(&self, fail_calls: &[(u32, &str)]) -> String {
        let d = self.root().display();
        let fails: Vec<String> = fail_calls
            .iter()
            .map(|(n, err)| format!("[ \"$n\" = \"{n}\" ] && {{ echo '{err}' 1>&2; exit 1; }}"))
            .collect();
        let path = self.root().join("agent.sh");
        common::write_sync(
            &path,
            &format!(
                "#!/bin/sh\nn=$(cat '{d}/count' 2>/dev/null || echo 0); n=$((n+1)); echo \"$n\" > '{d}/count'\nenv -0 > \"{d}/env-$n.tmp\"; mv \"{d}/env-$n.tmp\" \"{d}/env-$n\"\ntouch '{d}/agent_running'\nwhile [ -f '{d}/slow' ]; do sleep 0.05; done\n{}\ncase \"$*\" in\n  *'stand in for a decision model'*) echo '{{\"q1\": 0.9}}' ;;\n  *) printf 'X%.0s' $(seq 1 300); echo ;;\nesac\n",
                fails.join("\n")
            ),
        );
        let mut perm = fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&path, perm).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// How many times the stub ran.
    fn calls(&self) -> u32 {
        fs::read_to_string(self.root().join("count"))
            .map(|s| s.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The variable names call `n` of the agent was given.
    fn names(&self, n: u32) -> BTreeSet<String> {
        fs::read(self.root().join(format!("env-{n}")))
            .unwrap()
            .split(|b| *b == 0)
            .filter_map(|e| {
                String::from_utf8_lossy(e)
                    .split_once('=')
                    .map(|(k, _)| k.to_string())
            })
            .collect()
    }

    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root().join(".apb/runs").join(run_id)
    }
}

/// The `agent_env_applied` events: (attempt, agent, withheld).
fn applied(events: &[Event]) -> Vec<(u32, String, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::AgentEnvApplied {
                attempt,
                agent,
                withheld,
                ..
            } => Some((*attempt, agent.clone(), withheld.clone())),
            _ => None,
        })
        .collect()
}

fn attempts(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::AttemptStarted { .. }))
        .count()
}

const ALLOWLIST: &str = "process_env:\n  policy: allowlist\n";

/// One `agent_env_applied` per attempt, each for its own agent: the codex
/// fallback does not get claude's variables, and neither gets the
/// operator's.
#[test]
fn each_attempt_journals_its_own_agents_allowlist_across_a_fallback() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::new(ALLOWLIST);
    let _env = p.env(p.agent(&[(1, "boom")]), home.path(), cfg.path());
    let res = run(p.root(), "envpb", None, RunOptions::default()).expect("run");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    let got = applied(&events);
    assert_eq!(got.len(), attempts(&events), "one event per attempt");
    assert_eq!(
        got.iter()
            .map(|(a, agent, _)| (*a, agent.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "claude-code"), (2, "codex")]
    );
    assert!(got[0].2.iter().any(|n| n == OPERATOR_CANARY));
    assert!(!got[0].2.iter().any(|n| n == CLAUDE_CANARY));
    assert!(got[1].2.iter().any(|n| n == CLAUDE_CANARY));

    let first = p.names(1);
    assert!(first.contains(CLAUDE_CANARY), "claude keeps its own set");
    assert!(
        !first.contains(OPERATOR_CANARY),
        "the operator canary leaked"
    );
    let second = p.names(2);
    assert!(!second.contains(CLAUDE_CANARY), "codex got claude's set");
    assert!(second.contains("CODEX_HOME"));

    // An older apb skips the new event type only when a checkpoint follows
    // it: rename every one to an unknown type and the journal still reads.
    let path = p.run_dir(&res.run_id).join("events.jsonl");
    let journal = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        journal.replace("\"agent_env_applied\"", "\"agent_env_applied_v99\""),
    )
    .unwrap();
    let older = read_all(&p.run_dir(&res.run_id)).expect("settled by a checkpoint");
    assert!(applied(&older).is_empty());
}

#[test]
fn inherit_journals_nothing_and_passes_the_operator_env() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::new("");
    let _env = p.env(p.agent(&[]), home.path(), cfg.path());
    let res = run(p.root(), "envpb", None, RunOptions::default()).expect("run");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    assert!(applied(&events).is_empty());
    assert!(p.names(1).contains(OPERATOR_CANARY));
}

/// A `--now` pause interrupts the attempt; the resume runs it again under
/// the policy the run started with, although the live profile dropped it in
/// between.
#[test]
fn a_resume_keeps_the_snapshotted_policy_after_a_live_profile_edit() {
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::new(ALLOWLIST);
    let _env = p.env(p.agent(&[]), home.path(), cfg.path());
    let _grace = EnvVar::set("APB_PAUSE_GRACE_MS", "300");
    fs::write(p.root().join("slow"), "").unwrap();
    let (tx, rx) = mpsc::channel();
    let root = p.root().to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "envpb", None, RunOptions::default()));
    });
    poll_until("the agent to start", || {
        p.root().join("agent_running").is_file().then_some(())
    });
    let run_id = poll_until("the run dir", || {
        fs::read_dir(p.root().join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("envpb-"))
    });
    let pause = PauseRequest {
        by: Some(PauseBy::Operator),
        mode: PauseMode::Now,
        ..PauseRequest::default()
    };
    request_pause(p.root(), &run_id, &pause).unwrap();
    let paused = rx.recv_timeout(DEADLINE).expect("the drive returned");
    assert_eq!(paused.expect("drive ok").outcome, RunStatus::Paused);

    // The live profile drops the allowlist; the run must not notice.
    let profile = p.root().join(".apb/profiles/main/profile.yaml");
    let live = fs::read_to_string(&profile).unwrap().replace(ALLOWLIST, "");
    fs::write(&profile, live).unwrap();
    fs::remove_file(p.root().join("slow")).unwrap();

    let res = resume(p.root(), &run_id, None).expect("resume");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&p.run_dir(&run_id)).unwrap();
    assert_eq!(
        applied(&events).len(),
        2,
        "the resumed attempt journals too"
    );
    assert!(
        !p.names(2).contains(OPERATOR_CANARY),
        "the resume inherited"
    );
}

/// Asserts that call `n` of the stub ran under claude's allowlist: its own
/// set arrived, the operator's and codex's did not.
fn assert_claude_allowlist(p: &Project, n: u32, what: &str) {
    let got = p.names(n);
    assert!(
        got.contains(CLAUDE_CANARY),
        "{what}: claude's set is missing"
    );
    assert!(
        !got.contains(OPERATOR_CANARY),
        "{what}: the operator canary leaked"
    );
    assert!(!got.contains(CODEX_CANARY), "{what}: codex's set leaked");
}

/// A judge node with no decision model answers through its profile's
/// agent: that agent runs under the profile's allowlist and journals it.
#[test]
fn a_judge_emulation_runs_under_the_allowlist() {
    const JUDGE: &str = "schema: 2\nid: envpb\nname: Env\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    state: { note: \"ok\" }\n    questions:\n      risky: { type: noul, instructions: \"Is `note` risky?\" }\n    on_unavailable: emulate\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { from: triage, to: done }\n";
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::with(JUDGE, &[], ALLOWLIST);
    let _env = p.env(p.agent(&[]), home.path(), cfg.path());
    let res = run(p.root(), "envpb", None, RunOptions::default()).expect("run");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(p.calls(), 1, "one emulation call");
    assert_claude_allowlist(&p, 1, "judge");
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    assert_eq!(applied(&events).len(), 1, "the emulation attempt journals");
}

/// A finish node with a prompt composes the closing answer through the
/// profile's agent, under the profile's allowlist.
#[test]
fn a_finish_answer_runs_under_the_allowlist() {
    const FINISH: &str = "schema: 1\nid: envpb\nname: Env\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success, prompt: \"compose the answer\" }\nedges:\n  - { from: start, to: done }\n";
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::with(FINISH, &[], ALLOWLIST);
    let _env = p.env(p.agent(&[]), home.path(), cfg.path());
    let res = run(p.root(), "envpb", None, RunOptions::default()).expect("run");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(p.calls(), 1, "one closing-answer call");
    assert_claude_allowlist(&p, 1, "finish answer");
}

/// The background supervisor of an agent-supervised run gets its profile's
/// allowlist.
#[test]
fn a_supervised_runs_supervisor_gets_the_allowlist() {
    const SUPERVISED: &str = "schema: 1\nid: envpb\nname: Env\nversion: 1.0.0\ndefaults:\n  profile: main\nsupervisor:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: p1, type: prompt, prompt: \"hi\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: p1 }\n  - { from: p1, to: done }\n";
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::with(SUPERVISED, &[], ALLOWLIST);
    let _env = p.env(p.agent(&[]), home.path(), cfg.path());
    let opts = RunOptions {
        mode: RunMode::AgentSupervised,
        ..RunOptions::default()
    };
    let run_id = run_background(p.root(), "envpb", None, opts).expect("run");
    poll_until("the supervisor to run", || {
        p.root().join("env-1").is_file().then_some(())
    });
    // The drive runs on its own thread: let it end before the env guard
    // and the temp dirs drop under it.
    poll_until("the run to finish", || {
        let events = read_all(&p.run_dir(&run_id)).ok()?;
        RunState::fold(&events)
            .run_status
            .is_terminal()
            .then_some(())
    });
    assert_claude_allowlist(&p, 1, "supervisor");
}

/// The wake-up probe of a quota pause runs the paused executor under the
/// entry's allowlist for that executor's agent.
#[test]
fn a_quota_probe_runs_under_the_allowlist() {
    const QUOTA_429: &str =
        "429 FreeUsageLimitError: Rate limit exceeded. Please try again later. Retry-After: 1";
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::with(WORK, &[], ALLOWLIST);
    let _env = p.env(p.agent(&[(1, QUOTA_429)]), home.path(), cfg.path());
    let _backoff = [
        EnvVar::set("APB_INFRA_BACKOFF_MS", "20,20"),
        EnvVar::set("APB_QUOTA_BACKOFF_MS", "600000,1200000"),
    ];
    apb_core::run_origin::ensure_key().unwrap();
    let res = run(p.root(), "envpb", None, RunOptions::default()).expect("run");
    assert_eq!(res.outcome, RunStatus::Paused, "the quota pauses the run");
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    let seq = events
        .iter()
        .rev()
        .find(|e| matches!(e.payload, EventPayload::RunPaused { .. }))
        .expect("a run_paused event")
        .seq;
    assert_eq!(
        probe_before_wake(p.root(), &res.run_id, seq).unwrap(),
        WakeProbe::Passed
    );
    assert_eq!(p.calls(), 2, "one attempt, one probe");
    assert_claude_allowlist(&p, 2, "quota probe");
}

/// Context compaction in a run that restricts an agent's environment runs
/// the summarizer under an empty allowlist for claude.
#[test]
fn context_compaction_follows_the_runs_allowlist() {
    const THREE: &str = "schema: 1\nid: envpb\nname: Env\nversion: 1.0.0\ndefaults:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: \"do a\" }\n  - { id: b, type: agent_task, prompt: \"do b\" }\n  - { id: c, type: agent_task, prompt: \"do c\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: c }\n  - { from: c, to: done }\n";
    let _l = common::env_lock();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let p = Project::with(THREE, &[], ALLOWLIST);
    let _env = p.env(p.agent(&[]), home.path(), cfg.path());
    let opts = RunOptions {
        context_max_bytes: Some(500),
        ..RunOptions::default()
    };
    let res = run(p.root(), "envpb", None, opts).expect("run");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&p.run_dir(&res.run_id)).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ContextCompacted { .. })),
        "the run compacted its context"
    );
    // The summarizer is the one call without a node id.
    let summarizer: Vec<u32> = (1..=p.calls())
        .filter(|n| !p.names(*n).contains("APB_NODE_ID"))
        .collect();
    assert!(!summarizer.is_empty(), "no summarizer call");
    for n in summarizer {
        assert_claude_allowlist(&p, n, "compaction");
    }
}
