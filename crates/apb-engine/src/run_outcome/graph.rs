//! The run graph: what the journal proves about every node and edge of the
//! definitions a run executed, for the run page, `run_report.route`, `apb
//! runs <id>` and the overlay of a run on a playbook version.
//!
//! An edge is `taken` only when an established delivery names it, `not
//! taken` only when every execution of its source in that definition ended
//! with a complete routing decision that did not take it, and `not
//! established` otherwise: no count, never drawn as not taken. Each
//! traversal is read in the segment in force at its seq; a segment whose
//! definition is unknown has no edge states at all.
//!
//! Built from original evidence; a surface applies the run's redactor to
//! the serialized graph before it clips or shows anything
//! ([`RunGraph::present`]).

use std::collections::BTreeMap;

use apb_core::edge_address::{EdgeAddress, edge_addresses};
use serde::Serialize;

use super::route_evidence::{PolicyOwner, RouteEvidence};
use crate::diagnostics::Diagnostic;
use crate::event::Event;
use crate::run_definitions::{BoundaryKind, DefinitionsInForce, Resolved};

/// One stretch of the journal executed under one definition.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefinitionSegmentView {
    pub index: usize,
    pub from_seq: u64,
    /// `None` for the segment in force now.
    pub to_seq: Option<u64>,
    /// `start`, `resume`, `patch`, or `none` for a journal without a
    /// boundary.
    pub opened_by: String,
    /// The digest of its stored definition, when it is known.
    pub digest: Option<String>,
    pub known: bool,
    /// Why it is not known: `definition_not_journaled`,
    /// `definition_not_stored` or `definition_unreadable`.
    pub reason: Option<String>,
    /// A resume recorded a definition mismatch against it (detail only).
    pub mismatch_recorded: bool,
}

/// One node over the whole run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeRun {
    pub node: String,
    /// Executions begun, race losers left out.
    pub executions: u32,
    /// Of those, the ones still open.
    pub open: u32,
    /// `join: any` race losers.
    pub cancelled: u32,
    /// Ended executions by the status they ended with.
    pub by_status: BTreeMap<String, u32>,
    /// The status the run reports for the node now (liveness included).
    pub status: Option<String>,
    /// The status its latest ended execution recorded.
    pub last_status: Option<String>,
    /// Whether a known definition of the run declares the node.
    pub in_definition: bool,
}

/// What the journal established about one edge of one known segment.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EdgeState {
    /// Established deliveries over it in this segment, counted once per
    /// source execution. Counts are per segment: a reader combining
    /// segments (several of the same bytes, or the overlay) sums them.
    Taken { count: u32 },
    /// Every execution of its source in the segment ended with a complete
    /// routing decision that took another way (or the source never ran in
    /// a closed segment).
    NotTaken,
    /// Neither is proven. Carries no count.
    NotEstablished { reason: String },
}

/// One edge of one known segment and its state.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeRun {
    pub segment: usize,
    pub edge: EdgeAddress,
    /// The address as the canvas and the journal spell it.
    pub rendered: String,
    pub from: String,
    pub to: String,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub state: EdgeState,
}

/// One `edge_traversed` record and what it resolved to.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraversalRow {
    pub seq: u64,
    pub from: String,
    pub to: String,
    /// The ordinal of the source's execution that routed it.
    pub execution: Option<u32>,
    pub via_policy: bool,
    /// `defaults` or `fork:<id>` for a policy delivery.
    pub policy: Option<String>,
    /// The record spends a bounded edge's budget.
    pub counted: bool,
    pub edges: Vec<EdgeAddress>,
    pub segment: Option<usize>,
    /// It resolved to edges of a known definition (or is a policy hop).
    pub resolved: bool,
    /// Why it did not resolve.
    pub reason: Option<String>,
}

/// The run graph.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunGraph {
    pub segments: Vec<DefinitionSegmentView>,
    /// The latest known segment: the definition a run canvas draws, so
    /// edge states are matched by address against the bytes that ran.
    pub shown_segment: Option<usize>,
    pub nodes: Vec<NodeRun>,
    pub edges: Vec<EdgeRun>,
    pub traversals: Vec<TraversalRow>,
    pub judge_nodes: Vec<super::judge_answers::JudgeNodeAnswers>,
    pub judge_edges: Vec<super::judge_answers::JudgeEdgeAnswer>,
    pub not_run: Vec<super::route_explain::NotRun>,
    /// The run journaled its end.
    pub ended: bool,
}

