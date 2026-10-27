//! Promotion by quality, end to end through the engine: a run of a
//! playbook with a `quality:` block is graded by rules as it ends
//! (`run_graded`), a candidate trial is compared with `current`'s graded
//! runs and held (`candidate_held`), promoted or kept out, one failed
//! trial alone rejects nothing, spent trials go to a person, a host grade
//! stored later re-settles the candidate, and a copied run folder or an
//! unsigned grade file counts for nothing.

use std::fs;
use std::path::Path;

use apb_core::quality::compare::Verdict;
use apb_core::quality::grade::{Cite, Level};
use apb_core::registry::init_project;
use apb_core::versioning::read_provenance;
use apb_engine::candidate::{PersonDecision, decide_by_person, quality_view, resettle, trial_of};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::grade::evidence::Proposal;
use apb_engine::grade::reader::ReadRequest;
use apb_engine::grade::{SubmitOutcome, grades_of, read, start_session, submit};
use apb_engine::state::RunStatus;

use crate::candidate_trial_test::{ID, events_of, forward, forward_err, pointer, start};

/// A small gate: three graded runs a side, four trials, and a margin wide
/// enough that three clean trials are shown not worse (0 poor of 3 has a
/// Wilson upper bound of 0.56) while one poor trial of three or four is
/// not.
const POLICY: &str = "{ when: quality, min_runs: 3, max_trials: 4, margin: 0.6 }";

/// The trial playbook graded by `quality`, its node `p1` running `script`.
fn playbook(quality: &str, script: &str) -> String {
    format!(
        r#"
schema: 2
id: trial
name: Trial
version: 1.0.0
supervisor:
  policy:
    promote_supervisor_patches: {POLICY}
{quality}
nodes:
  - {{ id: start, type: start }}
  - {{ id: p1, type: script, script: "scripts/{script}", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: p1 }}
  - {{ from: p1, to: done }}
"#
    )
}

/// `maybe.sh` fails once when the project holds a `fail-next` file. The
/// installation's run-origin key exists, as `apb` makes it: the runs apb
/// creates are stamped and their grades signed, and only those are
/// compared (`run_history::runs_of` checks each run's stamp, the
/// comparison each grade's signature).
fn seed(root: &Path, quality: &str) {
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    let dir = root.join(".apb/playbooks").join(ID).join("1.0.0");
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("playbook.yaml"), playbook(quality, "ok.sh")).unwrap();
    fs::write(dir.join("scripts/ok.sh"), "echo ok\n").unwrap();
    fs::write(
        dir.join("scripts/break.sh"),
        "printf 'not: [yaml' > .apb/playbooks/trial/1.0.0/playbook.yaml\necho broke\n",
    )
    .unwrap();
    fs::write(
        dir.join("scripts/maybe.sh"),
        "if [ -f fail-next ]; then rm fail-next; echo broken 1>&2; exit 1; fi\necho fine\n",
    )
    .unwrap();
    fs::write(
        root.join(".apb/playbooks").join(ID).join("current"),
        "1.0.0",
    )
    .unwrap();
}

fn tail_kinds(root: &Path, run_id: &str, n: usize) -> Vec<&'static str> {
    let events = events_of(root, run_id);
    events[events.len() - n..]
        .iter()
        .map(|e| match e {
            EventPayload::RunGraded { .. } => "run_graded",
            EventPayload::CandidateHeld { .. } => "candidate_held",
            EventPayload::CandidatePromoted { .. } => "candidate_promoted",
            EventPayload::CandidateRejected { .. } => "candidate_rejected",
            EventPayload::RunFinished { .. } => "run_finished",
            EventPayload::NodeFinished { .. } => "node_finished",
            _ => "other",
        })
        .collect()
}

fn run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn held(root: &Path, run_id: &str) -> String {
    events_of(root, run_id)
        .into_iter()
        .find_map(|e| match e {
            EventPayload::CandidateHeld { verdict, .. } => Some(verdict),
            _ => None,
        })
        .expect("a held candidate")
}

