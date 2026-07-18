use crate::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, run_background};
use apb_engine::state::{RunState, RunStatus};
use apb_engine::{PersistedSession, find_session_by_token};

/// Restores `PATH` to its captured value (or unsets it) on drop. Used by the
/// fallback-spawn test, which clobbers `PATH` to force the primary agent to
/// fail; a Drop guard keeps a panic from leaking that clobbered `PATH` to
/// other modules in the consolidated test binary.
struct PathGuard(Option<std::ffi::OsString>);
impl Drop for PathGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

/// Restores `APB_SUPERVISOR_HEARTBEAT_MS` to its captured value (or unsets
/// it) on drop. Same rationale as `PathGuard`: the heartbeat-lost test below
/// clobbers this var to force a near-zero threshold, then relies on plain
/// cleanup at the end of the function; a panic before that cleanup (e.g. an
/// `.unwrap()` or a `wait_for_terminal` timeout) would leak the stale "0"
/// value to sibling supervisor tests in the consolidated binary that never
/// set it themselves.
struct HeartbeatMsGuard(Option<std::ffi::OsString>);
impl Drop for HeartbeatMsGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("APB_SUPERVISOR_HEARTBEAT_MS", v),
                None => std::env::remove_var("APB_SUPERVISOR_HEARTBEAT_MS"),
            }
        }
    }
}

// Both environment variables this file controls (APB_AGENT_CMD and
// APB_SUPERVISOR_HEARTBEAT_MS) are shared across the cargo test process (parallel
// #[test] threads) - the same serialize-guard trick as in supervised_drive_test.rs.

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(20);

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

/// Agent stub: writes ALL received arguments (including the brief passed via
/// `-p`) into `invocation_file`, separating invocations with a marker, then exits
/// immediately - we don't spin up a live agent, only record the spawn's fact and content.
fn agent_stub(dir: &Path, invocation_file: &Path) -> String {
    let path = dir.join("agent_stub.sh");
    let body = format!(
        "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo '---end---'; }} >> '{}'\n",
        invocation_file.display()
    );
    common::write_sync(&path, &body);
    set_executable(&path);
    path.to_string_lossy().to_string()
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

fn run_dir_of(root: &Path, run_id: &str) -> PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn wait_for_terminal(run_dir: &Path) -> RunStatus {
    poll_until("run to reach a terminal status", || {
        let events = read_all(run_dir).ok()?;
        if events.is_empty() {
            return None;
        }
        let state = RunState::fold(&events);
        match state.run_status {
            RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted | RunStatus::Paused => {
                Some(state.run_status)
            }
            _ => None,
        }
    })
}

fn read_invocation(invocation_file: &Path) -> String {
    poll_until("supervisor invocation to be recorded", || {
        let text = fs::read_to_string(invocation_file).ok()?;
        if text.is_empty() { None } else { Some(text) }
    })
}

// A pipeline WITHOUT agent_task nodes (only start/prompt/finish), so the run
// deterministically reaches succeeded on its own - the agent stub
// (APB_AGENT_CMD) in this test is needed only for SPAWNING the supervisor, not for
// executing nodes.
const WF_NO_AGENT_TASK: &str = r#"
schema: 1
id: bgspv1
name: BG Supervisor Test 1
version: 1.0.0
defaults:
  profile: main
supervisor:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

// Scenario 1: initial spawn of the background agent when run_background(supervisor_expected:true,
// Supervised). The brief (passed to the stub via -p) must contain the run_id, a token
// of format `sv-...`, and the playbook id; a persisted session must exist on disk,
// resolvable by this token via find_session_by_token.
#[test]
fn initial_spawn_writes_brief_and_persists_session() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "bgspv1", WF_NO_AGENT_TASK);

    let invocation_file = dir.path().join("supervisor_invocation.txt");
    let prog = agent_stub(dir.path(), &invocation_file);

    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        supervisor_expected: true,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "bgspv1", None, opts).unwrap();
    let run_dir = run_dir_of(dir.path(), &run_id);

    let status = wait_for_terminal(&run_dir);

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(
        status,
        RunStatus::Succeeded,
        "playbook without agent_task nodes must finish on its own"
    );

    // The session is persisted on disk and resolves by token.
    let session_path = run_dir.join("supervisor").join("session.json");
    let session_bytes = poll_until("supervisor/session.json to appear", || {
        fs::read(&session_path).ok()
    });
    let session: PersistedSession =
        serde_json::from_slice(&session_bytes).expect("valid session.json");
    // session.json stores only an irreversible token fingerprint (sha256:...), not
    // the token itself: the secret is never written to disk.
    assert!(
        session.token_hash.starts_with("sha256:"),
        "session.json must persist a token hash, not the raw token, got `{}`",
        session.token_hash
    );
    assert!(
        !session.token_hash.contains("sv-"),
        "raw supervisor token must never be persisted on disk"
    );
    assert!(
        session.capabilities.contains(&"observe".to_string()),
        "default capabilities must include observe"
    );

    // Take the real token from the brief (it is placed there to authenticate the agent) and
    // verify it resolves against disk via a fingerprint comparison.
    let content = read_invocation(&invocation_file);
    let start = content
        .find("sv-")
        .expect("brief must contain the sv- supervisor token");
    let token: String = content[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let (found_run_id, caps) = find_session_by_token(dir.path(), &token)
        .unwrap()
        .expect("find_session_by_token must resolve the freshly minted token");
    assert_eq!(
        found_run_id, run_id,
        "resolved session must point back at this run"
    );
    assert!(caps.contains(&"retry".to_string()));

    // The brief passed to the stub via -p contains run_id/token/playbook id.
    assert!(
        content.contains(&run_id),
        "brief must mention the run_id:\n{content}"
    );
    assert!(
        content.contains(&token),
        "brief must mention the supervisor token:\n{content}"
    );
    assert!(
        content.contains("bgspv1"),
        "brief must mention the playbook id:\n{content}"
    );
    assert!(
        content.contains("-p"),
        "brief must be passed via -p:\n{content}"
    );
}

