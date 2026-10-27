//! The send intents of a run (see [`crate::connector::intent`]), as rows for
//! the run surfaces and `apb intent list`: what each send was, whether it
//! went out, and the ones nobody can tell about, which a person resolves.

use serde::Serialize;

use crate::connector::intent::{
    IntentRecord, IntentState, Ledger, effective_state, ledger, send_verdict,
};
use crate::event::Event;

/// One send intent and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunIntent {
    pub id: String,
    pub node: String,
    /// The attempt number that recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub connector: String,
    pub function: String,
    pub account: String,
    /// The caller's `--intent` name, or `args:<hash>` without one.
    pub label: String,
    /// `key`, `marker` or `none`.
    pub mode: String,
    /// `pending`, `confirmed`, `not_sent` or `unknown`. A pending intent
    /// whose sender is gone reads as `unknown`.
    pub state: String,
    /// What settled it: `response`, `reconcile`, `key_replay`, `gate`,
    /// `person`, or `orphan` for a pending intent whose sender is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What the service returned for it (a message id, a URL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_ref: Option<String>,
    /// Who decided, for a person's settlement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<String>,
    /// A later intent that sent it again with the same nonce: what became
    /// of that one is what became of this send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replayed_by: Option<String>,
    /// A later intent that sent it again as a new send.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resent_by: Option<String>,
    /// Whether this intent holds its send open: nobody can tell whether the
    /// send went out, and no replay of it tells either, so a person resolves
    /// it. A resend is a separate send and does not close it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
}

const PENDING: &str = "pending";

impl RunIntent {
    fn of(ledger: &Ledger, rec: &IntentRecord, now_ms: u64) -> Self {
        let (state, source, detail, effect_ref) = match effective_state(rec, now_ms) {
            IntentState::Pending { .. } => (PENDING, None, None, None),
            IntentState::Confirmed { source, effect_ref } => {
                (ledger::CONFIRMED, Some(source), None, effect_ref)
            }
            IntentState::NotSent { source } => (ledger::NOT_SENT, Some(source), None, None),
            IntentState::Unknown { source, detail } => {
                (ledger::UNKNOWN, Some(source), detail, None)
            }
        };
        let verdict = send_verdict(ledger, rec, now_ms);
        let open =
            matches!(verdict.state, IntentState::Unknown { .. }) && verdict.member.id == rec.id;
        let next = rec.superseded_by.as_deref().and_then(|id| ledger.get(id));
        let replay = next.filter(|n| n.replay_of.as_deref() == Some(rec.id.as_str()));
        RunIntent {
            id: rec.id.clone(),
            node: rec.key.node.clone(),
            attempt: rec.attempt,
            connector: rec.key.connector.clone(),
            function: rec.key.function.clone(),
            account: rec.key.account.clone(),
            label: rec.key.label.clone(),
            mode: rec.mode.as_str().to_string(),
            state: state.to_string(),
            source,
            detail,
            effect_ref,
            settled_by: rec.settled_by.clone(),
            replayed_by: replay.map(|n| n.id.clone()),
            resent_by: next.filter(|_| replay.is_none()).map(|n| n.id.clone()),
            open,
        }
    }

    /// `telegram.send_message`.
    pub fn target(&self) -> String {
        format!("{}.{}", self.connector, self.function)
    }

    /// Whether this is a notification apb sent a person about the run (see
    /// `crate::notify`), not one of the run's own sends. `apb intent list`
    /// shows it; the outcome views leave it out.
    pub fn is_notification(&self) -> bool {
        self.node == apb_core::connector::sites::NOTIFY_SITE
    }

    /// Whether nobody can tell if this send went out, and no replay of it
    /// tells either: a person has to resolve it.
    pub fn is_open_unknown(&self) -> bool {
        self.open
    }

    /// `by announce attempt 2`.
    fn by(&self) -> String {
        let attempt = self
            .attempt
            .map(|a| format!(" attempt {a}"))
            .unwrap_or_default();
        format!("by {}{attempt}", self.node)
    }

    /// `intent "announce" (si-1) by announce attempt 2`.
    fn who(&self) -> String {
        format!("intent \"{}\" ({}) {}", self.label, self.id, self.by())
    }

    /// The detail of this intent's gap in the run outcome, with the command
    /// that resolves it.
    pub fn gap_detail(&self, run_id: &str) -> String {
        let why = self.detail.as_deref().unwrap_or("no answer was recorded");
        format!(
            "{}: whether it was sent is not known ({why}); resolve with apb intent resolve {run_id} {} --sent|--not-sent",
            self.who(),
            self.id
        )
    }

    /// The done line of a send confirmed without a successful call of its
    /// own (found by apb's lookup, or confirmed by a person); `None` for
    /// every other intent, whose call already counts.
    pub fn done_line(&self) -> Option<String> {
        if self.state != ledger::CONFIRMED {
            return None;
        }
        let how = match self.source.as_deref() {
            Some("reconcile") => "found sent by apb's lookup".to_string(),
            Some("person") => format!(
                "confirmed sent by {}",
                self.settled_by.as_deref().unwrap_or("a person")
            ),
            _ => return None,
        };
        let at = self
            .effect_ref
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        Some(format!("{}: {} {how}{at}", self.target(), self.who()))
    }

