use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::versioning::{create_patch_version, read_provenance};
use apb_engine::control::Control;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{
    PreparedRun, RunMode, RunOptions, RunResult, drive_prepared, post_supervisor_command,
    prepare_supervised_background, run,
};
use apb_engine::state::RunStatus;

const POLL_DEADLINE: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(10);

const WF_PROMPTS: &str = r#"
schema: 1
id: migrate
name: Migration
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "first" }
  - { id: p2, type: prompt, prompt: "second" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: p2 }
  - { from: p2, to: done }
"#;

const PATCH_IMPROVEMENT: &str = r#"
schema: 1
id: migrate
name: Migration
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "improved first" }
  - { id: p2, type: prompt, prompt: "second" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: p2 }
  - { from: p2, to: done }
"#;

const PATCH_SECOND: &str = r#"
schema: 1
id: migrate
name: Migration
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "improved first" }
  - { id: p2, type: prompt, prompt: "improved second" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: p2 }
  - { from: p2, to: done }
"#;

const WF_SLOW_GATE: &str = r#"
schema: 1
id: migrate
name: Migration
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "first" }
  - { id: gate, type: script, script: "scripts/gate.sh", runner: sh }
  - { id: p2, type: prompt, prompt: "second" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: gate }
  - { from: gate, to: p2 }
  - { from: p2, to: done }
"#;

const PATCH_CHANGES_EXECUTED: &str = r#"
schema: 1
id: migrate
name: Migration
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "changed executed node" }
  - { id: gate, type: script, script: "scripts/gate.sh", runner: sh }
  - { id: p2, type: prompt, prompt: "second" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: gate }
  - { from: gate, to: p2 }
  - { from: p2, to: done }
"#;

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = f() {
            return value;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

fn seed(root: &Path, playbook: &str) {
    init_project(root).unwrap();
    let version_dir = root.join(".apb/playbooks/migrate/1.0.0");
    fs::create_dir_all(&version_dir).unwrap();
    fs::write(version_dir.join("playbook.yaml"), playbook).unwrap();
    fs::write(root.join(".apb/playbooks/migrate/current"), "1.0.0").unwrap();
}

fn seed_slow_gate(root: &Path) {
    seed(root, WF_SLOW_GATE);
    let scripts = root.join(".apb/playbooks/migrate/1.0.0/scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("gate.sh"), "sleep 0.4\n").unwrap();
}

fn prepare(root: &Path) -> (PreparedRun, String, PathBuf) {
    prepare_with_limit(root, None)
}

fn prepare_with_limit(
    root: &Path,
    max_patches_per_run: Option<u32>,
) -> (PreparedRun, String, PathBuf) {
    let prepared = prepare_supervised_background(
        root,
        "migrate",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            max_patches_per_run,
            ..Default::default()
        },
    )
    .unwrap();
    let run_id = prepared.run_id().to_string();
    let run_dir = root.join(".apb/runs").join(&run_id);
    (prepared, run_id, run_dir)
}

fn drive_in_background(
    root: PathBuf,
    prepared: PreparedRun,
) -> mpsc::Receiver<Result<RunResult, apb_engine::EngineError>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(drive_prepared(&root, prepared));
    });
    rx
}

fn wait_result(rx: &mpsc::Receiver<Result<RunResult, apb_engine::EngineError>>) -> RunResult {
    rx.recv_timeout(POLL_DEADLINE)
        .unwrap_or_else(|_| panic!("drive did not complete within {POLL_DEADLINE:?}"))
        .unwrap()
}

#[test]
fn valid_patch_migrates_run_and_promotes_improvement() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_PROMPTS);
    let (prepared, run_id, run_dir) = prepare(dir.path());
    let version = create_patch_version(
        dir.path(),
        "migrate",
        "1.0.0",
        PATCH_IMPROVEMENT,
        &run_id,
        "improvement",
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version: version.clone(),
            classification: "improvement".into(),
            continue_from: "p1".into(),
        },
    )
    .unwrap();

    let result = wait_result(&drive_in_background(dir.path().to_path_buf(), prepared));
    assert_eq!(result.outcome, RunStatus::Succeeded);

    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::PatchApplied { version: applied, classification, continue_from }
            if applied == &version && classification == "improvement" && continue_from == "p1"
    )));
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::RunMigrated { from_version, to_version, continue_from }
            if from_version == "1.0.0" && to_version == &version && continue_from == "p1"
    )));
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::VersionPromoted { version: promoted } if promoted == &version
    )));
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/migrate/current"))
            .unwrap()
            .trim(),
        version
    );
    assert!(
        read_provenance(dir.path(), "migrate", &version)
            .unwrap()
            .unwrap()
            .promoted
    );
}

