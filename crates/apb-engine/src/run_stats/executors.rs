//! The executor report of `apb stats` (U21, U36): per node of a version and
//! per executor group (profile, bundle and executor digest), the figures of
//! each chain step, the transitions between steps with their reasons and,
//! for host groups, how often the host followed the profile's hint.
//!
//! Attempts without a recorded chain position form one step row with
//! `step: None` ("position not recorded"); a transition without a recorded
//! reason reads `not_recorded` and is never reclassified.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::digests::short;
use super::rate::Rate;
use super::stats_run::StatsRun;
use crate::decision::report::stats::median;
use crate::event::{Event, EventPayload};
use crate::execution_identity::StepSource;
use crate::execution_identity::fold::{
    ExecutionIdentity, ExecutionIdentityView, ReportedModel, same_model,
};
use crate::run_view::CostByBasis;

/// The reason a transition shows when its journal recorded none.
pub const NOT_RECORDED: &str = "not_recorded";

/// One node's figures over the runs that bound it to one executor.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ExecutorGroup {
    /// The profile digest, shortened.
    pub profile: String,
    /// The bundle digest, shortened.
    pub bundle: String,
    /// The executor digest, shortened.
    pub executor: String,
    /// The group is a run-local ephemeral override.
    pub ephemeral: bool,
    pub runs: usize,
    pub attempts: usize,
    pub steps: Vec<StepStats>,
    pub transitions: Vec<TransitionStats>,
    /// Host groups only.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint_follow: Option<HintFollow>,
}

/// One chain step of a group. A declared step never reached in the window
/// is listed with `attempts: 0`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StepStats {
    /// The binding snapshot's digest, shortened; `None` when not recorded.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    /// The step index in its binding's chain; `None`: position not
    /// recorded.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    /// `primary`, `fallback`, `tier`, `host_fallback` or `ephemeral`.
    pub source: String,
    /// `cli`, `host` or `scenario`.
    pub executed_by: String,
    pub attempts: usize,
    pub successes: Rate,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_attempt_ms: Option<u64>,
    /// The reported cost of the step's finished attempts, summed like the
    /// version's spend.
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Finished attempts that reported a cost, over every finished one.
    pub cost_known: Rate,
    /// `cost_usd` split by basis: billed, a subscription equivalent nobody
    /// is billed, or of unknown basis.
    #[cfg_attr(
        feature = "ts",
        ts(as = "Option<crate::run_view::CostByBasis>", optional)
    )]
    #[serde(skip_serializing_if = "CostByBasis::is_empty")]
    pub cost_by_basis: CostByBasis,
    /// Attempts by reported model: `one:<model>`, `mixed`, `partial`,
    /// `none`.
    pub reported_models: BTreeMap<String, usize>,
    /// Closes of the step by reason (the close reason's label).
    pub closes_by_reason: BTreeMap<String, usize>,
}

/// Transitions from one step to another, by reason.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TransitionStats {
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_step: Option<u32>,
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_step: Option<u32>,
    /// The close reason's label, or [`NOT_RECORDED`].
    pub reason: String,
    pub count: usize,
}

/// Of the host attempts whose hint was set, how many reported a model that
/// names the hint, another one, or none.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct HintFollow {
    pub hinted: usize,
    pub followed: usize,
    pub not_followed: usize,
    pub not_reported: usize,
}

/// The source word of a recorded step.
fn source_word(s: &StepSource) -> &'static str {
    match s {
        StepSource::Primary => "primary",
        StepSource::Fallback { .. } => "fallback",
        StepSource::Tier { .. } => "tier",
        StepSource::HostFallback => "host_fallback",
        StepSource::Ephemeral => "ephemeral",
    }
}

/// The key of a reported model in [`StepStats::reported_models`].
fn reported_key(r: &ReportedModel) -> String {
    match r {
        ReportedModel::One { model } => format!("one:{model}"),
        ReportedModel::Mixed { .. } => "mixed".into(),
        ReportedModel::Partial { .. } => "partial".into(),
        ReportedModel::None => "none".into(),
    }
}

