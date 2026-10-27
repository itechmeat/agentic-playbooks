//! Budget rules (V96-V98): the `budget` block limits something, its limits
//! are positive amounts, and the playbook has a node the gate applies to.

use super::*;

/// Runs every budget rule.
pub(crate) fn check_budget(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(block) = &playbook.budget else {
        return;
    };
    if block.tokens.is_none() && block.usd.is_none() {
        r.error(
            "V96",
            None,
            "budget declares neither `tokens` nor `usd`, so it limits nothing".into(),
        );
    }
    if block.tokens == Some(0) {
        r.error("V97", None, "budget.tokens must be greater than 0".into());
    }
    if let Some(usd) = block.usd
        && !(usd.is_finite() && usd > 0.0)
    {
        r.error(
            "V97",
            None,
            "budget.usd must be a finite amount greater than 0".into(),
        );
    }
    if !playbook.nodes.iter().any(|n| n.kind.budget_gated()) {
        r.warn(
            "V98",
            None,
            "budget never applies: the playbook has no agent node and no `playbook` node".into(),
        );
    }
}
