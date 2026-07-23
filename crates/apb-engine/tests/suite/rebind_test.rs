//! Mid-run executor rebinding (issue #45 finding 5): a supervisor switches a
//! wedged node's profile to a working one, the rebind is journaled, and the next
//! retry attempt runs under the new profile. The anti-TOCTOU pin is preserved -
//! a rebind whose bundle drifts from the gate's digest is refused.

use crate::common;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::profile::{ProfileScope, QualifiedProfileRef};
use apb_core::profile_store::{PlaybookOrigin, compute_bundle};
use apb_core::registry::init_project;
use apb_engine::control::Control;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunMode, RunOptions, post_supervisor_command, run_background};
use apb_engine::state::{RunState, RunStatus};

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

fn wait_for_wake(run_dir: &Path) {
    poll_until("a WakeRaised event", || {
        read_all(run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::WakeRaised { .. }))
            .then_some(())
    });
}

fn wait_for_run_status(run_dir: &Path, want: RunStatus) -> Vec<Event> {
    poll_until(&format!("run status {want:?}"), || {
        let events = read_all(run_dir).ok()?;
        (RunState::fold(&events).run_status == want).then_some(events)
    })
}

/// Seeds a profile whose SOUL carries a marker, under agent `claude` (the stub
/// via APB_AGENT_CMD stands in for the real binary). The SOUL text reaches the
/// spawned agent's argv, so the stub can tell which profile ran.
fn seed_profile_with_soul(root: &Path, name: &str, soul: &str) {
    let dir = root.join(".apb/profiles").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n"),
    )
    .unwrap();
    fs::write(dir.join("SOUL.md"), soul).unwrap();
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(path, p).unwrap();
}

/// A stub agent that succeeds ONLY when the received argv carries the backup
/// profile's SOUL marker, and fails otherwise. So the initial `main` attempt
/// fails, and the run can only reach success if the retried attempt actually
/// ran under the rebound `backup` profile.
fn soul_gated_agent(dir: &Path) -> String {
    let path = dir.join("soul_gate.sh");
    let body = "#!/bin/sh\ncase \"$*\" in\n  *BACKUP-SOUL-MARKER*) echo ok; exit 0;;\n  *) echo mainfail 1>&2; exit 1;;\nesac\n";
    fs::write(&path, body).unwrap();
    set_executable(&path);
    path.to_string_lossy().to_string()
}

const WF_SUPERVISED: &str = r#"
schema: 1
id: rebindflow
name: Rebind
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

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/rebindflow/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), WF_SUPERVISED).unwrap();
    fs::write(root.join(".apb/playbooks/rebindflow/current"), "1.0.0").unwrap();
    seed_profile_with_soul(root, "main", "MAIN-SOUL-MARKER");
    seed_profile_with_soul(root, "backup", "BACKUP-SOUL-MARKER");
}

fn backup_bundle(root: &Path) -> String {
    let r = QualifiedProfileRef {
        name: "backup".into(),
        scope: ProfileScope::Auto,
    };
    let (_loaded, _pairs, bundle) = compute_bundle(root, PlaybookOrigin::Project, &r).unwrap();
    bundle
}

#[test]
fn rebind_switches_profile_and_next_retry_uses_it() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let prog = soul_gated_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "rebindflow", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);

    // The `main` attempt fails (its SOUL is not the backup marker), so the run
    // parks on `work` and raises a wake.
    wait_for_wake(&run_dir);

    // Rebind the wedged node to `backup`, then retry: the retried attempt runs
    // under `backup`, whose SOUL the stub accepts, so the run succeeds.
    let bundle = backup_bundle(dir.path());
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Rebind {
            node: "work".into(),
            profile: "backup".into(),
            scope: ProfileScope::Auto,
            bundle: bundle.clone(),
            reason: Some("codex is down".into()),
        },
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Retry {
            node: "work".into(),
            prompt_override: None,
        },
    )
    .unwrap();

    let events = wait_for_run_status(&run_dir, RunStatus::Succeeded);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    // The rebind is journaled with the new profile key, the pinned bundle, and
    // the supervisor's reason.
    let rebound = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ProfileRebound {
                node,
                profile,
                bundle: b,
                reason,
            } if node == "work" => Some((profile.clone(), b.clone(), reason.clone())),
            _ => None,
        })
        .expect("a ProfileRebound event for node work");
    assert_eq!(
        rebound.0, "project/backup",
        "rebound to the new profile key"
    );
    assert_eq!(rebound.1, bundle, "the pinned bundle is the gate's digest");
    assert_eq!(
        rebound.2, "codex is down",
        "the reason is journaled verbatim"
    );

    // No rejection was journaled, and the overlay records the new binding.
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::RebindRejected { .. })),
        "a successful rebind must not also journal a rejection"
    );
    let overlay = fs::read_to_string(run_dir.join("rebinds.yaml")).expect("rebind overlay written");
    assert!(
        overlay.contains("backup"),
        "overlay must map the node to the backup profile: {overlay}"
    );
    // The new profile was snapshotted into the run for future attempts.
    assert!(
        run_dir
            .join("profiles/project/backup/profile.yaml")
            .is_file(),
        "the rebound profile must be snapshotted into the run"
    );
}

#[test]
fn rebind_with_drifted_bundle_is_refused_and_pinning_holds() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let prog = soul_gated_agent(dir.path());
    let _env = common::env_lock();
    unsafe {
        std::env::set_var("APB_AGENT_CMD", &prog);
    }

    let opts = RunOptions {
        mode: RunMode::Supervised,
        ..Default::default()
    };
    let run_id = run_background(dir.path(), "rebindflow", None, opts).unwrap();
    let run_dir = dir.path().join(".apb/runs").join(&run_id);
    wait_for_wake(&run_dir);

    // A rebind carrying a bundle that does NOT match what the run snapshot
    // recomputes is drift (the anti-TOCTOU case): it must be refused, not
    // silently applied.
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Rebind {
            node: "work".into(),
            profile: "backup".into(),
            scope: ProfileScope::Auto,
            bundle: "sha256:deadbeef".into(),
            reason: None,
        },
    )
    .unwrap();

    let rejected = poll_until("a RebindRejected event", || {
        read_all(&run_dir)
            .ok()?
            .into_iter()
            .find_map(|e| match e.payload {
                EventPayload::RebindRejected { node, reason } if node == "work" => Some(reason),
                _ => None,
            })
    });
    assert!(
        rejected.contains("bundle mismatch"),
        "the refusal must name the bundle drift, got: {rejected}"
    );

    // Stop the parked run and confirm the binding never changed: no
    // ProfileRebound was journaled and no overlay was written.
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Abort {
            reason: "test cleanup".into(),
        },
    )
    .unwrap();
    let events = wait_for_run_status(&run_dir, RunStatus::Aborted);
    unsafe {
        std::env::remove_var("APB_AGENT_CMD");
    }
    drop(_env);

    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ProfileRebound { .. })),
        "a drifted rebind must not change the binding"
    );
    assert!(
        !run_dir.join("rebinds.yaml").is_file(),
        "a refused rebind must not write an overlay"
    );
}
