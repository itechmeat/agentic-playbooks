//! The report of one scenario run: the trace, not only the outcome.
//!
//! Built only from what the run left behind (its journal, the execution
//! index derived from it, the diagnostics sidecar, the run outputs
//! projection) plus what the scenario drive reports about its answer book
//! ([`DriveOutcome`]). Every expectation of the scenario's `expect:` block
//! becomes one named [`ExpectationResult`], so a run that ends with the
//! right outcome over the wrong route, loop count or retry count still
//! fails, and says which check it failed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use apb_core::scenario::{LoadedScenario, OnExtra, ScenarioLevel, TemplatesEmpty};
use apb_core::schema::{NodeKind, Playbook};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{BoundaryKey, BoundaryKind};
use crate::diagnostics::Diagnostic;
use crate::event::{Event, EventPayload};
use crate::identity::ExecutionIndex;

/// The report's file inside the scenario run directory.
pub const REPORT_FILE: &str = "report.json";

/// The diagnostics record kind of a template reference that rendered as
/// the empty string in a scenario run. Its fields are those of
/// [`EmptyRef`]; the drive writes it, the report reads it.
pub const TEMPLATE_EMPTY_KIND: &str = super::session::TEMPLATE_EMPTY_RECORD;

/// What a scenario run showed.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(rename = "ScenarioVerdict"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every expectation held.
    Passed,
    /// The run ended and at least one expectation did not hold.
    Failed,
    /// The scenario could not be played as written: a boundary had no
    /// answer under `on_missing: error`, or the scenario or the version
    /// changed after it was checked.
    Invalid,
    /// The scenario does not fit the version (its `versions` or its digest
    /// pin), and nothing ran.
    Stale,
    /// The run did not end: the drive failed, timed out or died.
    Incomplete,
}

impl Verdict {
    /// Every verdict, in declaration order.
    pub const ALL: [Verdict; 5] = [
        Verdict::Passed,
        Verdict::Failed,
        Verdict::Invalid,
        Verdict::Stale,
        Verdict::Incomplete,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Passed => "passed",
            Verdict::Failed => "failed",
            Verdict::Invalid => "invalid",
            Verdict::Stale => "stale",
            Verdict::Incomplete => "incomplete",
        }
    }
}

/// One expectation of the scenario, checked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectationResult {
    /// `run.outcome`, `finish`, `route.path`, `route.visits`,
    /// `route.not_visits`, `visits[<node>]`, `edges.taken[<address>]`,
    /// `edges.not_taken[<address>]`, `joins.dead[<join>]`,
    /// `outputs[<node>]`, `outputs[<node>.<field>]`, `run_outputs[<name>]`,
    /// `templates_empty`, `unused_answers`.
    pub check: String,
    pub passed: bool,
    pub expected: String,
    pub observed: String,
}

/// Where an attempt's scripted answer came from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerSource {
    /// A numbered entry for this visit.
    Used,
    /// The node's `visit: default` entry.
    Default,
    /// No entry: the boundary applied `on_missing`.
    Missing,
}

/// One attempt of an execution (or of one list item in it).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceAttempt {
    /// 1-based inside the execution and item, counted from the journal's
    /// `attempt_started` events (never the display ordinal).
    pub n: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AnswerSource>,
}

/// One execution of a node, in settlement order.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceExecution {
    pub node: String,
    /// The execution ordinal (`identity::index`), 1-based.
    pub visit: u32,
    /// The status its `node_finished` recorded; absent while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<TraceAttempt>>", optional))]
    pub attempts: Vec<TraceAttempt>,
}

/// One routing decision, as the `route_evaluated` diagnostics recorded it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteTrace {
    pub node: String,
    pub visit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    pub mode: String,
    pub checks: Vec<crate::diagnostics::route::RouteCheck>,
    pub chosen: Vec<String>,
}

/// How often one edge (by address) was taken.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeCount {
    pub edge: String,
    pub count: u32,
}

/// The inputs a join went on without.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinDead {
    pub join: String,
    pub inputs: Vec<String>,
}

/// A template reference that rendered as the empty string.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyRef {
    pub node: String,
    #[serde(default)]
    pub visit: u32,
    /// The reference as written (`nodes.qa.output`).
    pub reference: String,
    /// `never ran`, `field absent` or `not JSON`.
    pub why: String,
}

/// One scripted answer by its boundary and key.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AnswerRef {
    pub kind: BoundaryKind,
    #[serde(flatten)]
    pub key: BoundaryKey,
}

impl AnswerRef {
    /// `node visit 2 attempt 1 item a.md (agent_attempt)`.
    pub fn describe(&self) -> String {
        let mut out = match &self.key.node {
            Some(n) => format!("{n} visit {}", self.key.visit),
            None => "run".to_string(),
        };
        if let Some(a) = self.key.attempt {
            out.push_str(&format!(" attempt {a}"));
        }
        if let Some(i) = &self.key.item {
            out.push_str(&format!(" item {i}"));
        }
        if let Some(p) = &self.key.part {
            out.push_str(&format!(" {p}"));
        }
        format!("{out} ({})", self.kind.as_str())
    }
}

