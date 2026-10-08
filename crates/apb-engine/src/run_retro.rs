//! The retrospective of one run (issue #192 part 3): the numbers whoever
//! improves a playbook needs, so a supervisor or a final `retro` node does
//! not re-derive them from the raw journal. Read by MCP `run_retro_context`
//! and rendered for prompts as `{{run.retro}}` (`text::prompt_text`).
//!
//! Model-free and read-only. Per node: time against the declared
//! `expected_duration`, executions, attempts, retries, fallbacks,
//! re-entries, tokens and cost, the model each attempt actually ran on (a
//! CLI attempt's `attempt_started.model`; a host task's
//! `host_task_submitted.model`, never inferred), the host wait (from
//! `host_task_requested` to the host's `host_task_submitted`) and the
//! status-file verdicts. Per run: the goal results and a baseline: the
//! medians of the last N finished runs of the same playbook version, read
//! through the `apb stats` journal walk ([`crate::run_stats::StatsRun`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::attempt_models::{AttemptModel, AttemptWalk, EXECUTED_BY_HOST};
use crate::decision::report::stats::median;
use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::manifest::RunExecutionManifest;
use crate::run_outcome::{RunGoal, goal_from};
use crate::run_stats::{Rate, StatsFilter, StatsRun};

pub mod text;

/// How many earlier runs the baseline compares with by default.
pub const DEFAULT_COMPARE_LAST: usize = 10;
/// The most earlier runs a caller may ask for.
pub const MAX_COMPARE_LAST: usize = 100;

/// One attempt of a node.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AttemptRetro {
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model the attempt actually ran on, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Where `model` comes from: `cli` (the binding the CLI was started
    /// with), `host` (the model the host reported on submission) or
    /// `unreported` (a host task whose host named no model).
    pub model_source: String,
    /// For a host attempt: the model the profile or a fallback hint
    /// declared, which the host was free to ignore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_model: Option<String>,
    /// `succeeded`, `failed`, ...; absent while the attempt runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    /// The attempt's status-file verdict: `success`, `failure` or
    /// `invalid` (a file the engine could not read as a verdict); absent
    /// when the attempt wrote none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Input plus output tokens the agent or host reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Host tasks the attempt handed out (a blocked task's follow-up is a
    /// second one).
    #[serde(skip_serializing_if = "is_zero")]
    pub host_tasks: u32,
    /// Time from each host task's request to the host's submission, summed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_wait_ms: Option<u64>,
}

/// One node over the run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NodeRetro {
    pub node: String,
    /// The profile the manifest bound the node to (`<scope>/<name>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The last result (`succeeded`, `failed`, ...); absent while it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Times the node started (a loop back into it starts it again).
    pub executions: usize,
    pub reentries: usize,
    pub retries: usize,
    pub fallbacks: usize,
    /// Time over its finished executions, summed.
    pub duration_ms: u64,
    /// The declared `expected_duration`, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_s: Option<u64>,
    /// `duration_ms` is over `expected_s`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_expected: Option<bool>,
    /// The node's median time over the baseline runs it ran in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The distinct models the attempts actually ran on, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_wait_ms: Option<u64>,
    pub attempts: Vec<AttemptRetro>,
}

/// The run against the last N finished runs of the same version.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Baseline {
    /// The runs compared with, newest first.
    pub run_ids: Vec<String>,
    pub success: Rate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_duration_ms: Option<u64>,
    /// This run's duration minus the median.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_delta_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_cost_usd: Option<f64>,
}

/// The retrospective of one run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RetroReport {
    pub run_id: String,
    pub playbook: String,
    pub version: String,
    /// `succeeded`, `failed` or `aborted`; absent while the run is live
    /// (the report is as of its last journal line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// `cli` or `host`, from the manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<String>,
    /// Run start to its end, or to the last journal line while live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// In the order the nodes first started.
    pub nodes: Vec<NodeRetro>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<RunGoal>,
    /// Absent when no earlier finished run of the version was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Everything the report reads about the run itself.
