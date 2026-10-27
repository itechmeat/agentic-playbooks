//! The `quality:` block and promotion by quality: the block parses and
//! round-trips, a playbook without it re-saves unchanged, and the
//! comparison reaches each verdict from the grades alone.

use apb_core::quality::compare::{
    CompareMode, CriterionResult, Expected, QualityGate, Verdict, decide, expected_of,
};
use apb_core::quality::criteria::{
    CriterionSource, EvidenceKind, GradeOnFinish, ReadKind, Subject, builtin,
};
use apb_core::quality::grade::{CriterionGrade, Grade, GraderId, Level, ReadTrace};
use apb_core::schema::Playbook;

const BASE: &str = "schema: 2\nid: q\nname: Q\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: draft, type: agent_task, prompt: hi, profile: x }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: draft }\n  - { from: draft, to: done }\n";

const BLOCK: &str = r#"quality:
  task_type: "{{params.kind}}"
  builtins:
    scope: { off: "the task is to reorganize the whole repository" }
  criteria:
    - id: reply-tone
      subject: { node: draft }
      applies_when: "the run sent at least one reply"
      evidence: both
      reads: [outputs, connector_calls]
      levels:
        good: "every reply matches the brand voice"
        partial: "one reply breaks the voice"
        poor: "a reply misleads the customer"
"#;

#[test]
fn the_block_parses_and_round_trips() {
    let p = Playbook::from_yaml(&format!("{BASE}{BLOCK}")).unwrap();
    let q = p.quality.clone().unwrap();
    assert_eq!(q.task_type.as_deref(), Some("{{params.kind}}"));
    assert_eq!(q.grade_on_finish, GradeOnFinish::Rules);
    let c = &q.criteria[0];
    assert_eq!(
        c.subject,
        Subject::Node {
            node: "draft".into()
        }
    );
    assert_eq!(c.evidence, EvidenceKind::Both);
    assert_eq!(c.reads, [ReadKind::Outputs, ReadKind::ConnectorCalls]);
    let defs = q.criteria_defs();
    let ids: Vec<&str> = defs.iter().map(|d| d.id()).collect();
    assert_eq!(
        ids,
        ["outcome", "loops", "consent", "skill_use", "reply-tone"]
    );
    assert_eq!(defs[4].source, CriterionSource::Playbook);
    let again = Playbook::from_yaml(&yaml_serde::to_string(&p).unwrap()).unwrap();
    assert_eq!(again.quality, p.quality);
}

#[test]
fn a_playbook_without_the_block_saves_without_it() {
    let p = Playbook::from_yaml(BASE).unwrap();
    assert!(p.quality.is_none());
    assert!(!yaml_serde::to_string(&p).unwrap().contains("quality"));
}

#[test]
fn an_edited_criterion_gets_a_new_digest() {
    let p = Playbook::from_yaml(&format!("{BASE}{BLOCK}")).unwrap();
    let edited = Playbook::from_yaml(&format!(
        "{BASE}{}",
        BLOCK.replace("one reply breaks", "two replies break")
    ))
    .unwrap();
    let digest = |p: &Playbook| {
        p.quality.as_ref().unwrap().criteria_defs()[4]
            .digest
            .clone()
    };
    assert_ne!(digest(&p), digest(&edited));
}

// --- the comparison ---

const LOOPS: &str = "loops";

fn loops_digest() -> String {
    builtin(LOOPS).unwrap().digest
}

fn expected() -> Vec<Expected> {
    expected_of(&[builtin(LOOPS).unwrap()])
}

/// A rules grade of one run with `level` on `loops`.
fn grade(run: &str, level: Level, task_type: &str) -> Grade {
    Grade {
        schema: 1,
        grade_id: format!("g-{run}"),
        run_id: run.into(),
        playbook: "q".into(),
        version: "1.0.0".into(),
        task_type: task_type.into(),
        grader: GraderId::RULES,
        created_at: 1,
        criteria: vec![CriterionGrade {
            id: LOOPS.into(),
            source: CriterionSource::Builtin,
            digest: loops_digest(),
            subject: "run".into(),
            level,
            reason_code: "test".into(),
            why: "test".into(),
            cites: Vec::new(),
            absence_basis: Vec::new(),
            counts_for_promotion: true,
        }],
        reading: ReadTrace::default(),
        skills: Vec::new(),
        mac: None,
    }
}

