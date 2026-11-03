//! The billing pause: right before an agent spawn, the basis of its work is
//! read again from the facts the spawn sees (`crate::adapter::read_cost_basis`),
//! and work that is billed or of unknown basis the run holds no confirmation
//! for ([`apb_core::billing::BillingRecord::covers`]) is not started: the
//! engine pauses the run instead (drain, no `until`), with a reason that
//! names the agent and the basis, `billing (codex: billed)`.
//!
//! Only a person continues it: a resume confirms that agent and basis
//! (`crate::resume_launch`), a plain resume, a supervisor and the engine
//! are refused. A run whose launch record holds no billing record (started
//! before this check existed, or by an engine entry that passes no gate) is
//! checked against the `billing:` settings in force.

use std::path::Path;

use apb_core::agents::CostBasis;
use apb_core::billing::{BillingEntry, EffectiveBilling};

use super::cause::{BILLING_PREFIX, parse_billing_reason};
use super::{PauseBy, PauseMode, PauseRequest};
use crate::event::{Event, EventPayload};

/// The `run_paused.reason` of a billing pause of `entry`.
pub fn billing_reason(entry: &BillingEntry) -> String {
    format!("{BILLING_PREFIX}{})", entry.label())
}

/// The pause request of a billing pause: by the engine, drain, no `until`.
pub fn billing_pause(entry: &BillingEntry) -> PauseRequest {
    PauseRequest {
        reason: Some(billing_reason(entry)),
        by: Some(PauseBy::Engine),
        mode: PauseMode::Drain,
        until: None,
        keep_lock: false,
    }
}

/// The entry and the seq of the billing pause that holds the run now: its
/// latest pause or resume is an engine pause with a billing reason.
pub fn held_entry(events: &[Event]) -> Option<(BillingEntry, u64)> {
    let last = events.iter().rev().find(|e| {
        matches!(
            e.payload,
            EventPayload::RunPaused { .. } | EventPayload::RunResumed { .. }
        )
    })?;
    let EventPayload::RunPaused {
        reason,
        by: Some(PauseBy::Engine),
        ..
    } = &last.payload
    else {
        return None;
    };
    let (agent, basis) = parse_billing_reason(reason)?;
    let basis = CostBasis::ALL.into_iter().find(|b| b.key() == basis)?;
    Some((BillingEntry { agent, basis }, last.seq))
}

/// The pause a spawn of `agent` whose work reads as `basis` takes in the
/// run at `run_dir`; `None` when the run may start it. A scenario run
/// starts no agent (its agents are scripted, and its process guard refuses
/// any program start), so nothing there is billed or held.
pub fn spawn_hold(run_dir: &Path, agent: &str, basis: CostBasis) -> Option<PauseRequest> {
    if crate::scenario::session::is_scenario(run_dir) {
        return None;
    }
    let agent = crate::gate::billing::canonical(agent);
    let covered = match crate::gate::launch::run_authorization(run_dir).and_then(|r| r.billing) {
        Some(record) => record.covers(&agent, basis),
        None => !crate::gate::billing::held(&settings_of(run_dir), basis),
    };
    (!covered).then(|| billing_pause(&BillingEntry { agent, basis }))
}

/// The `billing:` settings of the workspace a run directory belongs to
/// (`<root>/.apb/runs/<id>`); a file that does not read denies
/// ([`EffectiveBilling::load`]).
pub fn settings_of(run_dir: &Path) -> EffectiveBilling {
    let root = run_dir.ancestors().nth(3);
    let machine = apb_core::policy_file::machine_path();
    EffectiveBilling::load(root, machine.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_round_trips_and_only_an_engine_pause_holds() {
        let entry = BillingEntry {
            agent: "codex".into(),
            basis: CostBasis::Billed,
        };
        let reason = billing_reason(&entry);
        assert_eq!(reason, "billing (codex: billed)");
        let paused = |by| Event {
            seq: 4,
            ts: 1,
            payload: EventPayload::RunPaused {
                reason: reason.clone(),
                by: Some(by),
                mode: None,
                until: None,
                keep_lock: false,
            },
        };
        assert_eq!(
            held_entry(&[paused(PauseBy::Engine)]),
            Some((entry.clone(), 4))
        );
        assert_eq!(held_entry(&[paused(PauseBy::Operator)]), None);
        assert_eq!(
            super::super::cause_of(&reason, PauseBy::Engine),
            Some(super::super::PauseCause::Billing)
        );
    }
}
