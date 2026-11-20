//! Why the engine paused a run, read back from the reason it journaled: a
//! quota (`quota (<agent>/<model>)`, see `pause::quota`), the budget
//! (`budget (...)`, see `pause::budget`), billed use (`billing (<agent>:
//! <basis>)`, see `pause::billing`) or a wait park (`wait (<node>)`, see
//! `scheduler::wait_park`). A leaf: the reason grammar both pause kinds
//! write and every surface reads.

use super::kind::{PauseBy, PauseCause};
use apb_core::registry::is_safe_segment;

/// How a quota pause's reason starts.
pub const QUOTA_PREFIX: &str = "quota (";
/// How a budget pause's reason starts.
pub const BUDGET_PREFIX: &str = "budget (";
/// How a billing pause's reason starts.
pub const BILLING_PREFIX: &str = "billing (";
/// How a wait park's reason starts.
pub const WAIT_PREFIX: &str = "wait (";

/// The reason of a park on `node`: `wait (<node>)`, the node verbatim.
pub fn wait_reason(node: &str) -> String {
    format!("{WAIT_PREFIX}{node})")
}

/// The node of the one reason form `wait (<node>)`: the fixed prefix, the
/// final `)`, the node id verbatim between, and it must be a safe segment.
/// No second form exists; a later wait kind brings its own reason form.
pub fn parse_wait_reason(reason: &str) -> Option<&str> {
    let node = reason.strip_prefix(WAIT_PREFIX)?.strip_suffix(')')?;
    is_safe_segment(node).then_some(node)
}

/// The `(agent, basis)` a billing pause names, read back from its reason
/// `billing (<agent>: <basis>)`.
pub fn parse_billing_reason(reason: &str) -> Option<(String, String)> {
    let inner = reason.strip_prefix(BILLING_PREFIX)?.strip_suffix(')')?;
    let (agent, basis) = inner.split_once(": ")?;
    (!agent.is_empty() && !basis.is_empty()).then(|| (agent.to_string(), basis.to_string()))
}

/// The `(agent, model)` a quota pause names, read back from its reason. An
/// agent id holds no `/`, so the first one separates it from the model
/// (which may hold more, `zai-start/GLM-5.3`).
pub fn parse_quota_reason(reason: &str) -> Option<(String, String)> {
    let inner = reason.strip_prefix(QUOTA_PREFIX)?.strip_suffix(')')?;
    let (agent, model) = inner.split_once('/')?;
    (!agent.is_empty()).then(|| (agent.to_string(), model.to_string()))
}

/// Whether `reason` is a budget pause's.
pub fn is_budget_reason(reason: &str) -> bool {
    reason.starts_with(BUDGET_PREFIX) && reason.ends_with(')')
}

/// The cause of an engine pause with `reason`: a quota, the budget, billed
/// use or a wait park. `None` for a pause someone else asked for, whatever
/// its reason says.
pub fn cause_of(reason: &str, by: PauseBy) -> Option<PauseCause> {
    if by != PauseBy::Engine {
        return None;
    }
    if parse_quota_reason(reason).is_some() {
        Some(PauseCause::Quota)
    } else if is_budget_reason(reason) {
        Some(PauseCause::Budget)
    } else if parse_billing_reason(reason).is_some() {
        Some(PauseCause::Billing)
    } else if parse_wait_reason(reason).is_some() {
        Some(PauseCause::Wait)
    } else {
        None
    }
}

/// Whether a journaled `run_paused` with `reason` by `by` is a park: the
/// engine's own pause with the wait reason. The one predicate the clock,
/// the waker, the notify fold and the interventions count share.
pub fn is_park_event(reason: &str, by: Option<PauseBy>) -> bool {
    by.is_some_and(|by| cause_of(reason, by) == Some(PauseCause::Wait))
}

#[cfg(test)]
mod tests {
    use super::{PauseCause, cause_of, parse_wait_reason, wait_reason};
    use crate::pause::kind::PauseBy;

    #[test]
    fn a_wait_reason_is_a_wait_only_from_the_engine() {
        assert_eq!(
            cause_of("wait (pause)", PauseBy::Engine),
            Some(PauseCause::Wait)
        );
        assert_eq!(
            cause_of("wait (a, b (c))", PauseBy::Engine),
            Some(PauseCause::Wait)
        );
        assert_eq!(cause_of("wait (pause)", PauseBy::Operator), None);
        assert_eq!(cause_of("wait (pause)", PauseBy::Supervisor), None);
        // `a/b` is not a safe node id segment, so the reason reads as
        // nothing even from the engine.
        assert_eq!(cause_of("wait (a/b)", PauseBy::Engine), None);
    }

    #[test]
    fn parse_wait_reason_refuses_an_unsafe_node() {
        assert_eq!(parse_wait_reason("wait (a/b)"), None);
        assert_eq!(parse_wait_reason("wait (a..b)"), None);
    }

    #[test]
    fn parse_wait_reason_round_trips_any_valid_node() {
        for node in ["w", "a, inbox", "x (y)", "a)", "a, b (c)"] {
            assert_eq!(
                parse_wait_reason(&wait_reason(node)),
                Some(node),
                "node {node}"
            );
        }
        // The malformed forms read as nothing.
        assert_eq!(parse_wait_reason("wait ()"), None);
        assert_eq!(parse_wait_reason("wait (w"), None);
        assert_eq!(parse_wait_reason("wait (w) x"), None);
    }
}