/// `n` runs named `<side><i>`, the first `poor` of them poor and the rest
/// at `rest`.
fn side(name: &str, n: usize, poor: usize, rest: Level) -> Vec<Grade> {
    (0..n)
        .map(|i| {
            let level = if i < poor { Level::Poor } else { rest };
            grade(&format!("{name}{i}"), level, "default")
        })
        .collect()
}

fn gate(compare: CompareMode) -> QualityGate {
    QualityGate {
        compare,
        min_runs: 5,
        max_trials: 10,
        window: 20,
        margin: 0.35,
    }
}

#[test]
fn not_worse_promotes_when_no_criterion_is_worse() {
    let d = decide(
        &expected(),
        &side("c", 6, 0, Level::Good),
        &side("k", 10, 1, Level::Good),
        &gate(CompareMode::NotWorse),
        6,
    );
    assert_eq!(d.verdict, Verdict::Promote, "{}", d.reason);
    assert_eq!(d.per_criterion[0].result, CriterionResult::Same);
}

#[test]
fn a_worse_criterion_keeps_current() {
    let d = decide(
        &expected(),
        &side("c", 8, 7, Level::Good),
        &side("k", 10, 0, Level::Good),
        &gate(CompareMode::NotWorse),
        8,
    );
    assert_eq!(d.verdict, Verdict::KeepCurrent);
    assert_eq!(d.per_criterion[0].result, CriterionResult::Worse);
    assert!(d.reason.contains("loops"), "{}", d.reason);
}

#[test]
fn a_small_sample_waits_then_needs_a_person() {
    let few = side("c", 3, 0, Level::Good);
    let current = side("k", 10, 0, Level::Good);
    let waits = decide(&expected(), &few, &current, &gate(CompareMode::NotWorse), 3);
    assert_eq!(waits.verdict, Verdict::Wait);
    assert_eq!(waits.per_criterion[0].result, CriterionResult::Insufficient);
    let spent = decide(
        &expected(),
        &few,
        &current,
        &gate(CompareMode::NotWorse),
        10,
    );
    assert_eq!(spent.verdict, Verdict::NeedsPerson);
}

#[test]
fn mostly_not_enough_data_is_insufficient() {
    let mut cand = side("c", 5, 0, Level::Good);
    cand.extend(side("n", 6, 0, Level::NotEnoughData));
    let d = decide(
        &expected(),
        &cand,
        &side("k", 10, 0, Level::Good),
        &gate(CompareMode::NotWorse),
        11,
    );
    assert_eq!(d.per_criterion[0].result, CriterionResult::Insufficient);
    assert_eq!(d.verdict, Verdict::NeedsPerson);
}

#[test]
fn better_without_a_gain_keeps_current_and_with_one_promotes() {
    let same = decide(
        &expected(),
        &side("c", 10, 0, Level::Good),
        &side("k", 10, 0, Level::Good),
        &gate(CompareMode::Better),
        10,
    );
    assert_eq!(same.verdict, Verdict::KeepCurrent);
    assert!(same.reason.contains("no measurable improvement"));
    let gain = decide(
        &expected(),
        &side("c", 20, 0, Level::Good),
        &side("k", 20, 0, Level::Partial),
        &gate(CompareMode::Better),
        10,
    );
    assert_eq!(gain.verdict, Verdict::Promote, "{}", gain.reason);
    assert_eq!(gain.per_criterion[0].result, CriterionResult::Better);
}

#[test]
fn grades_under_another_key_are_left_out() {
    // Current's runs were graded under another digest, another grader or
    // another task type: none of them is comparable, so the candidate has
    // nothing to be compared with.
    let mut other_digest = side("a", 10, 0, Level::Good);
    for g in &mut other_digest {
        g.criteria[0].digest = "sha256:other".into();
    }
    let mut other_grader = side("b", 10, 0, Level::Good);
    for g in &mut other_grader {
        g.grader = GraderId::HOST;
    }
    let other_task: Vec<Grade> = (0..10)
        .map(|i| grade(&format!("t{i}"), Level::Good, "refund"))
        .collect();
    let current: Vec<Grade> = [other_digest, other_grader, other_task].concat();
    let d = decide(
        &expected(),
        &side("c", 6, 0, Level::Good),
        &current,
        &gate(CompareMode::NotWorse),
        6,
    );
    assert_eq!(d.per_criterion.len(), 1);
    assert_eq!(d.per_criterion[0].current.total(), 0);
    assert_eq!(d.verdict, Verdict::Wait);
}

