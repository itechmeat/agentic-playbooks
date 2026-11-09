use super::*;
use crate::manifest::{ManifestExecution, RunExecutionManifest};

/// Which single source marks a seeded run.
#[derive(Clone, Copy, Debug)]
enum Source {
    None,
    Boolean,
    Origin,
    Suffix,
    Journal,
}

const ALL: [Source; 4] = [
    Source::Boolean,
    Source::Origin,
    Source::Suffix,
    Source::Journal,
];

/// A finished, succeeded top-level run `id` of playbook `p` under `root`,
/// created on this machine, started at `ts`, marked by `source` only.
fn seed(root: &Path, id: &str, ts: u64, source: Source) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut started = serde_json::json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "p", "version": "1.0.0"});
    if matches!(source, Source::Journal) {
        started["external_data"] = true.into();
    }
    let lines = [
        started,
        serde_json::json!({"seq": 2, "ts": ts, "type": "run_provenance", "origin": "project"}),
        serde_json::json!({"seq": 3, "ts": ts, "type": "attempt_finished", "node": "a", "attempt": 1, "status": "succeeded", "session": format!("sess-{id}")}),
        serde_json::json!({"seq": 4, "ts": ts + 1, "type": "run_finished", "outcome": "succeeded"}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
    let mut m = RunExecutionManifest {
        unattended: true,
        ..Default::default()
    };
    match source {
        Source::Boolean => m.external_data = true,
        Source::Origin => m.origin = Some(RunOrigin::ExternalData),
        Source::Suffix => {
            m.execution = Some(ManifestExecution {
                mode: format!("cli{}", ManifestExecution::EXTERNAL_SUFFIX),
                source: None,
                client: None,
                fallback_to_host: false,
            })
        }
        Source::None | Source::Journal => {}
    }
    crate::manifest::write(&dir, &m).unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
    dir
}

/// Runs `f` under a fresh config dir in a plain child process, in a fresh
/// project with an empty runs directory.
fn child(name: &str, f: impl FnOnce(&Path)) {
    apb_core::process_mode::testing::in_plain_child(name, || {
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: a child process runs this one test on one thread.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        apb_core::run_origin::ensure_key().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        std::fs::create_dir_all(root.join(".apb/runs")).unwrap();
        f(root);
    });
}

fn name(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// Each source alone flags a run, and every reader of earlier runs skips
/// it: the history filter, the bounded directory filter and the
/// `previous_success` lookup, not only `is_flagged`.
#[test]
fn each_source_alone_flags_and_every_history_reader_skips_it() {
    child(
        "external_data::tests::each_source_alone_flags_and_every_history_reader_skips_it",
        |root| {
            let old = seed(root, "old", 1, Source::None);
            assert!(!is_flagged(&old));
            assert!(learns_from(&old));
            for (i, source) in ALL.into_iter().enumerate() {
                let id = format!("flagged{i}");
                let dir = seed(root, &id, 10 + i as u64, source);
                assert!(is_flagged(&dir), "{source:?}");
                assert!(!learns_from(&dir), "{source:?}");
                let run = crate::run_stats::StatsRun::load(&dir).unwrap();
                assert!(!crate::run_history::counts_as_history(&run), "{source:?}");
                assert!(!crate::run_history::production_dir(&dir), "{source:?}");
                let err = refuse_if_flagged(&dir, "a test").unwrap_err().to_string();
                assert!(err.contains(RUN), "{source:?}: {err}");
                // The newest success is the flagged run; the lookup names
                // the earlier unflagged one.
                let found = crate::previous_success::find(root, "p", "project").unwrap();
                assert_eq!(found.as_deref().map(name), Some("old".into()), "{source:?}");
                std::fs::remove_dir_all(&dir).unwrap();
            }
            let run = crate::run_stats::StatsRun::load(&old).unwrap();
            assert!(crate::run_history::counts_as_history(&run));
        },
    );
}

/// A manifest that does not read is never learned from, and a tool that
/// learns from one named run refuses it rather than guess.
#[test]
fn an_unreadable_manifest_never_counts_and_refuses() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.yaml"), "profiles: [\n").unwrap();
    assert!(!learns_from(dir.path()));
    let err = refuse_if_flagged(dir.path(), "a test")
        .unwrap_err()
        .to_string();
    assert!(err.contains(RUN), "{err}");
}

/// Flagged runs never crowd earlier history out of a capped window: two
/// unflagged runs followed by more flagged runs than the cap still give
/// the two.
#[test]
fn flagged_runs_do_not_crowd_out_history() {
    child(
        "external_data::tests::flagged_runs_do_not_crowd_out_history",
        |root| {
            let cap = 5;
            let base = std::time::SystemTime::now() - std::time::Duration::from_secs(10_000);
            let touch = |dir: &Path, secs: u64| {
                std::fs::File::options()
                    .write(true)
                    .open(dir.join("events.jsonl"))
                    .unwrap()
                    .set_modified(base + std::time::Duration::from_secs(secs))
                    .unwrap();
            };
            for (i, id) in ["a", "b"].iter().enumerate() {
                let d = seed(root, id, i as u64, Source::None);
                touch(&d, i as u64);
            }
            for i in 0..cap + 10 {
                let d = seed(root, &format!("x{i:02}"), 100 + i as u64, Source::Boolean);
                touch(&d, 100 + i as u64);
            }
            let dirs = learnable_run_dirs(&[root.to_path_buf()], cap);
            let names: Vec<String> = dirs.iter().map(|d| name(d)).collect();
            assert_eq!(names, vec!["b".to_string(), "a".to_string()]);
            let runs = crate::run_history::runs_of(&[root.to_path_buf()], "p", None, cap);
            assert_eq!(runs.len(), 2);
        },
    );
}

/// The start rule: a flagged predecessor makes the start flagged, an
/// explicit `false` under it is refused, and an unflagged one leaves the
/// request as it is; the remember refusal; the resume check both ways.
#[test]
fn the_start_the_remember_and_the_resume_rules() {
    child(
        "external_data::tests::the_start_the_remember_and_the_resume_rules",
        |root| {
            let plain = seed(root, "plain", 1, Source::None);
            let flagged = seed(root, "flagged", 2, Source::Boolean);
            assert_eq!(for_start(root, None, None, Some("flagged")), Ok(true));
            assert_eq!(for_start(root, Some(true), None, Some("flagged")), Ok(true));
            assert_eq!(for_start(root, None, Some("flagged"), None), Ok(true));
            let refused = for_start(root, Some(false), None, Some("flagged")).unwrap_err();
            assert_eq!(refused.code, IMMUTABLE);
            assert_eq!(refused.body()["error"], IMMUTABLE);
            assert_eq!(for_start(root, None, None, Some("plain")), Ok(false));
            assert_eq!(for_start(root, Some(true), None, Some("plain")), Ok(true));
            assert_eq!(for_start(root, Some(false), None, None), Ok(false));
            assert_eq!(for_start(root, None, None, Some("../x")), Ok(false));

            assert_eq!(
                check_remember(true, true).unwrap_err().code,
                REMEMBER_REFUSED
            );
            assert!(check_remember(true, false).is_ok());
            assert!(check_remember(false, true).is_ok());

            assert_eq!(
                check_resume(&flagged, Some(false)).unwrap_err().code,
                IMMUTABLE
            );
            assert_eq!(
                check_resume(&plain, Some(true)).unwrap_err().code,
                IMMUTABLE
            );
            assert!(check_resume(&flagged, Some(true)).is_ok());
            assert!(check_resume(&plain, Some(false)).is_ok());
            assert!(check_resume(&flagged, None).is_ok());
        },
    );
}

/// The markers name the flagged runs' ids and agent sessions only.
#[test]
fn the_markers_name_flagged_runs_only() {
    child(
        "external_data::tests::the_markers_name_flagged_runs_only",
        |root| {
            seed(root, "plain", 1, Source::None);
            seed(root, "flagged", 2, Source::Journal);
            let m = flagged_markers(&[root.to_path_buf()]);
            assert_eq!(m.run_ids.iter().collect::<Vec<_>>(), vec!["flagged"]);
            assert_eq!(m.sessions.iter().collect::<Vec<_>>(), vec!["sess-flagged"]);
        },
    );
}

/// The newest run's manifest does not read: the `previous_success` lookup
/// refuses rather than skip it as a run it may not learn from.
#[test]
fn previous_success_refuses_an_unreadable_newest_manifest() {
    child(
        "external_data::tests::previous_success_refuses_an_unreadable_newest_manifest",
        |root| {
            seed(root, "old", 1, Source::None);
            let bad = seed(root, "bad", 10, Source::None);
            std::fs::remove_file(bad.join("manifest.yaml")).unwrap();
            std::fs::write(bad.join("manifest.yaml"), "profiles: [\n").unwrap();
            apb_core::run_origin::stamp(&bad, "bad").unwrap();
            let err = crate::previous_success::find(root, "p", "project").unwrap_err();
            assert_eq!((err.run.as_str(), err.what), ("bad", "manifest"));
        },
    );
}

/// The settlement backstop: a run whose manifest marks a candidate trial
/// but that processed external data settles nothing. Its manifest is
/// rewritten to mark the trial, which no start writes for a flagged run;
/// `current` and the candidate pointer stay as they were and no candidate
/// event is journaled.
#[test]
fn a_flagged_trial_never_settles_the_candidate() {
    child(
        "external_data::tests::a_flagged_trial_never_settles_the_candidate",
        |root| {
            const PB: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nsupervisor:\n  policy:\n    promote_supervisor_patches: on_success\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: \"scripts/ok.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
            apb_core::registry::init_project(root).unwrap();
            let pb = root.join(".apb/playbooks/p");
            std::fs::create_dir_all(pb.join("1.0.0/scripts")).unwrap();
            std::fs::write(pb.join("1.0.0/playbook.yaml"), PB).unwrap();
            std::fs::write(pb.join("1.0.0/scripts/ok.sh"), "echo ok\n").unwrap();
            std::fs::write(pb.join("current"), "1.0.0").unwrap();
            let first = crate::scheduler::run(root, "p", None, Default::default()).unwrap();
            let candidate = crate::forward_patch::create(
                root,
                &first.run_id,
                &crate::forward_patch::ForwardPatchRequest {
                    yaml: PB.replace("name: p", "name: p two"),
                    classification: "improvement".into(),
                    rationale: None,
                    evidence: Vec::new(),
                },
            )
            .unwrap()
            .version;
            let flagged = crate::scheduler::run(
                root,
                "p",
                Some(&candidate),
                crate::scheduler::RunOptions {
                    external_data: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let dir = root.join(".apb/runs").join(&flagged.run_id);
            let mut m = crate::manifest::read(&dir).unwrap().unwrap();
            assert!(!m.candidate_trial);
            m.candidate_trial = true;
            std::fs::remove_file(dir.join("manifest.yaml")).unwrap();
            crate::manifest::write(&dir, &m).unwrap();
            let pointers = || {
                (
                    std::fs::read(pb.join("current")).unwrap(),
                    std::fs::read(pb.join("candidate")).ok(),
                )
            };
            let before = pointers();
            let mut log = crate::event::EventLog::open(&dir).unwrap();
            crate::candidate::settle(&mut log, &dir, true);
            assert_eq!(before, pointers());
            assert!(
                !crate::event::read_all(&dir)
                    .unwrap()
                    .iter()
                    .any(|e| matches!(
                        e.payload,
                        crate::event::EventPayload::CandidatePromoted { .. }
                            | crate::event::EventPayload::CandidateRejected { .. }
                            | crate::event::EventPayload::CandidateHeld { .. }
                    ))
            );
        },
    );
}
