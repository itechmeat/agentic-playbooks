//! What a run's journal proves about its routing, read once and shared by
//! the run graph ([`super::graph`]) and the "why it did not run"
//! explanations ([`super::route_explain`]), so the two never disagree.
//!
//! Three layers, kept apart:
//!
//! 1. **Records**: every `edge_traversed` as written, with its raw flags
//!    (`counted`, `via_policy`), the segment in force at its seq and the
//!    execution of its source that routed it ([`ExecutionIndex::routed_by`]).
//!    A policy record is dispatched first and never resolved through an
//!    explicit edge; a record without addresses resolves through its pair
//!    only when the pair is unique in a known definition.
//! 2. **Deliveries**: the records normalized by source execution, segment
//!    and edge, so one established delivery counts once however many
//!    records describe it (a decision record plus a counted one, a resume
//!    that advanced again, a delayed join push).
//! 3. **Episodes**: one routing decision per ended execution. Only a
//!    complete episode supports a negative claim: a correctly attributed
//!    `route_evaluated` diagnostic, a policy delivery, a one-of edge taken
//!    (one-of routing takes exactly one edge), or a fan-out that names every
//!    unconditional edge not provably spent. A partial journal prefix (one
//!    hop of a fan-out, nothing after a finish) stays not established.

use std::collections::{BTreeMap, BTreeSet};

use apb_core::edge_address::{EdgeAddress, edge_addresses, resolve};
use apb_core::schema::{Edge, Playbook};

use crate::diagnostics::Diagnostic;
use crate::diagnostics::route::RouteEvaluated;
use crate::event::{Event, EventPayload};
use crate::identity::ExecutionIndex;
use crate::run_definitions::{DefinitionsInForce, Resolved};

/// Who owns a policy delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyOwner {
    /// `defaults.on_failure: <node>`.
    Defaults,
    /// A fork's `on_failure`, with the fork's id (from its `branch_failed`).
    Fork(String),
}

/// One `edge_traversed` record as the journal holds it.
#[derive(Debug, Clone)]
pub struct Record {
    pub seq: u64,
    pub from: String,
    pub to: String,
    pub via_policy: bool,
    /// The record spends a bounded edge's budget (neither `uncounted` nor a
    /// policy hop).
    pub counted: bool,
    /// The addresses it named, parsed (a string that does not parse is
    /// dropped and the record reads as naming none).
    pub named: Vec<EdgeAddress>,
    /// The index of the segment in force at `seq`.
    pub segment: Option<usize>,
    /// The ordinal of the source's execution this record routed.
    pub execution: Option<u32>,
    pub policy: Option<PolicyOwner>,
    /// The edges (indices into the segment definition's `edges`) this record
    /// establishes, when it resolves; empty for a policy record.
    pub edges: Vec<usize>,
    /// Why it does not resolve to edges of a known definition; `None` when it
    /// does (and for a policy record, which has no edge to resolve).
    pub unresolved: Option<&'static str>,
}

/// One established delivery: a source execution delivering into a target
/// over an edge of one segment (or over a policy route).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Delivery {
    pub segment: usize,
    pub from: String,
    pub to: String,
    pub execution: Option<u32>,
    /// The edge's index in the segment definition; `None` for a policy route.
    pub edge: Option<usize>,
    /// The first record's seq.
    pub seq: u64,
}

/// What makes two records one delivery: segment, source, target, source
/// execution and edge (`None` for a policy route).
type DeliveryKey = (usize, String, String, Option<u32>, Option<usize>);

/// One routing decision of one ended execution.
#[derive(Debug, Clone)]
pub struct Episode {
    pub node: String,
    pub execution: u32,
    /// The segment whose definition the decision was made in.
    pub segment: Option<usize>,
    /// Whether the journal holds the whole decision.
    pub complete: bool,
    /// Why it is not complete.
    pub reason: Option<&'static str>,
    /// The explicit edges taken (indices into the segment definition).
    pub chosen: BTreeSet<usize>,
    /// The failure went to a policy handler.
    pub policy_to: Option<String>,
    /// The diagnostic that recorded the decision, by position in
    /// [`RouteEvidence::routes`].
    pub diagnostic: Option<usize>,
}

/// Everything the journal proves about a run's routing.
#[derive(Debug)]
pub struct RouteEvidence<'a> {
    pub events: &'a [Event],
    pub index: ExecutionIndex,
    pub definitions: &'a DefinitionsInForce,
    pub records: Vec<Record>,
    pub deliveries: Vec<Delivery>,
    /// By `(node, execution ordinal)`.
    pub episodes: BTreeMap<(String, u32), Episode>,
    /// The `route_evaluated` diagnostics inside the journal prefix read,
    /// attributed to an execution.
    pub routes: Vec<RouteEvaluated>,
    /// The run journaled its end.
    pub ended: bool,
    /// The last seq of the journal read.
    pub last_seq: u64,
}