#[test]
fn a_criterion_without_any_candidate_grade_is_insufficient_and_shows_current() {
    let d = decide(
        &expected(),
        &[],
        &side("k", 10, 2, Level::Good),
        &gate(CompareMode::NotWorse),
        1,
    );
    let row = &d.per_criterion[0];
    assert_eq!(row.result, CriterionResult::Insufficient);
    assert_eq!(row.candidate.total(), 0);
    // The row shows what current has, not an empty tally.
    assert_eq!((row.current.good, row.current.poor), (8, 2));
    assert_eq!(d.verdict, Verdict::Wait);
}

#[test]
fn a_host_criterion_no_trial_graded_says_a_host_grades_it() {
    let host = Expected {
        criterion: "reply-tone".into(),
        digest: "sha256:t".into(),
        grader: GraderId::HOST,
    };
    let d = decide(&[host], &[], &[], &gate(CompareMode::NotWorse), 2);
    assert_eq!(d.verdict, Verdict::Wait);
    assert!(d.reason.contains("run_grade_start"), "{}", d.reason);
}

#[test]
fn no_evidence_never_promotes() {
    // Every criterion applied to no run: nothing was compared.
    let d = decide(
        &expected(),
        &side("c", 6, 0, Level::NotApplicable),
        &side("k", 10, 0, Level::NotApplicable),
        &gate(CompareMode::NotWorse),
        6,
    );
    assert_eq!(d.per_criterion[0].result, CriterionResult::NotApplicable);
    assert_eq!(d.verdict, Verdict::Wait, "{}", d.reason);
    // No criterion in force at all: the same, and a person once the trials
    // are spent.
    let none = decide(&[], &[], &[], &gate(CompareMode::NotWorse), 10);
    assert!(none.per_criterion.is_empty());
    assert_eq!(none.verdict, Verdict::NeedsPerson, "{}", none.reason);
}

#[test]
fn four_poor_trials_of_five_are_never_promoted() {
    // Not shown worse at this size (the intervals overlap), but not shown
    // within the margin either: the gate waits, then a person decides.
    let cand = side("c", 5, 4, Level::Good);
    let current = side("k", 5, 0, Level::Good);
    let d = decide(
        &expected(),
        &cand,
        &current,
        &gate(CompareMode::NotWorse),
        5,
    );
    assert_eq!(d.per_criterion[0].result, CriterionResult::Inconclusive);
    assert_eq!(d.verdict, Verdict::Wait, "{}", d.reason);
    let spent = decide(
        &expected(),
        &cand,
        &current,
        &gate(CompareMode::NotWorse),
        10,
    );
    assert_eq!(spent.verdict, Verdict::NeedsPerson);
}

#[test]
fn a_clean_candidate_is_shown_not_worse_once_its_bound_is_inside_the_margin() {
    let current = side("k", 20, 0, Level::Good);
    let early = decide(
        &expected(),
        &side("c", 5, 0, Level::Good),
        &current,
        &gate(CompareMode::NotWorse),
        5,
    );
    assert_eq!(early.per_criterion[0].result, CriterionResult::Inconclusive);
    let enough = decide(
        &expected(),
        &side("c", 8, 0, Level::Good),
        &current,
        &gate(CompareMode::NotWorse),
        8,
    );
    assert_eq!(enough.verdict, Verdict::Promote, "{}", enough.reason);
}

#[test]
fn the_latest_grade_of_a_run_counts_once() {
    // Run c0 was graded twice: the newer grade replaces the older one,
    // by created_at and not by order (the newer one comes first here).
    let mut regrade = grade("c0", Level::Poor, "default");
    regrade.created_at = 2;
    let mut cand = vec![regrade];
    cand.extend(side("c", 5, 0, Level::Good));
    let d = decide(
        &expected(),
        &cand,
        &side("k", 10, 0, Level::Good),
        &gate(CompareMode::NotWorse),
        5,
    );
    assert_eq!(d.per_criterion[0].candidate.total(), 5);
    assert_eq!(d.per_criterion[0].candidate.poor, 1);
}

#[test]
fn on_a_tie_the_grade_listed_later_counts() {
    let mut tie = grade("c0", Level::Poor, "default");
    tie.grade_id = "g-c0-later".into();
    let mut cand = side("c", 5, 0, Level::Good);
    cand.push(tie);
    let d = decide(
        &expected(),
        &cand,
        &side("k", 10, 0, Level::Good),
        &gate(CompareMode::NotWorse),
        5,
    );
    assert_eq!(d.per_criterion[0].candidate.poor, 1);
}
