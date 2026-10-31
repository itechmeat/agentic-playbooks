//! `apb stats` (C3): cross-run workflow metrics per playbook version and per
//! node, computed from run journals only.
//!
//! Model-free and read-only, like `apb decisions report`, whose journal walk
//! it reuses ([`crate::decision::report::run_dirs`], [`RunJournal`]): runs
//! apb did not create on this machine are left out. Every rate is reported
//! with its count (`7/9`), because small samples mislead; a version with
//! fewer than [`MIN_RUNS`] runs carries a note saying so.
//!
//! Figures per version: run outcomes; first-pass runs (succeeded with no
//! retry, no fallback, no loop traversal and every node on attempt 1); retries, fallbacks and loop
//! traversals (bounded edges back into a node that ran) per run; human gate wait (`review_requested` to the matching
//! `review_decided`) and question wait (`question_asked` to
//! `question_answered`); run duration; tokens and cost per run; missing
//! deliverables and output fields; the goal criteria results; the findings
//! and warnings the attempts reported, and each baseline observation's
//! results ([`findings`]); the levels the runs' signed grades gave each
//! criterion ([`quality`]). Per node:
//! runs it ran in, first-pass executions, retries, fallbacks, re-entries,
//! duration against its declared `expected_duration`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::decision::report::run_dirs;
use crate::event::EventPayload;
use crate::run_estimate::sample::RunSample;
use crate::run_view::RunUsage;

/// Below this many runs a version's rates are flagged as indicative only.
pub const MIN_RUNS: usize = 10;
/// Printed when no run matches.
pub const NO_RUNS: &str = "no runs recorded";

/// What to include.
#[derive(Debug, Clone, Default)]
pub struct StatsFilter {
    pub playbook: Option<String>,
    /// Epoch milliseconds: runs started at or after it.
    pub since_ms: Option<u128>,
    /// Compare this version with the latest other version seen (needs
    /// `playbook`).
    pub compare: Option<String>,
}

/// A total over runs and its mean per run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct PerRun {
    pub total: u64,
    pub runs: usize,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_run: Option<f64>,
}

impl PerRun {
    fn new(total: u64, runs: usize) -> Self {
        PerRun {
            total,
            runs,
            per_run: (runs > 0).then(|| round2(total as f64 / runs as f64)),
        }
    }
}

/// How the runs ended.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Outcomes {
    pub succeeded: usize,
    pub failed: usize,
    pub aborted: usize,
    /// Still running, paused, or ended without a terminal event.
    pub other: usize,
}

/// Token usage and reported cost over the runs that reported usage.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Spend {
    /// Runs whose attempts reported usage.
    pub runs_with_usage: usize,
    /// Input plus output tokens (cache reads and writes not included).
    pub tokens: PerRun,
    /// Runs that reported a cost.
    pub runs_with_cost: usize,
    /// Finished attempts that reported a cost, over every finished attempt:
    /// how much of the spend the cost figures cover.
    pub cost_known: Rate,
    pub cost_usd: f64,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_run_usd: Option<f64>,
}

/// One goal criterion over the runs that checked it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GoalStats {
    pub index: usize,
    pub description: String,
    /// `script`, `marker` or `manual`.
    pub check: String,
    /// Runs that checked it.
    pub checked: usize,
    pub passed: Rate,
    pub failed: usize,
    /// `error`: the check could not run.
    pub errors: usize,
    pub manual: usize,
}

/// One node over the runs it ran in.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NodeStats {
    pub node: String,
    /// Runs the node started in.
    pub runs: usize,
    /// Runs where the node's first result was a success on attempt 1, with
    /// no retry, fallback or later re-entry.
    pub first_pass: Rate,
    pub retries: usize,
    pub fallbacks: usize,
    /// Starts after the first in the same run (a loop back into it).
    pub reentries: usize,
    pub duration: Waits,
    /// The declared `expected_duration`, in seconds.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_s: Option<u64>,
    /// Finished executions that took longer than `expected_s`.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_expected: Option<Rate>,
    pub deliverable_missing: usize,
    pub output_fields_missing: usize,
    /// The models its attempts actually ran on (issue #193).
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub model_use: models::ModelUse,
    /// Its figures per executor digest, when the runs of the version bound
    /// it to more than one ([`digests`]).
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<digests::DigestStats>>", optional))]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub digests: Vec<digests::DigestStats>,
}