/// An executor group's key: the full profile, bundle and executor digests
/// of the binding an attempt ran on, [`NOT_RECORDED`] when unknown.
type GroupKey = (String, String, String);

fn group_key(id: &ExecutionIdentity) -> GroupKey {
    let d = |x: &Option<String>| x.clone().unwrap_or_else(|| NOT_RECORDED.to_string());
    (
        d(&id.profile_digest),
        d(&id.bundle_digest),
        d(&id.executor_digest),
    )
}

/// A step row's key: the binding, the position, the source, who ran it.
type StepKey = (Option<String>, Option<u32>, String, String);

fn step_key(id: &ExecutionIdentity) -> StepKey {
    match &id.step {
        Some(s) => (
            Some(short(&s.binding)),
            Some(s.step),
            source_word(&s.source).to_string(),
            s.executed_by.clone(),
        ),
        None => (
            None,
            None,
            NOT_RECORDED.to_string(),
            id.executed_by().to_string(),
        ),
    }
}

/// How an attempt ended, from its `attempt_finished`.
#[derive(Default, Clone, Copy)]
struct Finish {
    succeeded: bool,
    ms: Option<u64>,
    cost: Option<f64>,
    basis: apb_core::agent_output::CostBasis,
}

#[derive(Default)]
struct StepAcc {
    attempts: usize,
    finished: usize,
    succeeded: usize,
    ms: Vec<u64>,
    cost: f64,
    cost_attempts: usize,
    cost_by_basis: CostByBasis,
    reported: BTreeMap<String, usize>,
    closes: BTreeMap<String, usize>,
}

#[derive(Default)]
struct GroupAcc {
    ephemeral: bool,
    runs: BTreeSet<String>,
    attempts: usize,
    steps: BTreeMap<StepKey, StepAcc>,
    transitions: BTreeMap<(Option<u32>, Option<u32>, String), usize>,
    hint: Option<HintFollow>,
}

impl GroupAcc {
    /// Counts the close of the step `closed` belongs to under `reason`.
    fn close(&mut self, closed: &ExecutionIdentity, reason: &str) {
        let row = self.steps.entry(step_key(closed)).or_default();
        *row.closes.entry(reason.to_string()).or_default() += 1;
    }

    fn transition(&mut self, from: Option<u32>, to: Option<u32>, reason: &str) {
        *self
            .transitions
            .entry((from, to, reason.to_string()))
            .or_default() += 1;
    }

    fn attempt(&mut self, id: &ExecutionIdentity, finish: Option<Finish>) {
        self.attempts += 1;
        let row = self.steps.entry(step_key(id)).or_default();
        row.attempts += 1;
        *row.reported
            .entry(reported_key(&id.reported_model()))
            .or_default() += 1;
        if let Some(f) = finish {
            row.finished += 1;
            row.succeeded += usize::from(f.succeeded);
            row.ms.extend(f.ms);
            if let Some(c) = f.cost {
                row.cost += c;
                row.cost_attempts += 1;
                row.cost_by_basis.add(f.basis, c);
            }
        }
        if id.is_host() {
            hint_follow(self.hint.get_or_insert_with(HintFollow::default), id);
        }
    }
}

/// Adds a hinted host attempt to `h` ([`hint_outcome`]).
fn hint_follow(h: &mut HintFollow, id: &ExecutionIdentity) {
    if let Some(hint) = id.model_requested_by_apb.as_deref() {
        h.add(hint_outcome(hint, &id.reported_model()));
    }
}

/// What a host did with a hint, for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintOutcome {
    Followed,
    NotFollowed,
    NotReported,
}

impl HintOutcome {
    /// The words a surface shows.
    pub fn as_str(self) -> &'static str {
        match self {
            HintOutcome::Followed => "followed",
            HintOutcome::NotFollowed => "not followed",
            HintOutcome::NotReported => "not reported",
        }
    }
}