/// What the answer book saw: the answers used by number, those a
/// `visit: default` answered, the boundaries with no answer, and the
/// answers nothing used.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AnswerUse {
    #[serde(default)]
    pub used: Vec<AnswerRef>,
    #[serde(default)]
    pub defaults: Vec<AnswerRef>,
    #[serde(default)]
    pub missing: Vec<AnswerRef>,
    #[serde(default)]
    pub extra: Vec<AnswerRef>,
}

/// How often the run reached one boundary, by what the site did.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryUse {
    pub kind: BoundaryKind,
    /// Answered from the scenario.
    #[serde(default)]
    pub answered: u32,
    /// Not crossed (no snapshot, no lock, no send, no sleep).
    #[serde(default)]
    pub skipped: u32,
    /// No answer: `on_missing` applied.
    #[serde(default)]
    pub missing: u32,
    /// Acted for real. A quick run must show none.
    #[serde(default)]
    pub real: u32,
}

/// What the scenario drive hands back: the run it drove and what its
/// answer book and session saw. The report reads everything else from the
/// run directory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DriveOutcome {
    pub run_id: String,
    /// `.apb/scenario-runs/<run_id>`.
    pub run_dir: PathBuf,
    /// Set when the drive stopped the scenario because it could not be
    /// played as written (a boundary with no answer under
    /// `on_missing: error`, naming the boundary and its key), or when a
    /// guard of the scenario process refused an operation while it ran.
    pub invalid: Option<String>,
    pub answers: AnswerUse,
    pub boundaries: Vec<BoundaryUse>,
    /// Decision requests sent (smoke only).
    pub decision_requests: u32,
    pub cost_usd: Option<f64>,
}

/// The report of one scenario run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioReport {
    /// Absent when nothing ran (stale, or a drive that failed before it
    /// allocated a run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub level: ScenarioLevel,
    pub scenario: String,
    pub scenario_digest: String,
    pub playbook: String,
    pub version: String,
    pub playbook_digest: String,
    pub verdict: Verdict,
    /// Why the verdict is `invalid`, `stale` or `incomplete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    #[serde(default)]
    pub checks: Vec<ExpectationResult>,
    #[serde(default)]
    pub executions: Vec<TraceExecution>,
    #[serde(default)]
    pub routes: Vec<RouteTrace>,
    #[serde(default)]
    pub edges_taken: Vec<EdgeCount>,
    #[serde(default)]
    pub joins_dead: Vec<JoinDead>,
    #[serde(default)]
    pub templates_empty: Vec<EmptyRef>,
    #[serde(default)]
    pub answers: AnswerUse,
    #[serde(default)]
    pub boundaries: Vec<BoundaryUse>,
    #[serde(default)]
    pub decision_requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// What a report names the run by, before anything is read.
#[derive(Debug, Clone, PartialEq)]
pub struct Subject {
    pub level: ScenarioLevel,
    pub scenario: String,
    pub scenario_digest: String,
    pub playbook: String,
    pub version: String,
    pub playbook_digest: String,
}

impl Subject {
    pub fn of(
        level: ScenarioLevel,
        scenario: &LoadedScenario,
        playbook: &str,
        version: &str,
        playbook_digest: &str,
    ) -> Self {
        Subject {
            level,
            scenario: scenario.name.clone(),
            scenario_digest: scenario.digest.clone(),
            playbook: playbook.to_string(),
            version: version.to_string(),
            playbook_digest: playbook_digest.to_string(),
        }
    }
}

impl ScenarioReport {
    /// A report of a scenario that did not run (or whose run left nothing
    /// to read): `stale`, `invalid` or `incomplete` with its reason.
    pub fn not_run(subject: &Subject, verdict: Verdict, reason: String) -> Self {
        ScenarioReport {
            run_id: None,
            level: subject.level,
            scenario: subject.scenario.clone(),
            scenario_digest: subject.scenario_digest.clone(),
            playbook: subject.playbook.clone(),
            version: subject.version.clone(),
            playbook_digest: subject.playbook_digest.clone(),
            verdict,
            reason: Some(reason),
            outcome: None,
            finish: None,
            checks: Vec::new(),
            executions: Vec::new(),
            routes: Vec::new(),
            edges_taken: Vec::new(),
            joins_dead: Vec::new(),
            templates_empty: Vec::new(),
            answers: AnswerUse::default(),
            boundaries: Vec::new(),
            decision_requests: 0,
            cost_usd: None,
        }
    }

    /// The checks that did not hold.
    pub fn failed_checks(&self) -> impl Iterator<Item = &ExpectationResult> {
        self.checks.iter().filter(|c| !c.passed)
    }
}

