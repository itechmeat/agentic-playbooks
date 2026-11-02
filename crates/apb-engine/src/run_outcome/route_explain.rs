//! "Why it did not run": for every node of the run's known definitions that
//! never executed, one fact per way into it, and a node summary only when
//! every entry is proven. A statement is made only when the journal (or a
//! re-evaluation of exactly the recorded state) proves it; absence of proof
//! is `unknown`, never "not traversed".
//!
//! Classes: `observed` (the journal or a diagnostic says so), `inferred`
//! (re-derived from the definition and the recorded state), `unknown`.

use apb_core::edge_address::{EdgeAddress, edge_addresses};
use apb_core::schema::{FailurePolicy, Playbook};
use serde::Serialize;

use super::route_evidence::{PolicyOwner, RouteEvidence};
use crate::event::EventPayload;
use crate::parallel::CheckResult;

/// How a statement is known.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainClass {
    Observed,
    Inferred,
    Unknown,
}

impl ExplainClass {
    pub const ALL: [ExplainClass; 3] = [
        ExplainClass::Observed,
        ExplainClass::Inferred,
        ExplainClass::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ExplainClass::Observed => "observed",
            ExplainClass::Inferred => "inferred",
            ExplainClass::Unknown => "unknown",
        }
    }
}

/// What a statement says.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotRunCode {
    /// The entry's source never ran.
    SourceNotExecuted,
    /// Its source's routing took another way.
    RoutedElsewhere,
    /// The entry's condition did not match.
    ConditionNotMatched,
    /// A bounded edge had spent its `max_traversals`.
    BudgetSpent,
    /// A condition node at its `max_loops` took its fallback.
    LoopExhausted,
    /// No execution of the run failed, so no failure policy routed.
    NoFailureToRoute,
    /// Every failure went somewhere else.
    FailureRoutedElsewhere,
    /// Something delivered into the node; it did not run (summary).
    DeliveredNotExecuted,
    /// The run ended after the delivery, before the node ran.
    RunEndedFirst,
    /// A `join: any` race cancelled it.
    CancelledByRace,
    /// No way into the node is reachable in its definition.
    Unreachable,
    /// Every entry is proven not to have delivered (several entries).
    NoInputDelivered,
    /// The run is still going and has not settled this.
    RunNotFinished,
    DefinitionUnknown,
    DuplicatePairBeforeEdges,
    OldLoopRound,
    OutputClipped,
    OpenExecution,
    RoutingNotRecorded,
    CannotTell,
}

