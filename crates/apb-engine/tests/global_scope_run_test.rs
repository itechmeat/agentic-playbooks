use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::store::resolve;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run_background_resolved};
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

// A linear playbook without an agent: runs entirely synchronously on a background thread.
const MINI: &str = r#"
schema: 1
id: mini
name: Mini
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed_global(config_dir: &Path) {
    let vdir = config_dir.join("playbooks/mini/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), MINI).unwrap();
    fs::write(config_dir.join("playbooks/mini/current"), "1.0.0").unwrap();
}

// The only test in the file: it sets APB_CONFIG_DIR for the whole test binary
// process, so there must not be a second env-dependent test here
// (otherwise a race over the shared env).
#[test]
fn global_playbook_runs_in_project_root_with_provenance() {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    // SAFETY: the only env setter in this test binary.
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", config.path());
    }

    seed_global(config.path());
    init_project(project.path()).unwrap(); // empty project .apb/, with no playbooks of its own

    let wref = PlaybookRef {
        origin: Origin::Global,
        id: "mini".into(),
        version: None,
    };
    let resolved = resolve(project.path(), &wref).unwrap();

    let run_id = run_background_resolved(&resolved, RunOptions::default()).unwrap();
    assert!(run_id.starts_with("mini-"), "unexpected run_id: {run_id}");

    // The run lands in the PROJECT's .apb/runs, not in the global store.
    let run_dir = project.path().join(".apb/runs").join(&run_id);
    assert!(
        !config.path().join(".apb").exists(),
        "run must not land in the global store"
    );

    poll_until("a RunFinished event", || {
        let events = read_all(&run_dir).ok()?;
        events
            .iter()
            .find(|e| matches!(e.payload, EventPayload::RunFinished { .. }))
            .map(|_| ())
    });

    let events = read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    assert_eq!(state.run_status, RunStatus::Succeeded);

    // Provenance: origin=global, digest is present, execution_root - the project root.
    let prov = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunProvenance {
                origin,
                digest,
                execution_root,
                ..
            } => Some((origin.clone(), digest.clone(), execution_root.clone())),
            _ => None,
        })
        .expect("RunProvenance event must be present");
    assert_eq!(prov.0.as_deref(), Some("global"));
    assert!(prov.1.as_deref().unwrap_or("").starts_with("sha256:"));
    assert_eq!(
        prov.2.as_deref(),
        Some(project.path().to_string_lossy().as_ref())
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