/// One playbook version over its runs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VersionStats {
    pub playbook: String,
    pub version: String,
    pub runs: usize,
    pub outcomes: Outcomes,
    /// Succeeded runs over finished runs (succeeded, failed, aborted).
    pub success: Rate,
    /// Succeeded runs with no retry, fallback or loop traversal, over
    /// finished runs.
    pub first_pass: Rate,
    pub retries: PerRun,
    pub fallbacks: PerRun,
    pub loop_traversals: PerRun,
    pub gate_wait: Waits,
    pub question_wait: Waits,
    /// Run start to its terminal event.
    pub duration: Waits,
    pub spend: Spend,
    pub deliverable_missing: usize,
    pub output_fields_missing: usize,
    /// Empty when the runs checked no goal.
    pub goal: Vec<GoalStats>,
    /// The findings and warnings the runs' attempts reported.
    pub findings: findings::FindingStats,
    /// Each baseline observation over the runs that compared it; empty
    /// when the runs had no baseline check.
    pub baseline: Vec<findings::BaselineStats>,
    /// The models its attempts actually ran on (issue #193).
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub model_use: models::ModelUse,
    /// The levels its runs' signed grades gave each criterion, one row per
    /// comparable key ([`quality`]); empty when no run was graded.
    #[cfg_attr(
        feature = "ts",
        ts(as = "Option<Vec<quality::QualityStats>>", optional)
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quality: Vec<quality::QualityStats>,
    pub nodes: Vec<NodeStats>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Runs of this version that were candidate trials (issue #192): the
    /// version was a forward patch on trial when they ran.
    #[cfg_attr(feature = "ts", ts(as = "Option<usize>", optional))]
    #[serde(default, skip_serializing_if = "is_zero")]
    pub candidate_trials: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `--compare`: the base version against the latest other version seen,
/// with the difference (`against` minus `base`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Comparison {
    pub playbook: String,
    pub base: String,
    /// `None` when no other version of the playbook has runs.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub against: Option<String>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_delta: Option<f64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_pass_delta: Option<f64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries_per_run_delta: Option<f64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loops_per_run_delta: Option<f64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_duration_delta_ms: Option<i64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_run_delta_usd: Option<f64>,
    /// Nodes whose latest executor (profile or bundle digest) differs
    /// between the two versions.
    #[cfg_attr(
        feature = "ts",
        ts(as = "Option<Vec<digests::ExecutorChange>>", optional)
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub executor_changes: Vec<digests::ExecutorChange>,
    /// How each criterion both versions graded under the same definition
    /// moved.
    #[cfg_attr(
        feature = "ts",
        ts(as = "Option<Vec<quality::QualityDelta>>", optional)
    )]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quality: Vec<quality::QualityDelta>,
}

/// The whole report.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StatsReport {
    pub runs: usize,
    /// By playbook id, then version (oldest first).
    pub versions: Vec<VersionStats>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare: Option<Comparison>,
    /// [`NO_RUNS`] when no run matched.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A semver-ish sort key: numeric components first, the text as tiebreak.
type VersionKey = (Vec<u64>, String);

fn version_key(v: &str) -> VersionKey {
    (
        v.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect(),
        v.to_string(),
    )
}

/// Reads every run under `roots` that apb created here and builds the report.
pub fn stats(roots: &[PathBuf], filter: &StatsFilter) -> StatsReport {
    let runs: Vec<StatsRun> = run_dirs(roots)
        .iter()
        .filter_map(|d| StatsRun::load(d).map(|r| r.with_grades(d)))
        .collect();
    build(&runs, filter)
}

/// Builds the report over already-read runs.
pub fn build(runs: &[StatsRun], filter: &StatsFilter) -> StatsReport {
    let kept: Vec<&StatsRun> = runs
        .iter()
        .filter(|r| !r.playbook.is_empty())
        .filter(|r| filter.playbook.as_ref().is_none_or(|p| &r.playbook == p))
        .filter(|r| {
            filter
                .since_ms
                .is_none_or(|s| r.started_ms().is_some_and(|t| t >= s))
        })
        .collect();
    let mut groups: BTreeMap<(String, VersionKey), Vec<&StatsRun>> = BTreeMap::new();
    for r in &kept {
        groups
            .entry((r.playbook.clone(), version_key(&r.version)))
            .or_default()
            .push(r);
    }
    let versions: Vec<VersionStats> = groups
        .iter()
        .map(|((playbook, (_, version)), runs)| version_stats(playbook, version, runs))
        .collect();
    let compare = match (&filter.playbook, &filter.compare) {
        (Some(p), Some(base)) => {
            let mut c = comparison(p, base, &versions);
            let of = |v: &str| {
                groups
                    .get(&(p.clone(), version_key(v)))
                    .map(Vec::as_slice)
                    .unwrap_or_default()
            };
            if let Some(against) = &c.against {
                c.executor_changes = digests::executor_changes(of(base), of(against));
            }
            Some(c)
        }
        _ => None,
    };
    StatsReport {
        runs: kept.len(),
        note: kept.is_empty().then(|| NO_RUNS.to_string()),
        versions,
        compare,
    }
}

