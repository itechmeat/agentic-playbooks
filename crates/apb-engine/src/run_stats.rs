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
//! deliverables and output fields; and the goal criteria results. Per node:
//! runs it ran in, first-pass executions, retries, fallbacks, re-entries,
//! duration against its declared `expected_duration`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::decision::report::stats::{median, rate};
use crate::decision::report::{RunJournal, run_dirs};
use crate::event::{Event, EventPayload};
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

/// One run as the statistics read it.
#[derive(Debug, Clone)]
pub struct StatsRun {
    pub run_id: String,
    pub playbook: String,
    pub version: String,
    pub events: Vec<Event>,
    /// The run's playbook snapshot, for `expected_duration`.
    pub snapshot: Option<apb_core::schema::Playbook>,
    /// The run was a candidate trial (issue #192, from its manifest).
    pub candidate_trial: bool,
    /// Each node's profile primary model, from the run manifest (issue
    /// #193).
    pub expected_models: BTreeMap<String, String>,
}

impl StatsRun {
    /// Reads one run directory; `None` when its journal does not read.
    pub fn load(run_dir: &Path) -> Option<Self> {
        let j = RunJournal::load(run_dir)?;
        let version = j
            .events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { version, .. } => Some(version.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Some(StatsRun {
            run_id: j.run_id,
            playbook: j.playbook,
            version,
            events: j.events,
            snapshot: crate::legacy_snapshot::load_run_playbook(run_dir),
            candidate_trial: crate::manifest::read(run_dir)
                .ok()
                .flatten()
                .is_some_and(|m| m.candidate_trial),
            expected_models: crate::attempt_models::primary_models(run_dir),
        })
    }

    fn started_ms(&self) -> Option<u128> {
        self.events.iter().find_map(|e| match &e.payload {
            EventPayload::RunStarted { .. } => Some(e.ts),
            _ => None,
        })
    }
}

/// `count` of `of`, and the share when `of` is not zero.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Rate {
    pub count: usize,
    pub of: usize,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
}

impl Rate {
    fn new(count: usize, of: usize) -> Self {
        Rate {
            count,
            of,
            rate: rate(count, of),
        }
    }

    /// `7/9 (78%)`, or `0/0`.
    pub fn text(&self) -> String {
        match self.rate {
            Some(r) => format!("{}/{} ({:.0}%)", self.count, self.of, r * 100.0),
            None => format!("{}/{}", self.count, self.of),
        }
    }
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

/// A distribution of millisecond durations: how many, and the median.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Waits {
    pub count: usize,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_ms: Option<u64>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
}

