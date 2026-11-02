//! A journal read for a person diagnosing it (`apb doctor --run`): every
//! line is classified, none fails the read.
//!
//! The engine's readers stop at the first line they cannot use, which is
//! right for anything that acts on a run and useless for the one tool that
//! must say why a run cannot be continued. This reader keeps the events it
//! decodes and names the rest: a record of a type this binary does not know
//! (with its seq), a line of a known type whose fields do not decode or that
//! is no event at all (with its line number), and a torn last line (a write
//! that did not finish). It also answers whether the open deferred retries
//! are protected by their guard (see `deferral_guard`).
//!
//! It only reads the bytes it is given; it takes no lock and opens nothing.

use std::collections::BTreeSet;

use super::{Event, EventHeader, UnknownEvent, is_event_tag, is_known_kind};

/// One line the survey could not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadLine {
    /// 1-based line number in `events.jsonl`.
    pub line: usize,
    /// The line's `seq` and `type`, when its envelope still parses.
    pub seq: Option<u64>,
    pub kind: Option<String>,
    /// Why it did not decode.
    pub error: String,
}

/// The whole journal as [`survey`] read it.
#[derive(Debug, Default)]
pub struct JournalSurvey {
    /// The events this binary decodes, in journal order.
    pub events: Vec<Event>,
    /// Records of a type this binary does not know.
    pub unknown: Vec<UnknownEvent>,
    /// Whole lines that do not decode (a known type with fields this binary
    /// cannot read, or no event at all).
    pub undecodable: Vec<BadLine>,
    /// The last line, unterminated and unparsable: a write that did not
    /// finish. Every apb's engine refuses such a journal.
    pub torn_tail: Option<BadLine>,
}

/// The open deferred retries of a surveyed journal and their guard.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeferralGuardState {
    /// The nodes whose retry is deferred and not yet started.
    pub open: BTreeSet<String>,
    /// The last checkpoint, when it was written with a deferral open and no
    /// `deferred_retry_guard` right after it: an apb that does not know
    /// deferrals would continue the run from there and re-run those nodes.
    pub unguarded_checkpoint: Option<u64>,
}

/// Classifies every line of `bytes` (the contents of `events.jsonl`).
pub fn survey(bytes: &[u8]) -> JournalSurvey {
    let mut out = JournalSurvey::default();
    let terminated = bytes.ends_with(b"\n");
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if body.is_empty() {
        return out;
    }
    let lines: Vec<&[u8]> = body.split(|b| *b == b'\n').collect();
    let last = lines.len();
    for (i, raw) in lines.into_iter().enumerate() {
        let number = i + 1;
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let text = match std::str::from_utf8(raw) {
            Ok(t) => t,
            Err(e) => {
                out.push_bad(number, number == last && !terminated, None, e.to_string());
                continue;
            }
        };
        if text.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(text) {
            Ok(ev) => out.events.push(ev),
            Err(err) => {
                let header = serde_json::from_str::<EventHeader>(text).ok();
                match header {
                    Some(h) if is_event_tag(&h.kind) && !is_known_kind(&h.kind) => {
                        out.unknown.push(UnknownEvent {
                            seq: h.seq,
                            ts: h.ts,
                            kind: h.kind,
                        });
                    }
                    _ => out.push_bad(
                        number,
                        number == last && !terminated,
                        header,
                        err.to_string(),
                    ),
                }
            }
        }
    }
    out
}

impl JournalSurvey {
    fn push_bad(&mut self, line: usize, torn: bool, header: Option<EventHeader>, error: String) {
        let (seq, kind) = header.map_or((None, None), |h| (Some(h.seq), Some(h.kind)));
        let bad = BadLine {
            line,
            seq,
            kind,
            error,
        };
        if torn {
            self.torn_tail = Some(bad);
        } else {
            self.undecodable.push(bad);
        }
    }

    /// The open deferrals and whether the last checkpoint carries their
    /// guard, from the decoded events in journal order.
    pub fn deferrals(&self) -> DeferralGuardState {
        guard_state(&self.events)
    }
}