/// The run's terminal outcome: its `run_finished`, else `paused` when the
/// last pause is still in force, else `incomplete`.
pub fn outcome(events: &[Event]) -> String {
    let mut paused = false;
    for e in events {
        match &e.payload {
            EventPayload::RunFinished { outcome } => return outcome.clone(),
            EventPayload::RunPaused { .. } => paused = true,
            EventPayload::RunResumed { .. } => paused = false,
            _ => {}
        }
    }
    if paused { "paused" } else { "incomplete" }.to_string()
}

/// Every execution in settlement order (the order their `node_finished`
/// was journaled; open ones last, in the order they began).
fn settled(index: &ExecutionIndex) -> Vec<&crate::identity::Execution> {
    let mut all: Vec<&crate::identity::Execution> =
        index.nodes().flat_map(|(_, list)| list.iter()).collect();
    all.sort_by_key(|x| (x.end_seq.is_none(), x.end_seq.unwrap_or(x.begin_seq)));
    all
}

fn answer_source(answers: &AnswerUse, key: &BoundaryKey) -> Option<AnswerSource> {
    let hit = |list: &[AnswerRef]| {
        list.iter()
            .any(|a| a.kind == BoundaryKind::AgentAttempt && &a.key == key)
    };
    if hit(&answers.used) {
        Some(AnswerSource::Used)
    } else if hit(&answers.defaults) {
        Some(AnswerSource::Default)
    } else if hit(&answers.missing) {
        Some(AnswerSource::Missing)
    } else {
        None
    }
}

/// The executions with their attempts, in settlement order.
fn executions(
    events: &[Event],
    index: &ExecutionIndex,
    answers: &AnswerUse,
) -> Vec<TraceExecution> {
    // (node, visit) -> attempts; (node, visit, item) -> attempts so far.
    let mut attempts: BTreeMap<(String, u32), Vec<TraceAttempt>> = BTreeMap::new();
    let mut counts: BTreeMap<(String, u32, Option<String>), u32> = BTreeMap::new();
    // (node, item, display ordinal) -> (node, visit, position) of the open attempt.
    type AttemptId = (String, Option<String>, u32);
    let mut open: BTreeMap<AttemptId, (String, u32, usize)> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::AttemptStarted {
                node,
                attempt,
                item,
                ..
            } => {
                let Some(exec) = index.containing(node, e.seq) else {
                    continue;
                };
                let n = counts
                    .entry((node.clone(), exec.ordinal, item.clone()))
                    .or_default();
                *n += 1;
                let key = BoundaryKey {
                    node: Some(node.clone()),
                    visit: exec.ordinal,
                    attempt: Some(*n),
                    item: item.clone(),
                    part: None,
                };
                let list = attempts.entry((node.clone(), exec.ordinal)).or_default();
                list.push(TraceAttempt {
                    n: *n,
                    item: item.clone(),
                    status: None,
                    failure_kind: None,
                    answer: answer_source(answers, &key),
                });
                open.insert(
                    (node.clone(), item.clone(), *attempt),
                    (node.clone(), exec.ordinal, list.len() - 1),
                );
            }
            EventPayload::AttemptFinished {
                node,
                attempt,
                status,
                failure_kind,
                item,
                ..
            } => {
                if let Some((n, visit, pos)) = open.remove(&(node.clone(), item.clone(), *attempt))
                    && let Some(a) = attempts.get_mut(&(n, visit)).and_then(|l| l.get_mut(pos))
                {
                    a.status = Some(status.clone());
                    a.failure_kind = failure_kind.clone();
                }
            }
            _ => {}
        }
    }
    settled(index)
        .into_iter()
        .map(|x| TraceExecution {
            node: x.node.clone(),
            visit: x.ordinal,
            status: x.status.clone(),
            cancelled: x.cancelled,
            attempts: attempts
                .remove(&(x.node.clone(), x.ordinal))
                .unwrap_or_default(),
        })
        .collect()
}

/// The hops over each edge address. A bounded edge journals two records
/// for one hop (the routing record and the counted one), so a hop is one
/// delivery: the same edge out of the same execution of its source counts
/// once, as the run graph counts it.
fn edges_taken(events: &[Event], index: &ExecutionIndex) -> Vec<EdgeCount> {
    let mut hops: BTreeSet<(String, String, Option<u32>)> = BTreeSet::new();
    for e in events {
        if let EventPayload::EdgeTraversed { from, edges, .. } = &e.payload {
            let execution = index.routed_by(from, e.seq).map(|x| x.ordinal);
            for a in edges {
                hops.insert((a.clone(), from.clone(), execution));
            }
        }
    }
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for (address, _, _) in hops {
        *counts.entry(address).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(edge, count)| EdgeCount { edge, count })
        .collect()
}

