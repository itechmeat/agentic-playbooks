use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::store::resolve;
use apb_core::versioning::{VersionProvenance, write_provenance};
use apb_engine::control::{Control, post_control};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::forward_patch::{ForwardPatchRequest, create as forward_create};
use apb_engine::scheduler::{RunOptions, run_background_resolved, run_resolved};
use apb_engine::state::{RunState, RunStatus};

use crate::common::{EnvVar, env_lock};

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
    let _env = env_lock();
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

// ---------------------------------------------------------------------------
// G0: a run of a global definition never touches a project playbook's
// candidate line. Every test here holds both a project `dup` (current 1.0.0,
// candidate 1.1.0) and a global `dup` at the same versions.
// ---------------------------------------------------------------------------

/// `dup` at `version`: a linear playbook with one script node and the
/// supervisor policy a candidate line needs.
fn dup(version: &str) -> String {
    format!(
        r#"
schema: 2
id: dup
name: Dup
version: {version}
supervisor:
  policy:
    promote_supervisor_patches: manual
nodes:
  - {{ id: start, type: start }}
  - {{ id: p1, type: script, script: "scripts/ok.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: p1 }}
  - {{ from: p1, to: done }}
"#
    )
}

/// The project `dup`: `current` 1.0.0 and a candidate 1.1.0 with a trial
/// record, seeded the way the engine writes one.
fn seed_project_candidate(root: &Path) {
    init_project(root).unwrap();
    let v1 = root.join(".apb/playbooks/dup/1.0.0");
    fs::create_dir_all(v1.join("scripts")).unwrap();
    fs::write(v1.join("playbook.yaml"), dup("1.0.0")).unwrap();
    fs::write(v1.join("scripts/ok.sh"), "echo ok\n").unwrap();
    fs::write(root.join(".apb/playbooks/dup/current"), "1.0.0").unwrap();
    let v2 = root.join(".apb/playbooks/dup/1.1.0");
    fs::create_dir_all(v2.join("scripts")).unwrap();
    fs::write(v2.join("playbook.yaml"), dup("1.1.0")).unwrap();
    fs::write(v2.join("scripts/ok.sh"), "echo patched\n").unwrap();
    fs::write(root.join(".apb/playbooks/dup/candidate"), "1.1.0").unwrap();
    write_provenance(
        root,
        "dup",
        "1.1.0",
        &VersionProvenance {
            created_by: "supervisor".into(),
            scope: Some(apb_core::candidate::SCOPE_NEXT_RUNS.into()),
            base_version: Some("1.0.0".into()),
            trial: Some(apb_core::versioning::TrialRecord::default()),
            ..Default::default()
        },
    )
    .unwrap();
}

/// The global `dup` in `config_dir`: versions 1.0.0 and 1.1.0, `current`
/// 1.0.0, no candidate.
fn seed_global_dup(config_dir: &Path) {
    for (version, line) in [("1.0.0", "echo ok\n"), ("1.1.0", "echo patched\n")] {
        let vdir = config_dir.join("playbooks/dup").join(version);
        fs::create_dir_all(vdir.join("scripts")).unwrap();
        fs::write(vdir.join("playbook.yaml"), dup(version)).unwrap();
        fs::write(vdir.join("scripts/ok.sh"), line).unwrap();
    }
    fs::write(config_dir.join("playbooks/dup/current"), "1.0.0").unwrap();
}

/// Every file of `dir` below `playbooks/dup`, path and bytes: the
/// byte-for-byte comparison of the project's candidate line.
fn snapshot_candidate_line(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(bytes) = fs::read(&p) {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.insert(rel, bytes);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(
        &root.join(".apb/playbooks/dup"),
        &root.join(".apb/playbooks/dup"),
        &mut out,
    );
    out
}

fn global_ref(version: Option<&str>) -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Global,
        id: "dup".into(),
        version: version.map(str::to_string),
    }
}

fn project_candidate_files(root: &Path) -> Vec<String> {
    ["candidate", "current", "meta/1.1.0.yaml"]
        .into_iter()
        .filter(|f| root.join(".apb/playbooks/dup").join(f).exists())
        .map(str::to_string)
        .collect()
}

fn no_candidate_event(run_dir: &Path) -> bool {
    read_all(run_dir).unwrap().iter().all(|e| {
        !matches!(
            e.payload,
            EventPayload::CandidatePromoted { .. }
                | EventPayload::CandidateRejected { .. }
                | EventPayload::CandidateHeld { .. }
        )
    })
}

/// A global `dup` runs to its end through `run_resolved`, once succeeding
/// (version 1.1.0) and once failing (the same version with its script made
/// to fail on disk); the project's candidate line is untouched and the run
/// journals no candidate event. This one passes before the guards too: it
/// is the isolation regression.
#[test]
fn a_global_run_leaves_a_project_candidate_alone() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    seed_global_dup(config.path());
    let before = snapshot_candidate_line(root);

    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let ok = run_resolved(&resolved, RunOptions::default()).unwrap();
    assert_eq!(ok.outcome, RunStatus::Succeeded);
    // The same version again, failing: the script on disk is made to exit 1.
    fs::write(
        config.path().join("playbooks/dup/1.1.0/scripts/ok.sh"),
        "echo broken 1>&2\nexit 1\n",
    )
    .unwrap();
    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let bad = run_resolved(&resolved, RunOptions::default()).unwrap();
    assert_eq!(bad.outcome, RunStatus::Failed);

    assert_eq!(
        snapshot_candidate_line(root),
        before,
        "the project candidate line must be byte for byte unchanged"
    );
    assert_eq!(
        project_candidate_files(root),
        ["candidate", "current", "meta/1.1.0.yaml"].map(str::to_string)
    );
    for run_id in [ok.run_id, bad.run_id] {
        assert!(no_candidate_event(&root.join(".apb/runs").join(run_id)));
    }
}

