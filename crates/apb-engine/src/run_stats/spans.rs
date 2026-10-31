//! The time spans `apb stats` and the run estimate measure: active time
//! (paused time left out) and the human waits of a run. A leaf of the
//! statistics, shared with `run_estimate::sample`.

use std::collections::BTreeMap;

use crate::event::{Event, EventPayload};

fn ms(from: u128, to: u128) -> u64 {
    u64::try_from(to.saturating_sub(from)).unwrap_or(u64::MAX)
}

/// Like [`ms`], without the time the run spent paused in between (issue
/// #200): run and node durations measure the run's active time.
/// `paused` comes from [`crate::pause::paused_intervals`] (or
/// [`crate::pause::inactive_intervals`]), computed once per run rather than
/// once per measured span.
pub(crate) fn active_ms(paused: &[(u128, u128)], from: u128, to: u128) -> u64 {
    ms(from, to).saturating_sub(
        u64::try_from(crate::pause::paused_ms_in(paused, from, to)).unwrap_or(u64::MAX),
    )
}

/// Pairs each `open` of a node with the next `close` of the same node, in
/// journal order, and returns the waits in milliseconds. A `withdraw` closes
/// the oldest open request of the node without a wait: the request was never
/// answered, and the next visit asks anew.
fn waits(
    events: &[Event],
    open: fn(&EventPayload) -> Option<&str>,
    close: fn(&EventPayload) -> Option<&str>,
    withdraw: fn(&EventPayload) -> Option<&str>,
) -> Vec<u64> {
    let mut pending: BTreeMap<&str, Vec<u128>> = BTreeMap::new();
    let mut out = Vec::new();
    for e in events {
        if let Some(n) = open(&e.payload) {
            pending.entry(n).or_default().push(e.ts);
        } else if let Some(n) = close(&e.payload)
            && let Some(q) = pending.get_mut(n)
            && !q.is_empty()
        {
            let t = q.remove(0);
            out.push(ms(t, e.ts));
        } else if let Some(n) = withdraw(&e.payload)
            && let Some(q) = pending.get_mut(n)
            && !q.is_empty()
        {
            q.remove(0);
        }
    }
    out
}

/// The human waits of a run, in milliseconds: gate waits
/// (`review_requested` to the matching `review_decided`) and question waits
/// (`question_asked` to `question_answered`).
pub(crate) fn human_waits(ev: &[Event]) -> (Vec<u64>, Vec<u64>) {
    let gate = waits(
        ev,
        |p| match p {
            EventPayload::ReviewRequested { node, .. } => Some(node),
            _ => None,
        },
        |p| match p {
            EventPayload::ReviewDecided { node, .. } => Some(node),
            _ => None,
        },
        |p| match p {
            EventPayload::ReviewWithdrawn { node, .. } => Some(node),
            _ => None,
        },
    );
    let question = waits(
        ev,
        |p| match p {
            EventPayload::QuestionAsked { node, .. } => Some(node),
            _ => None,
        },
        |p| match p {
            EventPayload::QuestionAnswered { node, .. } => Some(node),
            _ => None,
        },
        |_| None,
    );
    (gate, question)
}
