//! The routes out of a `human_review` gate: which edges each decision can
//! take ([`review_routes`]), and the warnings built on it: an option only
//! bounded edges route (V136), a `review_status` edge no decision can ever
//! match (V144), and an option no edge carries (V145).
//!
//! All three are warnings, never errors: a run start re-validates the
//! stored version and refuses on any error, so an error here would make a
//! version that runs correctly on the options it covers unrunnable after an
//! upgrade.

use super::*;

/// Whether edge `e` out of a `human_review` gate can be taken for decision
/// `option`, the fallback rule aside: an edge without a condition always,
/// a `review_status` edge when it names `option`, and an edge with any other
/// condition because it may match. Shared by the rules here and the
/// unattended rules.
pub(super) fn review_takes(e: &Edge, option: &str) -> bool {
    match &e.condition {
        None => true,
        Some(EdgeCondition::ReviewStatus { equals }) => equals == option,
        Some(_) => true,
    }
}

/// The routes out of review `node`: every effective option with the
/// indexes (in `playbook.edges`) of the non-fallback edges it can take
/// ([`review_takes`]), in declaration order. A fallback edge is left out:
/// it is taken only when nothing else matches. Empty for a node that is
/// not a `human_review`.
pub(crate) fn review_routes(playbook: &Playbook, node: &str) -> Vec<(String, Vec<usize>)> {
    let Some(NodeKind::HumanReview { options, .. }) = playbook.node(node).map(|n| &n.kind) else {
        return Vec::new();
    };
    crate::schema::effective_review_options(options)
        .into_iter()
        .map(|option| {
            let edges = playbook
                .edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.from == node && !e.fallback && review_takes(e, &option))
                .map(|(i, _)| i)
                .collect();
            (option, edges)
        })
        .collect()
}

fn has_fallback(playbook: &Playbook, node: &str) -> bool {
    playbook.edges.iter().any(|e| e.from == node && e.fallback)
}

/// V136 (warning): a `human_review` option that only bounded edges route.
/// Once their cap is spent the decision has no edge, and the run fails at
/// the review (the person decided, and nothing follows). A fallback edge out
/// of the review, or an unbounded edge that the option can take, is a way
/// out; an unbounded edge with a condition other than `review_status` may
/// match too and counts as one. The usual fix is to put the cap on the edge
/// that leads back into the review, so the review itself always routes.
pub(crate) fn check_review_caps(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        if has_fallback(playbook, &node.id) {
            continue;
        }
        for (option, edges) in review_routes(playbook, &node.id) {
            let edges: Vec<&Edge> = edges.iter().map(|i| &playbook.edges[*i]).collect();
            if edges.is_empty() || edges.iter().any(|e| e.max_traversals.is_none()) {
                continue;
            }
            r.warn_edges(
                playbook,
                "V136",
                Some(&node.id),
                &edges,
                format!(
                    "human_review `{}`: decision `{option}` is routed only by a bounded edge; once its cap is spent the decision has no edge and the run fails at the review. Put the cap on the edge into the review instead, or add a fallback edge",
                    node.id
                ),
            );
        }
    }
}

/// V144 (warning): a `review_status` edge that can never match: its source
/// has no decision (only a `human_review` and a `publish` node's consent
/// have one), or its value is not one of the source's options (a review's
/// effective options, a publish node's `publish` and `skip`).
pub(crate) fn check_review_status_values(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        let Some(EdgeCondition::ReviewStatus { equals }) = &e.condition else {
            continue;
        };
        let msg = match playbook.node(&e.from).map(|n| &n.kind) {
            Some(NodeKind::HumanReview { options, .. }) => {
                let options = crate::schema::effective_review_options(options);
                if options.contains(equals) {
                    continue;
                }
                format!(
                    "edge `{}` -> `{}` routes on review decision `{equals}`, which `{}` does not offer ({}), so it never matches. Use one of the options, or add `{equals}` to the review's `options`",
                    e.from,
                    e.to,
                    e.from,
                    options.join(", ")
                )
            }
            Some(NodeKind::Publish { .. }) => {
                if crate::publish::OPTIONS.contains(&equals.as_str()) {
                    continue;
                }
                format!(
                    "edge `{}` -> `{}` routes on publish decision `{equals}`, which `{}` does not offer ({}), so it never matches",
                    e.from,
                    e.to,
                    e.from,
                    crate::publish::OPTIONS.join(", ")
                )
            }
            // An unknown source is V06's.
            None => continue,
            Some(kind) => format!(
                "edge `{}` -> `{}` routes on a review decision, but `{}` is a {} node with no decision, so it never matches. Route on `node_status` or `output_field` instead",
                e.from,
                e.to,
                e.from,
                kind.type_str()
            ),
        };
        r.warn_edges(playbook, "V144", Some(&e.from), &[e], msg);
    }
}

/// V145 (warning): a `human_review` option no edge can carry: no
/// `review_status` edge with that value, no unconditional edge, no edge
/// with another condition that may match, and no fallback edge. A person
/// who picks it ends the run at the review.
pub(crate) fn check_review_options_routed(playbook: &Playbook, r: &mut ValidationReport) {
    for node in &playbook.nodes {
        if has_fallback(playbook, &node.id) {
            continue;
        }
        for (option, edges) in review_routes(playbook, &node.id) {
            if !edges.is_empty() {
                continue;
            }
            r.warn(
                "V145",
                Some(&node.id),
                format!(
                    "human_review `{}`: no edge carries decision `{option}`, so a person who picks it ends the run at the review. Add an edge with `review_status: {option}`, or a fallback edge",
                    node.id
                ),
            );
        }
    }
}
