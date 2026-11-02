//! Membership and "why it did not run" for the node statistics.
//!
//! A node belongs to a run when a definition the run executed is known to
//! declare it (every segment counted once per node and run), or when the
//! run executed it. A node only the run's final snapshot names, in a run
//! with a definition that is not established, has unknown membership,
//! counted apart rather than guessed. Each member node the run never
//! executed contributes its one explanation summary to the reason counts.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::run_outcome::route_evidence::RouteEvidence;
use crate::run_outcome::route_explain::{ExplainClass, NotRunCode};

use super::stats_run::StatsRun;

/// How many runs gave one reason a node did not run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ReasonCount {
    pub class: ExplainClass,
    pub code: NotRunCode,
    pub runs: usize,
}

/// One run's membership and reasons, by node.
#[derive(Debug, Default)]
pub(super) struct RunRoute {
    /// Nodes established as part of the run.
    pub members: BTreeSet<String>,
    /// Nodes only the final snapshot names while a definition is unknown.
    pub unknown: BTreeSet<String>,
    /// The summary of each member node the run never executed.
    pub not_run: BTreeMap<String, (ExplainClass, NotRunCode)>,
}

pub(super) fn run_route(run: &StatsRun) -> RunRoute {
    let ev = RouteEvidence::build(&run.events, &run.definitions, &run.diagnostics);
    let mut members: BTreeSet<String> = BTreeSet::new();
    for i in 0..run.definitions.segments.len() {
        if let Some(pb) = ev.playbook(i) {
            members.extend(pb.nodes.iter().map(|n| n.id.clone()));
        }
    }
    members.extend(ev.index.nodes().map(|(n, _)| n.to_string()));
    let all_known = !run.definitions.segments.is_empty() && run.definitions.all_known();
    let unknown = match (&run.snapshot, all_known) {
        (Some(pb), false) => pb
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .filter(|n| !members.contains(n))
            .collect(),
        _ => BTreeSet::new(),
    };
    let not_run = crate::run_outcome::route_explain::explain(&ev)
        .into_iter()
        .map(|n| (n.node, (n.class, n.code)))
        .collect();
    RunRoute {
        members,
        unknown,
        not_run,
    }
}

/// Adds one run's reasons for `node` to `acc`.
pub(super) fn add_reason(
    acc: &mut BTreeMap<(ExplainClass, NotRunCode), usize>,
    route: &RunRoute,
    node: &str,
) {
    if let Some(key) = route.not_run.get(node) {
        *acc.entry(*key).or_insert(0) += 1;
    }
}

/// The reason counts in a stable order: most runs first.
pub(super) fn counts(acc: BTreeMap<(ExplainClass, NotRunCode), usize>) -> Vec<ReasonCount> {
    let mut out: Vec<ReasonCount> = acc
        .into_iter()
        .map(|((class, code), runs)| ReasonCount { class, code, runs })
        .collect();
    out.sort_by(|a, b| b.runs.cmp(&a.runs).then(a.cmp(b)));
    out
}

/// One text fragment: `2 observed no_failure_to_route, 1 unknown
/// cannot_tell`.
pub(super) fn text(reasons: &[ReasonCount]) -> String {
    reasons
        .iter()
        .map(|r| format!("{} {} {}", r.runs, r.class.as_str(), r.code.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}