/// The open deferrals of `events` (in journal order) and whether the last
/// checkpoint is followed by a guard (see [`super::deferral_guard::unguarded`]).
fn guard_state(events: &[Event]) -> DeferralGuardState {
    let (open, unguarded_checkpoint) = super::deferral_guard::unguarded(super::guard_steps(events));
    DeferralGuardState {
        open,
        unguarded_checkpoint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        format!("{v}\n")
    }

    fn started(seq: u64) -> String {
        line(serde_json::json!({
            "seq": seq, "ts": 1, "type": "run_started", "playbook": "p", "version": "1.0.0"
        }))
    }

    fn finished(seq: u64, node: &str) -> String {
        line(serde_json::json!({
            "seq": seq, "ts": 1, "type": "node_finished", "node": node,
            "status": "success", "attempt": 1, "output": ""
        }))
    }

    fn deferred(seq: u64, node: &str) -> String {
        line(serde_json::json!({
            "seq": seq, "ts": 1, "type": "retry_deferred", "node": node,
            "after_attempt": 1, "retry": 1, "step": 0, "binding": "b", "delay_ms": 1000
        }))
    }

    fn guard(seq: u64, node: &str) -> String {
        line(serde_json::json!({
            "seq": seq, "ts": 1, "type": "deferred_retry_guard", "open": [node]
        }))
    }

    #[test]
    fn every_line_is_classified_and_none_fails_the_read() {
        let mut text = started(0);
        text.push_str(&line(
            serde_json::json!({"seq": 1, "ts": 1, "type": "corpus_future_record"}),
        ));
        // A known type whose fields do not decode.
        text.push_str(&line(
            serde_json::json!({"seq": 2, "ts": 1, "type": "node_finished", "node": 7}),
        ));
        text.push_str("not json\n");
        text.push_str(&finished(4, "a"));
        text.push_str("{\"seq\": 5, \"ts\": 1, \"type\": \"node_fin");
        let s = survey(text.as_bytes());
        assert_eq!(s.events.len(), 2);
        assert_eq!(s.unknown.len(), 1);
        assert_eq!(s.unknown[0].seq, 1);
        assert_eq!(s.unknown[0].kind, "corpus_future_record");
        let bad: Vec<(usize, Option<u64>, Option<&str>)> = s
            .undecodable
            .iter()
            .map(|b| (b.line, b.seq, b.kind.as_deref()))
            .collect();
        assert_eq!(
            bad,
            vec![(3, Some(2), Some("node_finished")), (4, None, None)]
        );
        assert_eq!(s.torn_tail.as_ref().map(|t| t.line), Some(6));
    }

    #[test]
    fn a_bad_last_line_with_its_newline_is_not_torn() {
        let mut text = started(0);
        text.push_str("{\"seq\": 1\n");
        let s = survey(text.as_bytes());
        assert!(s.torn_tail.is_none());
        assert_eq!(s.undecodable.len(), 1);
        assert_eq!(s.undecodable[0].line, 2);
    }

    #[test]
    fn an_open_deferral_is_guarded_only_with_its_guard_after_the_last_checkpoint() {
        let base = format!("{}{}", started(0), deferred(1, "a"));
        // No checkpoint since the deferral: its own record protects it.
        let s = survey(base.as_bytes()).deferrals();
        assert_eq!(s.open.iter().collect::<Vec<_>>(), vec!["a"]);
        assert_eq!(s.unguarded_checkpoint, None);

        let guarded = format!("{base}{}{}", finished(2, "b"), guard(3, "a"));
        let s = survey(guarded.as_bytes()).deferrals();
        assert_eq!(s.open.len(), 1);
        assert_eq!(s.unguarded_checkpoint, None);

        let bare = format!("{base}{}", finished(2, "b"));
        let s = survey(bare.as_bytes()).deferrals();
        assert_eq!(s.unguarded_checkpoint, Some(2));

        // A guard a later open appended (see `EventLog::open`) protects it
        // as well: it is newer than the last checkpoint.
        let repaired = format!(
            "{bare}{}{}",
            line(serde_json::json!({"seq": 3, "ts": 1, "type": "run_resumed", "from_node": "b"})),
            guard(4, "a")
        );
        let s = survey(repaired.as_bytes()).deferrals();
        assert_eq!(s.unguarded_checkpoint, None);

        // The node's own finish closes it: nothing open, nothing to guard.
        let closed = format!("{base}{}", finished(2, "a"));
        let s = survey(closed.as_bytes()).deferrals();
        assert!(s.open.is_empty());
        assert_eq!(s.unguarded_checkpoint, None);
    }
}
