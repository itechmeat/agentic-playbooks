//! codex's `exec --json` stdout (`stdout.log`): one JSON event per line.
//! Each line is one record whose text is the line as written, byte for
//! byte, so a citation reads what the agent printed; a finished item that
//! is a call (a command, a file change, an MCP call, a web search) also
//! carries its facts ([`ToolUse`], decoded by
//! [`apb_core::agent_actions::codex`]).
//!
//! An output with no codex event at all (a JSON object with a string
//! `type`) is not the machine form (a custom invocation, an older CLI): it
//! reads as plain lines ([`super::plain`]), so a stray `42` or `{}` line
//! in plain output never switches the mode.

use serde_json::Value;

use super::record::{Record, ToolUse};

/// `line` as a codex event: a JSON object with a string `type`.
fn event(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line)
        .ok()
        .filter(|v| v.get("type").is_some_and(Value::is_string))
}

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

/// The records of a codex stdout, and whether every line parsed. A line
/// that is not an event (a torn last line, a JSON value without a `type`)
/// makes the view incomplete: it is not the machine form the view is read
/// as; a blank line is an empty record that loses nothing.
pub fn parse(text: &str) -> (Vec<Record>, bool) {
    let parsed: Vec<Option<Value>> = text.lines().map(event).collect();
    if parsed.iter().all(Option::is_none) {
        return (super::plain::parse(text), true);
    }
    let mut whole = true;
    let records = text
        .lines()
        .zip(parsed)
        .enumerate()
        .map(|(i, (line, value))| {
            let mut rec = Record {
                index: i as u64,
                text: line.to_string(),
                ..Record::default()
            };
            match value {
                _ if line.trim().is_empty() => {}
                Some(v) => {
                    rec.tools = apb_core::agent_actions::codex::calls_of_exec_line(&v)
                        .iter()
                        .map(ToolUse::from)
                        .collect();
                    rec.tool_errors = u32::from(failed(&v));
                }
                None => whole = false,
            }
            rec
        })
        .collect();
    (records, whole)
}