/// What [`RunGraph::build`] reads.
pub struct GraphInput<'a> {
    pub events: &'a [Event],
    pub definitions: &'a DefinitionsInForce,
    pub diagnostics: &'a [Diagnostic],
    /// The status the run reports per node ([`crate::run_view::RunView::nodes`]).
    pub statuses: &'a BTreeMap<String, String>,
}

impl RunGraph {
    pub fn build(input: &GraphInput<'_>) -> Self {
        let ev = RouteEvidence::build(input.events, input.definitions, input.diagnostics);
        Self::from_evidence(&ev, input.statuses)
    }

    pub(crate) fn from_evidence(
        ev: &RouteEvidence<'_>,
        statuses: &BTreeMap<String, String>,
    ) -> Self {
        let segments = segment_views(ev.definitions);
        let shown_segment = segments.iter().rposition(|s| s.known);
        RunGraph {
            nodes: node_runs(ev, statuses),
            edges: edge_runs(ev),
            traversals: traversal_rows(ev),
            judge_nodes: super::judge_answers::judge_nodes(ev),
            judge_edges: super::judge_answers::judge_edges(ev),
            not_run: super::route_explain::explain(ev),
            ended: ev.ended,
            shown_segment,
            segments,
        }
    }

    /// The graph as a surface shows it: serialized, every string through
    /// the run's redactor (which withholds the whole value when the
    /// process cannot resolve the run's secrets), and only then clipped.
    pub fn present(&self, redactor: &crate::run_redact::RunRedactor) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if redactor.withholds() {
            return serde_json::Value::String(crate::run_redact::WITHHELD.into());
        }
        redactor.redact_json(&mut v);
        clip_strings(&mut v, PRESENT_MAX);
        v
    }

    /// The route as plain lines for a terminal or a report: the
    /// definitions, every edge state, the judge answers and why each node
    /// did not run. Built from original values: print them inside the
    /// run's redaction.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let unknown: Vec<String> = self
            .segments
            .iter()
            .filter(|s| !s.known)
            .map(|s| {
                format!(
                    "segment {} from seq {} ({})",
                    s.index,
                    s.from_seq,
                    s.reason.as_deref().unwrap_or("unknown")
                )
            })
            .collect();
        out.push(match unknown.is_empty() {
            true => format!(
                "definitions: {} segment(s), all established",
                self.segments.len()
            ),
            false => format!(
                "definitions: {} segment(s); not established: {}",
                self.segments.len(),
                unknown.join(", ")
            ),
        });
        for e in &self.edges {
            let state = match &e.state {
                EdgeState::Taken { count } => format!("taken {count}"),
                EdgeState::NotTaken => "not taken".to_string(),
                EdgeState::NotEstablished { reason } => format!("not established ({reason})"),
            };
            out.push(format!(
                "edge {} [segment {}]: {state}",
                e.rendered, e.segment
            ));
        }
        for t in self.traversals.iter().filter(|t| !t.resolved) {
            out.push(format!(
                "hop {} -> {} at seq {}: not established ({})",
                t.from,
                t.to,
                t.seq,
                t.reason.as_deref().unwrap_or("cannot_tell")
            ));
        }
        for j in &self.judge_nodes {
            let mut line = format!(
                "judge {} #{}: decided by {}",
                j.node,
                j.execution,
                j.decided_by.as_deref().unwrap_or("unknown")
            );
            for q in &j.questions {
                let routed = q
                    .routed_value
                    .as_ref()
                    .map_or("none".to_string(), |v| v.to_string());
                let said: Vec<String> = q
                    .answers
                    .iter()
                    .map(|a| {
                        let value = a
                            .value
                            .as_ref()
                            .map_or("none".to_string(), |v| v.to_string());
                        match &a.comparison {
                            Some(c) => format!("{} {value} ({})", a.source, c.label),
                            None => format!("{} {value}", a.source),
                        }
                    })
                    .collect();
                line.push_str(&format!("; {}: routed {routed}", q.id));
                if !said.is_empty() {
                    line.push_str(&format!(", answered {}", said.join(", ")));
                }
            }
            out.push(line);
        }
        for j in &self.judge_edges {
            let taken = match j.taken {
                Some(true) => "taken",
                Some(false) => "not taken",
                None => "not established",
            };
            out.push(format!(
                "judge edge {} #{}: {}, {taken}{}",
                j.rendered.as_deref().unwrap_or("unknown"),
                j.execution,
                j.label,
                if j.applied { "" } else { " (shadow)" }
            ));
        }
        for n in &self.not_run {
            out.push(format!(
                "not run {}: {} {}: {}",
                n.node,
                n.class.as_str(),
                n.code.as_str(),
                n.detail
            ));
            for f in &n.inputs {
                let entry = match &f.entry {
                    super::route_explain::Entry::Edge { rendered, .. } => {
                        format!("edge {rendered}")
                    }
                    super::route_explain::Entry::Policy { policy, source } => match source {
                        Some(s) => format!("{policy} policy of {s}"),
                        None => format!("{policy} policy"),
                    },
                };
                out.push(format!(
                    "  {entry}: {} {}: {}",
                    f.class.as_str(),
                    f.code.as_str(),
                    f.detail
                ));
            }
        }
        out
    }

    /// The edges of segment `segment`.
    pub fn edges_of(&self, segment: usize) -> impl Iterator<Item = &EdgeRun> {
        self.edges.iter().filter(move |e| e.segment == segment)
    }
}

