//! Promotion by quality: the deterministic comparison of a candidate's
//! graded trial runs with the graded runs of `current`.
//!
//! Grades are compared only under one [`ComparableKey`]: the same criterion
//! and criterion digest, the same grader and version, the same task type.
//! Per criterion (only determinate levels count):
//!
//! 1. fewer than `min_runs` determinate grades on either side, or more than
//!    half `not_enough_data` on the candidate side, is insufficient (a
//!    criterion that applied to no run on either side is left out);
//! 2. "worse": the Wilson 95 % lower bound of the candidate's poor rate is
//!    above the upper bound of current's (shown worse);
//! 3. "inconclusive": not shown worse, but not shown not worse either: the
//!    candidate's Wilson 95 % upper bound for poor is above current's poor
//!    rate plus the gate's `margin` (non-inferiority, so a small sample
//!    never passes for "no harm seen");
//! 4. "better": the candidate's lower bound for good is above current's
//!    upper bound for good.
//!
//! The verdict: any criterion worse keeps `current`; any insufficient or
//! inconclusive waits while trials are under `max_trials`, then needs a
//! person; so does a comparison where no criterion compared at all (no
//! evidence never promotes). Otherwise `not_worse` promotes, and `better`
//! promotes only when at least one criterion is better ("no measurable
//! improvement" keeps `current`).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::criteria::{CriteriaGrader, CriterionDef, CriterionSource, QualityBlock};
use super::grade::{Grade, GraderId, Level};
use crate::stats::wilson95;

/// The least `min_runs` a policy may ask for.
pub const MIN_RUNS_FLOOR: u32 = 3;
/// How far the candidate's poor rate may plausibly exceed current's (the
/// non-inferiority margin) when the policy names none. With the default
/// gate and a `current` with no poor run, a candidate with no poor trial
/// is shown not worse after 8 trials.
pub const DEFAULT_MARGIN: f64 = 0.35;

/// What a candidate must show against `current`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareMode {
    /// No criterion worse.
    #[default]
    NotWorse,
    /// No criterion worse and at least one better.
    Better,
}

/// The quality gate of `promote_supervisor_patches: { when: quality }`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QualityGate {
    pub compare: CompareMode,
    /// Determinate grades needed per side and per criterion.
    pub min_runs: u32,
    /// Trials after which an undecided comparison goes to a person.
    pub max_trials: u32,
    /// How many of current's latest graded runs are compared with.
    pub window: u32,
    /// The non-inferiority margin, 0 to 1: a criterion is not worse only
    /// when the candidate's upper bound for poor is at most current's poor
    /// rate plus this.
    pub margin: f64,
}

impl Default for QualityGate {
    fn default() -> Self {
        QualityGate {
            compare: CompareMode::NotWorse,
            min_runs: 5,
            max_trials: 10,
            window: 20,
            margin: DEFAULT_MARGIN,
        }
    }
}

/// The key grades are comparable under.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ComparableKey {
    pub criterion: String,
    pub digest: String,
    /// `<kind>/<version>`.
    pub grader: String,
    pub task_type: String,
}

/// A criterion `current` defines and the grader that grades it: built-ins
/// by the rules grader, the playbook's own by the block's
/// `criteria_grader` (a host unless it says `model`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub criterion: String,
    pub digest: String,
    pub grader: GraderId,
}

/// The grader of the playbook's own criteria in `quality`: a host, unless
/// the block's `criteria_grader` says `model` (an unknown value, a V110,
/// and an absent block read as a host).
pub fn own_grader_of(quality: Option<&QualityBlock>) -> GraderId {
    match quality.map(|q| &q.criteria_grader) {
        Some(CriteriaGrader::Model) => GraderId::MODEL,
        _ => GraderId::HOST,
    }
}

/// The criteria a comparison covers, from `current`'s definitions, with
/// `own` grading the playbook's own criteria ([`own_grader_of`]).
pub fn expected_of(defs: &[CriterionDef], own: GraderId) -> Vec<Expected> {
    defs.iter()
        .map(|d| Expected {
            criterion: d.id().to_string(),
            digest: d.digest.clone(),
            grader: match d.source {
                CriterionSource::Builtin => GraderId::RULES,
                CriterionSource::Playbook => own,
            },
        })
        .collect()
}

/// Levels counted on one side.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub good: u32,
    pub partial: u32,
    pub poor: u32,
    pub not_applicable: u32,
    pub not_enough_data: u32,
}

impl Tally {
    fn add(&mut self, level: Level) {
        let slot = match level {
            Level::Good => &mut self.good,
            Level::Partial => &mut self.partial,
            Level::Poor => &mut self.poor,
            Level::NotApplicable => &mut self.not_applicable,
            Level::NotEnoughData => &mut self.not_enough_data,
        };
        *slot += 1;
    }