fn comparison(playbook: &str, base: &str, versions: &[VersionStats]) -> Comparison {
    let of = |v: &str| {
        versions
            .iter()
            .find(|s| s.playbook == playbook && s.version == v)
    };
    // The latest other version seen (versions are sorted oldest first).
    let against = versions
        .iter()
        .rev()
        .find(|s| s.playbook == playbook && s.version != base)
        .map(|s| s.version.clone());
    let mut c = Comparison {
        playbook: playbook.to_string(),
        base: base.to_string(),
        against: against.clone(),
        ..Default::default()
    };
    if let (Some(a), Some(b)) = (of(base), against.as_deref().and_then(of)) {
        c.success_delta = delta(a.success.rate, b.success.rate);
        c.first_pass_delta = delta(a.first_pass.rate, b.first_pass.rate);
        c.retries_per_run_delta = delta(a.retries.per_run, b.retries.per_run);
        c.loops_per_run_delta = delta(a.loop_traversals.per_run, b.loop_traversals.per_run);
        c.median_duration_delta_ms = match (a.duration.median_ms, b.duration.median_ms) {
            (Some(x), Some(y)) => Some(y as i64 - x as i64),
            _ => None,
        };
        c.cost_per_run_delta_usd = delta(a.spend.cost_per_run_usd, b.spend.cost_per_run_usd)
            .map(|d| (d * 1e4).round() / 1e4);
        c.quality = quality::deltas(&a.quality, &b.quality);
    }
    c
}

/// What one run contributes.
#[derive(Default)]
struct RunFacts {
    outcome: Option<&'static str>,
    retries: u64,
    fallbacks: u64,
    loops: u64,
    /// A node visit whose result came from a later attempt than the first
    /// (an infrastructure retry journals no `retry_started`).
    later_attempt: bool,
    gate_waits: Vec<u64>,
    question_waits: Vec<u64>,
    duration: Option<u64>,
    deliverable_missing: usize,
    output_fields_missing: usize,
}

#[derive(Default)]
struct NodeAcc {
    runs: usize,
    first_pass: usize,
    retries: usize,
    fallbacks: usize,
    reentries: usize,
    durations: Vec<u64>,
    deliverable_missing: usize,
    output_fields_missing: usize,
}

#[derive(Default)]
struct GoalAcc {
    description: String,
    check: String,
    checked: usize,
    passed: usize,
    failed: usize,
    errors: usize,
    manual: usize,
}