#[derive(Debug, Clone, Default)]
pub struct RetroRun {
    pub run_id: String,
    pub events: Vec<Event>,
    pub snapshot: Option<apb_core::schema::Playbook>,
    pub manifest: Option<RunExecutionManifest>,
    /// Status-file verdicts by `(node, attempt)`.
    pub verdicts: BTreeMap<(String, u32), String>,
}

impl RetroRun {
    /// Reads the run at `run_dir`: its journal (tolerant of a torn last
    /// line, since a live run is still appending), playbook snapshot,
    /// manifest and status files.
    pub fn load(run_dir: &Path) -> Result<Self, EngineError> {
        let events = crate::run_view::read_events(run_dir)?;
        let run_id = run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let verdicts = read_verdicts(run_dir, &events);
        Ok(RetroRun {
            run_id,
            snapshot: crate::legacy_snapshot::load_run_playbook(run_dir),
            manifest: crate::manifest::read(run_dir).ok().flatten(),
            verdicts,
            events,
        })
    }
}

/// The status file of every attempt the journal names
/// (`agent-status/<node>-<attempt>.json`, the per-attempt path the drive
/// hands an agent as `APB_STATUS_FILE`).
fn read_verdicts(run_dir: &Path, events: &[Event]) -> BTreeMap<(String, u32), String> {
    let mut out = BTreeMap::new();
    for e in events {
        let EventPayload::AttemptStarted { node, attempt, .. } = &e.payload else {
            continue;
        };
        if !apb_core::registry::is_safe_segment(node) {
            continue;
        }
        let path = run_dir
            .join("agent-status")
            .join(format!("{node}-{attempt}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let status = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string));
        let verdict = match status.as_deref() {
            Some(s @ ("success" | "failure")) => s.to_string(),
            _ => "invalid".to_string(),
        };
        out.insert((node.clone(), *attempt), verdict);
    }
    out
}

/// The retrospective of the run at `run_dir` under the project `root`,
/// compared with up to `compare_last` earlier finished runs of the same
/// playbook version that apb created on this machine.
pub fn retro(root: &Path, run_dir: &Path, compare_last: usize) -> Result<RetroReport, EngineError> {
    let run = RetroRun::load(run_dir)?;
    let history = if compare_last == 0 {
        Vec::new()
    } else {
        crate::decision::report::run_dirs(&[root.to_path_buf()])
            .iter()
            .filter(|d| d.as_path() != run_dir)
            .filter_map(|d| StatsRun::load(d))
            .collect()
    };
    Ok(build(&run, &history, compare_last))
}

/// The project root of a run directory laid out as `<root>/.apb/runs/<id>`.
pub(crate) fn project_root_of(run_dir: &Path) -> Option<PathBuf> {
    let runs = run_dir.parent()?;
    let apb = runs.parent()?;
    (runs.file_name()? == "runs" && apb.file_name()? == ".apb")
        .then(|| apb.parent().map(Path::to_path_buf))
        .flatten()
}

/// One attempt while the journal is walked; its actual model comes from
/// the shared `AttemptWalk` slot of the same index.
struct Open {
    node: String,
    rec: AttemptRetro,
    host_hint: Option<String>,
}

#[derive(Default)]
struct NodeAcc {
    executions: usize,
    retries: usize,
    fallbacks: usize,
    duration_ms: u64,
    status: Option<String>,
    open_since: Option<u128>,
}

fn ms(from: u128, to: u128) -> u64 {
    u64::try_from(to.saturating_sub(from)).unwrap_or(u64::MAX)
}

fn tokens_of(u: &apb_core::agent_output::AgentUsage) -> u64 {
    u.input_tokens.saturating_add(u.output_tokens)
}

fn add_opt_u64(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0).saturating_add(y.unwrap_or(0))),
    }
}

fn add_opt_f64(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(round6(x.unwrap_or(0.0) + y.unwrap_or(0.0))),
    }
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6 + 0.0
}

/// The journal walk: per-node counters and the attempts in journal order.
/// `attempts[i]` is slot `i` of `models`, the fold every run surface reads
/// an attempt's actual model from.
#[derive(Default)]
struct Walk {
    order: Vec<String>,
    nodes: BTreeMap<String, NodeAcc>,
    attempts: Vec<Open>,
    models: AttemptWalk,
    /// Host task id -> its request ts.
    requested: BTreeMap<String, u128>,
}