    pub fn determinate(&self) -> u32 {
        self.good + self.partial + self.poor
    }

    pub fn total(&self) -> u32 {
        self.determinate() + self.not_applicable + self.not_enough_data
    }

    fn interval(&self, hits: u32) -> Option<(f64, f64)> {
        wilson95(hits as usize, self.determinate() as usize)
    }

    /// The share of determinate grades that are poor.
    fn poor_rate(&self) -> f64 {
        match self.determinate() {
            0 => 0.0,
            n => f64::from(self.poor) / f64::from(n),
        }
    }
}

/// How one criterion compared.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionResult {
    Worse,
    Better,
    Same,
    /// Not shown worse, and not shown within the margin either: more
    /// trials narrow it down.
    Inconclusive,
    Insufficient,
    NotApplicable,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionComparison {
    pub key: ComparableKey,
    pub candidate: Tally,
    pub current: Tally,
    pub result: CriterionResult,
}

/// The four outcomes of a comparison.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Promote,
    /// Change nothing: the candidate is dropped and `current` stays.
    KeepCurrent,
    /// More trials are needed.
    Wait,
    /// The trials are spent without a decision: a person decides.
    NeedsPerson,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Promote => "promote",
            Verdict::KeepCurrent => "keep_current",
            Verdict::Wait => "wait",
            Verdict::NeedsPerson => "needs_person",
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityDecision {
    pub verdict: Verdict,
    pub per_criterion: Vec<CriterionComparison>,
    pub reason: String,
}

/// A quality decision as a candidate's trial record keeps it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityRecord {
    pub verdict: Verdict,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compared: Vec<CriterionComparison>,
    /// Unix milliseconds.
    pub decided_at: u64,
    /// `apb` for a decision the rule made, else the surface a person used.
    pub decided_by: String,
}

/// The latest grade of each grader per run: the newest `created_at`, and
/// on a tie the one later in `grades` (stores list grades oldest first, by
/// time then id, and the dashboard picks the same one).
pub fn latest_per_run(grades: &[Grade]) -> BTreeMap<(&str, GraderId), &Grade> {
    let mut latest: BTreeMap<(&str, GraderId), &Grade> = BTreeMap::new();
    for g in grades {
        let slot = latest.entry((g.run_id.as_str(), g.grader)).or_insert(g);
        if g.created_at >= slot.created_at {
            *slot = g;
        }
    }
    latest
}

/// The level each run got per comparable key: the latest grade of each
/// grader per run, its criteria that count for promotion.
fn levels_by_key(grades: &[Grade]) -> BTreeMap<ComparableKey, Vec<Level>> {
    let latest = latest_per_run(grades);
    let mut out: BTreeMap<ComparableKey, Vec<Level>> = BTreeMap::new();
    for g in latest.values() {
        for c in g.criteria.iter().filter(|c| c.counts_for_promotion) {
            let key = ComparableKey {
                criterion: c.id.clone(),
                digest: c.digest.clone(),
                grader: g.grader.to_string(),
                task_type: g.task_type.clone(),
            };
            out.entry(key).or_default().push(c.level);
        }
    }
    out
}

fn tally(levels: Option<&Vec<Level>>) -> Tally {
    let mut t = Tally::default();
    for l in levels.into_iter().flatten() {
        t.add(*l);
    }
    t
}

fn compare_one(candidate: Tally, current: Tally, gate: &QualityGate) -> CriterionResult {
    if candidate.determinate() == 0
        && current.determinate() == 0
        && candidate.not_applicable > 0
        && candidate.not_enough_data * 2 <= candidate.total()
    {
        return CriterionResult::NotApplicable;
    }
    if candidate.determinate() < gate.min_runs
        || current.determinate() < gate.min_runs
        || candidate.not_enough_data * 2 > candidate.total()
    {
        return CriterionResult::Insufficient;
    }
    let above = |a: Option<(f64, f64)>, b: Option<(f64, f64)>| match (a, b) {
        (Some((low, _)), Some((_, high))) => low > high,
        _ => false,
    };
    let candidate_poor = candidate.interval(candidate.poor);
    if above(candidate_poor, current.interval(current.poor)) {
        CriterionResult::Worse
    } else if candidate_poor.is_none_or(|(_, high)| high > current.poor_rate() + gate.margin) {
        CriterionResult::Inconclusive
    } else if above(
        candidate.interval(candidate.good),
        current.interval(current.good),
    ) {
        CriterionResult::Better
    } else {
        CriterionResult::Same
    }
}

