//! Promotion by quality, end to end through the engine: a run of a
//! playbook with a `quality:` block is graded by rules as it ends
//! (`run_graded`), a candidate trial is compared with `current`'s graded
//! runs and held (`candidate_held`), promoted or kept out, one failed
//! trial alone rejects nothing, spent trials go to a person, a host grade
//! stored later re-settles the candidate, and a copied run folder or an
//! unsigned grade file counts for nothing. A person's promotion is checked
//! against the candidate summary under the candidate lock.

use std::fs;
use std::path::Path;

use apb_core::quality::compare::Verdict;
use apb_core::quality::grade::{Cite, Level};
use apb_core::registry::init_project;
use apb_core::scenario::ScenarioLevel;
use apb_core::versioning::read_provenance;
use apb_engine::candidate::summary::{
    AxisState, PromoteRefusal, SummaryCode, SummaryItem, VersionDigest,
};
use apb_engine::candidate::{
    PersonDecision, PromoteError, candidate_summary, decide_by_person, quality_view, resettle,
    trial_of,
};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::grade::evidence::Proposal;
use apb_engine::grade::reader::ReadRequest;
use apb_engine::grade::{SubmitOutcome, grades_of, read, start_session, submit};
use apb_engine::scenario::report::Verdict as ScenarioVerdict;
use apb_engine::scenario::store::IndexLine;
use apb_engine::state::RunStatus;

use crate::candidate_trial_test::{
    ID, assert_promoted_by, events_of, forward, forward_err, pointer, start,
};

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

    let rec = decide_by_person(root, ID, promote(root, &version), "cli").unwrap();
    assert_eq!(
        (rec.verdict, rec.decided_by.as_str()),
        (Verdict::Promote, "cli")
    );
    assert_eq!(pointer(root, "current").as_deref(), Some(version.as_str()));
    assert_promoted_by(root, "person", None);
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
    assert_promoted_by(root, "quality", Some(&t3));
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
    assert!(decide_by_person(root, ID, promote(root, &version), "cli").is_err());
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
    let first = apb_engine::grade::backfill(
        root,
        ID,
        Some("1.0.0"),
        10,
        apb_engine::grade::GraderChoice::Rules,
    );
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|b| b.grade_id.is_some()));
    for run in &runs {
        assert_eq!(grades_of(&run_dir(root, run)).len(), 1);
    }
    let again =
        apb_engine::grade::backfill(root, ID, None, 10, apb_engine::grade::GraderChoice::Rules);
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
    let report =
        apb_engine::grade::backfill(root, ID, None, 1, apb_engine::grade::GraderChoice::Rules);
    let graded: Vec<&str> = report
        .iter()
        .filter(|b| b.grade_id.is_some())
        .map(|b| b.run_id.as_str())
        .collect();
    assert_eq!(graded, [runs[1].as_str()]);
    assert_eq!(report[0].skipped.as_deref(), Some("graded"));
    assert!(grades_of(&run_dir(root, &runs[0])).is_empty());
}

/// A person's promotion of the candidate `version` as `apb candidate`
/// shows it: its version and trust digest, acknowledging the summary's
/// attention items under the id they stand under now.
fn promote(root: &Path, version: &str) -> PersonDecision {
    match candidate_summary(root, ID).unwrap() {
        Some(s) => promote_as(version, &s.candidate.digest, Some(&s.summary_id)),
        None => promote_as(version, &digest(root, version), None),
    }
}

/// Writes `yaml` (the template's text) as the definition of `version`, in
/// place.
fn rewrite(root: &Path, version: &str, yaml: &str) {
    let file = root
        .join(".apb/playbooks")
        .join(ID)
        .join(version)
        .join("playbook.yaml");
    let yaml = yaml.replace("version: 1.0.0", &format!("version: {version}"));
    assert_ne!(fs::read_to_string(&file).unwrap(), yaml);
    fs::write(&file, yaml).unwrap();
}

/// The trust digest `version` reads as now.
fn digest(root: &Path, version: &str) -> String {
    apb_core::registry::Registry::open(root)
        .unwrap()
        .load(ID, Some(version))
        .unwrap()
        .trust_digest()
        .unwrap()
}

