//! The stall streak (spec node-contracts D5): the one fold over the
//! journal that both the stall exit and the `stalled` notice read. A leaf:
//! it reads events only, so a reporting surface can use it without the
//! scheduler.

use crate::connector::intent::ledger::effect_scope_floor;
use crate::event::{Event, EventPayload};
use crate::exec_key::ExecKey;

/// Trailing earlier attempts of `key` in its effect scope that carry
/// `fingerprint`, from the journal, newest first. An attempt whose
/// `failure_kind` is `transient`, or whose status is `interrupted`, never
/// finished its work and is skipped without breaking or joining the streak.
pub(crate) fn streak(events: &[Event], key: ExecKey<'_>, fingerprint: &str) -> u32 {
    let floor = effect_scope_floor(events, key);
    let mut count = 0u32;
    for e in events.iter().rev() {
        if e.seq < floor {
            break;
        }
        let EventPayload::AttemptFinished {
            node,
            item,
            status,
            failure_kind,
            rejection_fingerprint,
            ..
        } = &e.payload
        else {
            continue;
        };
        if *node != key.node || item.as_deref() != key.item {
            continue;
        }
        if failure_kind.as_deref() == Some("transient") || status == "interrupted" {
            continue;
        }
        match rejection_fingerprint.as_deref() {
            Some(f) if f == fingerprint => count += 1,
            _ => break,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_with(events: Vec<EventPayload>) -> Vec<Event> {
        events
            .into_iter()
            .enumerate()
            .map(|(i, payload)| Event {
                seq: i as u64 + 1,
                ts: 0,
                payload,
            })
            .collect()
    }

    /// One payload built from its journal JSON, so the helpers name only
    /// what they mean.
    fn payload(json: &str) -> EventPayload {
        serde_json::from_str(json).unwrap()
    }

    fn finished(fp: Option<&str>, failure_kind: Option<&str>, status: &str) -> EventPayload {
        let tail = if fp.is_some() || failure_kind.is_some() {
            let f = fp
                .map(|x| format!("\"{x}\""))
                .unwrap_or_else(|| "null".into());
            let k = failure_kind
                .map(|x| format!("\"{x}\""))
                .unwrap_or_else(|| "null".into());
            format!(",\"rejection_fingerprint\":{f},\"failure_kind\":{k}}}")
        } else {
            "}".to_string()
        };
        payload(&format!(
            "{{\"type\":\"attempt_finished\",\"node\":\"w\",\"attempt\":1,\"status\":\"{status}\"{tail}"
        ))
    }

    fn finished_item(item: &str, fp: Option<&str>) -> EventPayload {
        let f = fp
            .map(|x| format!("\"{x}\""))
            .unwrap_or_else(|| "null".into());
        payload(&format!(
            "{{\"type\":\"attempt_finished\",\"node\":\"w\",\"attempt\":1,\"status\":\"failed\",\"item\":\"{item}\",\"rejection_fingerprint\":{f}}}"
        ))
    }

    fn started() -> EventPayload {
        payload(r#"{"type":"attempt_started","node":"w","attempt":1,"agent":"stub"}"#)
    }

    fn succeeded() -> EventPayload {
        payload(
            r#"{"type":"node_finished","node":"w","status":"succeeded","attempt":1,"output":""}"#,
        )
    }

    const KEY: ExecKey<'_> = ExecKey {
        node: "w",
        item: None,
    };

    #[test]
    fn trailing_equal_fingerprints_count() {
        let events = journal_with(vec![
            started(),
            finished(Some("a"), None, "failed"),
            finished(Some("a"), None, "failed"),
            finished(Some("a"), None, "failed"),
        ]);
        assert_eq!(streak(&events, KEY, "a"), 3);
        assert_eq!(streak(&events, KEY, "b"), 0);
    }

    #[test]
    fn a_transient_or_interrupted_attempt_is_skipped() {
        let events = journal_with(vec![
            started(),
            finished(Some("a"), None, "failed"),
            // A transient attempt with another fingerprint does not break
            // the streak and does not join it.
            finished(Some("b"), Some("transient"), "failed"),
            finished(Some("a"), None, "failed"),
            // An interrupted attempt (never finished its work).
            finished(Some("c"), None, "interrupted"),
            finished(Some("a"), None, "failed"),
        ]);
        assert_eq!(streak(&events, KEY, "a"), 3);
    }

    #[test]
    fn another_fingerprint_stops_the_count() {
        let events = journal_with(vec![
            started(),
            finished(Some("a"), None, "failed"),
            finished(Some("b"), None, "failed"),
            finished(Some("a"), None, "failed"),
        ]);
        assert_eq!(streak(&events, KEY, "a"), 1);
    }

    #[test]
    fn a_success_stops_the_count() {
        let events = journal_with(vec![
            started(),
            finished(Some("a"), None, "failed"),
            succeeded(),
            finished(Some("a"), None, "failed"),
        ]);
        assert_eq!(streak(&events, KEY, "a"), 1);
    }

    #[test]
    fn an_item_key_counts_only_its_item() {
        let events = journal_with(vec![
            started(),
            finished_item("one", Some("a")),
            finished_item("two", Some("a")),
            finished_item("one", Some("a")),
        ]);
        let key = ExecKey {
            node: "w",
            item: Some("one"),
        };
        assert_eq!(streak(&events, key, "a"), 2);
    }
}