// A pipeline with several prompt nodes (no agent_task), so drive passes
// through several iteration boundaries (several top-of-loop heartbeat checks)
// before reaching finish - respawn must have a chance to fire along the way.
const WF_MULTI_NODE: &str = r#"
schema: 1
id: bgspv2
name: BG Supervisor Test 2
version: 1.0.0
defaults:
  profile: main
supervisor:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "a" }
  - { id: p2, type: prompt, prompt: "b" }
  - { id: p3, type: prompt, prompt: "c" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: p2 }
  - { from: p2, to: p3 }
  - { from: p3, to: done }
"#;

/// Supervisor profile with a distinctive SOUL and a skill (to check the full
/// contract: SOUL is delivered, skill names go out as an advisory line).
fn seed_sup_profile(root: &Path, name: &str, executor: &str, soul: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\ndescription: t\n{executor}skills:\n  - sup-skill\n"),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), soul).unwrap();
    let sk = root.join(".agents/skills/sup-skill");
    fs::create_dir_all(&sk).unwrap();
    fs::write(sk.join("SKILL.md"), "sup skill body").unwrap();
}

// A pipeline WITHOUT a supervisor section - only defaults.profile. `--supervise`
// must still bring up the agent from defaults.profile (completion-plan Task 9).
// WITHOUT a supervisor section - only defaults.profile. Under --supervise
// (supervisor_expected: true) the supervisor binding is taken from defaults.profile
// precisely because the run is actually supervised (mode-gated). An autonomous
// run of the same playbook does NOT create the binding (review P2).
const WF_DEFAULTS_ONLY: &str = r#"
schema: 1
id: bgspv3
name: BG Supervisor Defaults Only
version: 1.0.0
defaults:
  profile: sup
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

// Scenario 3: --supervise with a single defaults.profile (WITHOUT a supervisor section)
// brings up the agent from defaults.profile, delivering its SOUL and advisory skills.
#[test]
fn supervise_with_defaults_profile_only_spawns_and_delivers_soul() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/bgspv3/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_DEFAULTS_ONLY).unwrap();
    fs::write(dir.path().join(".apb/playbooks/bgspv3/current"), "1.0.0").unwrap();
    seed_sup_profile(
        dir.path(),
        "sup",
        "executor:\n  agent: claude-code\n  model: haiku\n",
        "SUP-SOUL-MARKER",
    );

    let invocation_file = dir.path().join("supervisor_invocation.txt");
    let prog = agent_stub(dir.path(), &invocation_file);
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        supervisor_expected: true,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "bgspv3", None, opts).unwrap();
    let run_dir = run_dir_of(dir.path(), &run_id);
    let status = wait_for_terminal(&run_dir);

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert_eq!(status, RunStatus::Succeeded);
    let content = read_invocation(&invocation_file);
    assert!(
        content.contains("SUP-SOUL-MARKER"),
        "supervisor SOUL must be delivered:\n{content}"
    );
    assert!(
        content.contains("sup-skill"),
        "supervisor skills must be advertised by name:\n{content}"
    );
    // The chosen chain element is recorded in the diagnostic file.
    let ex = fs::read_to_string(run_dir.join("supervisor/executor")).unwrap();
    assert!(
        ex.contains("claude-code") || ex.contains("claude"),
        "chosen supervisor executor recorded: {ex}"
    );
}