fn promote_as(version: &str, digest: &str, ack: Option<&str>) -> PersonDecision {
    PersonDecision::Promote {
        expected: VersionDigest {
            version: version.to_string(),
            digest: digest.to_string(),
        },
        acknowledged: ack.map(str::to_string),
    }
}

fn refusal(r: Result<apb_core::quality::QualityRecord, PromoteError>) -> PromoteRefusal {
    match r {
        Err(PromoteError::Refused(r)) => r,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn codes(items: &[SummaryItem]) -> Vec<SummaryCode> {
    items.iter().map(|i| i.code).collect()
}

/// A scenario `green` of the playbook, answering `p1` once.
fn scenario(root: &Path, text: &str) -> String {
    let dir = root.join(".apb/playbooks").join(ID);
    let file = apb_core::scenario::scenarios_dir(&dir).join("green.yaml");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, text).unwrap();
    apb_core::scenario::load(&dir, "green").unwrap().digest
}

const GREEN: &str =
    "schema: 1\nid: green\nanswers:\n  p1:\n    - { visit: 1, script: { exit: 0, stdout: ok } }\n";

/// A result of `green` at quick in the results index, as a scenario run
/// leaves it.
fn result(
    root: &Path,
    version: &str,
    pd: &str,
    sd: &str,
    verdict: ScenarioVerdict,
    run: &str,
    at: u64,
) {
    apb_engine::scenario::store::append(
        root,
        ID,
        &IndexLine {
            scenario: "green".into(),
            scenario_digest: sd.into(),
            level: ScenarioLevel::Quick,
            version: version.into(),
            playbook_digest: pd.into(),
            verdict,
            run_id: run.into(),
            at_ms: at,
            decision_requests: 0,
            cost_usd: None,
            external_data: false,
        },
    )
    .unwrap();
}

#[test]
fn a_promotion_moves_exactly_what_the_person_saw_and_acknowledged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    let sd = scenario(root, GREEN);
    let pd = digest(root, &version);

    // Three axes apart: fit here, a scenario that never ran, a comparison
    // with too few grades.
    let s = candidate_summary(root, ID).unwrap().unwrap();
    assert_eq!(
        s.candidate,
        VersionDigest {
            version: version.clone(),
            digest: pd.clone()
        }
    );
    assert_eq!(s.current.version, "1.0.0");
    assert_eq!(
        s.fitness.state,
        AxisState::Ok,
        "{:?}",
        s.fitness.detail.report
    );
    assert_eq!(s.definition.state, AxisState::Attention);
    assert_eq!(s.quality.state, AxisState::Attention);
    assert!(s.blocking.is_empty());
    assert_eq!(
        codes(&s.attention),
        [SummaryCode::ScenarioNotRun, SummaryCode::QualityNotPromote]
    );
    assert_eq!(s.definition.detail.not_run, ["green"]);

    // Another version or digest than the one shown is stale.
    let wrong = refusal(decide_by_person(
        root,
        ID,
        promote_as(&version, "sha256:00", None),
        "cli",
    ));
    assert!(
        matches!(&wrong, PromoteRefusal::Stale { found: Some(f), .. } if f.digest == pd),
        "{wrong:?}"
    );
    let other = refusal(decide_by_person(
        root,
        ID,
        promote_as("1.0.0", &pd, None),
        "cli",
    ));
    assert!(matches!(other, PromoteRefusal::Stale { .. }), "{other:?}");
    // Attention items unacknowledged: refused with the id to name.
    let unacked = refusal(decide_by_person(
        root,
        ID,
        promote_as(&version, &pd, None),
        "cli",
    ));
    assert_eq!(
        unacked,
        PromoteRefusal::NeedsAcknowledgement {
            summary_id: s.summary_id.clone(),
            items: s.attention.clone()
        }
    );

    // A scenario result arrives between the view and the click: the id
    // the person acknowledged no longer covers what stands.
    result(root, &version, &pd, &sd, ScenarioVerdict::Passed, "s-1", 1);
    let after = candidate_summary(root, ID).unwrap().unwrap();
    assert_ne!(after.summary_id, s.summary_id);
    assert_eq!(codes(&after.attention), [SummaryCode::QualityNotPromote]);
    let late = refusal(decide_by_person(
        root,
        ID,
        promote_as(&version, &pd, Some(&s.summary_id)),
        "cli",
    ));
    assert!(
        matches!(&late, PromoteRefusal::NeedsAcknowledgement { summary_id, .. } if *summary_id == after.summary_id),
        "{late:?}"
    );
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));

    let rec = decide_by_person(
        root,
        ID,
        promote_as(&version, &pd, Some(&after.summary_id)),
        "cli",
    )
    .unwrap();
    assert_eq!(rec.summary_id.as_deref(), Some(after.summary_id.as_str()));
    assert_eq!(pointer(root, "current").as_deref(), Some(version.as_str()));
    let kept = read_provenance(root, ID, &version)
        .unwrap()
        .unwrap()
        .trial
        .unwrap();
    assert_eq!(kept.outcome.as_deref(), Some("promoted"));
    assert_eq!(kept.quality.unwrap().summary_id, rec.summary_id);
    let history =
        apb_core::versioning::current::history(&root.join(".apb/playbooks").join(ID), 5).unwrap();
    assert_eq!(history.entries[0].to.as_deref(), Some(version.as_str()));
}