fn run_facts(
    run: &StatsRun,
    nodes: &mut BTreeMap<String, NodeAcc>,
    goal: &mut BTreeMap<usize, GoalAcc>,
) -> RunFacts {
    let ev = &run.events;
    // Paused time and the spans the run sat ended before a resume are not
    // run time: the same clock as the run listing and the retro.
    let paused = crate::pause::inactive_intervals(ev, ev.last().map(|e| e.ts).unwrap_or_default());
    let mut f = RunFacts::default();
    let start = run.started_ms();
    // Per-node facts of this run.
    let mut started: BTreeMap<&str, usize> = BTreeMap::new();
    let mut open_at: BTreeMap<&str, u128> = BTreeMap::new();
    let mut first_result: BTreeMap<&str, (bool, u32)> = BTreeMap::new();
    let mut retried: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fell_back: BTreeMap<&str, usize> = BTreeMap::new();
    for e in ev {
        match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                *started.entry(node).or_default() += 1;
                open_at.insert(node, e.ts);
            }
            EventPayload::NodeFinished {
                node,
                status,
                attempt,
                ..
            } => {
                first_result
                    .entry(node)
                    .or_insert((status == "succeeded", *attempt));
                if *attempt > 1 {
                    f.later_attempt = true;
                }
                if let Some(t) = open_at.remove(node.as_str()) {
                    nodes
                        .entry(node.clone())
                        .or_default()
                        .durations
                        .push(active_ms(&paused, t, e.ts));
                }
            }
            EventPayload::RetryStarted { node, .. } => {
                f.retries += 1;
                *retried.entry(node).or_default() += 1;
            }
            // A host-mode execution fallback (a step whose CLIs could not
            // start, handed to the host) is a fallback like a profile one.
            EventPayload::FallbackTriggered { node, .. }
            | EventPayload::ExecutionFallback { node, .. } => {
                f.fallbacks += 1;
                *fell_back.entry(node).or_default() += 1;
            }
            // A loop traversal: a hop over a bounded edge (the one kind that
            // spends a `max_traversals` budget) back into a node that already
            // ran. Policy routes and unbounded edges do not count.
            EventPayload::EdgeTraversed {
                to,
                via_policy: false,
                uncounted: false,
                ..
            } if started.contains_key(to.as_str()) => {
                f.loops += 1;
            }
            EventPayload::DeliverableMissing { node, .. } => {
                f.deliverable_missing += 1;
                nodes.entry(node.clone()).or_default().deliverable_missing += 1;
            }
            EventPayload::OutputFieldsMissing { node, .. } => {
                f.output_fields_missing += 1;
                nodes.entry(node.clone()).or_default().output_fields_missing += 1;
            }
            EventPayload::GoalChecked {
                index,
                description,
                check,
                status,
                ..
            } => {
                let g = goal.entry(*index).or_default();
                g.description.clone_from(description);
                g.check.clone_from(check);
                g.checked += 1;
                match status.as_str() {
                    "passed" => g.passed += 1,
                    "failed" => g.failed += 1,
                    "manual" => g.manual += 1,
                    _ => g.errors += 1,
                }
            }
            EventPayload::RunFinished { outcome } => {
                f.outcome = Some(match outcome.as_str() {
                    "succeeded" => "succeeded",
                    "aborted" => "aborted",
                    _ => "failed",
                });
                f.duration = start.map(|s| active_ms(&paused, s, e.ts));
            }
            EventPayload::RunAborted { .. } => {
                f.outcome = Some("aborted");
                f.duration = start.map(|s| active_ms(&paused, s, e.ts));
            }
            _ => {}
        }
    }
    for (node, n) in &started {
        let acc = nodes.entry((*node).to_string()).or_default();
        acc.runs += 1;
        acc.reentries += n - 1;
        let r = retried.get(node).copied().unwrap_or(0);
        let fb = fell_back.get(node).copied().unwrap_or(0);
        acc.retries += r;
        acc.fallbacks += fb;
        if *n == 1 && r == 0 && fb == 0 && first_result.get(node) == Some(&(true, 1)) {
            acc.first_pass += 1;
        }
    }
    (f.gate_waits, f.question_waits) = spans::human_waits(ev);
    f
}

