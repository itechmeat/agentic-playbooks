//! The `isolation` rows of `apb doctor --run`: per attempt of an isolated
//! node, its level, backend, network and limits (from
//! `attempt_started.isolation`), and a FAIL row for a node the isolation
//! check refused before anything was spawned.

use super::check::{FAIL, OK, RunCheck, WARN};
use crate::event::{Event, EventPayload};
use crate::run_view::AttemptIsolationRow;

const SUBJECT: &str = "isolation";

/// The rows, in journal order.
pub(super) fn isolation_checks(events: &[Event]) -> Vec<RunCheck> {
    events
        .iter()
        .filter_map(|e| {
            if let Some(row) = AttemptIsolationRow::of_event(e) {
                let status = if row.sandboxed() { OK } else { WARN };
                return Some(RunCheck::new(status, SUBJECT, row.line()));
            }
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
        })
        .collect()
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
