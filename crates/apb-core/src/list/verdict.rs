//! One item's states within one visit of a list node, and the pure
//! transition from what just happened to the item to where it goes next.
//!
//! `passed` and `needs_look` are terminal. A rejected item is requeued
//! while it has `batch.retries` left; an interrupted item (a `--now` pause,
//! a quota pause, a crash) goes back to pending without spending one; an
//! item whose send's outcome is unknown either gets one free reconcile run
//! (a send the service can recognise again) or needs a look (one it cannot).

use crate::connector::idempotency::IdempotencyMode;

/// The verdict of an item check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemVerdict {
    Accept,
    AcceptWithNote,
    Reject,
}

impl ItemVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemVerdict::Accept => "accept",
            ItemVerdict::AcceptWithNote => "accept_with_note",
            ItemVerdict::Reject => "reject",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "accept" => Some(ItemVerdict::Accept),
            "accept_with_note" => Some(ItemVerdict::AcceptWithNote),
            "reject" => Some(ItemVerdict::Reject),
            _ => None,
        }
    }
}

/// Where an item stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Pending,
    Running,
    Passed,
    NeedsLook,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Running => "running",
            ItemState::Passed => "passed",
            ItemState::NeedsLook => "needs_look",
        }
    }

    /// The state `word` names (as [`ItemState::as_str`] writes it).
    pub fn parse(word: &str) -> Option<Self> {
        [
            ItemState::Pending,
            ItemState::Running,
            ItemState::Passed,
            ItemState::NeedsLook,
        ]
        .into_iter()
        .find(|s| s.as_str() == word)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, ItemState::Passed | ItemState::NeedsLook)
    }
}

/// Why an item needs a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsLookReason {
    /// Its check rejected it and its re-runs are spent.
    Rejected,
    /// Its check could not tell (an unreadable connector read, an
    /// unavailable judge set to `needs_look`).
    CheckUnknown,
    /// A send it made has an unknown outcome the service cannot tell.
    UnknownSend,
    /// Its step failed.
    Failed,
    /// Its JSON is larger than the step is handed.
    ItemTooLarge,
}

impl NeedsLookReason {
    pub fn as_str(self) -> &'static str {
        match self {
            NeedsLookReason::Rejected => "rejected",
            NeedsLookReason::CheckUnknown => "check_unknown",
            NeedsLookReason::UnknownSend => "unknown_send",
            NeedsLookReason::Failed => "failed",
            NeedsLookReason::ItemTooLarge => "item_too_large",
        }
    }
}

/// What just happened to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemEvent {
    /// Its step succeeded and its check (or the absence of one) decided.
    Checked(ItemVerdict),
    /// Its step succeeded and its check could not decide.
    CheckUnknown,
    /// Its step failed; `unknown_send` is the mode of a send it made whose
    /// outcome is unknown, if any.
    Failed {
        unknown_send: Option<IdempotencyMode>,
    },
    /// A pause, a quota pause or a crash cut it short.
    Interrupted,
}

/// Where an item goes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Terminal: it passed (`with_note` for `accept_with_note`).
    Pass { with_note: bool },
    /// Run it again: `round` is its round then; `reconcile` marks the free
    /// run that settles an unknown send.
    Requeue { round: u32, reconcile: bool },
    /// Terminal: it needs a look.
    NeedsLook(NeedsLookReason),
    /// Back to pending in the same round (nothing spent).
    Pending,
}