impl<'a> RouteEvidence<'a> {
    pub fn build(
        events: &'a [Event],
        definitions: &'a DefinitionsInForce,
        diagnostics: &[Diagnostic],
    ) -> Self {
        let index = crate::identity::index(events);
        let last_seq = events.last().map_or(0, |e| e.seq);
        let ended = events.iter().any(|e| {
            matches!(
                e.payload,
                EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
            )
        });
        let routes = attributed_routes(&index, diagnostics, last_seq);
        let records = records(events, &index, definitions);
        let mut ev = RouteEvidence {
            events,
            index,
            definitions,
            records,
            deliveries: Vec::new(),
            episodes: BTreeMap::new(),
            routes,
            ended,
            last_seq,
        };
        ev.episodes = ev.build_episodes();
        ev.deliveries = ev.normalize();
        ev
    }

    /// The index of the segment in force at `seq`.
    pub fn segment_at(&self, seq: u64) -> Option<usize> {
        segment_at(self.definitions, seq)
    }

    /// The definition of segment `i`, when it is known.
    pub fn playbook(&self, i: usize) -> Option<&Playbook> {
        match &self.definitions.segments.get(i)?.resolved {
            Resolved::Known { playbook, .. } => Some(playbook),
            Resolved::Unknown(_) => None,
        }
    }

    fn digest(&self, i: usize) -> Option<&str> {
        match &self.definitions.segments.get(i)?.resolved {
            Resolved::Known { digest, .. } => Some(digest),
            Resolved::Unknown(_) => None,
        }
    }

    /// Two segments executed the same bytes.
    pub fn same_definition(&self, a: usize, b: usize) -> bool {
        a == b || self.digest(a).is_some_and(|d| self.digest(b) == Some(d))
    }

    /// The segment an execution of `node` ended in, or began in while open.
    pub fn execution_segment(&self, node: &str, ordinal: u32) -> Option<usize> {
        let x = self.index.executions(node).get(ordinal as usize - 1)?;
        if let Some(ep) = self.episodes.get(&(node.to_string(), ordinal))
            && ep.segment.is_some()
        {
            return ep.segment;
        }
        self.segment_at(x.end_seq.unwrap_or(x.begin_seq))
    }

    fn build_episodes(&self) -> BTreeMap<(String, u32), Episode> {
        let mut out = BTreeMap::new();
        for (node, execs) in self.index.nodes() {
            for x in execs.iter().filter(|x| !x.is_open() && !x.cancelled) {
                out.insert(
                    (node.to_string(), x.ordinal),
                    self.episode(node, x.ordinal, x.end_seq.unwrap_or(0)),
                );
            }
        }
        out
    }