#[test]
fn a_scenario_result_counts_only_for_the_candidate_and_the_file_as_they_are() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    let sd = scenario(root, GREEN);
    let pd = digest(root, &version);
    let scenario_codes = |root: &Path| {
        let s = candidate_summary(root, ID).unwrap().unwrap();
        let only: Vec<SummaryCode> = codes(&s.attention)
            .into_iter()
            .filter(|c| *c != SummaryCode::QualityNotPromote)
            .collect();
        (
            only,
            s.definition.detail.not_run,
            s.definition.detail.scenarios,
        )
    };

    // A failed result of this very candidate needs acknowledging.
    result(root, &version, &pd, &sd, ScenarioVerdict::Failed, "s-1", 1);
    let (c, not_run, refs) = scenario_codes(root);
    assert_eq!(c, [SummaryCode::ScenarioFailed]);
    assert!(not_run.is_empty());
    assert!(refs[0].fresh);

    // A newer result for another digest of the playbook is stale.
    result(
        root,
        &version,
        "sha256:other",
        &sd,
        ScenarioVerdict::Passed,
        "s-2",
        2,
    );
    let (c, not_run, refs) = scenario_codes(root);
    assert_eq!(c, [SummaryCode::ScenarioStale]);
    assert_eq!(not_run, ["green"]);
    assert!(!refs[0].fresh);

    // A fresh pass, then the scenario file is edited: stale again.
    result(root, &version, &pd, &sd, ScenarioVerdict::Passed, "s-3", 3);
    assert_eq!(scenario_codes(root).0, Vec::<SummaryCode>::new());
    scenario(root, &format!("{GREEN}title: edited\n"));
    assert_eq!(scenario_codes(root).0, [SummaryCode::ScenarioStale]);
}