/// The inputs each join went on without, minus those a later revival
/// brought back.
fn joins_dead(events: &[Event]) -> Vec<JoinDead> {
    let mut dead: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for e in events {
        match &e.payload {
            EventPayload::JoinInputDead { node, sources } => {
                dead.entry(node.clone())
                    .or_default()
                    .extend(sources.iter().cloned());
            }
            EventPayload::JoinInputRevived { node, sources, .. } => {
                if let Some(set) = dead.get_mut(node) {
                    for s in sources {
                        set.remove(s);
                    }
                }
            }
            _ => {}
        }
    }
    dead.into_iter()
        .filter(|(_, s)| !s.is_empty())
        .map(|(join, s)| JoinDead {
            join,
            inputs: s.into_iter().collect(),
        })
        .collect()
}

fn routes(run_dir: &Path) -> Vec<RouteTrace> {
    crate::diagnostics::read(run_dir)
        .into_iter()
        .filter_map(|d| match d {
            Diagnostic::Route(r) => Some(RouteTrace {
                node: r.node,
                visit: r.execution,
                item: r.item,
                mode: r.mode,
                checks: r.checks,
                chosen: r.chosen,
            }),
            _ => None,
        })
        .collect()
}

/// The `template_empty` records of the sidecar, in file order.
fn templates_empty(run_dir: &Path) -> Vec<EmptyRef> {
    crate::diagnostics::index::read_raw(run_dir)
        .into_iter()
        .filter(|v| {
            v.get("v").and_then(Value::as_u64) == Some(u64::from(crate::diagnostics::VERSION))
                && v.get("kind").and_then(Value::as_str) == Some(TEMPLATE_EMPTY_KIND)
        })
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

fn check(
    name: impl Into<String>,
    passed: bool,
    expected: String,
    observed: String,
) -> ExpectationResult {
    ExpectationResult {
        check: name.into(),
        passed,
        expected,
        observed,
    }
}

fn list(items: &[impl AsRef<str>]) -> String {
    if items.is_empty() {
        return "none".to_string();
    }
    items
        .iter()
        .map(|s| s.as_ref())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The finish node the run ended at: the last settled finish execution.
fn finish_node(path: &[&str], playbook: &Playbook) -> Option<String> {
    let finishes: BTreeSet<&str> = playbook
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Finish { .. }))
        .map(|n| n.id.as_str())
        .collect();
    path.iter()
        .rev()
        .find(|n| finishes.contains(*n))
        .map(|s| s.to_string())
}

/// The value an output check reads: the node's output text, or one field
/// of it as JSON (a string as itself, anything else as JSON text).
fn output_value(text: &str, field: Option<&str>) -> Option<String> {
    match field {
        None => Some(text.to_string()),
        Some(f) => serde_json::from_str::<Value>(text.trim())
            .ok()
            .and_then(|v| v.get(f).cloned())
            .map(|v| match v {
                Value::String(s) => s,
                other => other.to_string(),
            }),
    }
}

fn output_check(
    o: &apb_core::eval::OutputCheck,
    outputs: &BTreeMap<String, String>,
) -> ExpectationResult {
    let name = match &o.field {
        Some(f) => format!("outputs[{}.{f}]", o.node),
        None => format!("outputs[{}]", o.node),
    };
    let mut want = Vec::new();
    if let Some(eq) = &o.equals {
        want.push(format!("equals `{eq}`"));
    }
    if let Some(p) = &o.matches {
        want.push(format!("matches `{p}`"));
    }
    if let Some(p) = &o.not_matches {
        want.push(format!("does not match `{p}`"));
    }
    if o.non_empty == Some(true) {
        want.push("non-empty".to_string());
    }
    let expected = list(&want);
    let Some(value) = outputs
        .get(&o.node)
        .and_then(|t| output_value(t, o.field.as_deref()))
    else {
        let why = if outputs.contains_key(&o.node) {
            "the output has no such field"
        } else {
            "no output recorded"
        };
        return check(name, false, expected, why.to_string());
    };
    let regex = |p: &str| regex::Regex::new(p).map(|r| r.is_match(&value));
    let mut ok = o.equals.as_ref().is_none_or(|eq| value.trim() == eq.trim())
        && (o.non_empty != Some(true) || !value.trim().is_empty());
    for (pattern, want) in [(&o.matches, true), (&o.not_matches, false)] {
        if let Some(p) = pattern {
            ok &= regex(p).is_ok_and(|m| m == want);
        }
    }
    check(name, ok, expected, format!("`{value}`"))
}

/// What [`build`] reads besides the run directory.
pub struct ReportInput<'a> {
    pub subject: Subject,
    pub scenario: &'a LoadedScenario,
    /// The version the run executed.
    pub playbook: &'a Playbook,
    pub drive: DriveOutcome,
}

