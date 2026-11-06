//! Retry delay rules (V126-V130): a `retry_delay` block on an `agent_task` or
//! on `defaults` has a parseable `initial` between 1 s and 7 days, a `factor` in
//! `[1, 10]` and a `max` between `initial` and 7 days; a list node may not
//! carry one, and a delay that no retry ever uses is reported.

use super::*;
use crate::schema::{RetryDelay, StatusEq};

/// Runs every retry delay rule.
pub(crate) fn check_retry_delay(playbook: &Playbook, r: &mut ValidationReport) {
    if let Some(delay) = &playbook.defaults.retry_delay {
        check_block(delay, None, "defaults.retry_delay", r);
        // Only a node without its own block falls back to the defaults.
        let any_retry = playbook.nodes.iter().any(|n| {
            matches!(
                &n.kind,
                NodeKind::AgentTask {
                    retry_delay: None,
                    ..
                }
            ) && playbook.effective_retry_delay(n).is_some()
                && playbook.effective_max_retries(n) > 0
        });
        if !any_retry {
            r.warn(
                "V130",
                None,
                "defaults.retry_delay is never used: no agent_task without its own retry_delay \
                 that is not a list node has `max_retries` above 0"
                    .into(),
            );
        }
    }
    for node in &playbook.nodes {
        let NodeKind::AgentTask {
            retry_delay: Some(delay),
            ..
        } = &node.kind
        else {
            continue;
        };
        let id = node.id.as_str();
        check_block(delay, Some(id), "retry_delay", r);
        if node.list.is_some() {
            r.error(
                "V129",
                Some(id),
                "retry_delay is not allowed on a list node: item retries are never delayed".into(),
            );
        } else if playbook.effective_max_retries(node) == 0 {
            r.warn(
                "V130",
                Some(id),
                "retry_delay is never used: the node's effective `max_retries` is 0".into(),
            );
        }
    }
}

/// The field rules of one block; `at` names it in the messages.
fn check_block(delay: &RetryDelay, node: Option<&str>, at: &str, r: &mut ValidationReport) {
    let initial = delay.initial_secs();
    match initial {
        None => r.error(
            "V126",
            node,
            format!(
                "{at}.initial `{}` is not a duration (`30s`, `5m`, `1h30m`)",
                delay.initial
            ),
        ),
        Some(0) => r.error("V126", node, format!("{at}.initial must be at least 1s")),
        Some(i) if i > RetryDelay::CEILING_SECS => {
            r.error("V126", node, format!("{at}.initial must be at most 7d"))
        }
        Some(_) => {}
    }
    if !(delay.factor.is_finite() && (1.0..=10.0).contains(&delay.factor)) {
        r.error(
            "V127",
            node,
            format!("{at}.factor must be between 1 and 10, got {}", delay.factor),
        );
    }
    let Some(raw_max) = &delay.max else {
        return;
    };
    match delay.max_secs() {
        None => r.error(
            "V128",
            node,
            format!("{at}.max `{raw_max}` is not a duration (`30s`, `5m`, `1h30m`)"),
        ),
        Some(max) if max > RetryDelay::CEILING_SECS => {
            r.error("V128", node, format!("{at}.max must be at most 7d"))
        }
        Some(max) if initial.is_some_and(|i| max < i) => r.error(
            "V128",
            node,
            format!("{at}.max must be at least {at}.initial"),
        ),
        Some(_) => {}
    }
}

/// V174 (error): `stall_after` outside 2 to 10 (on a node or `defaults`), or
/// on a node that is not an `agent_task`, or on a list node. V175 (warning):
/// a `node_status` edge with `equals: stalled` whose node has no effective
/// `stall_after` (it never matches), or that comes after an edge from the
/// same source with `node_status` `equals: failure` on the same node (the
/// failure edge is always taken first).
pub(crate) fn check_stall_after(playbook: &Playbook, r: &mut ValidationReport) {
    if let Some(value) = playbook.defaults.stall_after
        && !(2..=10).contains(&value)
    {
        r.error(
            "V174",
            None,
            format!("defaults.stall_after is {value}, outside the range 2 to 10"),
        );
    }
    for n in &playbook.nodes {
        if let Some(value) = n.stall_after {
            if !matches!(n.kind, NodeKind::AgentTask { .. }) {
                r.error(
                    "V174",
                    Some(&n.id),
                    format!(
                        "`stall_after` applies to agent_task nodes only; node `{}` is a {} node, which never retries a rejection",
                        n.id,
                        n.kind.type_str()
                    ),
                );
            } else if n.list().is_some() {
                r.error(
                    "V174",
                    Some(&n.id),
                    "`stall_after` is not allowed on a list node: its items are bounded by the batch retries".into(),
                );
            } else if !(2..=10).contains(&value) {
                r.error(
                    "V174",
                    Some(&n.id),
                    format!(
                        "node `{}` has stall_after {value}, outside the range 2 to 10",
                        n.id
                    ),
                );
            }
        }
    }
    // V175: a `stalled` edge that can never match.
    for e in &playbook.edges {
        let Some(EdgeCondition::NodeStatus {
            node,
            equals: StatusEq::Stalled,
        }) = &e.condition
        else {
            continue;
        };
        // The node the condition names, which need not be the edge's
        // source; `effective_stall_after` is `None` for anything but a
        // non-list agent_task, which never stalls either.
        let Some(named) = playbook.node(node) else {
            continue;
        };
        if playbook.effective_stall_after(named).is_none() {
            r.warn(
                "V175",
                Some(&e.from),
                format!(
                    "edge from `{}` routes on `stalled`, but node `{node}` has no effective stall_after, so it never stalls",
                    e.from
                ),
            );
            continue;
        }
        // A stalled edge after a failure edge on the same node, from the same
        // source: the failure edge is always taken first.
        let failure_first = playbook.edges.iter().any(|other| {
            other.from == e.from
                && other.to != e.to
                && matches!(
                    other.condition,
                    Some(EdgeCondition::NodeStatus {
                        node: ref n2,
                        equals: StatusEq::Failure,
                    }) if n2 == node
                )
                && playbook.edges.iter().position(|x| std::ptr::eq(x, other))
                    < playbook.edges.iter().position(|x| std::ptr::eq(x, e))
        });
        if failure_first {
            r.warn(
                "V175",
                Some(&e.from),
                format!(
                    "edge from `{}` routes on `stalled` after an edge of the same source already routes `failure` on `{node}`, which is always taken first",
                    e.from
                ),
            );
        }
    }
}