/// Compares the candidate's graded trials with `current`'s graded runs over
/// the criteria `expected` (from current's definition). `trials` is how
/// many trial runs the candidate has had.
pub fn decide(
    expected: &[Expected],
    candidate: &[Grade],
    current: &[Grade],
    gate: &QualityGate,
    trials: u32,
) -> QualityDecision {
    let cand = levels_by_key(candidate);
    let cur = levels_by_key(current);
    let mut per_criterion = Vec::new();
    for e in expected {
        let grader = e.grader.to_string();
        let matches = |k: &ComparableKey| {
            k.criterion == e.criterion && k.digest == e.digest && k.grader == grader
        };
        let task_types: BTreeSet<&str> = cand
            .keys()
            .filter(|k| matches(k))
            .map(|k| k.task_type.as_str())
            .collect();
        if task_types.is_empty() {
            // No graded trial yet: one row with every graded run of
            // current under this criterion, so the row says what the
            // candidate is compared with.
            let current: Tally = cur.iter().filter(|(k, _)| matches(k)).fold(
                Tally::default(),
                |mut t, (_, levels)| {
                    levels.iter().for_each(|l| t.add(*l));
                    t
                },
            );
            per_criterion.push(CriterionComparison {
                key: ComparableKey {
                    criterion: e.criterion.clone(),
                    digest: e.digest.clone(),
                    grader: grader.clone(),
                    task_type: String::new(),
                },
                candidate: Tally::default(),
                current,
                result: CriterionResult::Insufficient,
            });
            continue;
        }
        for t in task_types {
            let key = ComparableKey {
                criterion: e.criterion.clone(),
                digest: e.digest.clone(),
                grader: grader.clone(),
                task_type: t.to_string(),
            };
            let (c, k) = (tally(cand.get(&key)), tally(cur.get(&key)));
            per_criterion.push(CriterionComparison {
                result: compare_one(c, k, gate),
                candidate: c,
                current: k,
                key,
            });
        }
    }
    let (verdict, reason) = verdict_of(&per_criterion, gate, trials);
    QualityDecision {
        verdict,
        per_criterion,
        reason,
    }
}

fn names(per: &[CriterionComparison], result: CriterionResult) -> Vec<String> {
    let mut out: Vec<String> = per
        .iter()
        .filter(|c| c.result == result)
        .map(|c| c.key.criterion.clone())
        .collect();
    out.dedup();
    out
}

/// The hint for criteria no trial has a grade of: a host or the model
/// grader grades the playbook's own criteria, apb's rules never do.
fn host_hint(per: &[CriterionComparison], waiting: &[String]) -> String {
    let mut out = String::new();
    for (grader, who, how) in [
        (GraderId::HOST, "a host", "with run_grade_start"),
        (GraderId::MODEL, "a model", "with apb grade --model"),
    ] {
        let grader = grader.to_string();
        let ungraded: Vec<&str> = per
            .iter()
            .filter(|c| {
                c.key.grader == grader
                    && c.candidate.total() == 0
                    && waiting.contains(&c.key.criterion)
            })
            .map(|c| c.key.criterion.as_str())
            .collect();
        if !ungraded.is_empty() {
            out.push_str(&format!(
                "; no trial has a {} grade for {}: {who} grades the trials {how}",
                grader.split('/').next().unwrap_or_default(),
                ungraded.join(", ")
            ));
        }
    }
    out
}

fn verdict_of(per: &[CriterionComparison], gate: &QualityGate, trials: u32) -> (Verdict, String) {
    let worse = names(per, CriterionResult::Worse);
    if !worse.is_empty() {
        return (
            Verdict::KeepCurrent,
            format!("worse than current on {}", worse.join(", ")),
        );
    }
    let mut open = names(per, CriterionResult::Insufficient);
    open.extend(names(per, CriterionResult::Inconclusive));
    let mut seen = BTreeSet::new();
    open.retain(|n| seen.insert(n.clone()));
    let compared = per
        .iter()
        .any(|c| matches!(c.result, CriterionResult::Same | CriterionResult::Better));
    let pending = if !open.is_empty() {
        Some(format!(
            "not shown to be not worse yet on {}{}",
            open.join(", "),
            host_hint(per, &open)
        ))
    } else if !compared {
        // Nothing compared (no criterion, or none that applied): no
        // evidence never promotes.
        Some("no criterion has comparable grades on both sides".to_string())
    } else {
        None
    };
    if let Some(why) = pending {
        return if trials < gate.max_trials {
            (
                Verdict::Wait,
                format!("{why} ({trials} of at most {} trials)", gate.max_trials),
            )
        } else {
            (
                Verdict::NeedsPerson,
                format!("{} trials spent: {why}", gate.max_trials),
            )
        };
    }
    let better = names(per, CriterionResult::Better);
    match gate.compare {
        CompareMode::NotWorse => (
            Verdict::Promote,
            "no criterion is worse than current, each within the margin".to_string(),
        ),
        CompareMode::Better if better.is_empty() => (
            Verdict::KeepCurrent,
            "no measurable improvement over current".to_string(),
        ),
        CompareMode::Better => (
            Verdict::Promote,
            format!("better than current on {}", better.join(", ")),
        ),
    }
}
