//! The composed finish answer, kept until the finish node's checkpoint.
//!
//! A finish with a prompt composes the run answer with an agent, then takes
//! the baseline's after snapshot. A `--now` pause or a stop can interrupt
//! that snapshot; the resume re-enters the finish node and must continue at
//! the snapshot, never run the answer agent a second time. The answer is
//! not in the journal until `node_finished`, so it is kept in the run
//! directory next to the journal position it was composed at, and reused
//! only while nothing after that position can have changed what it
//! describes: a pause or a stop, a resume of the same node, the baseline's own
//! events, a notification about the run and the like. Anything else (a node that started, a supervisor
//! move, a patch, a rebind, a resume that names its start node) composes it
//! again.

use std::collections::HashSet;
use std::path::Path;

use apb_core::connector::sites::NOTIFY_SITE;
use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload, supervisor_action};

const FILE: &str = "finish_answer.json";

#[derive(Serialize, Deserialize)]
struct Composed {
    node: String,
    /// The seq the first event after the composition got.
    next_seq: u64,
    answer: String,
}

/// Keeps `answer` for finish node `node`, composed with the journal ending
/// just before `next_seq`.
pub(crate) fn keep(run_dir: &Path, node: &str, next_seq: u64, answer: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&Composed {
        node: node.to_string(),
        next_seq,
        answer: answer.to_string(),
    })?;
    apb_core::fsutil::atomic_write_private(&run_dir.join(FILE), &bytes)
}

/// The answer kept for `node`, when every event journaled since it was
/// composed leaves it standing.
pub(crate) fn kept(run_dir: &Path, node: &str, events: &[Event]) -> Option<String> {
    let raw = std::fs::read(run_dir.join(FILE)).ok()?;
    let c: Composed = serde_json::from_slice(&raw).ok()?;
    let notices = notification_intents(events);
    let standing = c.node == node
        && events
            .iter()
            .filter(|e| e.seq >= c.next_seq)
            .all(|e| leaves_the_answer(&e.payload, &notices));
    standing.then_some(c.answer)
}

/// Drops the kept answer once the finish node's checkpoint holds it.
pub(crate) fn discard(run_dir: &Path) {
    let _ = std::fs::remove_file(run_dir.join(FILE));
}

/// The ids of the run's notification intents (see `crate::notify`).
fn notification_intents(events: &[Event]) -> HashSet<&str> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SendIntentRecorded { node, intent, .. } if node == NOTIFY_SITE => {
                Some(intent.as_str())
            }
            _ => None,
        })
        .collect()
}

/// Events that do not change what a composed answer describes. A
/// notification about the run (`notices` are its intents) is one of them.
fn leaves_the_answer(p: &EventPayload, notices: &HashSet<&str>) -> bool {
    match p {
        EventPayload::RunPaused { .. }
        | EventPayload::RunAborted { .. }
        | EventPayload::RunQueued { .. }
        | EventPayload::WakeRaised { .. }
        | EventPayload::SupervisorLost { .. }
        | EventPayload::RunProgress { .. }
        | EventPayload::RunError { .. }
        | EventPayload::DecisionMade { .. }
        | EventPayload::ContextCompacted { .. }
        | EventPayload::EnvironmentDriftAccepted { .. }
        | EventPayload::BaselineObserved { .. }
        | EventPayload::BaselineCompared { .. }
        | EventPayload::NotifySet { .. } => true,
        EventPayload::SendIntentRecorded { node, .. }
        | EventPayload::ConnectorCall { node_id: node, .. } => node == NOTIFY_SITE,
        EventPayload::SendIntentSettled { intent, .. } => notices.contains(intent.as_str()),
        EventPayload::RunResumed { explicit, .. } => !explicit,
        EventPayload::SupervisorAction { action, .. } => ![
            supervisor_action::NODE_RETRY,
            supervisor_action::RUN_CONTINUE_FROM,
            supervisor_action::CONTEXT_APPEND,
        ]
        .contains(&action.as_str()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: 0,
            payload,
        }
    }

    fn resumed(explicit: bool) -> EventPayload {
        EventPayload::RunResumed {
            from_node: "done".into(),
            by: None,
            explicit,
        }
    }

    #[test]
    fn a_kept_answer_stands_across_a_pause_and_a_plain_resume_only() {
        let dir = tempfile::tempdir().unwrap();
        keep(dir.path(), "done", 5, "the answer").unwrap();
        let mut events = vec![
            at(
                4,
                EventPayload::NodeStarted {
                    node: "done".into(),
                    attempt: 1,
                },
            ),
            at(
                5,
                EventPayload::RunAborted {
                    reason: "stop".into(),
                    by: None,
                },
            ),
            at(6, resumed(false)),
        ];
        assert_eq!(
            kept(dir.path(), "done", &events).as_deref(),
            Some("the answer")
        );
        assert_eq!(kept(dir.path(), "other", &events), None);
        // A resume that names its start node composes the answer again, as
        // does a node that started since.
        events.push(at(7, resumed(true)));
        assert_eq!(kept(dir.path(), "done", &events), None);
        events.pop();
        events.push(at(
            7,
            EventPayload::NodeStarted {
                node: "work".into(),
                attempt: 2,
            },
        ));
        assert_eq!(kept(dir.path(), "done", &events), None);
        events.pop();
        discard(dir.path());
        assert_eq!(kept(dir.path(), "done", &events), None);
    }

    fn intent(seq: u64, id: &str, node: &str) -> Event {
        at(
            seq,
            EventPayload::SendIntentRecorded {
                item: None,
                intent: id.into(),
                nonce: "n".into(),
                node: node.into(),
                attempt: None,
                attempt_seq: None,
                connector: "chat".into(),
                function: "send".into(),
                account: "me".into(),
                mode: "none".into(),
                label: "notify:paused.5".into(),
                pid: 0,
                deadline_ms: 0,
                replay_of: None,
                resend_of: None,
            },
        )
    }

    fn settled(seq: u64, id: &str) -> Event {
        at(
            seq,
            EventPayload::SendIntentSettled {
                intent: id.into(),
                outcome: "confirmed".into(),
                source: "response".into(),
                detail: None,
                effect_ref: None,
                by: None,
            },
        )
    }

    #[test]
    fn a_kept_answer_stands_across_a_notification_about_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        keep(dir.path(), "done", 5, "the answer").unwrap();
        let mut events = vec![
            at(
                5,
                EventPayload::RunPaused {
                    reason: "quota".into(),
                    by: None,
                    mode: None,
                    until: None,
                    keep_lock: false,
                },
            ),
            intent(6, "si-n", NOTIFY_SITE),
            settled(7, "si-n"),
            at(8, resumed(false)),
        ];
        assert_eq!(
            kept(dir.path(), "done", &events).as_deref(),
            Some("the answer")
        );
        // A send of the run's own is not a notification.
        events.push(intent(9, "si-own", "work"));
        assert_eq!(kept(dir.path(), "done", &events), None);
    }
}