#[test]
fn trials_are_held_one_failure_rejects_nothing_and_spent_trials_go_to_a_person() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let mut base = String::new();
    for _ in 0..3 {
        let (run, outcome, _) = start(root);
        assert_eq!(outcome, RunStatus::Succeeded);
        assert_eq!(
            tail_kinds(root, &run, 3),
            ["node_finished", "run_graded", "run_finished"]
        );
        base = run;
    }
    let graded = grades_of(&run_dir(root, &base));
    assert_eq!(graded.len(), 1);
    assert_eq!(graded[0].task_type, "default");
    assert_eq!(graded[0].criterion("outcome").unwrap().level, Level::Good);
    assert!(graded[0].signed_here(), "a grade apb stored is signed");

    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));

    // Trial 1: too few comparable grades, so the candidate is held.
    let (t1, outcome, chosen) = start(root);
    assert_eq!(
        (outcome, chosen.as_deref()),
        (RunStatus::Succeeded, Some(version.as_str()))
    );
    assert_eq!(
        tail_kinds(root, &t1, 3),
        ["run_graded", "candidate_held", "run_finished"]
    );
    assert_eq!(held(root, &t1), "wait");

    // Trial 2 fails: under a quality policy one failure is a grade, not a
    // verdict.
    fs::write(root.join("fail-next"), "").unwrap();
    let (t2, outcome, _) = start(root);
    assert_eq!(outcome, RunStatus::Failed);
    assert_eq!(held(root, &t2), "wait");
    assert_eq!(
        pointer(root, "candidate").as_deref(),
        Some(version.as_str())
    );
    let view = quality_view(root, ID).unwrap().unwrap();
    assert_eq!(view.trials, 2);
    assert_eq!(view.decision.unwrap().verdict, Verdict::Wait);

    // Trial 3: enough grades, but one poor outcome of three is not shown
    // within the margin: still held.
    let (t3, _, _) = start(root);
    assert_eq!(held(root, &t3), "wait");
    // Trial 4 spends the trials: a person decides.
    let (t4, _, _) = start(root);
    assert_eq!(held(root, &t4), "needs_person");
    assert_eq!(
        pointer(root, "candidate").as_deref(),
        Some(version.as_str())
    );

    let rec = decide_by_person(root, ID, PersonDecision::Promote, "cli").unwrap();
    assert_eq!(
        (rec.verdict, rec.decided_by.as_str()),
        (Verdict::Promote, "cli")
    );
    assert_eq!(pointer(root, "current").as_deref(), Some(version.as_str()));
    // The held trial's run reports what stands now, not the stale verdict.
    let t4_dir = run_dir(root, &t4);
    let now = trial_of(&t4_dir, &read_all(&t4_dir).unwrap()).unwrap();
    assert_eq!(now.verdict, "promoted");
}

#[test]
fn a_current_that_no_longer_loads_still_lets_the_trial_be_judged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    forward(root, &base, &playbook("quality: {}", "break.sh"));
    // The trial breaks `current`'s definition on disk: the candidate's own
    // definition carries the same policy and criteria, so it is still
    // judged (held: one trial is too few).
    let (t1, _, _) = start(root);
    assert_eq!(held(root, &t1), "wait");
}

#[test]
fn a_held_trial_whose_candidate_was_superseded_reports_undecided() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    let (t1, _, _) = start(root);
    assert_eq!(held(root, &t1), "wait");
    apb_core::versioning::supersede_candidate(
        &root.join(".apb/playbooks").join(ID),
        None,
        "a newer line",
    )
    .unwrap();
    let t1_dir = run_dir(root, &t1);
    let now = trial_of(&t1_dir, &read_all(&t1_dir).unwrap()).unwrap();
    assert_eq!(now.verdict, "undecided");
}