/// Followed when every task reported the hinted model, not followed when
/// one reported another, not reported otherwise; an unreported model is
/// never taken for the hint.
pub fn hint_outcome(hint: &str, reported: &ReportedModel) -> HintOutcome {
    let other = |m: &String| !same_model(m, hint);
    match reported {
        ReportedModel::One { model } if !other(model) => HintOutcome::Followed,
        ReportedModel::One { .. } | ReportedModel::Mixed { .. } => HintOutcome::NotFollowed,
        ReportedModel::Partial { known, .. } if known.iter().any(other) => HintOutcome::NotFollowed,
        ReportedModel::Partial { .. } | ReportedModel::None => HintOutcome::NotReported,
    }
}

impl HintFollow {
    /// Counts one hinted attempt.
    pub fn add(&mut self, o: HintOutcome) {
        self.hinted += 1;
        match o {
            HintOutcome::Followed => self.followed += 1,
            HintOutcome::NotFollowed => self.not_followed += 1,
            HintOutcome::NotReported => self.not_reported += 1,
        }
    }

    /// The hint outcome of a host attempt on the run surfaces; `None` for a
    /// CLI attempt or a host attempt without a hint.
    pub fn of_view(v: &ExecutionIdentityView) -> Option<HintOutcome> {
        let hint = v.requested_model.as_deref()?;
        (v.executed_by == crate::execution_identity::EXECUTED_BY_HOST)
            .then(|| hint_outcome(hint, &v.reported))
    }

    /// The hint follow counts of one run's attempts; `None` when no host
    /// attempt had a hint.
    pub fn of_views(views: &[ExecutionIdentityView]) -> Option<HintFollow> {
        let mut h = HintFollow::default();
        views
            .iter()
            .filter_map(Self::of_view)
            .for_each(|o| h.add(o));
        (h.hinted > 0).then_some(h)
    }

    /// `N hinted, F followed, X not followed, U not reported`.
    pub fn line(&self) -> String {
        format!(
            "{} hinted, {} followed, {} not followed, {} not reported",
            self.hinted, self.followed, self.not_followed, self.not_reported
        )
    }
}

/// How each attempt of `node` ended, by attempt id.
fn finishes(node: &str, events: &[Event], ids: &[&ExecutionIdentity]) -> BTreeMap<u64, Finish> {
    let mut out = BTreeMap::new();
    for e in events {
        let EventPayload::AttemptFinished {
            node: n,
            attempt,
            status,
            duration_ms,
            usage,
            item,
            ..
        } = &e.payload
        else {
            continue;
        };
        if n != node {
            continue;
        }
        let started = ids
            .iter()
            .rev()
            .find(|id| id.number == *attempt && &id.item == item && id.attempt < e.seq);
        if let Some(id) = started {
            out.entry(id.attempt).or_insert(Finish {
                succeeded: status == "succeeded",
                ms: *duration_ms,
                cost: usage.as_ref().and_then(|u| u.cost_usd),
                basis: usage.as_ref().map(|u| u.cost_basis).unwrap_or_default(),
            });
        }
    }
    out
}

