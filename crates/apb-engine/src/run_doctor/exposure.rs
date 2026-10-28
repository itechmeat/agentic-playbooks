//! The `tool exposure` rows of `apb doctor --run`: the attempts whose agent
//! reported tools or MCP servers that differ from what apb handed it
//! (`tool_exposure_mismatch`). Names only, from the journal.

use super::check::{RunCheck, WARN};
use crate::event::{Event, EventPayload};

const SUBJECT: &str = "tool exposure";

/// One WARN row per `tool_exposure_mismatch`, in journal order.
pub(super) fn exposure_checks(events: &[Event]) -> Vec<RunCheck> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ToolExposureMismatch {
                node,
                attempt,
                agent,
                missing_tools,
                missing_servers,
                unexpected_servers,
                item,
            } => {
                let at = match item {
                    Some(item) => format!("node `{node}` item `{item}`"),
                    None => format!("node `{node}`"),
                };
                let parts: Vec<String> = [
                    ("missing tools", missing_tools),
                    ("servers not connected", missing_servers),
                    ("unexpected servers", unexpected_servers),
                ]
                .into_iter()
                .filter(|(_, names)| !names.is_empty())
                .map(|(label, names)| format!("{label}: {}", names.join(", ")))
                .collect();
                Some(RunCheck::new(
                    WARN,
                    SUBJECT,
                    format!(
                        "{at} attempt {attempt} ({agent}) did not see what apb handed it; {}",
                        parts.join("; ")
                    ),
                ))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mismatch_names_what_was_missing_and_what_was_extra() {
        let line = r#"{"seq":3,"ts":4,"type":"tool_exposure_mismatch","node":"ask","attempt":2,"agent":"claude","missing_tools":["mcp__apb__ask_user"],"missing_servers":["apb (failed)"],"unexpected_servers":["other"],"item":"a"}"#;
        let events = vec![serde_json::from_str::<Event>(line).unwrap()];
        let rows = exposure_checks(&events);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, WARN);
        assert_eq!(
            rows[0].detail,
            "node `ask` item `a` attempt 2 (claude) did not see what apb handed it; missing tools: mcp__apb__ask_user; servers not connected: apb (failed); unexpected servers: other"
        );
    }
}