/// The transition of an item in `round` (0 for its first run) after
/// `event`, with `retries` re-runs for a rejection and `reconciled` set once
/// it had its reconcile run.
pub fn transition(event: ItemEvent, round: u32, retries: u32, reconciled: bool) -> Transition {
    match event {
        ItemEvent::Checked(ItemVerdict::Accept) => Transition::Pass { with_note: false },
        ItemEvent::Checked(ItemVerdict::AcceptWithNote) => Transition::Pass { with_note: true },
        ItemEvent::Checked(ItemVerdict::Reject) if round < retries => Transition::Requeue {
            round: round + 1,
            reconcile: false,
        },
        ItemEvent::Checked(ItemVerdict::Reject) => Transition::NeedsLook(NeedsLookReason::Rejected),
        ItemEvent::CheckUnknown => Transition::NeedsLook(NeedsLookReason::CheckUnknown),
        ItemEvent::Failed {
            unknown_send: Some(IdempotencyMode::None),
        } => Transition::NeedsLook(NeedsLookReason::UnknownSend),
        ItemEvent::Failed {
            unknown_send: Some(IdempotencyMode::Key | IdempotencyMode::Marker),
        } if !reconciled => Transition::Requeue {
            round,
            reconcile: true,
        },
        ItemEvent::Failed {
            unknown_send: Some(_),
        } => Transition::NeedsLook(NeedsLookReason::UnknownSend),
        ItemEvent::Failed { unknown_send: None } => Transition::NeedsLook(NeedsLookReason::Failed),
        ItemEvent::Interrupted => Transition::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of the state machine: the event, the item's round, its
    /// re-runs, whether it had its reconcile run, and the transition.
    #[test]
    fn the_transition_table() {
        use IdempotencyMode::{Key, Marker, None as NoMode};
        use ItemEvent::*;
        use ItemVerdict::*;
        let rows: [(ItemEvent, u32, u32, bool, Transition); 12] = [
            (
                Checked(Accept),
                0,
                1,
                false,
                Transition::Pass { with_note: false },
            ),
            (
                Checked(AcceptWithNote),
                1,
                1,
                false,
                Transition::Pass { with_note: true },
            ),
            (
                Checked(Reject),
                0,
                1,
                false,
                Transition::Requeue {
                    round: 1,
                    reconcile: false,
                },
            ),
            (
                Checked(Reject),
                1,
                1,
                false,
                Transition::NeedsLook(NeedsLookReason::Rejected),
            ),
            (
                Checked(Reject),
                0,
                0,
                false,
                Transition::NeedsLook(NeedsLookReason::Rejected),
            ),
            (
                CheckUnknown,
                0,
                3,
                false,
                Transition::NeedsLook(NeedsLookReason::CheckUnknown),
            ),
            (
                Failed {
                    unknown_send: Some(NoMode),
                },
                0,
                3,
                false,
                Transition::NeedsLook(NeedsLookReason::UnknownSend),
            ),
            (
                Failed {
                    unknown_send: Some(Key),
                },
                1,
                3,
                false,
                Transition::Requeue {
                    round: 1,
                    reconcile: true,
                },
            ),
            (
                Failed {
                    unknown_send: Some(Marker),
                },
                0,
                3,
                true,
                Transition::NeedsLook(NeedsLookReason::UnknownSend),
            ),
            (
                Failed { unknown_send: None },
                0,
                3,
                false,
                Transition::NeedsLook(NeedsLookReason::Failed),
            ),
            (Interrupted, 2, 3, false, Transition::Pending),
            (Interrupted, 0, 0, true, Transition::Pending),
        ];
        for (event, round, retries, reconciled, want) in rows {
            assert_eq!(
                transition(event, round, retries, reconciled),
                want,
                "{event:?} in round {round} of {retries}, reconciled {reconciled}"
            );
        }
    }

    #[test]
    fn words_round_trip() {
        for v in [
            ItemVerdict::Accept,
            ItemVerdict::AcceptWithNote,
            ItemVerdict::Reject,
        ] {
            assert_eq!(ItemVerdict::parse(v.as_str()), Some(v));
        }
        assert_eq!(ItemVerdict::parse("maybe"), None);
        assert!(ItemState::Passed.is_terminal() && ItemState::NeedsLook.is_terminal());
        assert!(!ItemState::Pending.is_terminal() && !ItemState::Running.is_terminal());
    }
}