impl Walk {
    fn node(&mut self, node: &str) -> &mut NodeAcc {
        if !self.nodes.contains_key(node) {
            self.order.push(node.to_string());
        }
        self.nodes.entry(node.to_string()).or_default()
    }

    /// The attempt record of the model slot `i`, created on first sight.
    fn attempt(&mut self, i: usize) -> &mut Open {
        while self.attempts.len() <= i {
            let m = self.models.get(self.attempts.len());
            self.attempts.push(Open {
                node: m.node.clone(),
                rec: AttemptRetro {
                    attempt: m.attempt,
                    ..Default::default()
                },
                host_hint: None,
            });
        }
        &mut self.attempts[i]
    }

    fn step(&mut self, e: &Event) {
        let slot = self.models.step(e);
        match &e.payload {
            EventPayload::NodeStarted { node, .. } => {
                let n = self.node(node);
                n.executions += 1;
                n.open_since = Some(e.ts);
                n.status = None;
            }
            EventPayload::NodeFinished { node, status, .. } => {
                let n = self.node(node);
                if let Some(t) = n.open_since.take() {
                    n.duration_ms = n.duration_ms.saturating_add(ms(t, e.ts));
                }
                n.status = Some(status.clone());
            }
            EventPayload::RetryStarted { node, .. } => self.node(node).retries += 1,
            EventPayload::FallbackTriggered { node, .. }
            | EventPayload::ExecutionFallback { node, .. } => self.node(node).fallbacks += 1,
            EventPayload::AttemptStarted {
                node, agent, model, ..
            } => {
                self.node(node);
                let Some(i) = slot else { return };
                let rec = &mut self.attempt(i).rec;
                rec.agent = Some(agent.clone());
                rec.declared_model.clone_from(model);
            }
            EventPayload::AttemptFinished {
                node,
                status,
                duration_ms,
                failure_kind,
                usage,
                ..
            } => {
                self.node(node);
                let Some(i) = slot else { return };
                let rec = &mut self.attempt(i).rec;
                rec.status = Some(status.clone());
                rec.duration_ms = *duration_ms;
                rec.failure_kind.clone_from(failure_kind);
                rec.tokens = usage.as_ref().map(tokens_of);
                rec.cost_usd = usage.as_ref().and_then(|u| u.cost_usd);
            }
            EventPayload::HostTaskRequested {
                task_id,
                node,
                model_hint,
                ..
            } => {
                self.node(node);
                let Some(i) = slot else { return };
                let o = self.attempt(i);
                o.rec.host_tasks += 1;
                if o.host_hint.is_none() {
                    o.host_hint.clone_from(model_hint);
                }
                self.requested.insert(task_id.clone(), e.ts);
            }
            EventPayload::HostTaskSubmitted {
                task_id,
                submitted_by,
                ..
            } => {
                let (Some(i), Some(at)) = (slot, self.requested.remove(task_id)) else {
                    return;
                };
                // An engine closure (expired, cancelled, ...) is no host reply.
                if submitted_by == "engine" {
                    return;
                }
                let o = self.attempt(i);
                o.rec.host_wait_ms = add_opt_u64(o.rec.host_wait_ms, Some(ms(at, e.ts)));
            }
            _ => {}
        }
    }
}

/// Settles an attempt's model from its shared slot `m`: a CLI attempt ran
/// on the model it was started with, a host attempt on the one its host
/// reported (`unreported` when it named none), and the declared model of a
/// host attempt is the hint it was handed, else the profile's.
fn settle_model(o: &mut Open, m: &AttemptModel) {
    if m.executed_by != EXECUTED_BY_HOST {
        o.rec.model = m.model.clone();
        o.rec.declared_model = None;
        o.rec.model_source = "cli".to_string();
        return;
    }
    if o.host_hint.is_some() {
        o.rec.declared_model.clone_from(&o.host_hint);
    }
    o.rec.model = m.model.clone();
    o.rec.model_source = if m.model.is_some() {
        "host"
    } else {
        "unreported"
    }
    .to_string();
}

