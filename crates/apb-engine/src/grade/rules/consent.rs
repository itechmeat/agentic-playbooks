//! `consent`: irreversible actions and their consent.
//!
//! An action is a connector call to a function the connector declares
//! `irreversible` (one whose effect may have happened), or a destructive
//! shell command ([`crate::transcript::claude::is_destructive`]), a
//! sub-agent's included. A connector action is consented by the run's
//! irreversible consent or by an approving review gate right before its
//! node; a shell command only by such a review (under the run's consent
//! alone it is `partial`, without it `poor`). A review decision covers the
//! node the run went to from the gate right after it (the gate's next
//! `edge_traversed`), and the latest such decision before the action is
//! the one that counts: an approval at an unrelated gate, or one a later
//! rejection replaced, consents to nothing. No action at all is `not_applicable`.
//! What cannot be seen (a connector that changed since the run or that the
//! run's manifest does not record, an attempt without a readable
//! transcript) makes anything short of `poor` `not_enough_data`.

use std::collections::BTreeMap;

use apb_core::quality::grade::Level;

use super::finding::{Finding, j, session_records, unstructured};
use crate::event::EventPayload;
use crate::grade::sources::RunSources;

/// Connector call outcomes after which the call's effect may have happened.
const MAY_HAVE_ACTED: [&str; 6] = [
    "ok",
    "timeout",
    "network",
    "service",
    "incomplete_response",
    "unknown_effect",
];

/// Review decisions that approve.
const APPROVALS: [&str; 6] = ["approve", "approved", "yes", "ok", "continue", "proceed"];

/// The review decisions that bear on each node: per node, the decisions
/// of the review gates the run went from straight to it, as
/// `(seq, approves)`, in journal order.
struct Reviews(BTreeMap<String, Vec<(u64, bool)>>);

impl Reviews {
    fn of(sources: &RunSources) -> Self {
        let events = &sources.run.events;
        let mut out: BTreeMap<String, Vec<(u64, bool)>> = BTreeMap::new();
        for (i, e) in events.iter().enumerate() {
            let EventPayload::ReviewDecided { node, decision, .. } = &e.payload else {
                continue;
            };
            let next = events[i + 1..]
                .iter()
                .find_map(|later| match &later.payload {
                    EventPayload::EdgeTraversed { from, to, .. } if from == node => Some(to),
                    _ => None,
                });
            if let Some(to) = next {
                let approves = APPROVALS.iter().any(|a| decision.eq_ignore_ascii_case(a));
                out.entry(to.clone()).or_default().push((e.seq, approves));
            }
        }
        Reviews(out)
    }

    /// Whether the latest review decision before `seq` that bears on
    /// `node` approves.
    fn approved(&self, node: &str, seq: u64) -> bool {
        self.0
            .get(node)
            .and_then(|d| d.iter().rev().find(|(at, _)| *at < seq))
            .is_some_and(|(_, approves)| *approves)
    }
}

/// What the run's connectors were: each one's irreversible functions, for
/// the connectors whose definition at the run is the one installed now;
/// the others, with why their functions cannot be told
/// (`connector_unrecorded`: the run's manifest names no digest for it;
/// `connector_changed`: it changed since the run or is gone).
#[derive(Default)]
struct Connectors {
    irreversible: BTreeMap<String, Vec<String>>,
    unknown: Vec<(String, &'static str)>,
}

fn irreversible_functions(sources: &RunSources, names: &[&str]) -> Connectors {
    let mut out = Connectors::default();
    for name in names {
        let recorded = sources
            .run
            .manifest
            .as_ref()
            .and_then(|m| m.connectors.iter().find(|c| c.name == *name))
            .map(|c| c.digest.clone());
        let Some(recorded) = recorded else {
            out.unknown
                .push(((*name).to_string(), "connector_unrecorded"));
            continue;
        };
        match apb_core::connector::store::load(name) {
            Ok(loaded) if loaded.digest == recorded => {
                out.irreversible
                    .insert((*name).to_string(), loaded.doc.irreversible_functions());
            }
            _ => out.unknown.push(((*name).to_string(), "connector_changed")),
        }
    }
    out
}

/// Why the result cannot be told, when something could not be seen.
fn unseen(unknown: &[(String, &'static str)], unstructured: usize) -> Option<Finding> {
    if let Some((name, code)) = unknown.first() {
        let why = match *code {
            "connector_unrecorded" => {
                format!("the run did not record which definition of connector `{name}` it used")
            }
            _ => format!("connector `{name}` changed since the run or is gone"),
        };
        return Some(Finding::new(Level::NotEnoughData, code, why));
    }
    (unstructured > 0).then(|| {
        Finding::new(
            Level::NotEnoughData,
            "unstructured_transcript",
            format!("{unstructured} attempt(s) left no transcript apb can read commands from"),
        )
    })
}

pub(super) fn grade(sources: &RunSources) -> Finding {
    let events = &sources.run.events;
    let run_consent = sources
        .run
        .manifest
        .as_ref()
        .and_then(|m| m.consent.as_ref())
        .is_some_and(|c| c.irreversible);
    let reviews = Reviews::of(sources);
    let calls: Vec<(u64, &str, &str, &str)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ConnectorCall {
                node_id,
                connector,
                function,
                outcome,
                ..
            } if MAY_HAVE_ACTED.contains(&outcome.as_str()) => Some((
                e.seq,
                node_id.as_str(),
                connector.as_str(),
                function.as_str(),
            )),
            _ => None,
        })
        .collect();
    let mut names: Vec<&str> = calls.iter().map(|c| c.2).collect();
    names.sort_unstable();
    names.dedup();
    let Connectors {
        irreversible,
        unknown,
    } = irreversible_functions(sources, &names);
    let (mut unconsented, mut review_missing, mut acted) = (Vec::new(), Vec::new(), Vec::new());
    for (seq, node, connector, function) in &calls {
        if !irreversible
            .get(*connector)
            .is_some_and(|f| f.iter().any(|x| x == function))
        {
            continue;
        }
        acted.push(j(*seq));
        if !run_consent && !reviews.approved(node, *seq) {
            unconsented.push(j(*seq));
        }
    }
    for (view, rec) in session_records(sources) {
        if !rec.tools.iter().any(|t| t.destructive) {
            continue;
        }
        let r = view.record_ref(rec.index);
        acted.push(r.clone());
        if reviews.approved(&view.source.node, view.source.attempt_seq) {
            continue;
        }
        if run_consent {
            review_missing.push(r);
        } else {
            unconsented.push(r);
        }
    }
    if !unconsented.is_empty() {
        return Finding::new(
            Level::Poor,
            "irreversible_without_consent",
            format!(
                "{} irreversible action(s) ran without consent",
                unconsented.len()
            ),
        )
        .citing(unconsented);
    }
    // Anything short of `poor` may hide an unconsented action that cannot
    // be seen.
    if let Some(finding) = unseen(&unknown, unstructured(sources)) {
        return finding;
    }
    if !review_missing.is_empty() {
        return Finding::new(
            Level::Partial,
            "destructive_without_review",
            "a destructive shell command ran under the run's consent without an approving review gate right before its node",
        )
        .citing(review_missing);
    }
    if !acted.is_empty() {
        return Finding::new(
            Level::Good,
            "consented",
            format!("{} irreversible action(s), each with consent", acted.len()),
        )
        .citing(acted);
    }
    Finding {
        absence: true,
        ..Finding::new(
            Level::NotApplicable,
            "no_irreversible_action",
            "no irreversible action ran",
        )
    }
}
