//! The run outputs a playbook declares, projected from the journal: a pure
//! function of the events and the declaration in force for the journal's
//! last segment, computed on read and never journaled.
//!
//! Each declared source names a node. Its candidates are the node's
//! executions ([`crate::identity`]) whose `node_finished` says `succeeded`,
//! in settlement order; failed, timed-out and cancelled executions never
//! produce a value. A finish node counts like any node, so `{ node: done }`
//! projects the finish answer. `pick: last` takes the candidate settled
//! last across every source, `first` the one settled first, `all` every
//! one, so alternative branches and several finish nodes need no rule of
//! their own: the journal order decides.
//!
//! What a candidate gives:
//! - no `field`: the output text, as a JSON string;
//! - `field`: the output read as a JSON object; the key with a non-null
//!   value gives that value as JSON, a `null` gives `null`, an absent key is
//!   missing `field_absent`, an output that is no JSON object is missing
//!   `not_json_object`;
//! - `item_field` (list nodes): every item of the execution's aggregate
//!   output, in item order; an item that did not pass is missing
//!   `item_not_passed`, an item whose output was too large to keep is
//!   missing `truncated`.
//!
//! The raw `outputs` map of every surface and `{{nodes.X.output}}` are not
//! touched: the projection is a separate field, `run_outputs`.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::run_outputs::{OutputSource, Pick, RunOutput, RunOutputsDecl};
use serde::Serialize;
use serde_json::Value;

use crate::event::{Event, EventPayload};
use crate::identity::{Execution, ExecutionIndex, ListVisitRef};
use crate::result_ref::{ResultItem, ResultPart, ResultRef};

/// A run's projected outputs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunOutputs {
    /// The run is finished: the values will not change unless the run is
    /// revived or continued, which sets this back to false.
    #[serde(rename = "final")]
    pub is_final: bool,
    /// Which declaration the projection read.
    pub definition: OutputsDefinition,
    /// Every declared output, in declaration order.
    pub outputs: Vec<ProjectedOutput>,
}

/// Which declaration a projection read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputsDefinition {
    /// The stored definition the journal's last segment executed.
    Stored,
    /// The run's current `playbook.yaml`: the last segment's definition is
    /// not established (an older apb, a lost store).
    Current,
}

/// The state of one projected value.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Value,
    Null,
    Missing,
}

/// Why a value is missing.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    /// No source node has an execution.
    NotRun,
    /// Executions exist, none succeeded.
    NotSucceeded,
    /// The output is a JSON object without the field.
    FieldAbsent,
    /// The output is not a JSON object.
    NotJsonObject,
    /// The item's output was too large to keep.
    Truncated,
    /// The item did not pass.
    ItemNotPassed,
    /// The run's current `playbook.yaml` declares outputs differently from
    /// the definition it started with, and the definition in force is not
    /// established.
    DefinitionUnknown,
}

impl MissingReason {
    /// Every reason, in the order the dashboard lists them.
    pub const ALL: [MissingReason; 7] = [
        MissingReason::NotRun,
        MissingReason::NotSucceeded,
        MissingReason::FieldAbsent,
        MissingReason::NotJsonObject,
        MissingReason::Truncated,
        MissingReason::ItemNotPassed,
        MissingReason::DefinitionUnknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MissingReason::NotRun => "not_run",
            MissingReason::NotSucceeded => "not_succeeded",
            MissingReason::FieldAbsent => "field_absent",
            MissingReason::NotJsonObject => "not_json_object",
            MissingReason::Truncated => "truncated",
            MissingReason::ItemNotPassed => "item_not_passed",
            MissingReason::DefinitionUnknown => "definition_unknown",
        }
    }
}

/// One declared output with the value its pick took.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedOutput {
    pub name: String,
    pub pick: Pick,
    pub state: OutputState,
    /// The value, for state `value`. With `pick: all` the array of every
    /// candidate's value (`null` kept, missing ones left out); with an
    /// `item_field` an object from item id to value.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<MissingReason>,
    /// The result the value came from (`last` and `first`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<ResultRef>,
    /// `pick: all`: every candidate, in settlement order.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub values: Option<Vec<ProjectedValue>>,
    /// An `item_field` source: every item of the picked execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub items: Option<Vec<ProjectedItem>>,
}

