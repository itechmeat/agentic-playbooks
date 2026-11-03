//! Run quality in `apb stats`: per version, the levels its runs' grades
//! gave each criterion, one row per comparable key (criterion, its
//! definition's digest, grader, task type), and in `--compare` how the
//! good and poor shares moved for a key both versions graded alike.
//!
//! Only grades this installation signed for the very run count
//! ([`crate::grade::trusted_grades_of`]), and of each grader only the
//! latest per run ([`apb_core::quality::compare::latest_per_run`]): the
//! grades a promotion reads. Unlike a promotion, which weighs only the
//! criteria that count for it, every criterion is listed, informational
//! ones too.

use std::collections::BTreeMap;

use apb_core::quality::compare::{ComparableKey, Tally, latest_per_run};
use apb_core::quality::criteria::CriterionSource;
use apb_core::quality::grade::{Grade, Level};
use serde::Serialize;

use super::rate::{Rate, delta};
use super::stats_run::StatsRun;

/// One criterion over the graded runs of a version.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QualityStats {
    pub criterion: String,
    pub source: CriterionSource,
    /// `<kind>/<version>`.
    pub grader: String,
    pub task_type: String,
    /// The criterion definition's digest the levels were given under.
    pub digest: String,
    /// Runs whose latest grade of the grader has the criterion.
    pub graded_runs: usize,
    pub tally: Tally,
    /// Good over determinate levels (good, partial, poor).
    pub good: Rate,
    /// Poor over determinate levels.
    pub poor: Rate,
    /// The levels per executor group, only when the criterion's subject is
    /// a node and the graded runs bound it to more than one executor. A
    /// breakdown only: the comparable key and promotion never read it.
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<ExecutorQuality>>", optional))]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_executor: Vec<ExecutorQuality>,
}

/// One criterion's levels over the graded runs of one executor group.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExecutorQuality {
    /// The executor digest, shortened.
    pub executor: String,
    pub good: Rate,
    pub poor: Rate,
    pub graded_runs: usize,
}

/// How one criterion moved between the two versions of `--compare`
/// (`against` minus `base`), for a key both graded under the same digest.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QualityDelta {
    pub criterion: String,
    pub grader: String,
    pub task_type: String,
    /// The criterion definition's digest both versions graded under: a
    /// criterion whose definition changed has one delta per digest.
    pub digest: String,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good_delta: Option<f64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poor_delta: Option<f64>,
}

/// The quality rows of a version's runs; empty when none was graded.
pub(super) fn of_runs<'a>(runs: &[&'a StatsRun]) -> Vec<QualityStats> {
    let grades: Vec<Grade> = runs.iter().flat_map(|r| r.grades.iter().cloned()).collect();
    let mut levels: BTreeMap<ComparableKey, (CriterionSource, Vec<Graded<'a>>)> = BTreeMap::new();
    for g in latest_per_run(&grades).values() {
        for c in &g.criteria {
            let key = ComparableKey {
                criterion: c.id.clone(),
                digest: c.digest.clone(),
                grader: g.grader.to_string(),
                task_type: g.task_type.clone(),
            };
            let executor = c
                .subject
                .strip_prefix("node:")
                .and_then(|node| executor_of(runs, &g.run_id, node));
            levels
                .entry(key)
                .or_insert_with(|| (c.source, Vec::new()))
                .1
                .push(Graded {
                    level: c.level,
                    executor,
                });
        }
    }
    levels
        .into_iter()
        .map(|(key, (source, graded))| {
            let by_executor = by_executor(&graded);
            let levels: Vec<Level> = graded.iter().map(|g| g.level).collect();
            let tally = Tally::of_levels(&levels);
            let determinate = tally.determinate() as usize;
            QualityStats {
                graded_runs: levels.len(),
                good: Rate::new(tally.good as usize, determinate),
                poor: Rate::new(tally.poor as usize, determinate),
                tally,
                criterion: key.criterion,
                source,
                grader: key.grader,
                task_type: key.task_type,
                digest: key.digest,
                by_executor,
            }
        })
        .collect()
}

/// One run's level of a criterion, with the executor the run bound the
/// criterion's node to (`None` for a run criterion or an unknown binding).
struct Graded<'a> {
    level: Level,
    executor: Option<&'a str>,
}

/// The executor digest the run `run_id` bound `node` to, from its manifest.
fn executor_of<'a>(runs: &[&'a StatsRun], run_id: &str, node: &str) -> Option<&'a str> {
    runs.iter()
        .find(|r| r.run_id == run_id)
        .and_then(|r| r.node_profiles.get(node))
        .map(|d| d.executor.as_str())
        .filter(|e| !e.is_empty())
}

/// The levels per executor, only when every level names one and they name
/// more than one.
fn by_executor(graded: &[Graded<'_>]) -> Vec<ExecutorQuality> {
    let mut per: BTreeMap<&str, Vec<Level>> = BTreeMap::new();
    for g in graded {
        let Some(e) = g.executor else {
            return Vec::new();
        };
        per.entry(e).or_default().push(g.level);
    }
    if per.len() < 2 {
        return Vec::new();
    }
    per.into_iter()
        .map(|(executor, levels)| {
            let tally = Tally::of_levels(&levels);
            let determinate = tally.determinate() as usize;
            ExecutorQuality {
                executor: super::digests::short(executor),
                good: Rate::new(tally.good as usize, determinate),
                poor: Rate::new(tally.poor as usize, determinate),
                graded_runs: levels.len(),
            }
        })
        .collect()
}

/// The deltas of the keys `base` and `against` both have, digest included.
pub(super) fn deltas(base: &[QualityStats], against: &[QualityStats]) -> Vec<QualityDelta> {
    base.iter()
        .filter_map(|a| {
            let b = against.iter().find(|b| {
                (&b.criterion, &b.grader, &b.task_type, &b.digest)
                    == (&a.criterion, &a.grader, &a.task_type, &a.digest)
            })?;
            Some(QualityDelta {
                criterion: a.criterion.clone(),
                grader: a.grader.clone(),
                task_type: a.task_type.clone(),
                digest: a.digest.clone(),
                good_delta: delta(a.good.rate, b.good.rate),
                poor_delta: delta(a.poor.rate, b.poor.rate),
            })
        })
        .collect()
}

/// The text lines of a version's quality rows.
pub(super) fn lines(rows: &[QualityStats]) -> Vec<String> {
    rows.iter()
        .map(|q| {
            format!(
                "quality {} ({}, {}): good {}, poor {} over {} graded run{}",
                apb_core::text::visible(&q.criterion),
                q.grader,
                apb_core::text::visible(&q.task_type),
                q.good.text(),
                q.poor.text(),
                q.graded_runs,
                if q.graded_runs == 1 { "" } else { "s" }
            )
        })
        .collect()
}
