//! Why the engine paused a run, read back from the reason it journaled: a
//! quota (`quota (<agent>/<model>)`, see `pause::quota`), the budget
//! (`budget (...)`, see `pause::budget`) or billed use (`billing (<agent>:
//! <basis>)`, see `pause::billing`). A leaf: the reason grammar both
//! pause kinds write and every surface reads.

use super::kind::{PauseBy, PauseCause};

/// How a quota pause's reason starts.
pub const QUOTA_PREFIX: &str = "quota (";
/// How a budget pause's reason starts.
pub const BUDGET_PREFIX: &str = "budget (";
/// How a billing pause's reason starts.
pub const BILLING_PREFIX: &str = "billing (";

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

/// The cause of an engine pause with `reason`: a quota or the budget.
/// `None` for a pause someone else asked for, whatever its reason says.
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
    } else {
        None
    }
}