/// The transitions of `node` in one run, each into the group of the
/// attempt it led to: an attempt's recorded entry, else a journaled
/// fallback with its reason (`unstartable` for a host execution fallback)
/// or [`NOT_RECORDED`], never a reason read from anything else.
fn transitions(
    node: &str,
    events: &[Event],
    ids: &[&ExecutionIdentity],
    groups: &mut BTreeMap<GroupKey, GroupAcc>,
) {
    for id in ids {
        if let Some(entry) = id.step.as_ref().and_then(|s| s.entered.as_ref()) {
            let reason = entry.reason.label();
            let g = groups.entry(group_key(id)).or_default();
            g.transition(
                Some(entry.from_step),
                id.step.as_ref().map(|s| s.step),
                reason,
            );
            if let Some(closed) = ids.iter().find(|c| c.attempt == entry.closed_attempt) {
                g.close(closed, reason);
            }
        }
    }
    let mut entries: Vec<u64> = ids
        .iter()
        .filter_map(|id| id.step.as_ref()?.entered.as_ref())
        .map(|entry| entry.closed_attempt)
        .collect();
    entries.sort_unstable();
    let mut paired = vec![false; entries.len()];
    for e in events {
        let reason = match &e.payload {
            EventPayload::FallbackTriggered {
                node: n, reason, ..
            } if n == node => reason
                .as_ref()
                .map_or(NOT_RECORDED, |r| r.label())
                .to_string(),
            EventPayload::ExecutionFallback { node: n, .. } if n == node => {
                crate::execution_identity::CloseReason::Unstartable
                    .label()
                    .to_string()
            }
            _ => continue,
        };
        // A transition an attempt's entry already counted: the drive writes
        // `fallback_triggered` before or after the attempt it entered (the
        // batch write follows it), so the event pairs with the earliest
        // unpaired entry whose closed attempt started before it.
        if let Some(i) = (0..entries.len()).find(|&i| !paired[i] && entries[i] < e.seq) {
            paired[i] = true;
            continue;
        }
        let next = ids.iter().find(|id| id.attempt > e.seq);
        let prev = ids.iter().rev().find(|id| id.attempt < e.seq);
        let Some(owner) = next.or(prev) else {
            continue;
        };
        let g = groups.entry(group_key(owner)).or_default();
        let step = |x: Option<&&ExecutionIdentity>| x.and_then(|i| i.step.as_ref()).map(|s| s.step);
        g.transition(step(prev), step(next), &reason);
        if let Some(p) = prev {
            g.close(p, &reason);
        }
    }
}

/// The executor groups of `node` over `runs`, in the order the runs first
/// bound it to each; empty when no attempt of it ran.
pub(super) fn node_executors(node: &str, runs: &[&StatsRun]) -> Vec<ExecutorGroup> {
    let mut by_time: Vec<&StatsRun> = runs.to_vec();
    by_time.sort_by_cached_key(|r| r.started_ms());
    let mut order: Vec<GroupKey> = Vec::new();
    let mut groups: BTreeMap<GroupKey, GroupAcc> = BTreeMap::new();
    let mut declared: BTreeMap<String, &Vec<String>> = BTreeMap::new();
    for run in by_time {
        let ids: Vec<&ExecutionIdentity> =
            run.identities.iter().filter(|i| i.node == node).collect();
        let ends = finishes(node, &run.events, &ids);
        for id in &ids {
            let key = group_key(id);
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            let g = groups.entry(key.clone()).or_default();
            g.ephemeral |= id.ephemeral;
            g.runs.insert(run.run_id.clone());
            g.attempt(id, ends.get(&id.attempt).copied());
            if let Some(d) = run.declared.get(&key.2) {
                declared.entry(key.2.clone()).or_insert(d);
            }
        }
        transitions(node, &run.events, &ids, &mut groups);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let g = groups.remove(&key)?;
            Some(finish_group(&key, g, declared.get(&key.2).copied()))
        })
        .collect()
}

fn finish_group(key: &GroupKey, g: GroupAcc, declared: Option<&Vec<String>>) -> ExecutorGroup {
    let shown = |d: &str| {
        if d == NOT_RECORDED {
            d.to_string()
        } else {
            short(d)
        }
    };
    let executed_by = g
        .steps
        .keys()
        .next()
        .map_or_else(|| NOT_RECORDED.to_string(), |k| k.3.clone());
    let binding = g.steps.keys().find_map(|k| k.0.clone());
    let mut steps: Vec<StepStats> = g.steps.into_iter().map(|(k, a)| step_stats(k, a)).collect();
    // A declared step no attempt ran on, as a fact: it never ran. Only
    // when every attempt's position is recorded: an attempt of unknown
    // position may have run on any declared step.
    let positions_known = steps.iter().all(|s| s.step.is_some());
    let declared = declared.filter(|_| positions_known);
    for (i, source) in declared.into_iter().flatten().enumerate() {
        let i = i as u32;
        if !steps.iter().any(|s| s.step == Some(i)) {
            steps.push(StepStats {
                binding: binding.clone(),
                step: Some(i),
                source: source.clone(),
                executed_by: executed_by.clone(),
                ..StepStats::default()
            });
        }
    }
    steps.sort_by_key(|s| (s.step.is_none(), s.step));
    ExecutorGroup {
        profile: shown(&key.0),
        bundle: shown(&key.1),
        executor: shown(&key.2),
        ephemeral: g.ephemeral,
        runs: g.runs.len(),
        attempts: g.attempts,
        steps,
        transitions: g
            .transitions
            .into_iter()
            .map(|((from_step, to_step, reason), count)| TransitionStats {
                from_step,
                to_step,
                reason,
                count,
            })
            .collect(),
        hint_follow: g.hint,
    }
}

