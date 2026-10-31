//! A run's notifications as the run surfaces report them: whether they are
//! on, where they go, about what, what was sent, and why a subscription
//! could not be used. The target shows no digest and no secret.

use std::path::Path;

use apb_core::connector::sites::NOTIFY_SITE;
use apb_core::notify::{NotifyTarget, Transition};
use serde::Serialize;

use super::NotifyState;
use super::transitions::{state_name, transition_of};

use crate::connector::intent::{IntentState, Ledger, effective_state, ledger};
use crate::event::Event;
use crate::state::RunState;

/// The `notify` block of a run.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunNotify {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub target: Option<NotifyTarget>,
    pub on: Vec<Transition>,
    /// Where the subscription in force came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub source: Option<String>,
    /// Each transition notified, with what became of its latest try.
    pub sent: Vec<NotifySent>,
    pub problems: Vec<String>,
}

/// One notified transition.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotifySent {
    /// `<kind>.<seq>`, e.g. `review.12`.
    pub transition: String,
    /// `pending`, `confirmed`, `not_sent` or `unknown`.
    pub state: String,
    /// When its latest try was recorded, epoch milliseconds.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub at: u64,
}

impl RunNotify {
    /// The block for the journal `events` as of `now_ms`, from the run's
    /// own subscription; `None` for a run that never had one.
    pub fn from_events(events: &[Event], now_ms: u64) -> Option<Self> {
        Self::build(events, NotifyState::fold(events), now_ms)
    }

    /// The block of the run at `run_dir` whose journal is `events`, with
    /// the subscription in force as the sends read it: for a sub-playbook,
    /// its parent's current target and transitions.
    pub fn for_run(run_dir: &Path, events: &[Event], now_ms: u64) -> Option<Self> {
        Self::build(events, NotifyState::in_force(run_dir, events), now_ms)
    }

    fn build(events: &[Event], state: NotifyState, now_ms: u64) -> Option<Self> {
        if !state.recorded {
            return None;
        }
        let ledger = Ledger::fold(events);
        let mut sent: Vec<NotifySent> = Vec::new();
        for rec in ledger
            .records()
            .iter()
            .filter(|r| r.key.node == NOTIFY_SITE)
        {
            let Some(transition) = transition_of(&rec.key.label) else {
                continue;
            };
            let try_state = effective_state(rec, now_ms);
            let earlier = sent.iter_mut().find(|s| s.transition == transition);
            // A later try that did not go out says nothing of an earlier one
            // nobody knows about: that one may have reached the person.
            let unknown_before = earlier.as_ref().is_some_and(|s| s.state == ledger::UNKNOWN);
            let name = match try_state {
                IntentState::NotSent { .. } if unknown_before => ledger::UNKNOWN,
                _ => state_name(&try_state),
            };
            let row = NotifySent {
                transition: transition.to_string(),
                state: name.to_string(),
                at: rec.ts_ms,
            };
            match earlier {
                Some(s) => *s = row,
                None => sent.push(row),
            }
        }
        let mut problems = state.problems;
        // The sweeps end with the drive: a notification that failed at the
        // end of the run is not tried again, and the person should know.
        if RunState::fold(events).run_status.is_terminal() {
            problems.extend(
                sent.iter()
                    .filter(|s| s.state == ledger::NOT_SENT)
                    .map(|s| {
                        format!(
                            "the notification of `{}` was not sent, and the run has ended, so nothing tries it again",
                            s.transition
                        )
                    }),
            );
        }
        let active = state.active.as_ref();
        Some(RunNotify {
            active: active.is_some(),
            target: active.map(|a| a.target.target()),
            on: active.map(|a| a.on.clone()).unwrap_or_default(),
            source: active.map(|a| a.source.clone()),
            sent,
            problems,
        })
    }
}
