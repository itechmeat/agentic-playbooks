//! Finding 7 of issue #42 / third item of issue #40: the repair channel must
//! not depend on the patient being healthy.
//!
//! Two properties, both bounded in wall-clock so a regression fails fast
//! instead of hanging the suite:
//!   (a) a control message posted while an attempt is RUNNING is journaled as
//!       received (a `control_received` supervisor action) before that attempt
//!       ends - not only discovered at the next node boundary;
//!   (b) a supervisor interrupt terminates the running attempt's agent process,
//!       the killed attempt is journaled FAILED via the exit-by-signal path,
//!       and a queued patch then applies at the attempt boundary the interrupt
//!       forced - recovering a run whose agent would otherwise have hung
//!       forever (the 90+ minute dead hang the incident describes).

use crate::common;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::versioning::create_patch_version;
use apb_engine::control::{Control, post_control};
use apb_engine::error::EngineError;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

/// The stub agent's first (hung) invocation sleeps this long. An interrupt that
/// does not actually terminate the process would make the drive take at least
/// this long, so every deadline below sits far under it.
const AGENT_SLEEP_SECS: u64 = 30;
const POLL_DEADLINE: Duration = Duration::from_secs(10);
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

fn find_run_id(root: &Path, prefix: &str) -> String {
    poll_until(&format!("a run dir with prefix `{prefix}`"), || {
        let runs_dir = root.join(".apb/runs");
        if !runs_dir.is_dir() {
            return None;
        }
        fs::read_dir(&runs_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|n| n.starts_with(prefix))
    })
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

/// A stub agent that touches a marker (so the test knows the node is really in
/// flight) and then sleeps far longer than the test is willing to wait.
fn sleepy_agent(dir: &Path) -> (String, PathBuf) {
    let marker = dir.join("agent_running.marker");
    let path = dir.join("sleepy.sh");
    let body = format!(
        "#!/bin/sh\ntouch '{m}'\nsleep {s}\necho done\n",
        m = marker.display(),
        s = AGENT_SLEEP_SECS
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    (path.to_string_lossy().to_string(), marker)
}

/// A stub agent that HANGS on its first invocation (sleeps 30s) and SUCCEEDS on
/// every one after. The hang is the wedged attempt the interrupt must break; the
/// success is what the patched re-run reaches once the boundary is forced.
fn flaky_sleepy_agent(dir: &Path) -> (String, PathBuf) {
    let running = dir.join("agent_running.marker");
    let seen = dir.join("agent_seen.marker");
    let path = dir.join("flaky_sleepy.sh");
    let body = format!(
        "#!/bin/sh\ntouch '{r}'\nif [ -f '{s}' ]; then echo ok; exit 0; fi\ntouch '{s}'\nsleep {sl}\necho late\n",
        r = running.display(),
        s = seen.display(),
        sl = AGENT_SLEEP_SECS
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    (path.to_string_lossy().to_string(), running)
}

/// Sets `APB_AGENT_CMD` for the lifetime of the guard and removes it on drop,
/// even on panic. Holds the shared env lock for the same span, since this suite
/// runs as modules of ONE process (see `stop_run_test.rs` for the same idiom).
struct AgentEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl AgentEnv {
    fn set(prog: &str) -> Self {
        let lock = common::env_lock();
        unsafe {
            std::env::set_var("APB_AGENT_CMD", prog);
        }
        Self { _lock: lock }
    }
}

impl Drop for AgentEnv {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
        }
    }
}

const WF: &str = r#"
schema: 1
id: PLAYBOOK_ID
name: Control Liveness
version: 1.0.0
defaults:
  profile: main
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// Two concurrent agent branches meeting at a barrier: the shape a targeted
/// interrupt exists for (spec 2026-08-05 section 1.6). Both branches are
/// batchable (non-interactive `agent_task`, neither is a join), so the drive runs
/// them at the same time and one control entry is visible to both observers. One
/// retry each, so an interrupted branch recovers on its own.
const FORK_WF: &str = r#"
schema: 1
id: PLAYBOOK_ID
name: Targeted Interrupt
version: 1.0.0
defaults:
  profile: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: alpha, type: agent_task, prompt: "BRANCH_ALPHA work" }
  - { id: beta, type: agent_task, prompt: "BRANCH_BETA work" }
  - { id: merged, type: prompt, prompt: "joined" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: alpha }
  - { from: start, to: beta }
  - { from: alpha, to: merged, join: all }
  - { from: beta, to: merged, join: all }
  - { from: merged, to: done }