/// One candidate: the value one succeeded execution gives.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedValue {
    pub state: OutputState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<MissingReason>,
    pub source: ResultRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub items: Option<Vec<ProjectedItem>>,
}

/// One item of a list execution under an `item_field` source.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedItem {
    pub id: String,
    pub state: OutputState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<MissingReason>,
    /// The item's settlement, when the journal has one in the visit.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<ResultRef>,
}

/// The projection of a run directory's journal: the declaration of the
/// stored definition its last segment executed, else the run's current
/// `playbook.yaml` (every output `definition_unknown` when that declares
/// outputs differently from the stored start definition). Read-only.
pub fn of_run(run_dir: &Path, events: &[Event]) -> RunOutputs {
    use crate::run_definitions::{DefinitionsInForce, Resolved};
    let defs = DefinitionsInForce::load(run_dir, events);
    if let Some(Resolved::Known { playbook, .. }) = defs.segments.last().map(|s| &s.resolved) {
        return project(events, &playbook.outputs, OutputsDefinition::Stored);
    }
    let current = crate::legacy_snapshot::load_run_playbook(run_dir)
        .map(|p| p.outputs)
        .unwrap_or_default();
    let mut out = project(events, &current, OutputsDefinition::Current);
    let start = defs.segments.first().and_then(|s| match &s.resolved {
        Resolved::Known { playbook, .. } => Some(&playbook.outputs),
        Resolved::Unknown(_) => None,
    });
    if start.is_some_and(|s| *s != current) {
        for o in &mut out.outputs {
            *o = missing(&o.name, o.pick, MissingReason::DefinitionUnknown);
        }
    }
    out
}

/// The projection of `events` under `decl`.
pub fn project(
    events: &[Event],
    decl: &RunOutputsDecl,
    definition: OutputsDefinition,
) -> RunOutputs {
    let journal = Journal::of(events);
    RunOutputs {
        is_final: crate::state::RunState::fold(events)
            .run_status
            .is_terminal(),
        definition,
        outputs: decl
            .iter()
            .map(|(name, o)| project_one(&journal, name, o))
            .collect(),
    }
}

/// What a `type: playbook` node with `output: outputs` hands its parent:
/// one compact JSON object of every output with a value (`null` kept),
/// `pick: all` as an array; missing outputs are left out.
pub fn child_object(outputs: &RunOutputs) -> String {
    let map: serde_json::Map<String, Value> = outputs
        .outputs
        .iter()
        .filter_map(|o| match o.state {
            OutputState::Missing => None,
            _ => Some((o.name.clone(), o.value.clone().unwrap_or(Value::Null))),
        })
        .collect();
    Value::Object(map).to_string()
}

/// The journal facts a projection reads, gathered in one pass.
struct Journal<'a> {
    index: ExecutionIndex,
    /// `node_finished` output by seq.
    finished: BTreeMap<u64, &'a str>,
    /// `attempt_started` per node: (seq, item).
    attempts: BTreeMap<&'a str, Vec<(u64, Option<&'a str>)>>,
    /// `list_item_settled` per node: (seq, item).
    settled: BTreeMap<&'a str, Vec<(u64, &'a str)>>,
    visits: Vec<ListVisitRef>,
}

impl<'a> Journal<'a> {
    fn of(events: &'a [Event]) -> Self {
        let mut finished = BTreeMap::new();
        let mut attempts: BTreeMap<&str, Vec<(u64, Option<&str>)>> = BTreeMap::new();
        let mut settled: BTreeMap<&str, Vec<(u64, &str)>> = BTreeMap::new();
        for e in events {
            match &e.payload {
                EventPayload::NodeFinished { output, .. } => {
                    finished.insert(e.seq, output.as_str());
                }
                EventPayload::AttemptStarted { node, item, .. } => attempts
                    .entry(node.as_str())
                    .or_default()
                    .push((e.seq, item.as_deref())),
                EventPayload::ListItemSettled { node, item, .. } => settled
                    .entry(node.as_str())
                    .or_default()
                    .push((e.seq, item.as_str())),
                _ => {}
            }
        }
        Journal {
            index: crate::identity::index(events),
            finished,
            attempts,
            settled,
            visits: crate::identity::list_visits(events),
        }
    }