/// Run outcome and the ts it ended at, from the journal.
fn run_end(events: &[Event]) -> (Option<String>, Option<u128>) {
    let mut out = (None, None);
    for e in events {
        match &e.payload {
            EventPayload::RunFinished { outcome } => {
                out = (Some(outcome.clone()), Some(e.ts));
            }
            EventPayload::RunAborted { .. } => out = (Some("aborted".into()), Some(e.ts)),
            // A resumed run is live again.
            EventPayload::RunResumed { .. } => out = (None, None),
            _ => {}
        }
    }
    out
}

fn run_started(events: &[Event]) -> Option<(u128, String, String)> {
    events.iter().find_map(|e| match &e.payload {
        EventPayload::RunStarted { playbook, version } => {
            Some((e.ts, playbook.clone(), version.clone()))
        }
        _ => None,
    })
}

/// Run duration: start to its end, or to the last journal line while live.
fn run_duration(events: &[Event]) -> Option<u64> {
    let (start, ..) = run_started(events)?;
    let (_, end) = run_end(events);
    let end = end.or_else(|| events.last().map(|e| e.ts))?;
    Some(ms(start, end))
}

/// Time per node over one run's finished executions.
fn node_times(events: &[Event]) -> BTreeMap<String, u64> {
    let mut w = Walk::default();
    for e in events {
        w.step(e);
    }
    w.nodes
        .into_iter()
        .map(|(k, n)| (k, n.duration_ms))
        .collect()
}

/// Builds the report for `run` against `history` (any runs; only finished
/// runs of the same playbook and version count, the newest `compare_last`).
pub fn build(run: &RetroRun, history: &[StatsRun], compare_last: usize) -> RetroReport {
    let mut w = Walk::default();
    for e in &run.events {
        w.step(e);
    }
    let (_, playbook, version) = run_started(&run.events).unwrap_or_default();
    let (outcome, _) = run_end(&run.events);
    let mut report = RetroReport {
        run_id: run.run_id.clone(),
        playbook,
        version,
        outcome,
        execution: run.manifest.as_ref().map(|m| {
            m.execution
                .as_ref()
                .map_or("cli", |x| x.mode.as_str())
                .to_string()
        }),
        duration_ms: run_duration(&run.events),
        goal: goal_from(run.snapshot.as_ref(), &run.events),
        ..Default::default()
    };
    report.nodes = node_reports(run, w);
    for n in &report.nodes {
        report.tokens = add_opt_u64(report.tokens, n.tokens);
        report.cost_usd = add_opt_f64(report.cost_usd, n.cost_usd);
    }
    let compare_last = compare_last.min(MAX_COMPARE_LAST);
    report.baseline = baseline(&report, history, compare_last);
    if let Some(b) = &report.baseline {
        let medians = node_medians(history, &b.run_ids);
        for n in &mut report.nodes {
            n.baseline_median_ms = medians.get(&n.node).copied();
        }
    }
    report
}

fn node_reports(run: &RetroRun, mut w: Walk) -> Vec<NodeRetro> {
    let models = std::mem::take(&mut w.models).finish(&BTreeMap::new());
    for (o, m) in w.attempts.iter_mut().zip(&models) {
        settle_model(o, m);
        o.rec.verdict = run.verdicts.get(&(o.node.clone(), o.rec.attempt)).cloned();
    }
    // A status file holds the latest execution of its attempt number only.
    let mut seen: Vec<(String, u32)> = Vec::new();
    for o in w.attempts.iter_mut().rev() {
        let key = (o.node.clone(), o.rec.attempt);
        if seen.contains(&key) {
            o.rec.verdict = None;
        } else {
            seen.push(key);
        }
    }
    let mut by_node: BTreeMap<String, Vec<AttemptRetro>> = BTreeMap::new();
    for o in w.attempts {
        by_node.entry(o.node).or_default().push(o.rec);
    }
    w.order
        .iter()
        .map(|id| {
            let acc = w.nodes.remove(id).unwrap_or_default();
            let attempts = by_node.remove(id).unwrap_or_default();
            node_report(run, id, acc, attempts)
        })
        .collect()
}