    fn episode(&self, node: &str, ordinal: u32, end_seq: u64) -> Episode {
        let rows: Vec<&Record> = self
            .records
            .iter()
            .filter(|r| r.from == node && r.execution == Some(ordinal))
            .collect();
        // A `max_loops` exit is the decision when one was recorded; otherwise
        // the latest record of the execution.
        let of_execution =
            |d: &RouteEvaluated| d.node == node && d.execution == ordinal && d.item.is_none();
        let diagnostic = self
            .routes
            .iter()
            .rposition(|d| of_execution(d) && d.mode == "loop_exhausted")
            .or_else(|| self.routes.iter().rposition(of_execution));
        let first_seq = rows
            .first()
            .map(|r| r.seq)
            .into_iter()
            .chain(diagnostic.map(|i| self.routes[i].after_seq))
            .min();
        let segment = self.segment_at(first_seq.unwrap_or(end_seq));
        let mut ep = Episode {
            node: node.to_string(),
            execution: ordinal,
            segment,
            complete: false,
            reason: None,
            chosen: BTreeSet::new(),
            policy_to: None,
            diagnostic,
        };
        let not = |mut ep: Episode, why: &'static str| {
            ep.reason = Some(why);
            ep
        };
        let Some(seg) = segment else {
            return not(ep, "definition_unknown");
        };
        let Some(pb) = self.playbook(seg) else {
            return not(ep, "definition_unknown");
        };
        if rows
            .iter()
            .any(|r| r.segment.is_none_or(|s| !self.same_definition(s, seg)))
        {
            return not(ep, "routed_across_definitions");
        }
        if let Some(r) = rows.iter().find(|r| r.via_policy) {
            ep.policy_to = Some(r.to.clone());
            ep.complete = true;
            return ep;
        }
        let out: Vec<(usize, &Edge)> = pb
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from == node)
            .collect();
        if let Some(d) = diagnostic.map(|i| &self.routes[i]) {
            let chosen: Option<BTreeSet<usize>> = d
                .chosen
                .iter()
                .map(|a| EdgeAddress::parse(a).ok().and_then(|a| resolve(pb, &a)))
                .collect();
            if let Some(chosen) = chosen {
                ep.chosen = chosen;
                ep.complete = true;
                return ep;
            }
        }
        if out.is_empty() {
            ep.complete = true;
            return ep;
        }
        if rows.is_empty() {
            return not(ep, "routing_not_recorded");
        }
        if rows.iter().any(|r| r.unresolved.is_some()) {
            let why = rows
                .iter()
                .find_map(|r| r.unresolved)
                .unwrap_or("cannot_tell");
            return not(ep, why);
        }
        ep.chosen = rows.iter().flat_map(|r| r.edges.iter().copied()).collect();
        let one_of = ep
            .chosen
            .iter()
            .any(|i| pb.edges[*i].fallback || pb.edges[*i].condition.is_some());
        if one_of {
            ep.complete = true;
            return ep;
        }
        // A fan-out takes every available unconditional edge: complete only
        // when each one is named or provably spent before this decision.
        let first = rows.first().map_or(end_seq, |r| r.seq);
        let covered = out
            .iter()
            .filter(|(_, e)| e.condition.is_none() && !e.fallback)
            .all(|(i, e)| ep.chosen.contains(i) || self.spent_before(e, first));
        match covered {
            true => {
                ep.complete = true;
                ep
            }
            false => not(ep, "routing_partly_recorded"),
        }
    }

    /// Whether bounded edge `e` had spent its whole budget before `seq`:
    /// the counted records of its pair reached its cap.
    pub fn spent_before(&self, e: &Edge, seq: u64) -> bool {
        let Some(cap) = e.max_traversals else {
            return false;
        };
        self.counted_before(&e.from, &e.to, seq) >= cap
    }

    /// The counted records of the pair `from -> to` before `seq`: what the
    /// fold had spent of the pair's shared budget.
    pub fn counted_before(&self, from: &str, to: &str, seq: u64) -> u32 {
        self.records
            .iter()
            .filter(|r| r.counted && r.from == from && r.to == to && r.seq < seq)
            .count() as u32
    }

    fn normalize(&self) -> Vec<Delivery> {
        let mut out: BTreeMap<DeliveryKey, u64> = BTreeMap::new();
        for r in &self.records {
            let segment = r
                .execution
                .and_then(|x| self.episodes.get(&(r.from.clone(), x)))
                .and_then(|ep| ep.segment)
                .filter(|s| r.segment.is_some_and(|rs| self.same_definition(rs, *s)))
                .or(r.segment);
            let Some(segment) = segment else { continue };
            if r.via_policy {
                out.entry((segment, r.from.clone(), r.to.clone(), r.execution, None))
                    .or_insert(r.seq);
                continue;
            }
            if r.unresolved.is_some() {
                continue;
            }
            for e in &r.edges {
                out.entry((segment, r.from.clone(), r.to.clone(), r.execution, Some(*e)))
                    .or_insert(r.seq);
            }
        }
        let mut list: Vec<Delivery> = out
            .into_iter()
            .map(|((segment, from, to, execution, edge), seq)| Delivery {
                segment,
                from,
                to,
                execution,
                edge,
                seq,
            })
            .collect();
        list.sort_by_key(|d| d.seq);
        list
    }

    /// The distinct deliveries over edge `edge` made in segment `segment`
    /// only. Counts are per segment, so a reader that combines segments
    /// (the overlay, a canvas over several segments of the same bytes) sums
    /// them without counting a delivery twice.
    pub fn taken(&self, segment: usize, edge: usize) -> u32 {
        self.deliveries
            .iter()
            .filter(|d| d.edge == Some(edge) && d.segment == segment)
            .count() as u32
    }

    /// Whether anything established a delivery into `node`.
    pub fn delivered_into(&self, node: &str) -> Option<&Delivery> {
        self.deliveries.iter().find(|d| d.to == node)
    }

    /// The executions of `node` whose routing belongs to segment `segment`
    /// (it ended there, or its first routing record is there), and how many
    /// are still open there.
    pub fn executions_in(&self, node: &str, segment: usize) -> (Vec<u32>, u32) {
        let mut ended = Vec::new();
        let mut open = 0;
        for x in self.index.executions(node).iter().filter(|x| !x.cancelled) {
            if self.execution_segment(node, x.ordinal) != Some(segment) {
                continue;
            }
            match x.is_open() {
                true => open += 1,
                false => ended.push(x.ordinal),
            }
        }
        (ended, open)
    }

    /// Whether `segment` is the one in force now (the run can still add to
    /// it).
    pub fn is_live_segment(&self, segment: usize) -> bool {
        !self.ended && segment + 1 == self.definitions.segments.len()
    }
}