/// The longest string a presented graph keeps, after redaction.
pub const PRESENT_MAX: usize = 240;

fn clip_strings(v: &mut serde_json::Value, max: usize) {
    match v {
        serde_json::Value::String(s) if s.chars().count() > max => {
            let mut t: String = s.chars().take(max).collect();
            t.push_str("...");
            *s = t;
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| clip_strings(x, max)),
        serde_json::Value::Object(m) => m.values_mut().for_each(|x| clip_strings(x, max)),
        _ => {}
    }
}

fn segment_views(defs: &DefinitionsInForce) -> Vec<DefinitionSegmentView> {
    defs.segments
        .iter()
        .enumerate()
        .map(|(index, s)| {
            let (digest, known, reason) = match &s.resolved {
                Resolved::Known { digest, .. } => (Some(digest.clone()), true, None),
                Resolved::Unknown(r) => (
                    s.boundary.as_ref().and_then(|b| b.definition.clone()),
                    false,
                    Some(r.as_str().to_string()),
                ),
            };
            DefinitionSegmentView {
                index,
                from_seq: s.from_seq,
                to_seq: s.to_seq,
                opened_by: match s.boundary.as_ref().map(|b| b.kind) {
                    Some(BoundaryKind::Start) => "start",
                    Some(BoundaryKind::Resume) => "resume",
                    Some(BoundaryKind::Patch) => "patch",
                    None => "none",
                }
                .into(),
                digest,
                known,
                reason,
                mismatch_recorded: s.mismatch_recorded,
            }
        })
        .collect()
}

fn node_runs(ev: &RouteEvidence<'_>, statuses: &BTreeMap<String, String>) -> Vec<NodeRun> {
    let mut ids: Vec<String> = Vec::new();
    let mut declared = std::collections::BTreeSet::new();
    for i in 0..ev.definitions.segments.len() {
        if let Some(pb) = ev.playbook(i) {
            for n in &pb.nodes {
                declared.insert(n.id.clone());
                if !ids.contains(&n.id) {
                    ids.push(n.id.clone());
                }
            }
        }
    }
    for (node, _) in ev.index.nodes() {
        if !ids.iter().any(|x| x == node) {
            ids.push(node.to_string());
        }
    }
    ids.into_iter()
        .map(|node| {
            let execs = ev.index.executions(&node);
            let begun: Vec<_> = execs.iter().filter(|x| !x.cancelled).collect();
            let mut by_status = BTreeMap::new();
            for x in begun.iter().filter(|x| !x.is_open()) {
                if let Some(s) = &x.status {
                    *by_status.entry(s.clone()).or_insert(0) += 1;
                }
            }
            NodeRun {
                executions: begun.len() as u32,
                open: begun.iter().filter(|x| x.is_open()).count() as u32,
                cancelled: (execs.len() - begun.len()) as u32,
                last_status: begun
                    .iter()
                    .rev()
                    .find(|x| !x.is_open())
                    .and_then(|x| x.status.clone()),
                status: statuses.get(&node).cloned(),
                in_definition: declared.contains(&node),
                by_status,
                node,
            }
        })
        .collect()
}

