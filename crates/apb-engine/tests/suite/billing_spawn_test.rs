//! Billed use of the background supervisor: the run gate predicts its
//! basis, and its spawn is checked against the run's billing like any agent
//! work, both on the facts of the process it really starts (the user's own
//! claude settings included, which a node attempt's minimal settings leave
//! out).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::read_all;
use apb_engine::scheduler::{RunMode, RunOptions, run_background};
use apb_engine::state::{RunState, RunStatus};

use crate::common::{self, EnvVar};

/// One node and the supervisor, both on profile `main` (claude-code).
const SUPERVISED: &str = "schema: 1\nid: billpb\nname: Bill\nversion: 1.0.0\ndefaults:\n  profile: main\nsupervisor:\n  profile: main\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: \"do\", max_retries: 0 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

/// A claude config dir with a subscription login whose user settings
/// (`settings.json`, loaded by a process without apb's minimal settings)
/// put an api key in the environment.
fn claude_login(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(".credentials.json"),
        r#"{"claudeAiOauth":{"subscriptionType":"max"}}"#,
    )
    .unwrap();
    fs::write(
        dir.join("settings.json"),
        r#"{"env":{"ANTHROPIC_API_KEY":"k"}}"#,
    )
    .unwrap();
}

/// A project with playbook `billpb` whose node and supervisor bind `main`.
fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/billpb/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), SUPERVISED).unwrap();
    fs::write(root.join(".apb/playbooks/billpb/current"), "1.0.0").unwrap();
    common::seed_profile(root, "main", "claude-code", "haiku", &[]);
}

#[test]
fn the_gate_predicts_the_supervisor_on_the_settings_it_really_loads() {
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let claude = tempfile::tempdir().unwrap();
    claude_login(claude.path());
    let _env = EnvVar::set("CLAUDE_CONFIG_DIR", claude.path());
    let playbook = apb_core::schema::Playbook::from_yaml(SUPERVISED).unwrap();
    let origin = apb_core::scope::Origin::Project { workspace_id: None };
    let predict = |supervised: bool| {
        apb_engine::gate::billing::check_tree(
            root,
            &playbook,
            &origin,
            &Default::default(),
            supervised,
            false,
            None,
        )
        .predicted
        .iter()
        .map(|e| e.label())
        .collect::<Vec<_>>()
    };
    // The node runs under apb's minimal settings: the subscription.
    assert_eq!(predict(false), ["claude: subscription_equivalent"]);
    // The supervisor loads the user's settings, whose key bills.
    assert_eq!(
        predict(true),
        ["claude: billed", "claude: subscription_equivalent"]
    );
}

/// A stub agent that counts its calls and answers.
fn stub(root: &Path) -> String {
    let d = root.display();
    let path = root.join("agent.sh");
    common::write_sync(
        &path,
        &format!(
            "#!/bin/sh\nn=$(cat '{d}/count' 2>/dev/null || echo 0); echo $((n+1)) > '{d}/count'\necho done\n"
        ),
    );
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().into_owned()
}

/// The journal of `run_dir` once its drive stopped on a pause or an end.
fn settled(run_dir: &Path) -> Vec<apb_engine::event::Event> {
    let start = Instant::now();
    loop {
        if let Ok(events) = read_all(run_dir) {
            let s = RunState::fold(&events).run_status;
            if s.is_terminal() || s == RunStatus::Paused {
                return events;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(20), "the run stops");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A supervised run never waits for a supervisor that was not started: a
/// supervisor its billing holds is journaled and pauses the run before its
/// first node, with nothing written that heartbeat monitoring would read
/// as a spawn. Once a person allows billed use and resumes, the resumed
/// drive starts the supervisor and the run goes on.
#[test]
fn a_held_supervisor_pauses_the_run_for_a_person_and_starts_on_resume() {
    use apb_engine::event::EventPayload;
    let _l = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root);
    let claude = tempfile::tempdir().unwrap();
    claude_login(claude.path());
    let cfg = tempfile::tempdir().unwrap();
    fs::write(cfg.path().join("policy.yaml"), "billing:\n  billed: ask\n").unwrap();
    let _env = [
        EnvVar::set("APB_AGENT_CMD", stub(root)),
        EnvVar::set("CLAUDE_CONFIG_DIR", claude.path()),
        EnvVar::set("APB_CONFIG_DIR", cfg.path()),
    ];
    let opts = RunOptions {
        mode: RunMode::AgentSupervised,
        ..RunOptions::default()
    };
    let run_id = run_background(root, "billpb", None, opts).expect("run");
    let run_dir = root.join(".apb/runs").join(&run_id);
    let events = settled(&run_dir);
    let state = RunState::fold(&events);
    assert_eq!(state.run_status, RunStatus::Paused);
    let reason = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunPaused { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert_eq!(reason.as_deref(), Some("billing (claude: billed)"));
    let marker = events.iter().find_map(|e| match &e.payload {
        EventPayload::SupervisorAction { action, detail, .. }
            if action == apb_engine::scheduler::SUPERVISOR_BILLING_HOLD =>
        {
            Some(detail.clone())
        }
        _ => None,
    });
    assert!(
        marker.is_some_and(|d| d.starts_with("billing (claude: billed)")),
        "the held supervisor is journaled"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::NodeStarted { .. })),
        "no node ran before the person was asked"
    );
    assert!(
        !run_dir.join("supervisor/spawned_at").exists(),
        "no spawn is recorded for a supervisor that was not started"
    );

    // The person allows billed use and resumes: the supervisor starts.
    fs::write(
        cfg.path().join("policy.yaml"),
        "billing:\n  billed: allow\n",
    )
    .unwrap();
    // The background drive releases the workdir right after its pause.
    let start = Instant::now();
    let resumed = loop {
        match apb_engine::scheduler::resume_by(
            root,
            &run_id,
            None,
            false,
            apb_engine::pause::PauseBy::Operator,
        ) {
            Err(apb_engine::error::EngineError::WorkdirBusy(_))
                if start.elapsed() < Duration::from_secs(20) =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => break other.expect("resume"),
        }
    };
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    assert!(
        run_dir.join("supervisor/executor").exists(),
        "the resumed drive started the supervisor"
    );
}