"#;

fn seed(root: &Path, id: &str) {
    seed_yaml(root, id, WF);
}

fn seed_yaml(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml.replace("PLAYBOOK_ID", id)).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

/// The branch token this invocation's prompt carries (`$2` is the prompt: the
/// adapter's argv is `-p <prompt> --model <model>`), as a shell prelude every
/// fork stub below shares.
const BRANCH_OF_PROMPT: &str = "case \"$2\" in\n\
     *BRANCH_ALPHA*) n=alpha ;;\n\
     *BRANCH_BETA*) n=beta ;;\n\
     *) n=other ;;\n\
     esac\n";

/// A fork stub whose branches are ASYMMETRIC, so a targeted interrupt has
/// something to spare: `alpha` hangs on its first invocation (the wedged branch)
/// and succeeds on every one after, while `beta` publishes its start marker and
/// then waits - bounded - for the test to release it, so it is genuinely in
/// flight when the interrupt lands and completes normally afterwards.
fn fork_agent(dir: &Path) -> String {
    let d = dir.display();
    let path = dir.join("fork_agent.sh");
    let body = format!(
        "#!/bin/sh\n{branch}\
         touch '{d}/running_'\"$n\"\n\
         if [ \"$n\" = alpha ]; then\n\
         \x20 if [ -f '{d}/seen_alpha' ]; then echo ok; exit 0; fi\n\
         \x20 touch '{d}/seen_alpha'\n\
         \x20 sleep {sleep}\n\
         \x20 echo late\n\
         \x20 exit 0\n\
         fi\n\
         i=0\n\
         while [ ! -f '{d}/release_beta' ]; do\n\
         \x20 i=$((i + 1))\n\
         \x20 if [ \"$i\" -gt 400 ]; then echo 'beta was never released'; exit 0; fi\n\
         \x20 sleep 0.05\n\
         done\n\
         echo ok\n",
        branch = BRANCH_OF_PROMPT,
        sleep = AGENT_SLEEP_SECS,
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

/// The SYMMETRIC fork stub: every branch hangs on its first invocation and
/// succeeds on every one after. A broadcast interrupt has to kill both, and both
/// then recover through their own retry.
fn symmetric_fork_agent(dir: &Path) -> String {
    let d = dir.display();
    let path = dir.join("symmetric_fork_agent.sh");
    let body = format!(
        "#!/bin/sh\n{branch}\
         touch '{d}/running_'\"$n\"\n\
         if [ -f '{d}/seen_'\"$n\" ]; then echo ok; exit 0; fi\n\
         touch '{d}/seen_'\"$n\"\n\
         sleep {sleep}\n\
         echo late\n",
        branch = BRANCH_OF_PROMPT,
        sleep = AGENT_SLEEP_SECS,
    );
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

/// Every `attempt_interrupted` supervisor action journaled for `node`.
fn interrupted_actions(run_dir: &Path, node: &str) -> Vec<String> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::SupervisorAction {
                action,
                node: Some(n),
                detail,
            } if action == "attempt_interrupted" && n == node => Some(detail),
            _ => None,
        })
        .collect()
}

/// Every `control_received` acknowledgment journaled for `node`.
fn control_acks(run_dir: &Path, node: &str) -> Vec<String> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::SupervisorAction {
                action,
                node: Some(n),
                detail,
            } if action == "control_received" && n == node => Some(detail),
            _ => None,
        })
        .collect()
}

fn attempt_statuses(run_dir: &Path, node: &str) -> Vec<String> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AttemptFinished {
                node: n, status, ..
            } if n == node => Some(status),
            _ => None,
        })
        .collect()
}