fn segment_at(definitions: &DefinitionsInForce, seq: u64) -> Option<usize> {
    definitions
        .segments
        .iter()
        .rposition(|s| s.from_seq <= seq && s.to_seq.is_none_or(|t| seq <= t))
}

/// The `route_evaluated` records inside the journal prefix (`after_seq` at
/// most `last_seq`) whose stored execution ordinal is the execution that
/// had ended by `after_seq` (inclusive: a dead end follows its own
/// `node_finished` with no hop in between).
fn attributed_routes(
    index: &ExecutionIndex,
    diagnostics: &[Diagnostic],
    last_seq: u64,
) -> Vec<RouteEvaluated> {
    diagnostics
        .iter()
        .filter_map(|d| match d {
            Diagnostic::Route(r) => Some(r),
            Diagnostic::Mismatch(_) => None,
        })
        .filter(|r| r.after_seq <= last_seq)
        .filter(|r| {
            routed_through(index, &r.node, r.after_seq).is_some_and(|ord| ord == r.execution)
        })
        .cloned()
        .collect()
}

/// The ordinal of the latest execution of `node` that ended at or before
/// `seq`: the attribution of a record that follows the event at `seq`.
pub fn routed_through(index: &ExecutionIndex, node: &str, seq: u64) -> Option<u32> {
    index
        .executions(node)
        .iter()
        .rev()
        .find(|x| x.end_seq.is_some_and(|end| end <= seq))
        .map(|x| x.ordinal)
}

fn records(
    events: &[Event],
    index: &ExecutionIndex,
    definitions: &DefinitionsInForce,
) -> Vec<Record> {
    let mut out = Vec::new();
    let mut last_branch_failed: Option<(u64, String, String, String)> = None;
    for e in events {
        match &e.payload {
            EventPayload::BranchFailed {
                fork,
                node,
                target: Some(t),
                ..
            } => last_branch_failed = Some((e.seq, fork.clone(), node.clone(), t.clone())),
            EventPayload::EdgeTraversed {
                from,
                to,
                via_policy,
                uncounted,
                edges,
            } => {
                let segment = segment_at(definitions, e.seq);
                let execution = index.routed_by(from, e.seq).map(|x| x.ordinal);
                let named: Vec<EdgeAddress> = edges
                    .iter()
                    .filter_map(|s| EdgeAddress::parse(s).ok())
                    .collect();
                let policy = via_policy.then(|| match &last_branch_failed {
                    Some((_, fork, node, t)) if node == from && t == to => {
                        PolicyOwner::Fork(fork.clone())
                    }
                    _ => PolicyOwner::Defaults,
                });
                let mut r = Record {
                    seq: e.seq,
                    from: from.clone(),
                    to: to.clone(),
                    via_policy: *via_policy,
                    counted: !*via_policy && !*uncounted,
                    named,
                    segment,
                    execution,
                    policy,
                    edges: Vec::new(),
                    unresolved: None,
                };
                if !r.via_policy {
                    resolve_record(&mut r, definitions);
                }
                out.push(r);
            }
            _ => {}
        }
    }
    out
}

/// Resolves a non-policy record to edges of its segment's definition.
fn resolve_record(r: &mut Record, definitions: &DefinitionsInForce) {
    let pb = match r.segment.map(|i| &definitions.segments[i].resolved) {
        Some(Resolved::Known { playbook, .. }) => playbook,
        _ => {
            r.unresolved = Some("definition_unknown");
            return;
        }
    };
    if !r.named.is_empty() {
        let found: Option<Vec<usize>> = r.named.iter().map(|a| resolve(pb, a)).collect();
        match found {
            Some(edges)
                if edges
                    .iter()
                    .all(|i| pb.edges[*i].from == r.from && pb.edges[*i].to == r.to) =>
            {
                r.edges = edges
            }
            _ => r.unresolved = Some("address_not_in_definition"),
        }
        return;
    }
    let pair: Vec<usize> = pb
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.from == r.from && e.to == r.to)
        .map(|(i, _)| i)
        .collect();
    match pair.len() {
        1 => r.edges = pair,
        0 => r.unresolved = Some("address_not_in_definition"),
        _ => r.unresolved = Some("duplicate_pair_before_edges"),
    }
}

/// The rendered address of edge `i` of `pb`.
pub fn address_of(pb: &Playbook, i: usize) -> EdgeAddress {
    edge_addresses(pb)
        .into_iter()
        .nth(i)
        .map(|a| a.address)
        .unwrap_or(EdgeAddress::Derived {
            from: String::new(),
            to: String::new(),
            ordinal: 0,
        })
}