fn edge_runs(ev: &RouteEvidence<'_>) -> Vec<EdgeRun> {
    let mut out = Vec::new();
    for seg in 0..ev.definitions.segments.len() {
        let Some(pb) = ev.playbook(seg) else { continue };
        for a in edge_addresses(pb) {
            let e = &pb.edges[a.index];
            out.push(EdgeRun {
                segment: seg,
                state: edge_state(ev, seg, a.index),
                edge: a.address,
                rendered: a.rendered,
                from: e.from.clone(),
                to: e.to.clone(),
            });
        }
    }
    out
}

/// The state of edge `edge` of known segment `seg`.
pub(crate) fn edge_state(ev: &RouteEvidence<'_>, seg: usize, edge: usize) -> EdgeState {
    let count = ev.taken(seg, edge);
    if count > 0 {
        return EdgeState::Taken { count };
    }
    let Some(pb) = ev.playbook(seg) else {
        return not_established("definition_unknown");
    };
    let source = &pb.edges[edge].from;
    let (ended, open) = ev.executions_in(source, seg);
    if open > 0 {
        return not_established("open_execution");
    }
    if ended.is_empty() {
        return match ev.is_live_segment(seg) {
            true => not_established("run_not_finished"),
            false => EdgeState::NotTaken,
        };
    }
    for x in ended {
        match ev.episodes.get(&(source.clone(), x)) {
            Some(ep) if ep.complete => {}
            Some(ep) => return not_established(ep.reason.unwrap_or("cannot_tell")),
            None => return not_established("cannot_tell"),
        }
    }
    EdgeState::NotTaken
}

fn not_established(reason: &str) -> EdgeState {
    EdgeState::NotEstablished {
        reason: reason.to_string(),
    }
}

fn traversal_rows(ev: &RouteEvidence<'_>) -> Vec<TraversalRow> {
    ev.records
        .iter()
        .map(|r| {
            let edges = match r.segment.and_then(|s| ev.playbook(s)) {
                Some(pb) if !r.edges.is_empty() => r
                    .edges
                    .iter()
                    .map(|i| super::route_evidence::address_of(pb, *i))
                    .collect(),
                _ => r.named.clone(),
            };
            TraversalRow {
                seq: r.seq,
                from: r.from.clone(),
                to: r.to.clone(),
                execution: r.execution,
                via_policy: r.via_policy,
                policy: r.policy.as_ref().map(|p| match p {
                    PolicyOwner::Defaults => "defaults".to_string(),
                    PolicyOwner::Fork(f) => format!("fork:{f}"),
                }),
                counted: r.counted,
                edges,
                segment: r.segment,
                resolved: r.unresolved.is_none(),
                reason: r.unresolved.map(str::to_string),
            }
        })
        .collect()
}

impl crate::run_view::RunView {
    /// The run graph of this view, with the run's stored definitions and
    /// diagnostics read from `run_dir`. Read-only.
    pub fn graph(&self, run_dir: &std::path::Path) -> RunGraph {
        self.graph_and_definition(run_dir).0
    }

    /// [`RunView::graph`] with the definition of its shown segment: what a
    /// run canvas draws, so it matches edge states by address against the
    /// bytes that ran rather than the run's mutable snapshot.
    pub fn graph_and_definition(
        &self,
        run_dir: &std::path::Path,
    ) -> (RunGraph, Option<apb_core::schema::Playbook>) {
        let definitions = DefinitionsInForce::load(run_dir, &self.events);
        let diagnostics = crate::diagnostics::read(run_dir);
        let graph = RunGraph::build(&GraphInput {
            events: &self.events,
            definitions: &definitions,
            diagnostics: &diagnostics,
            statuses: &self.nodes(),
        });
        let shown =
            graph
                .shown_segment
                .and_then(|i| match &definitions.segments.get(i)?.resolved {
                    Resolved::Known { playbook, .. } => Some((**playbook).clone()),
                    Resolved::Unknown(_) => None,
                });
        (graph, shown)
    }
}