impl Waits {
    fn of(mut values: Vec<u64>) -> Self {
        Waits {
            count: values.len(),
            max_ms: values.iter().copied().max(),
            median_ms: median(&mut values),
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
    /// The models its attempts actually ran on (issue #193).
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub model_use: models::ModelUse,
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

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn delta(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    Some(round2(b? - a?))
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
        .filter_map(|d| StatsRun::load(d))
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
        .into_iter()
        .map(|((playbook, (_, version)), runs)| version_stats(&playbook, &version, &runs))
        .collect();
    let compare = match (&filter.playbook, &filter.compare) {
        (Some(p), Some(base)) => Some(comparison(p, base, &versions)),
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

fn ms(from: u128, to: u128) -> u64 {
    u64::try_from(to.saturating_sub(from)).unwrap_or(u64::MAX)
}

/// Pairs each `open` of a node with the next `close` of the same node, in
/// journal order, and returns the waits in milliseconds. A `withdraw` closes
/// the oldest open request of the node without a wait: the request was never
/// answered, and the next visit asks anew.
fn waits(
    events: &[Event],
    open: fn(&EventPayload) -> Option<&str>,
    close: fn(&EventPayload) -> Option<&str>,
    withdraw: fn(&EventPayload) -> Option<&str>,
) -> Vec<u64> {
    let mut pending: BTreeMap<&str, Vec<u128>> = BTreeMap::new();
    let mut out = Vec::new();
    for e in events {
        if let Some(n) = open(&e.payload) {
            pending.entry(n).or_default().push(e.ts);
        } else if let Some(n) = close(&e.payload)
            && let Some(q) = pending.get_mut(n)
            && !q.is_empty()
        {
            let t = q.remove(0);
            out.push(ms(t, e.ts));
        } else if let Some(n) = withdraw(&e.payload)
            && let Some(q) = pending.get_mut(n)
            && !q.is_empty()
        {
            q.remove(0);
        }
    }
    out
}

fn run_facts(
    run: &StatsRun,
    nodes: &mut BTreeMap<String, NodeAcc>,
    goal: &mut BTreeMap<usize, GoalAcc>,
) -> RunFacts {
    let ev = &run.events;
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
                        .push(ms(t, e.ts));
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
                f.duration = start.map(|s| ms(s, e.ts));
            }
            EventPayload::RunAborted { .. } => {
                f.outcome = Some("aborted");
                f.duration = start.map(|s| ms(s, e.ts));
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
    f.gate_waits = waits(
        ev,
        |p| match p {
            EventPayload::ReviewRequested { node, .. } => Some(node),
            _ => None,
        },
        |p| match p {
            EventPayload::ReviewDecided { node, .. } => Some(node),
            _ => None,
        },
        |p| match p {
            EventPayload::ReviewWithdrawn { node, .. } => Some(node),
            _ => None,
        },
    );
    f.question_waits = waits(
        ev,
        |p| match p {
            EventPayload::QuestionAsked { node, .. } => Some(node),
            _ => None,
        },
        |p| match p {
            EventPayload::QuestionAnswered { node, .. } => Some(node),
            _ => None,
        },
        |_| None,
    );
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
    for run in runs {
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
        if let Some(u) = RunUsage::from_events(&run.events) {
            spend.runs_with_usage += 1;
            tokens = tokens.saturating_add(u.input_tokens.saturating_add(u.output_tokens));
            if let Some(c) = u.cost_usd {
                spend.runs_with_cost += 1;
                cost += c;
            }
        }
    }
    let finished = outcomes.succeeded + outcomes.failed + outcomes.aborted;
    spend.tokens = PerRun::new(tokens, spend.runs_with_usage);
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
        model_use,
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

/// `12 s`, `3.4 min`, `1.2 h`.
fn dur(ms: u64) -> String {
    let s = ms as f64 / 1000.0;
    if s < 60.0 {
        format!("{s:.0} s")
    } else if s < 3600.0 {
        format!("{:.1} min", s / 60.0)
    } else {
        format!("{:.1} h", s / 3600.0)
    }
}

fn waits_text(w: &Waits) -> String {
    match w.median_ms {
        Some(m) => format!("median {} over {}", dur(m), w.count),
        None => "none".to_string(),
    }
}

fn per_run_text(p: &PerRun) -> String {
    match p.per_run {
        Some(x) => format!("{x:.2} per run ({} over {} runs)", p.total, p.runs),
        None => "0".to_string(),
    }
}

/// The report as text for a terminal.
pub fn render_text(r: &StatsReport) -> String {
    let mut out = String::new();
    if let Some(note) = &r.note {
        out.push_str(note);
        out.push('\n');
        return out;
    }
    out.push_str(&format!("{} runs\n", r.runs));
    for v in &r.versions {
        out.push_str(&format!(
            "\n{} {}: {} runs\n",
            v.playbook, v.version, v.runs
        ));
        if let Some(note) = &v.note {
            out.push_str(&format!("  note: {note}\n"));
        }
        if v.candidate_trials > 0 {
            out.push_str(&format!(
                "  candidate trials: {} of {} runs\n",
                v.candidate_trials, v.runs
            ));
        }
        let o = &v.outcomes;
        out.push_str(&format!(
            "  outcome: {} succeeded ({} succeeded, {} failed, {} aborted, {} other)\n",
            v.success.text(),
            o.succeeded,
            o.failed,
            o.aborted,
            o.other
        ));
        out.push_str(&format!("  first pass: {}\n", v.first_pass.text()));
        out.push_str(&format!("  retries: {}\n", per_run_text(&v.retries)));
        out.push_str(&format!("  fallbacks: {}\n", per_run_text(&v.fallbacks)));
        out.push_str(&format!(
            "  loop traversals: {}\n",
            per_run_text(&v.loop_traversals)
        ));
        out.push_str(&format!("  gate wait: {}\n", waits_text(&v.gate_wait)));
        out.push_str(&format!(
            "  question wait: {}\n",
            waits_text(&v.question_wait)
        ));
        out.push_str(&format!("  run duration: {}\n", waits_text(&v.duration)));
        if let Some(m) = v.model_use.text() {
            out.push_str(&format!("  models: {m}\n"));
        }
        let s = &v.spend;
        if s.runs_with_usage > 0 {
            let mut line = format!("  tokens: {}", per_run_text(&s.tokens));
            if let Some(c) = s.cost_per_run_usd {
                line.push_str(&format!(
                    ", ${c:.4} per run reported by {} of {} runs",
                    s.runs_with_cost, v.runs
                ));
            }
            out.push_str(&line);
            out.push('\n');
        }
        if v.deliverable_missing + v.output_fields_missing > 0 {
            out.push_str(&format!(
                "  missing: {} deliverables, {} output field sets\n",
                v.deliverable_missing, v.output_fields_missing
            ));
        }
        for g in &v.goal {
            let result = if g.check == "manual" {
                format!("manual in {} runs", g.manual)
            } else {
                let mut t = format!("passed {}", g.passed.text());
                if g.errors > 0 {
                    t.push_str(&format!(", {} could not run", g.errors));
                }
                t
            };
            out.push_str(&format!(
                "  goal {}: {} ({}): {result}\n",
                g.index + 1,
                g.description,
                g.check
            ));
        }
        for n in &v.nodes {
            let mut line = format!(
                "  node {}: {} runs, first pass {}",
                n.node,
                n.runs,
                n.first_pass.text()
            );
            if n.retries + n.fallbacks + n.reentries > 0 {
                line.push_str(&format!(
                    ", {} retries, {} fallbacks, {} re-entries",
                    n.retries, n.fallbacks, n.reentries
                ));
            }
            if let Some(m) = n.duration.median_ms {
                line.push_str(&format!(", median {}", dur(m)));
            }
            if let (Some(e), Some(over)) = (n.expected_s, &n.over_expected) {
                line.push_str(&format!(
                    ", over the expected {} in {}",
                    dur(e * 1000),
                    over.text()
                ));
            }
            if let Some(m) = n.model_use.text() {
                line.push_str(&format!(", models {m}"));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if let Some(c) = &r.compare {
        out.push_str(&format!("\ncompare {} {}", c.playbook, c.base));
        let base_ran = r
            .versions
            .iter()
            .any(|v| v.playbook == c.playbook && v.version == c.base);
        match &c.against {
            _ if !base_ran => out.push_str(&format!(": no runs of {}\n", c.base)),
            None => out.push_str(": no other version has runs\n"),
            Some(a) => {
                out.push_str(&format!(" -> {a}\n"));
                let pct = |d: Option<f64>| {
                    d.map_or("n/a".to_string(), |x| format!("{:+.0} points", x * 100.0))
                };
                let num = |d: Option<f64>| d.map_or("n/a".to_string(), |x| format!("{x:+.2}"));
                out.push_str(&format!("  success: {}\n", pct(c.success_delta)));
                out.push_str(&format!("  first pass: {}\n", pct(c.first_pass_delta)));
                out.push_str(&format!(
                    "  retries per run: {}\n",
                    num(c.retries_per_run_delta)
                ));
                out.push_str(&format!(
                    "  loop traversals per run: {}\n",
                    num(c.loops_per_run_delta)
                ));
                out.push_str(&format!(
                    "  median run duration: {}\n",
                    c.median_duration_delta_ms
                        .map_or("n/a".to_string(), |d| format!("{d:+} ms"))
                ));
                out.push_str(&format!(
                    "  cost per run: {}\n",
                    c.cost_per_run_delta_usd
                        .map_or("n/a".to_string(), |d| format!("{d:+.4} USD"))
                ));
            }
        }
    }
    out
}

pub mod models;

#[cfg(test)]
mod tests;