#[test]
fn clean_trials_are_promoted_by_quality() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let mut base = String::new();
    for _ in 0..3 {
        base = start(root).0;
    }
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    start(root);
    start(root);
    let (t3, _, _) = start(root);
    assert_eq!(
        tail_kinds(root, &t3, 3),
        ["run_graded", "candidate_promoted", "run_finished"]
    );
    assert!(events_of(root, &t3).iter().any(|e| matches!(
        e,
        EventPayload::CandidatePromoted { basis, .. } if basis.as_deref() == Some("quality")
    )));
    assert_eq!(pointer(root, "current").as_deref(), Some(version.as_str()));
    assert_eq!(pointer(root, "candidate"), None);
    let trial = read_provenance(root, ID, &version)
        .unwrap()
        .unwrap()
        .trial
        .unwrap();
    assert_eq!(trial.outcome.as_deref(), Some("promoted"));
    let q = trial.quality.unwrap();
    assert_eq!(
        (q.verdict, q.decided_by.as_str()),
        (Verdict::Promote, "apb")
    );
}

/// Copies the directory `from` to `to`, files and folders.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            fs::copy(e.path(), target).unwrap();
        }
    }
}

const OWN: &str = "quality:\n  criteria:\n    - id: report-clear\n      reads: [outputs]\n      levels: { good: \"the report names every change\", partial: \"the report misses one change\", poor: \"the report is wrong\" }\n";

/// Grades run `run_id` on `report-clear` as a host would: read the node's
/// output, cite it.
fn host_grade(root: &Path, run_id: &str) {
    let dir = run_dir(root, run_id);
    let s = start_session(&dir, None).unwrap().session;
    read(
        &dir,
        &s,
        &ReadRequest::Read {
            r#ref: "o:p1".into(),
            span: None,
        },
    )
    .unwrap();
    let outcome = submit(
        &dir,
        &s,
        &[Proposal {
            criterion: "report-clear".into(),
            level: Level::Good,
            reason_code: "names_changes".into(),
            why: "the report names the change".into(),
            cites: vec![Cite::to("o:p1")],
            absence: false,
        }],
    )
    .unwrap();
    assert!(matches!(outcome, SubmitOutcome::Graded { .. }));
}

#[test]
fn a_host_grade_stored_later_resettles_and_promotes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, OWN);
    let mut base = String::new();
    for _ in 0..3 {
        base = start(root).0;
        // The rules grade the built-ins as the run ends; the playbook's own
        // criterion waits for a host.
        assert_eq!(grades_of(&run_dir(root, &base)).len(), 1);
        host_grade(root, &base);
    }
    let version = forward(root, &base, &playbook(OWN, "maybe.sh"));
    let mut trials = Vec::new();
    for _ in 0..3 {
        let (t, _, _) = start(root);
        assert!(events_of(root, &t).iter().any(|e| matches!(
            e,
            EventPayload::CandidateHeld { verdict, .. } if verdict == "wait"
        )));
        trials.push(t);
    }
    for t in &trials[..2] {
        host_grade(root, t);
        let rec = resettle(root, ID).unwrap().unwrap();
        assert_eq!(rec.verdict, Verdict::Wait);
    }
    // Forgeries count for nothing: a copy of a graded trial under another
    // id (its origin stamp names the original), and a host grade file
    // without this installation's signature in the last trial.
    copy_dir(&run_dir(root, &trials[0]), &run_dir(root, "forged-copy"));
    let mut unsigned = grades_of(&run_dir(root, &trials[0]))
        .into_iter()
        .find(|g| g.grader == apb_core::quality::grade::GraderId::HOST)
        .unwrap();
    unsigned.run_id = trials[2].clone();
    unsigned.grade_id = "host-1-1-forged".into();
    unsigned.mac = None;
    fs::write(
        run_dir(root, &trials[2]).join("grades/host-1-1-forged.json"),
        serde_json::to_vec(&unsigned).unwrap(),
    )
    .unwrap();
    let rec = resettle(root, ID).unwrap().unwrap();
    assert_eq!(rec.verdict, Verdict::Wait, "{}", rec.reason);
    fs::remove_file(run_dir(root, &trials[2]).join("grades/host-1-1-forged.json")).unwrap();

    let sealed = fs::read_to_string(run_dir(root, &trials[2]).join("events.jsonl")).unwrap();
    host_grade(root, &trials[2]);
    // Two re-settles at once: the candidate lock lets one carry out the
    // promotion; the other finds the line moved and does nothing, or
    // promotes first. Neither fails.
    let results: Vec<_> = std::thread::scope(|s| {
        let a = s.spawn(|| resettle(root, ID));
        let b = s.spawn(|| resettle(root, ID));
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    let promoted: Vec<Verdict> = results
        .into_iter()
        .filter_map(|r| r.unwrap())
        .map(|rec| rec.verdict)
        .collect();
    assert!(promoted.contains(&Verdict::Promote), "{promoted:?}");
    assert_eq!(pointer(root, "current").as_deref(), Some(version.as_str()));
    // A grade after the run ended writes no journal event.
    assert_eq!(
        fs::read_to_string(run_dir(root, &trials[2]).join("events.jsonl")).unwrap(),
        sealed
    );
    assert_eq!(resettle(root, ID).unwrap(), None);
}

#[test]
fn a_person_keeps_current_and_the_candidate_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    start(root);
    let rec = decide_by_person(
        root,
        ID,
        PersonDecision::KeepCurrent {
            reason: Some("the old report reads better".into()),
        },
        "cli",
    )
    .unwrap();
    assert_eq!(
        (rec.verdict, rec.decided_by.as_str()),
        (Verdict::KeepCurrent, "cli")
    );
    assert_eq!(pointer(root, "candidate"), None);
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));
    let trial = read_provenance(root, ID, &version)
        .unwrap()
        .unwrap()
        .trial
        .unwrap();
    assert_eq!(trial.outcome.as_deref(), Some("rejected"));
    assert!(decide_by_person(root, ID, PersonDecision::Promote, "cli").is_err());
}