    /// The last attempt of `item` (`None`: the node's own) inside `x`.
    fn attempt_in(&self, x: &Execution, item: Option<&str>) -> Option<u64> {
        let end = x.end_seq?;
        self.attempts
            .get(x.node.as_str())?
            .iter()
            .rev()
            .find(|(seq, i)| *seq >= x.begin_seq && *seq <= end && *i == item)
            .map(|(seq, _)| *seq)
    }

    /// The latest `attempt_started` of `x` (of `item`) at or before `seq`.
    fn attempt_before(&self, x: &Execution, item: Option<&str>, seq: u64) -> Option<u64> {
        self.attempts
            .get(x.node.as_str())?
            .iter()
            .rev()
            .find(|(s, i)| *s >= x.begin_seq && *s <= seq && *i == item)
            .map(|(s, _)| *s)
    }

    /// The list visit `x` belongs to: the latest one started before it ended.
    fn visit_of(&self, x: &Execution) -> Option<&ListVisitRef> {
        let end = x.end_seq?;
        self.visits
            .iter()
            .rev()
            .find(|v| v.node == x.node && v.started_seq <= end)
    }

    /// The latest settlement of `item` in `visit`, no later than `x`'s end.
    fn settlement_of(&self, x: &Execution, visit: &ListVisitRef, item: &str) -> Option<u64> {
        let end = x.end_seq?;
        self.settled
            .get(x.node.as_str())?
            .iter()
            .rev()
            .find(|(seq, i)| *i == item && *seq >= visit.started_seq && *seq <= end)
            .map(|(seq, _)| *seq)
    }
}

fn missing(name: &str, pick: Pick, reason: MissingReason) -> ProjectedOutput {
    ProjectedOutput {
        name: name.to_string(),
        pick,
        state: OutputState::Missing,
        value: None,
        reason: Some(reason),
        source: None,
        values: None,
        items: None,
    }
}

fn project_one(j: &Journal<'_>, name: &str, decl: &RunOutput) -> ProjectedOutput {
    let mut ran = false;
    let mut candidates: Vec<ProjectedValue> = Vec::new();
    for source in decl.from.as_slice() {
        let executions = j.index.executions(&source.node);
        ran |= !executions.is_empty();
        for x in executions
            .iter()
            .filter(|x| x.status.as_deref() == Some("succeeded"))
        {
            if let Some(c) = candidate(j, source, x) {
                candidates.push(c);
            }
        }
    }
    candidates.sort_by_key(|c| c.source.settlement_seq);
    let picked = match decl.pick {
        Pick::Last => candidates.pop(),
        Pick::First => (!candidates.is_empty()).then(|| candidates.remove(0)),
        Pick::All if candidates.is_empty() => None,
        Pick::All => {
            let value: Vec<Value> = candidates
                .iter()
                .filter(|c| c.state != OutputState::Missing)
                .map(|c| c.value.clone().unwrap_or(Value::Null))
                .collect();
            return ProjectedOutput {
                name: name.to_string(),
                pick: decl.pick,
                state: OutputState::Value,
                value: Some(Value::Array(value)),
                reason: None,
                source: None,
                values: Some(candidates),
                items: None,
            };
        }
    };
    let Some(c) = picked else {
        let reason = if ran {
            MissingReason::NotSucceeded
        } else {
            MissingReason::NotRun
        };
        return missing(name, decl.pick, reason);
    };
    ProjectedOutput {
        name: name.to_string(),
        pick: decl.pick,
        state: c.state,
        value: c.value,
        reason: c.reason,
        source: Some(c.source),
        values: None,
        items: c.items,
    }
}