impl NotRunCode {
    pub const ALL: [NotRunCode; 20] = [
        NotRunCode::SourceNotExecuted,
        NotRunCode::RoutedElsewhere,
        NotRunCode::ConditionNotMatched,
        NotRunCode::BudgetSpent,
        NotRunCode::LoopExhausted,
        NotRunCode::NoFailureToRoute,
        NotRunCode::FailureRoutedElsewhere,
        NotRunCode::DeliveredNotExecuted,
        NotRunCode::RunEndedFirst,
        NotRunCode::CancelledByRace,
        NotRunCode::Unreachable,
        NotRunCode::NoInputDelivered,
        NotRunCode::RunNotFinished,
        NotRunCode::DefinitionUnknown,
        NotRunCode::DuplicatePairBeforeEdges,
        NotRunCode::OldLoopRound,
        NotRunCode::OutputClipped,
        NotRunCode::OpenExecution,
        NotRunCode::RoutingNotRecorded,
        NotRunCode::CannotTell,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotRunCode::SourceNotExecuted => "source_not_executed",
            NotRunCode::RoutedElsewhere => "routed_elsewhere",
            NotRunCode::ConditionNotMatched => "condition_not_matched",
            NotRunCode::BudgetSpent => "budget_spent",
            NotRunCode::LoopExhausted => "loop_exhausted",
            NotRunCode::NoFailureToRoute => "no_failure_to_route",
            NotRunCode::FailureRoutedElsewhere => "failure_routed_elsewhere",
            NotRunCode::DeliveredNotExecuted => "delivered_not_executed",
            NotRunCode::RunEndedFirst => "run_ended_first",
            NotRunCode::CancelledByRace => "cancelled_by_race",
            NotRunCode::Unreachable => "unreachable",
            NotRunCode::NoInputDelivered => "no_input_delivered",
            NotRunCode::RunNotFinished => "run_not_finished",
            NotRunCode::DefinitionUnknown => "definition_unknown",
            NotRunCode::DuplicatePairBeforeEdges => "duplicate_pair_before_edges",
            NotRunCode::OldLoopRound => "old_loop_round",
            NotRunCode::OutputClipped => "output_clipped",
            NotRunCode::OpenExecution => "open_execution",
            NotRunCode::RoutingNotRecorded => "routing_not_recorded",
            NotRunCode::CannotTell => "cannot_tell",
        }
    }

    /// The code of an evidence reason (an episode's or a record's).
    fn of_reason(reason: &str) -> Self {
        match reason {
            "definition_unknown" => NotRunCode::DefinitionUnknown,
            "duplicate_pair_before_edges" => NotRunCode::DuplicatePairBeforeEdges,
            "open_execution" => NotRunCode::OpenExecution,
            "routing_not_recorded" | "routing_partly_recorded" => NotRunCode::RoutingNotRecorded,
            "run_not_finished" => NotRunCode::RunNotFinished,
            _ => NotRunCode::CannotTell,
        }
    }
}

/// One way into a node.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// An incoming edge.
    Edge {
        edge: EdgeAddress,
        rendered: String,
        from: String,
    },
    /// A failure policy that names the node: `defaults` (its
    /// `defaults.on_failure`) or `fork` (a fork's `on_failure`, `source`
    /// naming the fork).
    Policy {
        policy: String,
        source: Option<String>,
    },
}

/// What is established about one entry.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputFact {
    pub segment: usize,
    pub entry: Entry,
    pub class: ExplainClass,
    pub code: NotRunCode,
    pub detail: String,
}

/// Why one node did not run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotRun {
    pub node: String,
    pub class: ExplainClass,
    pub code: NotRunCode,
    pub detail: String,
    pub inputs: Vec<InputFact>,
}

fn fact(
    segment: usize,
    entry: &Entry,
    class: ExplainClass,
    code: NotRunCode,
    detail: String,
) -> InputFact {
    InputFact {
        segment,
        entry: entry.clone(),
        class,
        code,
        detail,
    }
}

/// Every node of a known definition of the run that never executed (race
/// losers count as not executed), with its explanation.
pub(crate) fn explain(ev: &RouteEvidence<'_>) -> Vec<NotRun> {
    let mut nodes: Vec<String> = Vec::new();
    for seg in 0..ev.definitions.segments.len() {
        if let Some(pb) = ev.playbook(seg) {
            for n in &pb.nodes {
                if !nodes.contains(&n.id) {
                    nodes.push(n.id.clone());
                }
            }
        }
    }
    nodes
        .into_iter()
        .filter(|n| ev.index.executions(n).iter().all(|x| x.cancelled))
        .map(|n| explain_node(ev, &n))
        .collect()
}

