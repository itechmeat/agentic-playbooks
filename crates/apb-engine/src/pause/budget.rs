//! The budget pause: the engine pauses a run before a gated node (or a list
//! batch) its budget's gate refused (see `crate::budget`). Drain semantics,
//! no `until`: a budget pause waits for a person, who raises or sets the
//! budget, or lets the node run once, at resume.

use crate::budget::kind::{BudgetAmount, GateBasis, tokens_text, usd_text};

use super::{PauseBy, PauseMode, PauseRequest};

use super::cause::BUDGET_PREFIX;
pub use super::cause::is_budget_reason;

/// The `run_paused.reason` of a budget pause before `node`, for example
/// `budget (tokens: 12000 left, `review` needs ~30000)`.
pub fn budget_reason(
    node: &str,
    basis: GateBasis,
    need: Option<&BudgetAmount>,
    remaining: &BudgetAmount,
) -> String {
    let inner = match basis {
        GateBasis::NoEstimate => format!("`{node}` has no estimate"),
        GateBasis::UnknownSpend => format!("spent amount unknown before `{node}`"),
        GateBasis::Exhausted => format!("nothing left before `{node}`"),
        _ => {
            let pick = |left: Option<f64>, needs: Option<f64>, f: fn(f64) -> String, dim: &str| {
                let left = left?;
                let needs = needs.unwrap_or(0.0);
                (left <= 0.0 || left < needs)
                    .then(|| format!("{dim}: {} left, `{node}` needs ~{}", f(left), f(needs)))
            };
            let need = need.copied().unwrap_or_default();
            pick(remaining.tokens, need.tokens, tokens_text, "tokens")
                .or_else(|| pick(remaining.usd, need.usd, usd_text, "usd"))
                .unwrap_or_else(|| format!("`{node}` needs more than is left"))
        }
    };
    format!("{BUDGET_PREFIX}{inner})")
}

/// The pause request of a budget pause: by the engine, drain (the members
/// already in flight finish), no `until`.
pub fn budget_pause(reason: String) -> PauseRequest {
    PauseRequest {
        reason: Some(reason),
        by: Some(PauseBy::Engine),
        mode: PauseMode::Drain,
        until: None,
        keep_lock: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventPayload};
    use crate::pause::{PauseCause, pause_info};
    use crate::state::RunStatus;

    #[test]
    fn reason_round_trips() {
        let r = budget_reason(
            "review",
            GateBasis::Median,
            Some(&BudgetAmount {
                tokens: Some(30_000.0),
                usd: None,
            }),
            &BudgetAmount {
                tokens: Some(12_000.0),
                usd: None,
            },
        );
        assert_eq!(r, "budget (tokens: 12000 left, `review` needs ~30000)");
        assert!(is_budget_reason(&r));
        let r = budget_reason("x", GateBasis::NoEstimate, None, &BudgetAmount::default());
        assert!(is_budget_reason(&r));
        assert!(!is_budget_reason("quota (claude/opus)"));
        assert!(!is_budget_reason("operator pause"));
        let req = budget_pause(r.clone());
        assert_eq!((req.by, req.until), (Some(PauseBy::Engine), None));
        assert_eq!(req.journal_reason(), r);
    }

    #[test]
    fn pause_info_reports_cause_budget_and_quota() {
        let paused = |reason: &str, by: PauseBy| {
            let events = vec![Event {
                seq: 1,
                ts: 10,
                payload: EventPayload::RunPaused {
                    reason: reason.into(),
                    by: Some(by),
                    mode: None,
                    until: None,
                    keep_lock: false,
                },
            }];
            pause_info(&events, RunStatus::Paused, None, None)
                .unwrap()
                .cause
        };
        assert_eq!(
            paused("budget (`a` has no estimate)", PauseBy::Engine),
            Some(PauseCause::Budget)
        );
        assert_eq!(
            paused(
                &crate::pause::quota::quota_reason("claude", "opus"),
                PauseBy::Engine
            ),
            Some(PauseCause::Quota)
        );
        // A person's reason that looks like one is not an engine cause.
        assert_eq!(paused("budget (mine)", PauseBy::Operator), None);
        assert_eq!(paused("lunch", PauseBy::Engine), None);
    }
}