/// The report of a driven scenario run, from its run directory and what
/// the drive saw.
pub fn build(input: ReportInput<'_>) -> ScenarioReport {
    let ReportInput {
        subject,
        scenario,
        playbook,
        drive,
    } = input;
    let run_dir = drive.run_dir.clone();
    let events = crate::event::read_all_lossy_tail(&run_dir).unwrap_or_default();
    let index = crate::identity::index(&events);
    let executions = executions(&events, &index, &drive.answers);
    let path: Vec<&str> = executions
        .iter()
        .filter(|x| !x.cancelled)
        .map(|x| x.node.as_str())
        .collect();
    let outcome = outcome(&events);
    let finish = finish_node(&path, playbook);
    let edges_taken = edges_taken(&events, &index);
    let joins_dead = joins_dead(&events);
    let templates = match scenario.scenario.expect.templates_empty {
        TemplatesEmpty::Ignore => Vec::new(),
        _ => templates_empty(&run_dir),
    };
    let expect = &scenario.scenario.expect;
    let mut checks = Vec::new();

    let accepted = expect
        .run
        .as_ref()
        .map(|r| r.outcome.clone())
        .unwrap_or_else(|| vec!["succeeded".to_string()]);
    checks.push(check(
        "run.outcome",
        accepted.contains(&outcome),
        format!("one of {}", list(&accepted)),
        outcome.clone(),
    ));
    if let Some(want) = &expect.finish {
        checks.push(check(
            "finish",
            finish.as_deref() == Some(want.as_str()),
            want.clone(),
            finish.clone().unwrap_or_else(|| "none".to_string()),
        ));
    }
    if let Some(route) = &expect.route {
        if !route.path.is_empty() {
            checks.push(check(
                "route.path",
                route
                    .path
                    .iter()
                    .map(String::as_str)
                    .eq(path.iter().copied()),
                route.path.join(" -> "),
                path.join(" -> "),
            ));
        }
        if !route.visits.is_empty() {
            let present = route.visits.iter().all(|v| path.contains(&v.as_str()));
            let ordered = !route.in_order || {
                let mut it = path.iter();
                route.visits.iter().all(|v| it.any(|s| s == v))
            };
            let order = if route.in_order { " in order" } else { "" };
            checks.push(check(
                "route.visits",
                present && ordered,
                format!("{}{order}", route.visits.join(", ")),
                path.join(" -> "),
            ));
        }
        if !route.not_visits.is_empty() {
            let hit: Vec<&String> = route
                .not_visits
                .iter()
                .filter(|v| path.contains(&v.as_str()))
                .collect();
            checks.push(check(
                "route.not_visits",
                hit.is_empty(),
                format!("none of {}", route.not_visits.join(", ")),
                format!("visited {}", list(&hit)),
            ));
        }
    }
    for (node, want) in &expect.visits {
        let n = path.iter().filter(|s| **s == node.as_str()).count();
        checks.push(check(
            format!("visits[{node}]"),
            n == *want as usize,
            want.to_string(),
            n.to_string(),
        ));
    }
    let taken = |addr: &str| {
        edges_taken
            .iter()
            .find(|e| e.edge == addr)
            .map_or(0, |e| e.count)
    };
    for (addr, want) in &expect.edges.taken {
        let n = taken(addr);
        checks.push(check(
            format!("edges.taken[{addr}]"),
            n == *want,
            want.to_string(),
            n.to_string(),
        ));
    }
    for addr in &expect.edges.not_taken {
        let n = taken(addr);
        checks.push(check(
            format!("edges.not_taken[{addr}]"),
            n == 0,
            "0".to_string(),
            n.to_string(),
        ));
    }
    for (join, want) in &expect.joins.dead {
        let seen: Vec<String> = joins_dead
            .iter()
            .find(|j| &j.join == join)
            .map(|j| j.inputs.clone())
            .unwrap_or_default();
        let want_set: BTreeSet<&String> = want.iter().collect();
        let seen_set: BTreeSet<&String> = seen.iter().collect();
        checks.push(check(
            format!("joins.dead[{join}]"),
            want_set == seen_set,
            list(want),
            list(&seen),
        ));
    }
    if !expect.outputs.is_empty() {
        let state = crate::state::RunState::fold(&events);
        for o in &expect.outputs {
            checks.push(output_check(o, &state.outputs));
        }
    }
    if !expect.run_outputs.is_empty() {
        let projected = crate::run_outputs::of_run(&run_dir, &events);
        for (name, want) in &expect.run_outputs {
            let seen = projected
                .outputs
                .iter()
                .find(|o| &o.name == name)
                .and_then(|o| serde_json::to_value(o.state).ok())
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "not declared".to_string());
            checks.push(check(
                format!("run_outputs[{name}]"),
                seen == want.state,
                want.state.clone(),
                seen,
            ));
        }
    }
    if expect.templates_empty == TemplatesEmpty::Error {
        let seen: Vec<String> = templates
            .iter()
            .map(|t| format!("{} visit {}: {} ({})", t.node, t.visit, t.reference, t.why))
            .collect();
        checks.push(check(
            "templates_empty",
            seen.is_empty(),
            "none".to_string(),
            list(&seen),
        ));
    }
    if scenario.scenario.on_extra == OnExtra::Error {
        let seen: Vec<String> = drive
            .answers
            .extra
            .iter()
            .map(AnswerRef::describe)
            .collect();
        checks.push(check(
            "unused_answers",
            seen.is_empty(),
            "none".to_string(),
            list(&seen),
        ));
    }

    let (verdict, reason) = if let Some(why) = &drive.invalid {
        (Verdict::Invalid, Some(why.clone()))
    } else if outcome == "incomplete" {
        (
            Verdict::Incomplete,
            Some("the run did not end: no run_finished in its journal".to_string()),
        )
    } else if checks.iter().all(|c| c.passed) {
        (Verdict::Passed, None)
    } else {
        (Verdict::Failed, None)
    };
    ScenarioReport {
        run_id: Some(drive.run_id),
        level: subject.level,
        scenario: subject.scenario,
        scenario_digest: subject.scenario_digest,
        playbook: subject.playbook,
        version: subject.version,
        playbook_digest: subject.playbook_digest,
        verdict,
        reason,
        outcome: Some(outcome),
        finish,
        checks,
        executions,
        routes: routes(&run_dir),
        edges_taken,
        joins_dead,
        templates_empty: templates,
        answers: drive.answers,
        boundaries: drive.boundaries,
        decision_requests: drive.decision_requests,
        cost_usd: drive.cost_usd,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use apb_core::scenario::LoadedScenario;
    use serde_json::json;

    /// A loop `a -> b -> a -> done`, with `failed` never reached.
    pub(crate) const PLAYBOOK: &str = "schema: 2\nid: loop\nname: loop\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: done, type: finish, outcome: success }\n  - { id: failed, type: finish, outcome: failure }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { id: again, from: b, to: a, max_traversals: 1 }\n  - { from: a, to: done }\n  - { from: a, to: failed, condition: { type: node_status, node: a, equals: failure } }\n";

    /// The journal of that loop: `a` fails once (transient) and passes on
    /// its second attempt, `b` passes, `a` passes again, the join `done`
    /// goes on without `b`, the run succeeds.
    pub(crate) fn green_journal() -> Vec<Value> {
        let attempt = |seq: u64, node: &str, n: u32| json!({"seq": seq, "ts": seq, "type": "attempt_started", "node": node, "attempt": n, "agent": "scenario"});
        let finished = |seq: u64, node: &str, n: u32, status: &str, kind: Option<&str>| {
            let mut v = json!({"seq": seq, "ts": seq, "type": "attempt_finished", "node": node, "attempt": n, "status": status});
            if let Some(k) = kind {
                v["failure_kind"] = json!(k);
            }
            v
        };
        let node_started = |seq: u64, node: &str| json!({"seq": seq, "ts": seq, "type": "node_started", "node": node, "attempt": 1});
        let node_finished = |seq: u64, node: &str, output: &str| json!({"seq": seq, "ts": seq, "type": "node_finished", "node": node, "status": "succeeded", "attempt": 1, "output": output});
        let edge = |seq: u64, from: &str, to: &str, addr: &str| json!({"seq": seq, "ts": seq, "type": "edge_traversed", "from": from, "to": to, "edges": [addr]});
        vec![
            json!({"seq": 1, "ts": 1, "type": "run_started", "playbook": "loop", "version": "1.0.0"}),
            node_started(2, "a"),
            attempt(3, "a", 1),
            finished(4, "a", 1, "failed", Some("transient")),
            attempt(5, "a", 2),
            finished(6, "a", 2, "succeeded", None),
            node_finished(7, "a", r#"{"pushed":"no"}"#),
            edge(8, "a", "b", "e:a>b#1"),
            node_started(9, "b"),
            attempt(10, "b", 1),
            finished(11, "b", 1, "succeeded", None),
            node_finished(12, "b", "ok"),
            edge(13, "b", "a", "id:again"),
            node_started(14, "a"),
            attempt(15, "a", 1),
            finished(16, "a", 1, "succeeded", None),
            node_finished(17, "a", r#"{"pushed":"no"}"#),
            json!({"seq": 18, "ts": 18, "type": "join_input_dead", "node": "done", "sources": ["b"]}),
            edge(19, "a", "done", "e:a>done#1"),
            node_finished(20, "done", ""),
            json!({"seq": 21, "ts": 21, "type": "run_finished", "outcome": "succeeded"}),
        ]
    }

    pub(crate) fn write_run(dir: &Path, journal: &[Value]) {
        std::fs::create_dir_all(dir).unwrap();
        let lines: String = journal.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(dir.join("events.jsonl"), lines).unwrap();
    }

    fn write_sidecar(dir: &Path) {
        let route = json!({"v": 1, "kind": "route_evaluated", "after_seq": 7, "key": "a|1", "node": "a", "execution": 1, "mode": "one_of",
            "checks": [{"edge": "e:a>failed#1", "rank": 2, "kind": "node_status", "result": "not_matched", "observed": "succeeded"}],
            "chosen": ["e:a>b#1"]});
        let empty = json!({"v": 1, "kind": TEMPLATE_EMPTY_KIND, "after_seq": 9, "key": "b|1|x", "node": "b", "visit": 1,
            "reference": "nodes.x.output", "why": "never ran"});
        std::fs::write(
            dir.join(crate::diagnostics::FILE),
            format!("{route}\n{empty}\n"),
        )
        .unwrap();
    }

    fn key(node: &str, visit: u32, attempt: u32) -> AnswerRef {
        AnswerRef {
            kind: BoundaryKind::AgentAttempt,
            key: BoundaryKey {
                node: Some(node.into()),
                visit,
                attempt: Some(attempt),
                item: None,
                part: None,
            },
        }
    }

    pub(crate) fn green_answers() -> AnswerUse {
        AnswerUse {
            used: vec![key("a", 1, 1), key("a", 1, 2), key("b", 1, 1)],
            defaults: vec![key("a", 2, 1)],
            missing: Vec::new(),
            extra: Vec::new(),
        }
    }

    /// The expectations the green journal meets, one per kind.
    pub(crate) const GREEN_EXPECT: &str = "expect:\n  run: { outcome: [succeeded] }\n  finish: done\n  route: { path: [a, b, a, done], visits: [a, done], in_order: true, not_visits: [failed] }\n  visits: { a: 2, b: 1 }\n  edges: { taken: { \"id:again\": 1 }, not_taken: [\"e:a>failed#1\"] }\n  joins: { dead: { done: [b] } }\n  outputs: [ { node: a, field: pushed, equals: \"no\" } ]\n  templates_empty: report\n";

    pub(crate) fn scenario(body: &str) -> LoadedScenario {
        let raw = format!("schema: 1\nid: green\n{body}");
        let scenario = apb_core::scenario::parse(&raw, "green").unwrap();
        LoadedScenario {
            name: "green".into(),
            path: PathBuf::from("green.yaml"),
            digest: apb_core::scenario::scenario_digest(&scenario),
            scenario,
        }
    }

    pub(crate) fn subject(s: &LoadedScenario) -> Subject {
        Subject::of(ScenarioLevel::Quick, s, "loop", "1.0.0", "sha256:pb")
    }

    fn report_of(run_dir: &Path, body: &str, drive: DriveOutcome) -> ScenarioReport {
        let s = scenario(body);
        let playbook = Playbook::from_yaml(PLAYBOOK).unwrap();
        build(ReportInput {
            subject: subject(&s),
            scenario: &s,
            playbook: &playbook,
            drive: DriveOutcome {
                run_id: "r1".into(),
                run_dir: run_dir.to_path_buf(),
                ..drive
            },
        })
    }

    fn green_drive() -> DriveOutcome {
        DriveOutcome {
            answers: green_answers(),
            ..DriveOutcome::default()
        }
    }

    /// The report shows the trace, not only the outcome: every execution in
    /// settlement order with its visit and its attempts counted per
    /// execution (with where each answer came from), the edges by address,
    /// the dead join input, the routing decisions and the empty templates.
    #[test]
    fn the_report_traces_executions_attempts_edges_and_dead_joins() {
        let dir = tempfile::tempdir().unwrap();
        write_run(dir.path(), &green_journal());
        write_sidecar(dir.path());
        let r = report_of(dir.path(), GREEN_EXPECT, green_drive());
        assert_eq!(r.verdict, Verdict::Passed, "{:#?}", r.checks);
        assert_eq!(
            (r.outcome.as_deref(), r.finish.as_deref()),
            (Some("succeeded"), Some("done"))
        );
        type Attempts<'a> = Vec<(u32, Option<&'a str>, Option<AnswerSource>)>;
        let trace: Vec<(&str, u32, Attempts)> = r
            .executions
            .iter()
            .map(|x| {
                (
                    x.node.as_str(),
                    x.visit,
                    x.attempts
                        .iter()
                        .map(|a| (a.n, a.failure_kind.as_deref(), a.answer))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            trace,
            vec![
                (
                    "a",
                    1,
                    vec![
                        (1, Some("transient"), Some(AnswerSource::Used)),
                        (2, None, Some(AnswerSource::Used))
                    ]
                ),
                ("b", 1, vec![(1, None, Some(AnswerSource::Used))]),
                ("a", 2, vec![(1, None, Some(AnswerSource::Default))]),
                ("done", 1, vec![]),
            ]
        );
        assert_eq!(
            r.edges_taken
                .iter()
                .map(|e| (e.edge.as_str(), e.count))
                .collect::<Vec<_>>(),
            [("e:a>b#1", 1), ("e:a>done#1", 1), ("id:again", 1)]
        );
        assert_eq!(
            r.joins_dead,
            [JoinDead {
                join: "done".into(),
                inputs: vec!["b".into()]
            }]
        );
        assert_eq!(r.routes.len(), 1);
        assert_eq!((r.routes[0].node.as_str(), r.routes[0].visit), ("a", 1));
        assert_eq!(r.routes[0].checks[0].result, "not_matched");
        assert_eq!(r.templates_empty[0].reference, "nodes.x.output");
    }

    /// Each expectation is its own named check: one wrong expectation fails
    /// the verdict with exactly that name, and the rest still hold.
    #[test]
    fn each_expectation_fails_by_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        write_run(dir.path(), &green_journal());
        write_sidecar(dir.path());
        let edit = |from: &str, to: &str| {
            assert!(GREEN_EXPECT.contains(from), "{from}");
            GREEN_EXPECT.replacen(from, to, 1)
        };
        let cases = [
            (edit("[succeeded]", "[failed]"), "run.outcome"),
            (edit("finish: done", "finish: failed"), "finish"),
            (
                edit("path: [a, b, a, done]", "path: [a, b, done]"),
                "route.path",
            ),
            (
                edit("visits: [a, done]", "visits: [done, a]"),
                "route.visits",
            ),
            (
                edit("not_visits: [failed]", "not_visits: [b]"),
                "route.not_visits",
            ),
            (edit("{ a: 2, b: 1 }", "{ a: 1, b: 1 }"), "visits[a]"),
            (
                edit("\"id:again\": 1", "\"id:again\": 2"),
                "edges.taken[id:again]",
            ),
            (
                edit("not_taken: [\"e:a>failed#1\"]", "not_taken: [\"e:a>b#1\"]"),
                "edges.not_taken[e:a>b#1]",
            ),
            (edit("done: [b]", "done: [a]"), "joins.dead[done]"),
            (
                edit("equals: \"no\"", "equals: \"yes\""),
                "outputs[a.pushed]",
            ),
            (
                edit("templates_empty: report", "templates_empty: error"),
                "templates_empty",
            ),
            (
                format!("{GREEN_EXPECT}  run_outputs: {{ pr: {{ state: value }} }}\n"),
                "run_outputs[pr]",
            ),
        ];
        for (body, name) in cases {
            let r = report_of(dir.path(), &body, green_drive());
            let failed: Vec<&str> = r.failed_checks().map(|c| c.check.as_str()).collect();
            assert_eq!(failed, [name], "{body}");
            assert_eq!(r.verdict, Verdict::Failed, "{name}");
        }
        // An answer nothing used fails as `unused_answers` by default and is
        // only listed under `on_extra: warn`.
        let extra = DriveOutcome {
            answers: AnswerUse {
                extra: vec![key("b", 1, 2)],
                ..green_answers()
            },
            ..DriveOutcome::default()
        };
        let r = report_of(dir.path(), GREEN_EXPECT, extra.clone());
        let unused: Vec<&ExpectationResult> = r.failed_checks().collect();
        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0].check, "unused_answers");
        assert!(
            unused[0].observed.contains("b visit 1 attempt 2"),
            "{}",
            unused[0].observed
        );
        let r = report_of(
            dir.path(),
            &format!("on_extra: warn\n{GREEN_EXPECT}"),
            extra,
        );
        assert_eq!(r.verdict, Verdict::Passed);
        assert_eq!(r.answers.extra.len(), 1);
    }

    /// A scenario the drive stopped is `invalid` whatever its checks say; a
    /// run with no `run_finished` is `incomplete`; a run that ended on a
    /// pause reads `paused`, which a scenario may expect.
    #[test]
    fn a_stopped_or_unfinished_run_has_its_own_verdict() {
        let dir = tempfile::tempdir().unwrap();
        write_run(dir.path(), &green_journal());
        let invalid = DriveOutcome {
            invalid: Some("no answer for a visit 1 attempt 1 (agent_attempt)".into()),
            ..green_drive()
        };
        let r = report_of(dir.path(), "", invalid);
        assert_eq!(r.verdict, Verdict::Invalid);
        assert!(r.reason.unwrap().contains("a visit 1 attempt 1"));

        let mut journal = green_journal();
        journal.pop();
        write_run(dir.path(), &journal);
        let r = report_of(dir.path(), "", green_drive());
        assert_eq!(
            (r.verdict, r.outcome.as_deref()),
            (Verdict::Incomplete, Some("incomplete"))
        );

        journal.push(
            json!({"seq": 21, "ts": 21, "type": "run_paused", "reason": "budget (requests)"}),
        );
        write_run(dir.path(), &journal);
        let r = report_of(
            dir.path(),
            "expect: { run: { outcome: [paused] } }\n",
            green_drive(),
        );
        assert_eq!(
            (r.verdict, r.outcome.as_deref()),
            (Verdict::Passed, Some("paused"))
        );
    }
}