fn explain_node(ev: &RouteEvidence<'_>, node: &str) -> NotRun {
    let mut inputs = Vec::new();
    let mut reachable = false;
    for seg in 0..ev.definitions.segments.len() {
        let Some(pb) = ev.playbook(seg) else { continue };
        if pb.node(node).is_none() {
            continue;
        }
        reachable |= apb_core::graphutil::reachable_with_policies(pb).contains(node);
        inputs.extend(entry_facts(ev, seg, pb, node));
    }
    let summary = |class, code, detail: String| NotRun {
        node: node.to_string(),
        class,
        code,
        detail,
        inputs: inputs.clone(),
    };
    if ev.records.iter().any(|r| r.to == node) {
        let cancelled = !ev.index.executions(node).is_empty();
        return match (cancelled, ev.ended) {
            (true, _) => summary(
                ExplainClass::Observed,
                NotRunCode::CancelledByRace,
                "a `join: any` race cancelled it".into(),
            ),
            (false, true) => summary(
                ExplainClass::Observed,
                NotRunCode::RunEndedFirst,
                "something delivered into it and the run ended before it ran".into(),
            ),
            (false, false) => summary(
                ExplainClass::Unknown,
                NotRunCode::RunNotFinished,
                "something delivered into it; the run has not finished".into(),
            ),
        };
    }
    if ev
        .definitions
        .segments
        .iter()
        .any(|s| !s.resolved.is_known())
    {
        return summary(
            ExplainClass::Unknown,
            NotRunCode::DefinitionUnknown,
            "part of the run executed a definition that is not established".into(),
        );
    }
    if !reachable {
        return summary(
            ExplainClass::Inferred,
            NotRunCode::Unreachable,
            "no way into it is reachable from the start or a failure policy".into(),
        );
    }
    let proven = !inputs.is_empty() && inputs.iter().all(|f| f.class != ExplainClass::Unknown);
    if proven {
        let class = inputs
            .iter()
            .map(|f| f.class)
            .max()
            .unwrap_or(ExplainClass::Unknown);
        let (code, detail) = match inputs.as_slice() {
            [one] => (one.code, one.detail.clone()),
            many => (
                NotRunCode::NoInputDelivered,
                format!("none of its {} inputs delivered", many.len()),
            ),
        };
        return summary(class, code, detail);
    }
    match ev.ended {
        true => summary(
            ExplainClass::Unknown,
            NotRunCode::CannotTell,
            "not every way into it is proven not to have delivered".into(),
        ),
        false => summary(
            ExplainClass::Unknown,
            NotRunCode::RunNotFinished,
            "the run has not finished".into(),
        ),
    }
}

fn entry_facts(ev: &RouteEvidence<'_>, seg: usize, pb: &Playbook, node: &str) -> Vec<InputFact> {
    let mut out = Vec::new();
    for a in edge_addresses(pb) {
        let e = &pb.edges[a.index];
        if e.to != node {
            continue;
        }
        let entry = Entry::Edge {
            edge: a.address.clone(),
            rendered: a.rendered.clone(),
            from: e.from.clone(),
        };
        out.push(edge_fact(ev, seg, pb, a.index, &a.rendered, &entry));
    }
    if matches!(&pb.defaults.on_failure, FailurePolicy::Node(t) if t == node) {
        let entry = Entry::Policy {
            policy: "defaults".into(),
            source: None,
        };
        out.push(policy_fact(ev, seg, &entry, None));
    }
    for n in &pb.nodes {
        if n.fork
            .as_ref()
            .and_then(|f| f.on_failure.as_deref())
            .is_some_and(|t| t == node)
        {
            let entry = Entry::Policy {
                policy: "fork".into(),
                source: Some(n.id.clone()),
            };
            out.push(policy_fact(ev, seg, &entry, Some(&n.id)));
        }
    }
    out
}