// Scenario 4: the supervisor's primary executor fails to spawn (no binary present),
// the engine moves to the next chain element (fallback) and records it.
#[test]
fn supervisor_spawn_falls_back_on_primary_failure() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/bgspv4/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    let playbook = r#"
schema: 1
id: bgspv4
name: BG Supervisor Fallback
version: 1.0.0
supervisor:
  profile: sup
defaults:
  profile: sup
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;
    fs::write(vdir.join("playbook.yaml"), playbook).unwrap();
    fs::write(dir.path().join(".apb/playbooks/bgspv4/current"), "1.0.0").unwrap();
    // primary codex (not in PATH -> spawn fails), fallback claude (stub in PATH).
    seed_sup_profile(
        dir.path(),
        "sup",
        "executor:\n  agent: codex\n  model: o1\n  fallbacks:\n    - { agent: claude, model: haiku }\n",
        "",
    );

    // Stub `claude` is on PATH; we do NOT set APB_AGENT_CMD (otherwise both agents would be the same stub).
    let bin = tempfile::tempdir().unwrap();
    let invocation_file = dir.path().join("supervisor_invocation.txt");
    let claude_stub = bin.path().join("claude");
    fs::write(
        &claude_stub,
        format!(
            "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo '---end---'; }} >> '{}'\n",
            invocation_file.display()
        ),
    )
    .unwrap();
    set_executable(&claude_stub);

    let _env = common::env_lock();
    // Drop-based restore (not a manual statement) so a panic in the run below
    // cannot leak a clobbered PATH into sibling modules of the consolidated
    // binary - the apb-mcp consolidation was bitten by exactly this. `_path`
    // is declared after `_env`, so it drops first: PATH is restored while the
    // env lock is still held.
    let _path = PathGuard(std::env::var_os("PATH"));
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
        std::env::set_var("PATH", bin.path());
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        supervisor_expected: true,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "bgspv4", None, opts).unwrap();
    let run_dir = run_dir_of(dir.path(), &run_id);
    let status = wait_for_terminal(&run_dir);

    assert_eq!(status, RunStatus::Succeeded);
    // Fallback claude is chosen and recorded (primary codex failed to spawn).
    let ex = fs::read_to_string(run_dir.join("supervisor/executor")).unwrap();
    assert!(
        ex.starts_with("claude:"),
        "fallback executor must be recorded as the chosen one, got: {ex}"
    );
    let content = read_invocation(&invocation_file);
    assert!(
        content.contains(&run_id),
        "fallback supervisor must receive the brief:\n{content}"
    );
}

// Scenario 2: the agent stub never touches the heartbeat (only records the
// invocation and exits) - with a near-zero threshold (APB_SUPERVISOR_HEARTBEAT_MS=0)
// drive must, at one of the following iteration boundaries, declare SupervisorLost
// and spawn the agent again (exactly one respawn per run, per 4c).
#[test]
fn heartbeat_lost_triggers_single_respawn() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "bgspv2", WF_MULTI_NODE);

    let invocation_file = dir.path().join("supervisor_invocation.txt");
    let prog = agent_stub(dir.path(), &invocation_file);

    let _env = common::env_lock();
    // Drop-based restore (not just the manual cleanup below) so a panic
    // between here and that cleanup (e.g. the `.unwrap()` or
    // `wait_for_terminal` call below) cannot leak the near-zero heartbeat
    // threshold into sibling supervisor tests in the consolidated binary.
    // Declared after `_env` so it drops first: the var is restored while
    // the shared env lock is still held.
    let _heartbeat = HeartbeatMsGuard(std::env::var_os("APB_SUPERVISOR_HEARTBEAT_MS"));
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
        std::env::set_var("APB_SUPERVISOR_HEARTBEAT_MS", "0");
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        supervisor_expected: true,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "bgspv2", None, opts).unwrap();
    let run_dir = run_dir_of(dir.path(), &run_id);

    let status = wait_for_terminal(&run_dir);

    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
        std::env::remove_var("APB_SUPERVISOR_HEARTBEAT_MS");
    }
    drop(_env);

    assert_eq!(
        status,
        RunStatus::Succeeded,
        "multi-node prompt-only playbook must finish on its own"
    );

    let events = read_all(&run_dir).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::SupervisorLost { .. })),
        "expected a SupervisorLost event once heartbeat silence exceeded the (near-zero) threshold"
    );
    // Only one SupervisorLost for the whole run (a guard in drive - single respawn in 4c).
    let lost_count = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::SupervisorLost { .. }))
        .count();
    assert_eq!(
        lost_count, 1,
        "drive must declare the supervisor lost at most once per run in 4c"
    );

    // The stub must be invoked at least twice: initial spawn + one respawn.
    let content = read_invocation(&invocation_file);
    let invocations = content.matches("---end---").count();
    assert!(
        invocations >= 2,
        "expected at least 2 supervisor spawns (initial + respawn), got {invocations}:\n{content}"
    );
}