/// (a) A control message posted while the agent is mid-flight must be
/// acknowledged LIVE - a `control_received` supervisor action before the
/// attempt ends - not discovered only at the next node boundary.
#[cfg(unix)]
#[test]
fn a_mid_attempt_control_message_is_journaled_received_before_the_attempt_ends() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "liveflow");
    let (prog, marker) = sleepy_agent(dir.path());
    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "liveflow", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "liveflow-");
    // Wait until the agent process is genuinely running (mid-sleep), so the note
    // really lands mid-attempt rather than before the node ever started.
    poll_until("the stub agent to start", || marker.is_file().then_some(()));
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // A supervisor note posted while the agent is mid-sleep.
    let note = "seen you mid-flight";
    post_control(&run_dir, Control::ContextAppend { note: note.into() }).unwrap();

    // It must be acknowledged live: the `control_received` action appears while
    // the agent is still sleeping (this poll returns far under the agent's 30s),
    // which is exactly what "before the attempt ends" means here.
    let ack = poll_until("a control_received supervisor action for the node", || {
        read_all(&run_dir).ok()?.into_iter().find(|e| {
            matches!(&e.payload, EventPayload::SupervisorAction { action, node, .. }
                if action == "control_received" && node.as_deref() == Some("work"))
        })
    });
    // The ack really did precede the attempt's own end: no attempt_finished has
    // been journaled for the still-running attempt yet.
    let has_finished = read_all(&run_dir)
        .unwrap()
        .iter()
        .any(|e| matches!(e.payload, EventPayload::AttemptFinished { .. }));
    assert!(
        !has_finished,
        "the control message was acknowledged only after the attempt ended"
    );
    let _ = ack;

    // Clean up: stop the run so the test does not wait out the 30s sleep.
    post_control(
        &run_dir,
        Control::Abort {
            reason: "test done".into(),
        },
    )
    .unwrap();
    let res = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("the drive must return after the abort")
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Aborted);
}

/// (b) The core of the task: a supervisor interrupt terminates the running
/// attempt's agent process, the killed attempt is journaled FAILED, and a patch
/// queued while the attempt hung then applies at the forced boundary - the
/// repair channel works against a wedged attempt.
#[cfg(unix)]
#[test]
fn an_interrupt_terminates_a_hung_attempt_and_a_queued_patch_then_applies() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), "repairflow");
    let (prog, running) = flaky_sleepy_agent(dir.path());
    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(
            &root,
            "repairflow",
            None,
            RunOptions {
                mode: RunMode::Supervised,
                ..Default::default()
            },
        ));
    });

    let run_id = find_run_id(dir.path(), "repairflow-");
    // Attempt 1 is now genuinely running - and will hang for 30s.
    poll_until("the stub agent's first (hung) attempt to start", || {
        running.is_file().then_some(())
    });
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // The exact shape of the incident: a repair (a patch) posted against a run
    // whose attempt never ends. The patch tweaks the failed node's prompt
    // (`continue_from` is exempt from the executed-node-unchanged rule).
    let patch_yaml = WF
        .replace("PLAYBOOK_ID", "repairflow")
        .replace(r#"prompt: "do""#, r#"prompt: "do (patched)""#);
    let version = create_patch_version(
        dir.path(),
        "repairflow",
        "1.0.0",
        &patch_yaml,
        &run_id,
        "improvement",
    )
    .unwrap();

    // Interrupt the hung attempt, then queue the patch. The interrupt forces the
    // attempt boundary; the patch applies there and the run recovers.
    post_control(
        &run_dir,
        Control::Interrupt {
            reason: "attempt is wedged".into(),
            node: None,
        },
    )
    .unwrap();
    post_control(
        &run_dir,
        Control::Patch {
            version: version.clone(),
            classification: "improvement".into(),
            continue_from: "work".into(),
        },
    )
    .unwrap();

    let res = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("the drive must return well under the agent's 30s sleep")
        .unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the interrupt must unblock the repair channel so the patched run completes"
    );

    let events = read_all(&run_dir).unwrap();
    // The interruption is journaled explanatorily (who/which control message).
    assert!(
        events.iter().any(
            |e| matches!(&e.payload, EventPayload::SupervisorAction { action, node, .. }
            if action == "attempt_interrupted" && node.as_deref() == Some("work"))
        ),
        "the interrupt must be journaled with an attempt_interrupted action"
    );
    // The killed attempt is journaled FAILED (the exit-by-signal path), which is
    // what lets ordinary retry/fallback/patch proceed at the boundary.
    assert!(
        events.iter().any(
            |e| matches!(&e.payload, EventPayload::AttemptFinished { node, attempt: 1, status, .. }
            if node == "work" && status == "failed")
        ),
        "the killed attempt must be journaled failed"
    );
    // ...and the queued patch applied at the boundary the interrupt forced.
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::PatchApplied { .. })),
        "the queued patch must apply at the forced attempt boundary"
    );
}