#[test]
fn a_validator_error_blocks_and_a_missing_profile_is_attention_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    // A node bound to a profile this machine does not have: fitness, not
    // correctness.
    let agent = playbook("quality: {}", "maybe.sh")
        .replace(
            "  - { id: done, type: finish, outcome: success }",
            "  - { id: done, type: finish, outcome: success }\n  - { id: ask, type: agent_task, profile: ghost, prompt: hi }",
        )
        .replace(
            "  - { from: p1, to: done }",
            "  - { from: p1, to: ask }\n  - { from: ask, to: done }",
        );
    rewrite(root, &version, &agent);
    let s = candidate_summary(root, ID).unwrap().unwrap();
    assert_eq!(
        s.fitness.state,
        AxisState::Attention,
        "{:?}",
        s.fitness.detail.report
    );
    assert!(s.blocking.is_empty(), "{:?}", s.blocking);
    assert!(codes(&s.attention).contains(&SummaryCode::FitnessProblem));
    let acked = decide_by_person(
        root,
        ID,
        promote_as(&version, &s.candidate.digest, Some(&s.summary_id)),
        "cli",
    );
    assert!(acked.is_ok(), "{acked:?}");

    // A validator error on a candidate blocks, acknowledged or not.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    let broken = playbook("quality: {}", "maybe.sh")
        .replace("{ from: p1, to: done }", "{ from: p1, to: nowhere }");
    rewrite(root, &version, &broken);
    let s = candidate_summary(root, ID).unwrap().unwrap();
    assert_eq!(s.definition.state, AxisState::Blocking);
    assert!(
        codes(&s.blocking).contains(&SummaryCode::ValidatorError),
        "{:?}",
        s.blocking
    );
    let r = refusal(decide_by_person(
        root,
        ID,
        promote_as(&version, &s.candidate.digest, Some(&s.summary_id)),
        "cli",
    ));
    assert!(matches!(r, PromoteRefusal::Blocked { .. }), "{r:?}");
    assert_ne!(pointer(root, "current").as_deref(), Some(version.as_str()));
}

#[test]
fn a_candidate_edited_after_a_run_of_it_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "quality: {}");
    let base = start(root).0;
    let version = forward(root, &base, &playbook("quality: {}", "maybe.sh"));
    let (_, _, chosen) = start(root);
    assert_eq!(chosen.as_deref(), Some(version.as_str()));
    let file = root
        .join(".apb/playbooks")
        .join(ID)
        .join(&version)
        .join("playbook.yaml");
    let mut yaml = fs::read_to_string(&file).unwrap();
    yaml.push_str("# edited in place\n");
    fs::write(&file, yaml).unwrap();
    let s = candidate_summary(root, ID).unwrap().unwrap();
    assert_eq!(codes(&s.blocking), [SummaryCode::CandidateEdited]);
    let r = refusal(decide_by_person(
        root,
        ID,
        promote_as(&version, &s.candidate.digest, Some(&s.summary_id)),
        "cli",
    ));
    assert!(matches!(r, PromoteRefusal::Blocked { .. }), "{r:?}");
}

/// The quality counts a person saw are part of what they acknowledge: a
/// host grade stored after the view changes the counts while the verdict
/// and its reason stay, and the old acknowledgement no longer promotes.
#[test]
fn an_acknowledgement_does_not_cover_quality_counts_that_changed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, OWN);
    let mut base = String::new();
    for _ in 0..3 {
        base = start(root).0;
        host_grade(root, &base);
    }
    let version = forward(root, &base, &playbook(OWN, "maybe.sh"));
    let mut trials = Vec::new();
    for _ in 0..4 {
        trials.push(start(root).0);
    }
    host_grade(root, &trials[0]);
    let s = candidate_summary(root, ID).unwrap().unwrap();
    let before = s.quality.detail.view.decision.clone().unwrap();
    assert_eq!(before.verdict, Verdict::NeedsPerson, "{}", before.reason);
    host_grade(root, &trials[1]);
    let after = quality_view(root, ID).unwrap().unwrap().decision.unwrap();
    assert_eq!(
        (after.verdict, after.reason.as_str()),
        (before.verdict, before.reason.as_str())
    );
    assert_ne!(after.per_criterion, before.per_criterion);
    let late = refusal(decide_by_person(
        root,
        ID,
        promote_as(&version, &s.candidate.digest, Some(&s.summary_id)),
        "cli",
    ));
    assert!(
        matches!(&late, PromoteRefusal::NeedsAcknowledgement { summary_id, .. } if *summary_id != s.summary_id),
        "{late:?}"
    );
    assert_eq!(pointer(root, "current").as_deref(), Some("1.0.0"));
    let now = candidate_summary(root, ID).unwrap().unwrap();
    decide_by_person(
        root,
        ID,
        promote_as(&version, &now.candidate.digest, Some(&now.summary_id)),
        "cli",
    )
    .unwrap();
    assert_eq!(pointer(root, "current").as_deref(), Some(version.as_str()));
}
