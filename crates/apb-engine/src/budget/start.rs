//! The budget before a run starts, shared by every launch surface: the
//! estimate the person sees, the budget that will be in force, whether the
//! person must decide what happens before an unestimated node, and the
//! warnings worth showing next to it.

use std::path::PathBuf;

use apb_core::budget::{BudgetRequest, EffectiveBudget, OnNoEstimate};
use apb_core::store::ResolvedPlaybook;

use super::kind::{tokens_text, usd_text};
use crate::error::EngineError;
use crate::run_estimate::{EstimateTarget, RunEstimate, estimate};

/// What a launch surface checks before it starts a run.
#[derive(Debug, Clone)]
pub struct StartCheck {
    pub estimate: RunEstimate,
    /// The budget the run will start with; `None` without one.
    pub budget: Option<EffectiveBudget>,
    /// The policy is `ask`, the argument left it so, and a gated node has
    /// no estimate in a dimension the budget limits: the person picks
    /// `pause` or `continue` first.
    pub needs_decision: bool,
    /// Those gated nodes (see [`RunEstimate::unestimated_for`]).
    pub undecided: Vec<String>,
    /// Why the budget may not work as the person expects.
    pub warnings: Vec<String>,
}

/// The check for `target`, from the runs under `roots`, with the run's
/// budget argument `req`.
pub fn check(
    roots: &[PathBuf],
    target: &EstimateTarget,
    req: Option<&BudgetRequest>,
) -> StartCheck {
    let est = estimate(roots, target);
    let budget = apb_core::budget::effective(target.playbook.budget.as_ref(), req);
    let undecided = budget
        .filter(|b| b.on_no_estimate == OnNoEstimate::Ask)
        .filter(|_| req.and_then(|r| r.on_no_estimate).is_none())
        .map(|b| est.unestimated_for(&b.limit))
        .unwrap_or_default();
    let warnings = budget.map(|b| warnings(&est, &b)).unwrap_or_default();
    StartCheck {
        estimate: est,
        budget,
        needs_decision: !undecided.is_empty(),
        undecided,
        warnings,
    }
}

/// [`check`] for a resolved playbook, from the runs of the project it runs
/// in, in execution mode `execution` (`cli` or `host`).
pub fn check_resolved(
    resolved: &ResolvedPlaybook,
    execution: &str,
    req: Option<&BudgetRequest>,
) -> Result<StartCheck, EngineError> {
    let target = EstimateTarget::for_resolved(resolved, execution)?;
    Ok(check(
        std::slice::from_ref(&resolved.execution_root),
        &target,
        req,
    ))
}

fn warnings(est: &RunEstimate, b: &EffectiveBudget) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(limit) = b.limit.usd {
        match &est.cost_usd {
            None => out.push(
                "a dollar limit is set, but no comparable attempt reported a cost: the gate \
                 cannot weigh dollars, so on_no_estimate decides before each gated node"
                    .to_string(),
            ),
            Some(c) if c.spread.p50 > limit => out.push(format!(
                "the estimated cost (~{}) is over the dollar limit ({}): the run is likely to pause",
                usd_text(c.spread.p50),
                usd_text(limit)
            )),
            Some(_) => out.extend(partial(est, &est.unestimated_cost, "cost")),
        }
    }
    if b.limit.tokens.is_some() && est.tokens.is_some() {
        out.extend(partial(est, &est.unestimated_tokens, "token"));
    }
    if let (Some(limit), Some(t)) = (b.limit.tokens, &est.tokens)
        && t.spread.p50 > limit
    {
        out.push(format!(
            "the estimated tokens (~{}) are over the token limit ({}): the run is likely to pause",
            tokens_text(t.spread.p50 as f64),
            tokens_text(limit as f64)
        ));
    }
    out
}

/// The warning that the run's figure in a dimension (`what`) leaves out
/// nodes estimated in the other one only; `None` when it leaves out none.
fn partial(est: &RunEstimate, missing: &[String], what: &str) -> Option<String> {
    let left_out: Vec<&str> = missing
        .iter()
        .filter(|n| !est.unestimated.contains(n))
        .map(String::as_str)
        .collect();
    (!left_out.is_empty()).then(|| {
        format!(
            "the estimated {what} figure leaves out {} (no {what} history): on_no_estimate decides before {}",
            left_out.join(", "),
            if left_out.len() == 1 { "it" } else { "each of them" }
        )
    })
}