/// (c) Targeted interrupt (spec 2026-08-05 section 1.6): with two branches in
/// flight at once, an interrupt naming ONE node kills only that node's attempt.
/// The sibling never even sees the message - it is not acknowledged, not
/// journaled, and not consumed on its behalf - and completes normally.
#[cfg(unix)]
#[test]
fn a_node_targeted_interrupt_kills_only_the_named_branch() {
    let dir = tempfile::tempdir().unwrap();
    seed_yaml(dir.path(), "targetflow", FORK_WF);
    let prog = fork_agent(dir.path());
    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "targetflow", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "targetflow-");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    // Both branches are genuinely in flight: alpha is mid-hang and beta is
    // waiting on its release, so ONE control entry is visible to both observers.
    for branch in ["alpha", "beta"] {
        let marker = dir.path().join(format!("running_{branch}"));
        poll_until(&format!("branch {branch} to start"), || {
            marker.is_file().then_some(())
        });
    }

    post_control(
        &run_dir,
        Control::Interrupt {
            reason: "alpha is wedged".into(),
            node: Some("alpha".into()),
        },
    )
    .unwrap();

    // Only the named branch dies. Waiting for its interruption first is what makes
    // the sibling assertions below meaningful: by the time beta is released, the
    // targeted entry has been on disk long enough for beta's own poll loop to have
    // read it many times over.
    poll_until("alpha's attempt to be interrupted", || {
        (!interrupted_actions(&run_dir, "alpha").is_empty()).then_some(())
    });
    assert!(
        interrupted_actions(&run_dir, "beta").is_empty(),
        "a targeted interrupt must not kill the sibling branch"
    );
    assert!(
        control_acks(&run_dir, "beta").is_empty(),
        "an interrupt naming another node must not even be acknowledged by the sibling: {:?}",
        control_acks(&run_dir, "beta")
    );

    fs::write(dir.path().join("release_beta"), "go").unwrap();
    let res = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("the drive must return well under the agent's 30s hang")
        .unwrap();
    assert_eq!(
        res.outcome,
        RunStatus::Succeeded,
        "the untouched branch and the retried one must both complete"
    );

    assert_eq!(
        attempt_statuses(&run_dir, "alpha"),
        vec!["failed".to_string(), "succeeded".to_string()],
        "the targeted branch's killed attempt must fail and its retry succeed"
    );
    assert_eq!(
        attempt_statuses(&run_dir, "beta"),
        vec!["succeeded".to_string()],
        "the sibling must complete on its first attempt, untouched by the interrupt"
    );
    assert!(
        interrupted_actions(&run_dir, "beta").is_empty(),
        "the sibling must never be journaled interrupted"
    );
}

/// (d) The broadcast boundary that must not move: an interrupt with NO node keeps
/// today's documented semantics byte for byte - every attempt running in the run
/// observes it, dies, and recovers through its own retry.
#[cfg(unix)]
#[test]
fn an_interrupt_without_a_node_still_kills_every_running_attempt() {
    let dir = tempfile::tempdir().unwrap();
    seed_yaml(dir.path(), "broadcastflow", FORK_WF);
    let prog = symmetric_fork_agent(dir.path());
    let _env = AgentEnv::set(&prog);

    let root = dir.path().to_path_buf();
    let (tx, rx) = mpsc::channel::<Result<RunResult, EngineError>>();
    std::thread::spawn(move || {
        let _ = tx.send(run(&root, "broadcastflow", None, RunOptions::default()));
    });

    let run_id = find_run_id(dir.path(), "broadcastflow-");
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    for branch in ["alpha", "beta"] {
        let marker = dir.path().join(format!("running_{branch}"));
        poll_until(&format!("branch {branch} to hang"), || {
            marker.is_file().then_some(())
        });
    }

    post_control(
        &run_dir,
        Control::Interrupt {
            reason: "both branches are wedged".into(),
            node: None,
        },
    )
    .unwrap();

    let res = rx
        .recv_timeout(POLL_DEADLINE)
        .expect("the drive must return well under the agent's 30s hang")
        .unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    for branch in ["alpha", "beta"] {
        assert!(
            !interrupted_actions(&run_dir, branch).is_empty(),
            "a broadcast interrupt must kill branch {branch} too"
        );
        assert_eq!(
            attempt_statuses(&run_dir, branch),
            vec!["failed".to_string(), "succeeded".to_string()],
            "branch {branch} must recover through its own retry after the broadcast"
        );
    }
}