fn version_stats(playbook: &str, version: &str, runs: &[&StatsRun]) -> VersionStats {
    let mut nodes: BTreeMap<String, NodeAcc> = BTreeMap::new();
    let mut goal: BTreeMap<usize, GoalAcc> = BTreeMap::new();
    let mut outcomes = Outcomes::default();
    let (mut retries, mut fallbacks, mut loops) = (0u64, 0u64, 0u64);
    let mut first_pass = 0usize;
    let (mut gate, mut question, mut durations) = (Vec::new(), Vec::new(), Vec::new());
    let (mut dm, mut ofm) = (0usize, 0usize);
    let mut spend = Spend::default();
    let (mut tokens, mut cost) = (0u64, 0f64);
    let (mut cost_attempts, mut finished_attempts) = (0usize, 0usize);
    let mut notes = findings::Acc::default();
    for run in runs {
        notes.add(
            &run.run_id,
            run.started_ms().unwrap_or(0),
            run.snapshot.as_ref(),
            &run.events,
        );
        let f = run_facts(run, &mut nodes, &mut goal);
        match f.outcome {
            Some("succeeded") => outcomes.succeeded += 1,
            Some("failed") => outcomes.failed += 1,
            Some("aborted") => outcomes.aborted += 1,
            _ => outcomes.other += 1,
        }
        if f.outcome == Some("succeeded")
            && f.retries == 0
            && f.fallbacks == 0
            && f.loops == 0
            && !f.later_attempt
        {
            first_pass += 1;
        }
        retries += f.retries;
        fallbacks += f.fallbacks;
        loops += f.loops;
        gate.extend(f.gate_waits);
        question.extend(f.question_waits);
        durations.extend(f.duration);
        dm += f.deliverable_missing;
        ofm += f.output_fields_missing;
        let u = RunUsage::totals(&run.events);
        cost_attempts += u.cost_attempts as usize;
        finished_attempts += u.finished_attempts as usize;
        if u.attempts > 0 {
            spend.runs_with_usage += 1;
            tokens = tokens.saturating_add(u.billable_tokens());
            if let Some(c) = u.cost_usd {
                spend.runs_with_cost += 1;
                cost += c;
            }
        }
    }
    let finished = outcomes.succeeded + outcomes.failed + outcomes.aborted;
    spend.tokens = PerRun::new(tokens, spend.runs_with_usage);
    spend.cost_known = Rate::new(cost_attempts, finished_attempts);
    let (findings, baseline) = notes.finish();
    // `+ 0.0`: an empty float sum is -0.0.
    spend.cost_usd = (cost * 1e8).round() / 1e8 + 0.0;
    spend.cost_per_run_usd = (spend.runs_with_cost > 0)
        .then(|| ((cost / spend.runs_with_cost as f64) * 1e6).round() / 1e6);
    // The expected durations from the newest snapshot of the version.
    let snapshot = runs.iter().rev().find_map(|r| r.snapshot.as_ref());
    let (model_use, mut node_models) = models::model_use(
        runs.iter()
            .map(|r| (r.events.as_slice(), &r.expected_models)),
    );
    // Samples only when some node changed its executor within the version.
    let samples: BTreeMap<String, RunSample> = if digests::any_change(runs) {
        runs.iter()
            .map(|r| (r.run_id.clone(), RunSample::of(&r.run_id, &r.events, None)))
            .collect()
    } else {
        BTreeMap::new()
    };
    let nodes: Vec<NodeStats> = nodes
        .into_iter()
        .map(|(node, a)| {
            let expected_s = snapshot
                .and_then(|p| p.node(&node))
                .and_then(|n| n.expected_duration.as_ref())
                .and_then(|d| d.parsed());
            let over_expected = expected_s.map(|s| {
                let limit = s.saturating_mul(1000);
                Rate::new(
                    a.durations.iter().filter(|d| **d > limit).count(),
                    a.durations.len(),
                )
            });
            NodeStats {
                first_pass: Rate::new(a.first_pass, a.runs),
                runs: a.runs,
                retries: a.retries,
                fallbacks: a.fallbacks,
                reentries: a.reentries,
                duration: Waits::of(a.durations),
                expected_s,
                over_expected,
                deliverable_missing: a.deliverable_missing,
                output_fields_missing: a.output_fields_missing,
                model_use: node_models.remove(&node).unwrap_or_default(),
                digests: digests::node_digests(&node, runs, &samples),
                node,
            }
        })
        .collect();
    let goal = goal
        .into_iter()
        .map(|(index, g)| GoalStats {
            index,
            description: g.description,
            check: g.check,
            checked: g.checked,
            // Over the runs that checked it automatically (manual left out).
            passed: Rate::new(g.passed, g.checked - g.manual),
            failed: g.failed,
            errors: g.errors,
            manual: g.manual,
        })
        .collect();
    VersionStats {
        candidate_trials: runs.iter().filter(|r| r.candidate_trial).count(),
        playbook: playbook.to_string(),
        version: version.to_string(),
        runs: runs.len(),
        success: Rate::new(outcomes.succeeded, finished),
        first_pass: Rate::new(first_pass, finished),
        outcomes,
        retries: PerRun::new(retries, runs.len()),
        fallbacks: PerRun::new(fallbacks, runs.len()),
        loop_traversals: PerRun::new(loops, runs.len()),
        gate_wait: Waits::of(gate),
        question_wait: Waits::of(question),
        duration: Waits::of(durations),
        spend,
        deliverable_missing: dm,
        output_fields_missing: ofm,
        goal,
        findings,
        baseline,
        model_use,
        quality: quality::of_runs(runs),
        nodes,
        note: (runs.len() < MIN_RUNS).then(|| {
            format!(
                "{} run{}: fewer than {MIN_RUNS}, the rates are indicative only",
                runs.len(),
                if runs.len() == 1 { "" } else { "s" }
            )
        }),
    }
}

pub mod digests;
pub mod findings;
pub mod models;
pub mod quality;
mod rate;
mod render;
pub(crate) mod spans;
mod stats_run;
mod waits;
pub use quality::{QualityDelta, QualityStats};
pub use rate::Rate;
use rate::{delta, round2};
pub use render::render_text;
use spans::active_ms;
pub use stats_run::StatsRun;
pub use waits::Waits;

#[cfg(test)]
mod tests;