    /// One plain line for `apb intent list`.
    pub fn line(&self) -> String {
        let source = self
            .source
            .as_deref()
            .map(|s| format!(" via {s}"))
            .unwrap_or_default();
        let detail = self
            .detail
            .as_deref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        let next = match (&self.replayed_by, &self.resent_by) {
            (Some(id), _) => format!(" (replayed by {id})"),
            (None, Some(id)) => format!(" (sent again by {id})"),
            _ => String::new(),
        };
        format!(
            "{} {} intent \"{}\" {} [{}{source}]{detail}{next}",
            self.id,
            self.target(),
            self.label,
            self.by(),
            self.state
        )
    }
}

/// The send intents the outcome views weigh: every one of `events` but
/// the notifications about the run.
pub fn outcome_intents(events: &[Event], now_ms: u64) -> Vec<RunIntent> {
    let mut rows = intents_from(events, now_ms);
    rows.retain(|r| !r.is_notification());
    rows
}

/// Every send intent of `events`, in journal order, read as of `now_ms`.
pub fn intents_from(events: &[Event], now_ms: u64) -> Vec<RunIntent> {
    let ledger = Ledger::fold(events);
    ledger
        .records()
        .iter()
        .map(|rec| RunIntent::of(&ledger, rec, now_ms))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventPayload;

    fn recorded(seq: u64, id: &str, mode: &str, replay_of: Option<&str>) -> Event {
        Event {
            seq,
            ts: 1_000,
            payload: EventPayload::SendIntentRecorded {
                item: None,
                intent: id.into(),
                nonce: "n".into(),
                node: "announce".into(),
                attempt: Some(2),
                attempt_seq: None,
                connector: "telegram".into(),
                function: "send_message".into(),
                account: "main".into(),
                mode: mode.into(),
                label: "v1".into(),
                pid: 0,
                deadline_ms: 0,
                replay_of: replay_of.map(str::to_string),
                resend_of: None,
            },
        }
    }

    fn settled(seq: u64, id: &str, outcome: &str, source: &str) -> Event {
        Event {
            seq,
            ts: 2_000,
            payload: EventPayload::SendIntentSettled {
                intent: id.into(),
                outcome: outcome.into(),
                source: source.into(),
                detail: (outcome == "unknown").then(|| "timeout".to_string()),
                effect_ref: (outcome == "confirmed").then(|| "https://x/1".to_string()),
                by: (source == "person").then(|| "ana".to_string()),
            },
        }
    }

    #[test]
    fn an_unknown_send_is_open_until_a_replay_or_a_person_settles_it() {
        let events = [
            recorded(1, "si-a", "key", None),
            settled(2, "si-a", "unknown", "response"),
        ];
        let rows = intents_from(&events, 3_000);
        assert!(rows[0].is_open_unknown());
        assert_eq!(
            rows[0].gap_detail("r1"),
            "intent \"v1\" (si-a) by announce attempt 2: whether it was sent is not known (timeout); resolve with apb intent resolve r1 si-a --sent|--not-sent"
        );
        let replayed = [
            events[0].clone(),
            events[1].clone(),
            recorded(3, "si-b", "key", Some("si-a")),
            settled(4, "si-b", "confirmed", "key_replay"),
        ];
        let rows = intents_from(&replayed, 5_000);
        assert!(!rows[0].is_open_unknown());
        assert_eq!(rows[0].replayed_by.as_deref(), Some("si-b"));
        // A replay's own call counts as the write: no extra done line.
        assert_eq!(rows[1].done_line(), None);
    }

    /// A replay that did not go out leaves the send it repeated open: the
    /// outcome still names it, and a replay that went out closes it.
    #[test]
    fn a_replay_that_did_not_go_out_keeps_the_unknown_send_open() {
        let events = [
            recorded(1, "si-a", "key", None),
            settled(2, "si-a", "unknown", "response"),
            recorded(3, "si-b", "key", Some("si-a")),
            settled(4, "si-b", "not_sent", "response"),
        ];
        let rows = intents_from(&events, 5_000);
        assert!(rows[0].is_open_unknown());
        assert!(!rows[1].is_open_unknown());
        assert_eq!(rows.iter().filter(|r| r.is_open_unknown()).count(), 1);
    }

    #[test]
    fn an_orphaned_pending_intent_reads_as_unknown() {
        let rows = intents_from(&[recorded(1, "si-a", "none", None)], 3_000);
        assert_eq!(rows[0].state, "unknown");
        assert_eq!(rows[0].source.as_deref(), Some("orphan"));
        assert!(rows[0].is_open_unknown());
    }

    #[test]
    fn a_send_found_by_lookup_or_confirmed_by_a_person_gets_a_done_line() {
        let events = [
            recorded(1, "si-a", "marker", None),
            settled(2, "si-a", "confirmed", "reconcile"),
            recorded(3, "si-b", "none", None),
            settled(4, "si-b", "unknown", "response"),
            settled(5, "si-b", "confirmed", "person"),
        ];
        let rows = intents_from(&events, 6_000);
        assert_eq!(
            rows[0].done_line().as_deref(),
            Some(
                "telegram.send_message: intent \"v1\" (si-a) by announce attempt 2 found sent by apb's lookup (https://x/1)"
            )
        );
        assert_eq!(
            rows[1].done_line().as_deref(),
            Some(
                "telegram.send_message: intent \"v1\" (si-b) by announce attempt 2 confirmed sent by ana (https://x/1)"
            )
        );
        assert_eq!(
            rows[1].line(),
            "si-b telegram.send_message intent \"v1\" by announce attempt 2 [confirmed via person]"
        );
    }
}