fn edge_fact(
    ev: &RouteEvidence<'_>,
    seg: usize,
    pb: &Playbook,
    edge: usize,
    rendered: &str,
    entry: &Entry,
) -> InputFact {
    use ExplainClass::*;
    let source = &pb.edges[edge].from;
    let unknown = |code: NotRunCode, detail: &str| fact(seg, entry, Unknown, code, detail.into());
    let ran_anywhere = ev.index.executions(source).iter().any(|x| !x.cancelled);
    if !ran_anywhere {
        return match ev.ended {
            true => fact(
                seg,
                entry,
                Observed,
                NotRunCode::SourceNotExecuted,
                format!("`{source}` never ran"),
            ),
            false => unknown(NotRunCode::RunNotFinished, "the run has not finished"),
        };
    }
    let (ended, open) = ev.executions_in(source, seg);
    if open > 0 {
        return unknown(
            NotRunCode::OpenExecution,
            &format!("`{source}` has not finished"),
        );
    }
    if ended.is_empty() {
        return match ev.is_live_segment(seg) {
            true => unknown(NotRunCode::RunNotFinished, "the run has not finished"),
            false => fact(
                seg,
                entry,
                Observed,
                NotRunCode::SourceNotExecuted,
                format!("`{source}` did not run under this definition"),
            ),
        };
    }
    let mut facts: Vec<InputFact> = Vec::new();
    for x in ended {
        facts.push(execution_fact(
            ev, seg, pb, edge, rendered, entry, source, x,
        ));
    }
    let class = facts.iter().map(|f| f.class).max().unwrap_or(Unknown);
    let mut last = facts
        .pop()
        .unwrap_or_else(|| unknown(NotRunCode::CannotTell, "no record"));
    last.class = class;
    last
}

#[allow(clippy::too_many_arguments)]
fn execution_fact(
    ev: &RouteEvidence<'_>,
    seg: usize,
    pb: &Playbook,
    edge: usize,
    rendered: &str,
    entry: &Entry,
    source: &str,
    x: u32,
) -> InputFact {
    use ExplainClass::*;
    let Some(ep) = ev.episodes.get(&(source.to_string(), x)) else {
        return fact(
            seg,
            entry,
            Unknown,
            NotRunCode::CannotTell,
            "no record".into(),
        );
    };
    if !ep.complete {
        if ep.reason == Some("routing_not_recorded")
            && ev.ended
            && let Some(f) = infer(ev, seg, pb, edge, entry, source, x)
        {
            return f;
        }
        let code = NotRunCode::of_reason(ep.reason.unwrap_or("cannot_tell"));
        return fact(
            seg,
            entry,
            Unknown,
            code,
            format!("the routing of `{source}` is not established"),
        );
    }
    if let Some(h) = &ep.policy_to {
        return fact(
            seg,
            entry,
            Observed,
            NotRunCode::RoutedElsewhere,
            format!("`{source}` failed and its failure went to `{h}` by policy"),
        );
    }
    if let Some(d) = ep.diagnostic.map(|i| &ev.routes[i]) {
        if d.mode == "loop_exhausted" {
            return fact(
                seg,
                entry,
                Observed,
                NotRunCode::LoopExhausted,
                format!("`{source}` reached its max_loops and took its fallback"),
            );
        }
        if let Some(c) = d.checks.iter().find(|c| c.edge == rendered) {
            let seen = c
                .observed
                .as_ref()
                .map(|v| format!(" (observed {v})"))
                .unwrap_or_default();
            let (code, detail) = match c.result.as_str() {
                "not_matched" => (
                    NotRunCode::ConditionNotMatched,
                    format!("its condition did not match{seen}"),
                ),
                "exhausted" => (
                    NotRunCode::BudgetSpent,
                    "its max_traversals was spent".to_string(),
                ),
                "judge_unavailable" | "shadowed" => (
                    NotRunCode::ConditionNotMatched,
                    format!("no applied judge answer took it ({})", c.result),
                ),
                _ => (
                    NotRunCode::RoutedElsewhere,
                    format!("`{source}` took {}", chosen_text(&d.chosen)),
                ),
            };
            return fact(seg, entry, Observed, code, detail);
        }
    }
    if ep.chosen.is_empty() {
        return fact(
            seg,
            entry,
            Unknown,
            NotRunCode::CannotTell,
            "no route was recorded".into(),
        );
    }
    let chosen: Vec<String> = ep
        .chosen
        .iter()
        .filter_map(|i| apb_core::edge_address::rendered_at(pb, *i))
        .collect();
    fact(
        seg,
        entry,
        Observed,
        NotRunCode::RoutedElsewhere,
        format!("`{source}` took {}", chosen_text(&chosen)),
    )
}