/// The global store itself carries a stray candidate pointer (hand made):
/// a global run of the candidate version is no trial.
#[test]
fn a_stray_global_candidate_pointer_marks_no_trial() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    seed_global_dup(config.path());
    fs::write(config.path().join("playbooks/dup/candidate"), "1.1.0").unwrap();

    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let res = run_resolved(&resolved, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    // A run of a global definition is never a candidate trial: the manifest
    // says so, or is not written at all (an empty manifest is left out).
    let manifest = apb_engine::manifest::read(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    assert_ne!(
        manifest.as_ref().map(|m| m.candidate_trial),
        Some(true),
        "a run of a global definition is never a candidate trial"
    );
    assert!(no_candidate_event(
        &root.join(".apb/runs").join(&res.run_id)
    ));
}

/// A forward patch made from a finished global run is refused
/// `patch_global_unsupported` and the project playbook is unchanged.
#[test]
fn a_forward_patch_of_a_global_run_is_refused() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    seed_global_dup(config.path());
    let before = snapshot_candidate_line(root);

    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let res = run_resolved(&resolved, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let err = forward_create(
        root,
        &res.run_id,
        &ForwardPatchRequest {
            yaml: dup("1.1.0"),
            classification: "improvement".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("patch_global_unsupported"),
        "{err}"
    );
    assert_eq!(
        snapshot_candidate_line(root),
        before,
        "the project playbook directory must be byte for byte unchanged"
    );
}

/// A patch queued for a global run held at a webhook wait is rejected with
/// the code and the project playbook is unchanged.
#[test]
fn a_queued_patch_of_a_global_run_is_rejected() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    seed_global_dup(config.path());
    let before = snapshot_candidate_line(root);
    // The global 1.1.0 becomes a webhook wait: the run holds there.
    let waiting = dup("1.1.0").replace(
        r#"{ id: p1, type: script, script: "scripts/ok.sh", runner: sh }"#,
        r#"{ id: p1, type: wait, wait_for: { type: webhook, key: ci }, timeout_seconds: 120 }"#,
    );
    fs::write(
        config.path().join("playbooks/dup/1.1.0/playbook.yaml"),
        waiting,
    )
    .unwrap();

    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let run_id = run_background_resolved(&resolved, RunOptions::default()).unwrap();
    let run_dir = root.join(".apb/runs").join(&run_id);
    poll_until("the run to reach its wait", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::WaitStarted { .. }))
            .then_some(())
    });
    post_control(
        &run_dir,
        // A version the project holds: without the scope check the drive
        // would load the project's 1.1.0 for the global run.
        Control::Patch {
            version: "1.1.0".into(),
            classification: "improvement".into(),
            continue_from: "p1".into(),
        },
    )
    .unwrap();
    poll_until("the patch rejection with the code", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::PatchRejected { reason } => Some(reason.clone()),
                _ => None,
            })
            .filter(|r| r.contains("patch_global_unsupported"))
    });
    assert_eq!(
        snapshot_candidate_line(root),
        before,
        "the project playbook directory must be byte for byte unchanged"
    );
    post_control(
        &run_dir,
        Control::Abort {
            reason: "test done".into(),
            by: None,
            graceful: false,
        },
    )
    .unwrap();
    poll_until("the run to abort", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunAborted { .. }))
            .then_some(())
    });
}

