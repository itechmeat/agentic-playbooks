//! codex's `exec --json` stdout: a finished item (`item.completed`) that is
//! a call (a command, a file change, an MCP call, a web search) carries its
//! facts, decoded by [`apb_core::agent_actions::codex`]; one that reports a
//! `failed` status or a non-zero exit code is a failed call.

use serde_json::Value;

use super::events::{self, Decoder};
use super::record::Record;

/// Whether a finished item reports a failure: a `failed` status or a
/// non-zero exit code.
fn failed(line: &Value) -> bool {
    apb_core::agent_actions::codex::completed_item(line).is_some_and(|item| {
        item.get("status").and_then(Value::as_str) == Some("failed")
            || item
                .get("exit_code")
                .and_then(Value::as_i64)
                .is_some_and(|c| c != 0)
    })
}

const DECODER: Decoder = Decoder {
    calls: apb_core::agent_actions::codex::calls_of_exec_line,
    failed,
};

/// The records of a codex stdout, and whether every line parsed.
pub fn parse(text: &str) -> (Vec<Record>, bool) {
    events::parse(text, &DECODER)
}