fn chosen_text(chosen: &[String]) -> String {
    match chosen {
        [] => "no edge".to_string(),
        many => many
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Re-evaluates the routing of execution `x` of `source` against the state
/// folded up to its own finish, in its known definition, with the as-run
/// availability: an inferred fact, or `None` when the re-evaluation would
/// have taken the edge or did not reach it.
#[allow(clippy::too_many_arguments)]
fn infer(
    ev: &RouteEvidence<'_>,
    seg: usize,
    pb: &Playbook,
    edge: usize,
    entry: &Entry,
    source: &str,
    x: u32,
) -> Option<InputFact> {
    let end = ev.index.executions(source).get(x as usize - 1)?.end_seq?;
    // Only the latest execution: the as-run predicate reads the latest
    // hops, so an earlier round cannot be re-derived from a later state.
    if ev
        .index
        .executions(source)
        .iter()
        .rev()
        .find(|e| !e.cancelled)?
        .ordinal
        != x
    {
        return None;
    }
    let prefix: Vec<crate::event::Event> = ev
        .events
        .iter()
        .take_while(|e| e.seq <= end)
        .cloned()
        .collect();
    let state = crate::state::RunState::fold(&prefix);
    let evaluation = crate::parallel::evaluate_route_as_run(pb, source, &state);
    let check = evaluation.checks.iter().find(|c| c.index == edge)?;
    let (code, detail) = match check.result {
        CheckResult::NotMatched => (
            NotRunCode::ConditionNotMatched,
            "its condition does not match the recorded state",
        ),
        CheckResult::Exhausted => (NotRunCode::BudgetSpent, "its max_traversals was spent"),
        _ => return None,
    };
    Some(fact(
        seg,
        entry,
        ExplainClass::Inferred,
        code,
        detail.into(),
    ))
}

fn policy_fact(ev: &RouteEvidence<'_>, seg: usize, entry: &Entry, fork: Option<&str>) -> InputFact {
    use ExplainClass::*;
    let not_finished = || {
        fact(
            seg,
            entry,
            Unknown,
            NotRunCode::RunNotFinished,
            "the run has not finished".into(),
        )
    };
    if let Some(fork) = fork {
        let failed = ev
            .events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::BranchFailed { fork: f, .. } if f == fork));
        return match (failed, ev.ended) {
            (false, true) => fact(
                seg,
                entry,
                Observed,
                NotRunCode::NoFailureToRoute,
                format!("no branch of `{fork}` failed"),
            ),
            (false, false) => not_finished(),
            (true, _) => fact(
                seg,
                entry,
                Unknown,
                NotRunCode::CannotTell,
                format!("a branch of `{fork}` failed"),
            ),
        };
    }
    let failures: Vec<(String, u32)> = ev
        .index
        .nodes()
        .flat_map(|(n, xs)| {
            xs.iter()
                .filter(|x| matches!(x.status.as_deref(), Some("failed" | "timed_out")))
                .map(move |x| (n.to_string(), x.ordinal))
        })
        .collect();
    if failures.is_empty() {
        return match ev.ended {
            true => fact(
                seg,
                entry,
                Observed,
                NotRunCode::NoFailureToRoute,
                "no execution of the run failed".into(),
            ),
            false => not_finished(),
        };
    }
    let handled = failures.iter().all(|(n, x)| {
        ev.episodes.get(&(n.clone(), *x)).is_some_and(|ep| {
            ep.complete
                && (!ep.chosen.is_empty()
                    || ev.records.iter().any(|r| {
                        r.via_policy
                            && &r.from == n
                            && r.execution == Some(*x)
                            && r.policy != Some(PolicyOwner::Defaults)
                    }))
        })
    });
    match handled {
        true => fact(
            seg,
            entry,
            Observed,
            NotRunCode::FailureRoutedElsewhere,
            "every failure of the run took an edge or another policy".into(),
        ),
        false => fact(
            seg,
            entry,
            Unknown,
            NotRunCode::CannotTell,
            "a failure of the run is not proven to have gone elsewhere".into(),
        ),
    }
}
