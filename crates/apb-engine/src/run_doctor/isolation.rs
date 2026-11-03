//! The `isolation` rows of `apb doctor --run`: per attempt of an isolated
//! node, its level, backend, network and limits (from
//! `attempt_started.isolation`), and a FAIL row for a node the isolation
//! check refused before anything was spawned. A `read_only` node adds a
//! row per attempt with its tier (`read_only_checked`: enforced,
//! detected, or detected but partial), and a FAIL row for each
//! `read_only_violated`.

use super::check::{FAIL, OK, RunCheck, WARN};
use crate::event::{Event, EventPayload, ReadOnlyCheck, ReadOnlyLimit, ReadOnlyTier};
use crate::run_view::AttemptIsolationRow;

const SUBJECT: &str = "isolation";

/// The rows, in journal order.
pub(super) fn isolation_checks(events: &[Event]) -> Vec<RunCheck> {
    let mut out = Vec::new();
    for e in events {
        if let Some(row) = AttemptIsolationRow::of_event(e) {
            let status = if row.sandboxed() { OK } else { WARN };
            out.push(RunCheck::new(status, SUBJECT, row.line()));
        }
        out.extend(read_only_check(e));
        out.extend(refusal(e));
    }
    out
}

/// The FAIL row of a node the isolation check refused.
fn refusal(e: &Event) -> Option<RunCheck> {
    match &e.payload {
        EventPayload::NodeFinished {
            node,
            status,
            output,
            ..
        } if status == "failed"
            && output.starts_with(apb_core::isolation::contract::UNAVAILABLE) =>
        {
            Some(RunCheck::new(
                FAIL,
                SUBJECT,
                format!("node `{node}`: {output}"),
            ))
        }
        _ => None,
    }
}

/// The `read_only` row of one event: an attempt's tier, or a violation.
fn read_only_check(e: &Event) -> Option<RunCheck> {
    match &e.payload {
        EventPayload::ReadOnlyChecked {
            node,
            attempt,
            check,
        } => {
            let status = if check.tier == ReadOnlyTier::DetectedPartial {
                WARN
            } else {
                OK
            };
            Some(RunCheck::new(
                status,
                SUBJECT,
                format!("node `{node}` attempt {attempt}: {}", tier_line(check)),
            ))
        }
        EventPayload::ReadOnlyViolated {
            node,
            attempt,
            changes,
            truncated,
            evidence,
            ..
        } => {
            let listed: Vec<String> = changes
                .iter()
                .take(3)
                .map(|c| format!("{} {}", c.path, c.change))
                .collect();
            let more = (changes.len() as u64 + truncated).saturating_sub(listed.len() as u64);
            let mut detail = format!(
                "node `{node}` attempt {attempt}: read_only, the tree changed: {}",
                listed.join(", ")
            );
            if more > 0 {
                detail.push_str(&format!(" (and {more} more)"));
            }
            if let Some(path) = evidence {
                detail.push_str(&format!("; evidence {path}"));
            }
            Some(RunCheck::new(FAIL, SUBJECT, detail))
        }
        _ => None,
    }
}

/// One attempt's read-only tier as a line.
fn tier_line(check: &ReadOnlyCheck) -> String {
    match check.tier {
        ReadOnlyTier::Enforced => "read_only enforced by the sandbox".to_string(),
        ReadOnlyTier::Detected => format!(
            "read_only detected after the attempt ({} entries, {} bytes hashed)",
            check.entries, check.bytes
        ),
        ReadOnlyTier::DetectedPartial => format!(
            "read_only detected after the attempt, partial: the {} limit stopped the check at {} entries and {} bytes, so a change past it goes unseen",
            match check.partial {
                Some(ReadOnlyLimit::Bytes) => "byte",
                _ => "entry",
            },
            check.entries,
            check.bytes
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_isolated_attempt_and_each_refusal_gets_a_row() {
        let lines = [
            r#"{"seq":1,"ts":2,"type":"attempt_started","node":"w","attempt":1,"agent":"claude","isolation":{"level":"full","backend":"bwrap","network":"none","holds":[],"limits":["user.host_uid"],"view":[]}}"#,
            r#"{"seq":2,"ts":3,"type":"attempt_started","node":"b","attempt":1,"agent":"claude","isolation":{"level":"best_effort","backend":"none","network":"open","limits":["no_sandbox"],"fallbacks":["disabled (APB_SANDBOX=off)"]}}"#,
            r#"{"seq":3,"ts":4,"type":"attempt_started","node":"plain","attempt":1,"agent":"claude"}"#,
            r#"{"seq":4,"ts":5,"type":"node_finished","node":"r","status":"failed","attempt":2,"output":"isolation_unavailable: disabled (APB_SANDBOX=off)"}"#,
            r#"{"seq":5,"ts":6,"type":"node_finished","node":"echo","status":"succeeded","attempt":1,"output":"isolation_unavailable is a word this reply starts with"}"#,
        ];
        let events: Vec<Event> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let rows = isolation_checks(&events);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].status, OK);
        assert_eq!(
            rows[0].detail,
            "node `w` attempt 1: full on bwrap, network none; limits: user.host_uid"
        );
        assert_eq!(rows[1].status, WARN);
        assert!(
            rows[1]
                .detail
                .ends_with("no sandbox: disabled (APB_SANDBOX=off)")
        );
        assert_eq!(rows[2].status, FAIL);
    }
}