/// A forward patch made from a global run whose journal ends in a torn
/// line is refused `patch_origin_unreadable`: the run's scope is unknown,
/// so no project definition is read, and the project playbook is unchanged.
#[test]
fn a_forward_patch_of_an_unreadable_run_is_refused() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    seed_global_dup(config.path());
    let before = snapshot_candidate_line(root);

    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let res = run_resolved(&resolved, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let journal = root
        .join(".apb/runs")
        .join(&res.run_id)
        .join("events.jsonl");
    let mut bytes = fs::read(&journal).unwrap();
    bytes.extend_from_slice(br#"{"seq":999,"ts":1,"type":"no"#);
    fs::write(&journal, bytes).unwrap();

    let err = forward_create(
        root,
        &res.run_id,
        &ForwardPatchRequest {
            yaml: dup("1.1.0"),
            classification: "improvement".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("patch_origin_unreadable"), "{err}");
    assert_eq!(snapshot_candidate_line(root), before);
}

/// A forward patch made from a run directory without a journal is refused
/// `patch_origin_unreadable`, and the project playbook is unchanged.
#[test]
fn a_forward_patch_of_a_run_without_a_journal_is_refused() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    let before = snapshot_candidate_line(root);
    fs::create_dir_all(root.join(".apb/runs/dup-gone")).unwrap();

    let err = forward_create(
        root,
        "dup-gone",
        &ForwardPatchRequest {
            yaml: dup("1.1.0"),
            classification: "improvement".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("patch_origin_unreadable"), "{err}");
    assert_eq!(snapshot_candidate_line(root), before);
}

/// A global run whose manifest carries the trial bit (as a manifest
/// written before the start checked the scope can) settles nothing: it
/// ends and leaves the project's candidate pointer, trial record and
/// `current` byte for byte, and `trial_of` reports no trial for it. The
/// script of the global 1.1.0 sets the bit in its own run's manifest.
#[test]
fn a_global_run_with_the_trial_bit_settles_no_project_candidate() {
    let _lock = env_lock();
    let config = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _var = EnvVar::set("APB_CONFIG_DIR", config.path());
    seed_project_candidate(root);
    seed_global_dup(config.path());
    fs::write(
        config.path().join("playbooks/dup/1.1.0/scripts/ok.sh"),
        // A run without profiles writes no manifest: the script writes the
        // smallest one that reads, else appends the bit to the one there.
        "m=.apb/runs/$APB_RUN_ID/manifest.yaml\n\
         if [ -f \"$m\" ]; then chmod u+w \"$m\"; else printf 'profiles: []\\nnode_bindings: {}\\n' > \"$m\"; fi\n\
         printf 'candidate_trial: true\\n' >> \"$m\"\n",
    )
    .unwrap();
    let before = snapshot_candidate_line(root);

    let resolved = resolve(root, &global_ref(Some("1.1.0"))).unwrap();
    let res = run_resolved(&resolved, RunOptions::default()).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = root.join(".apb/runs").join(&res.run_id);
    let manifest = apb_engine::manifest::read(&run_dir).unwrap().unwrap();
    assert!(manifest.candidate_trial, "the script set the trial bit");

    assert_eq!(
        snapshot_candidate_line(root),
        before,
        "the project candidate line must be byte for byte unchanged"
    );
    assert!(no_candidate_event(&run_dir));
    let events = read_all(&run_dir).unwrap();
    assert_eq!(apb_engine::candidate::trial_of(&run_dir, &events), None);
}