/// The value one succeeded execution gives; `None` when the journal does
/// not hold its `node_finished` (it always does for a closed execution).
fn candidate(j: &Journal<'_>, source: &OutputSource, x: &Execution) -> Option<ProjectedValue> {
    let end = x.end_seq?;
    let output = *j.finished.get(&end)?;
    let source_ref = |part| ResultRef {
        execution: x.reference(),
        item: None,
        settlement_seq: end,
        part,
        produced_by_attempt: j.attempt_in(x, None),
    };
    if let Some(item_field) = &source.item_field {
        return Some(item_candidate(
            j,
            x,
            output,
            item_field,
            source_ref(ResultPart::Fields),
        ));
    }
    let Some(field) = &source.field else {
        return Some(ProjectedValue {
            state: OutputState::Value,
            value: Some(Value::String(output.to_string())),
            reason: None,
            source: source_ref(ResultPart::Output),
            items: None,
        });
    };
    let (state, value, reason) =
        field_of(serde_json::from_str::<Value>(output).ok().as_ref(), field);
    Some(ProjectedValue {
        state,
        value,
        reason,
        source: source_ref(ResultPart::Fields),
        items: None,
    })
}

/// One key of an output object: value, `null`, or missing with the reason.
fn field_of(
    output: Option<&Value>,
    field: &str,
) -> (OutputState, Option<Value>, Option<MissingReason>) {
    match output.and_then(Value::as_object) {
        None => (
            OutputState::Missing,
            None,
            Some(MissingReason::NotJsonObject),
        ),
        Some(obj) => match obj.get(field) {
            None => (OutputState::Missing, None, Some(MissingReason::FieldAbsent)),
            Some(Value::Null) => (OutputState::Null, None, None),
            Some(v) => (OutputState::Value, Some(v.clone()), None),
        },
    }
}

/// An `item_field` candidate: every item of a list execution's aggregate
/// output (`{ items: [ { id, state, output } ] }`), in item order.
fn item_candidate(
    j: &Journal<'_>,
    x: &Execution,
    output: &str,
    item_field: &str,
    source: ResultRef,
) -> ProjectedValue {
    let aggregate = serde_json::from_str::<Value>(output).ok();
    let Some(entries) = aggregate
        .as_ref()
        .and_then(|v| v.get("items"))
        .and_then(Value::as_array)
    else {
        return ProjectedValue {
            state: OutputState::Missing,
            value: None,
            reason: Some(MissingReason::NotJsonObject),
            source,
            items: None,
        };
    };
    let visit = j.visit_of(x);
    let items: Vec<ProjectedItem> = entries
        .iter()
        .map(|entry| {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let item_output = entry.get("output");
            let (state, value, reason) =
                if entry.get("state").and_then(Value::as_str) != Some("passed") {
                    (
                        OutputState::Missing,
                        None,
                        Some(MissingReason::ItemNotPassed),
                    )
                } else if item_output.is_some_and(is_truncated) {
                    (OutputState::Missing, None, Some(MissingReason::Truncated))
                } else {
                    field_of(item_output, item_field)
                };
            // A reopened visit keeps an item an earlier execution passed:
            // the citation is the execution and attempt that settled it.
            let source = visit.and_then(|v| {
                let settlement_seq = j.settlement_of(x, v, &id)?;
                let owner = j
                    .index
                    .containing(&x.node, settlement_seq)
                    .filter(|o| o.end_seq.is_some())
                    .unwrap_or(x);
                Some(ResultRef {
                    execution: owner.reference(),
                    item: Some(ResultItem {
                        visit: v.clone(),
                        id: id.clone(),
                    }),
                    settlement_seq,
                    part: ResultPart::Fields,
                    produced_by_attempt: j.attempt_before(owner, Some(&id), settlement_seq),
                })
            });
            ProjectedItem {
                id,
                state,
                value,
                reason,
                source,
            }
        })
        .collect();
    let value: serde_json::Map<String, Value> = items
        .iter()
        .filter(|i| i.state != OutputState::Missing)
        .map(|i| (i.id.clone(), i.value.clone().unwrap_or(Value::Null)))
        .collect();
    ProjectedValue {
        state: OutputState::Value,
        value: Some(Value::Object(value)),
        reason: None,
        source,
        items: Some(items),
    }
}

/// An item output the list kept only as `{"truncated": true}`.
fn is_truncated(output: &Value) -> bool {
    output
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.get("truncated") == Some(&Value::Bool(true)))
}

impl crate::run_view::RunView {
    /// The run's projected outputs; `None` when its definition declares
    /// none, so a run without them reads exactly as before.
    pub fn run_outputs(&self, run_dir: &Path) -> Option<RunOutputs> {
        Some(of_run(run_dir, &self.events)).filter(|o| !o.outputs.is_empty())
    }
}