#[test]
fn invalid_patch_rejects_change_to_executed_node() {
    let dir = tempfile::tempdir().unwrap();
    seed_slow_gate(dir.path());
    let (prepared, run_id, run_dir) = prepare(dir.path());
    let version = create_patch_version(
        dir.path(),
        "migrate",
        "1.0.0",
        PATCH_CHANGES_EXECUTED,
        &run_id,
        "improvement",
    )
    .unwrap();
    let rx = drive_in_background(dir.path().to_path_buf(), prepared);

    poll_until("gate node to start", || {
        read_all(&run_dir).ok()?.iter().any(|event| {
            matches!(&event.payload, EventPayload::NodeStarted { node, .. } if node == "gate")
        }).then_some(())
    });
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version,
            classification: "improvement".into(),
            continue_from: "p2".into(),
        },
    )
    .unwrap();

    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::PatchRejected { reason } if reason.contains("executed node changed: p1")
    )));
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/migrate/current"))
            .unwrap()
            .trim(),
        "1.0.0"
    );
}

#[test]
fn workaround_patch_succeeds_without_promotion() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_PROMPTS);
    let (prepared, run_id, run_dir) = prepare(dir.path());
    let version = create_patch_version(
        dir.path(),
        "migrate",
        "1.0.0",
        PATCH_IMPROVEMENT,
        &run_id,
        "workaround",
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version: version.clone(),
            classification: "workaround".into(),
            continue_from: "p1".into(),
        },
    )
    .unwrap();

    let result = wait_result(&drive_in_background(dir.path().to_path_buf(), prepared));
    assert_eq!(result.outcome, RunStatus::Succeeded);
    assert_eq!(
        fs::read_to_string(dir.path().join(".apb/playbooks/migrate/current"))
            .unwrap()
            .trim(),
        "1.0.0"
    );
    assert!(
        !read_provenance(dir.path(), "migrate", &version)
            .unwrap()
            .unwrap()
            .promoted
    );
    assert!(
        !read_all(&run_dir)
            .unwrap()
            .iter()
            .any(|event| { matches!(&event.payload, EventPayload::VersionPromoted { .. }) })
    );
}

#[test]
fn second_patch_pauses_when_patch_limit_is_exhausted() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_PROMPTS);
    let (prepared, run_id, run_dir) = prepare_with_limit(dir.path(), Some(1));

    let first = create_patch_version(
        dir.path(),
        "migrate",
        "1.0.0",
        PATCH_IMPROVEMENT,
        &run_id,
        "improvement",
    )
    .unwrap();
    let second = create_patch_version(
        dir.path(),
        "migrate",
        &first,
        PATCH_SECOND,
        &run_id,
        "improvement",
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version: first,
            classification: "improvement".into(),
            continue_from: "p1".into(),
        },
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version: second,
            classification: "improvement".into(),
            continue_from: "p2".into(),
        },
    )
    .unwrap();

    let result = wait_result(&drive_in_background(dir.path().to_path_buf(), prepared));
    assert_eq!(result.outcome, RunStatus::Paused);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        EventPayload::RunPaused { reason } if reason.starts_with("max patches per run exhausted:")
    )));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.payload, EventPayload::PatchApplied { .. }))
            .count(),
        1
    );
}

#[test]
fn run_playbook_ref_reports_active_version_after_migration() {
    use apb_engine::scheduler::run_playbook_ref;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_PROMPTS);
    let (prepared, run_id, _run_dir) = prepare(dir.path());

    // Before migration the active version is the starting one.
    let (id0, ver0) = run_playbook_ref(dir.path(), &run_id).unwrap();
    assert_eq!(id0, "migrate");
    assert_eq!(ver0, "1.0.0");

    let version = create_patch_version(
        dir.path(),
        "migrate",
        "1.0.0",
        PATCH_IMPROVEMENT,
        &run_id,
        "improvement",
    )
    .unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch {
            version: version.clone(),
            classification: "improvement".into(),
            continue_from: "p1".into(),
        },
    )
    .unwrap();
    let result = wait_result(&drive_in_background(dir.path().to_path_buf(), prepared));
    assert_eq!(result.outcome, RunStatus::Succeeded);

    // After migration the active version is the patched one.
    let (_id, ver1) = run_playbook_ref(dir.path(), &run_id).unwrap();
    assert_eq!(ver1, version);
}

#[test]
fn runs_without_patch_keep_autonomous_and_supervised_behavior() {
    let autonomous = tempfile::tempdir().unwrap();
    seed(autonomous.path(), WF_PROMPTS);
    let autonomous_result = run(autonomous.path(), "migrate", None, RunOptions::default()).unwrap();
    assert_eq!(autonomous_result.outcome, RunStatus::Succeeded);

    let supervised = tempfile::tempdir().unwrap();
    seed(supervised.path(), WF_PROMPTS);
    let supervised_result = run(
        supervised.path(),
        "migrate",
        None,
        RunOptions {
            mode: RunMode::Supervised,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(supervised_result.outcome, RunStatus::Succeeded);
}