fn step_stats((binding, step, source, executed_by): StepKey, mut a: StepAcc) -> StepStats {
    StepStats {
        binding,
        step,
        source,
        executed_by,
        attempts: a.attempts,
        successes: Rate::new(a.succeeded, a.finished),
        median_attempt_ms: median(&mut a.ms),
        // `+ 0.0`: an empty float sum is -0.0.
        cost_usd: (a.cost_attempts > 0).then(|| (a.cost * 1e8).round() / 1e8 + 0.0),
        cost_known: Rate::new(a.cost_attempts, a.finished),
        cost_by_basis: a.cost_by_basis,
        reported_models: a.reported,
        closes_by_reason: a.closes,
    }
}

/// The text block of a node's executor groups: one per group with more
/// than one step, any transition or a hint line.
pub(super) fn lines(groups: &[ExecutorGroup]) -> Vec<String> {
    let mut out = Vec::new();
    for g in groups
        .iter()
        .filter(|g| g.steps.len() > 1 || !g.transitions.is_empty() || g.hint_follow.is_some())
    {
        out.push(format!(
            "    executor {}{} (profile {}, bundle {}): {} runs, {} attempts",
            g.executor,
            if g.ephemeral { ", override" } else { "" },
            g.profile,
            g.bundle,
            g.runs,
            g.attempts
        ));
        for s in &g.steps {
            let at = match s.step {
                Some(i) => format!("step {i} {}", s.source),
                None => "position not recorded".to_string(),
            };
            if s.attempts == 0 {
                out.push(format!("      {at}: never ran"));
                continue;
            }
            let mut line = format!(
                "      {at} ({}): {} attempts, succeeded {}",
                s.executed_by,
                s.attempts,
                s.successes.text()
            );
            if let Some(c) = s.cost_usd {
                line.push_str(&format!(", cost {c:.4} USD over {}", s.cost_known.text()));
                if !s.cost_by_basis.is_empty() {
                    let split = s.cost_by_basis.describe(|v| format!("${v:.4}"));
                    line.push_str(&format!(" ({split})"));
                }
            }
            out.push(line);
        }
        for t in &g.transitions {
            let at = |x: Option<u32>| x.map_or_else(|| "?".to_string(), |i| i.to_string());
            out.push(format!(
                "      {} -> {} {} x{}",
                at(t.from_step),
                at(t.to_step),
                t.reason,
                t.count
            ));
        }
        if let Some(h) = &g.hint_follow {
            out.push(format!("      hints: {}", h.line()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host group with one step and no transition still shows its hint
    /// line, as STATS.md promises.
    #[test]
    fn a_one_step_group_with_hints_is_listed() {
        let g = ExecutorGroup {
            steps: vec![StepStats {
                step: Some(0),
                source: "primary".into(),
                executed_by: "host".into(),
                attempts: 1,
                ..Default::default()
            }],
            hint_follow: Some(HintFollow {
                hinted: 1,
                followed: 1,
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = lines(std::slice::from_ref(&g));
        assert!(out.iter().any(|l| l.contains("hints: ")), "{out:?}");
        let plain = ExecutorGroup {
            hint_follow: None,
            ..g
        };
        assert!(lines(&[plain]).is_empty());
    }
}
