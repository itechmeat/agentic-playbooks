//! Retry delay rules (V126-V130): a `retry_delay` block on an `agent_task` or
//! on `defaults` has a parseable `initial` between 1 s and 7 days, a `factor` in
//! `[1, 10]` and a `max` between `initial` and 7 days; a list node may not
//! carry one, and a delay that no retry ever uses is reported.

use super::*;
use crate::schema::RetryDelay;

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