fn node_report(run: &RetroRun, id: &str, acc: NodeAcc, attempts: Vec<AttemptRetro>) -> NodeRetro {
    let expected_s = run
        .snapshot
        .as_ref()
        .and_then(|p| p.node(id))
        .and_then(|n| n.expected_duration.as_ref())
        .and_then(|d| d.parsed());
    let mut n = NodeRetro {
        node: id.to_string(),
        profile: run
            .manifest
            .as_ref()
            .and_then(|m| m.node_bindings.get(id).cloned()),
        status: acc.status,
        executions: acc.executions,
        reentries: acc.executions.saturating_sub(1),
        retries: acc.retries,
        fallbacks: acc.fallbacks,
        duration_ms: acc.duration_ms,
        over_expected: expected_s.map(|s| acc.duration_ms > s.saturating_mul(1000)),
        expected_s,
        ..Default::default()
    };
    for a in &attempts {
        n.tokens = add_opt_u64(n.tokens, a.tokens);
        n.cost_usd = add_opt_f64(n.cost_usd, a.cost_usd);
        n.host_wait_ms = add_opt_u64(n.host_wait_ms, a.host_wait_ms);
        if let Some(m) = &a.model
            && !n.models.contains(m)
        {
            n.models.push(m.clone());
        }
    }
    n.attempts = attempts;
    n
}

/// The newest `n` finished runs of the report's playbook version.
fn baseline_runs<'a>(report: &RetroReport, history: &'a [StatsRun], n: usize) -> Vec<&'a StatsRun> {
    let mut runs: Vec<(u128, &StatsRun)> = history
        .iter()
        .filter(|r| r.run_id != report.run_id)
        .filter(|r| r.playbook == report.playbook && r.version == report.version)
        .filter(|r| run_end(&r.events).0.is_some())
        .filter_map(|r| run_started(&r.events).map(|(ts, ..)| (ts, r)))
        .collect();
    runs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.run_id.cmp(&a.1.run_id)));
    runs.into_iter().take(n).map(|(_, r)| r).collect()
}

fn baseline(report: &RetroReport, history: &[StatsRun], n: usize) -> Option<Baseline> {
    let runs = baseline_runs(report, history, n);
    if runs.is_empty() {
        return None;
    }
    let owned: Vec<StatsRun> = runs.iter().map(|r| (*r).clone()).collect();
    let filter = StatsFilter {
        playbook: Some(report.playbook.clone()),
        ..Default::default()
    };
    let stats = crate::run_stats::build(&owned, &filter);
    let success = stats
        .versions
        .first()
        .map(|v| v.success)
        .unwrap_or_default();
    let mut durations: Vec<u64> = runs
        .iter()
        .filter_map(|r| run_duration(&r.events))
        .collect();
    let usage: Vec<crate::run_view::RunUsage> = runs
        .iter()
        .filter_map(|r| crate::run_view::RunUsage::from_events(&r.events))
        .collect();
    let mut tokens: Vec<u64> = usage
        .iter()
        .map(|u| u.input_tokens.saturating_add(u.output_tokens))
        .collect();
    // Micro-dollars, so the integer median helper applies.
    let mut costs: Vec<u64> = usage
        .iter()
        .filter_map(|u| u.cost_usd)
        .map(|c| (c * 1e6).round().max(0.0) as u64)
        .collect();
    let median_duration_ms = median(&mut durations);
    Some(Baseline {
        run_ids: runs.iter().map(|r| r.run_id.clone()).collect(),
        success,
        duration_delta_ms: match (report.duration_ms, median_duration_ms) {
            (Some(a), Some(m)) => Some(a as i64 - m as i64),
            _ => None,
        },
        median_duration_ms,
        median_tokens: median(&mut tokens),
        median_cost_usd: median(&mut costs).map(|c| c as f64 / 1e6),
    })
}

/// Per node: the median of its time over the baseline runs it ran in.
fn node_medians(history: &[StatsRun], run_ids: &[String]) -> BTreeMap<String, u64> {
    let mut per_node: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for r in history.iter().filter(|r| run_ids.contains(&r.run_id)) {
        for (node, t) in node_times(&r.events) {
            per_node.entry(node).or_default().push(t);
        }
    }
    per_node
        .into_iter()
        .filter_map(|(k, mut v)| median(&mut v).map(|m| (k, m)))
        .collect()
}
