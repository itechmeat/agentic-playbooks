//! The run budget, engine side: the limit in force and its history
//! ([`fold`]), what the run spent ([`spent`]), the gate's pure decision
//! ([`gate`]), what a resume may change ([`resume`]), what a launch surface
//! checks before a start ([`start`]) and the status every surface reports
//! ([`view`]). The schema and the precedence of a run
//! argument over the playbook's block are `apb_core::budget`; the drive
//! side is `scheduler::budget_gate`; the pause is `pause::budget`.
//!
//! A budget is a predictive stop, not a security control: the gate admits a
//! gated node only while what is left covers the node's median spend from
//! comparable runs, and a node it admitted may still spend past the limit.
//! Spent is always derived from the journal, never stored: an attempt that
//! reported no usage counts as unknown (estimated by its node's median per
//! attempt), never as zero.

pub mod fold;
pub mod gate;
pub mod kind;
pub mod resume;
pub mod spent;
pub mod start;
pub mod view;

use apb_core::budget::{BudgetBlock, BudgetRequest, EffectiveBudget};

pub use fold::BudgetFold;
pub use kind::{BudgetAmount, BudgetSpent, GateBasis, GateDecision, SpentDim};
pub use resume::{BUDGET_PAUSED, BudgetResume, check_resume};
pub use spent::Spent;
pub use start::{StartCheck, check_resolved};
pub use view::{BudgetStatus, DimStatus, PausedAt};

use crate::error::EngineError;
use crate::event::{EventLog, EventPayload};

/// The `budget_set` a budget in force at start journals.
pub fn set_event(eff: &EffectiveBudget) -> EventPayload {
    EventPayload::BudgetSet {
        tokens: eff.limit.tokens,
        usd: eff.limit.usd,
        on_no_estimate: eff.on_no_estimate,
        source: Some(eff.source),
        by: None,
    }
}

/// Journals the run's budget at start, from the playbook's block and the
/// run argument; nothing for a run without a budget. Never called for a
/// sub-playbook child: its own block is ignored under a parent.
pub(crate) fn record_start(
    log: &mut EventLog,
    block: Option<&BudgetBlock>,
    req: Option<&BudgetRequest>,
) -> Result<Option<EffectiveBudget>, EngineError> {
    let eff = apb_core::budget::effective(block, req);
    if let Some(e) = &eff {
        log.append(set_event(e))?;
    }
    Ok(eff)
}