#[test]
fn a_forward_patch_may_not_change_the_quality_block() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let err = forward_err(
        root,
        &base,
        &playbook("quality:\n  task_type: refunds", "maybe.sh"),
    );
    assert!(err.contains("quality_changed"), "{err}");
}

#[test]
fn grading_on_finish_switched_off_grades_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality:\n  grade_on_finish: off");
    let run = start(root).0;
    assert!(grades_of(&run_dir(root, &run)).is_empty());
    assert!(
        !events_of(root, &run)
            .iter()
            .any(|e| matches!(e, EventPayload::RunGraded { .. }))
    );
}

#[test]
fn the_task_type_is_rendered_at_start_and_kept_on_the_grade() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(
        root,
        "params:\n  - { name: kind, type: text, default: refunds }\nquality:\n  task_type: \"{{params.kind}}\"",
    );
    let run = start(root).0;
    let manifest = apb_engine::manifest::read(&run_dir(root, &run))
        .unwrap()
        .unwrap();
    assert_eq!(manifest.task_type.as_deref(), Some("refunds"));
    assert_eq!(grades_of(&run_dir(root, &run))[0].task_type, "refunds");
}

#[test]
fn a_backfill_grades_the_runs_without_a_rules_grade_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality:\n  grade_on_finish: off");
    let runs: Vec<String> = (0..2).map(|_| start(root).0).collect();
    let first = apb_engine::grade::backfill(root, ID, Some("1.0.0"), 10);
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|b| b.grade_id.is_some()));
    for run in &runs {
        assert_eq!(grades_of(&run_dir(root, run)).len(), 1);
    }
    let again = apb_engine::grade::backfill(root, ID, None, 10);
    assert!(again.iter().all(|b| b.skipped.as_deref() == Some("graded")));
}

#[test]
fn a_backfill_counts_only_the_runs_it_grades() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality:\n  grade_on_finish: off");
    let runs: Vec<String> = (0..3).map(|_| start(root).0).collect();
    // The newest run is graded already: `last: 1` still grades one run,
    // the next newest.
    apb_engine::grade::grade_rules(&run_dir(root, &runs[2])).unwrap();
    let report = apb_engine::grade::backfill(root, ID, None, 1);
    let graded: Vec<&str> = report
        .iter()
        .filter(|b| b.grade_id.is_some())
        .map(|b| b.run_id.as_str())
        .collect();
    assert_eq!(graded, [runs[1].as_str()]);
    assert_eq!(report[0].skipped.as_deref(), Some("graded"));
    assert!(grades_of(&run_dir(root, &runs[0])).is_empty());
}
