//! A JSON-lines stdout (`stdout.log`) of an agent whose machine output is
//! one event per line (codex's `exec --json`, dsh's `--json`). Each line is
//! one record whose text is the line as written, byte for byte, so a
//! citation reads what the agent printed; a line that is a call also
//! carries its facts ([`ToolUse`]), decoded by the agent's own decoder in
//! [`apb_core::agent_actions`].
//!
//! An output with no event at all (a JSON object with a string `type`) is
//! not the machine form (a custom invocation, an older CLI): it reads as
//! plain lines ([`super::plain`]), so a stray `42` or `{}` line in plain
//! output never switches the mode.

use serde_json::Value;

use super::record::{Record, ToolUse};

/// One agent's decoder of its event lines.
pub struct Decoder {
    /// The calls one event reports.
    pub calls: fn(&Value) -> Vec<apb_core::agent_actions::ToolCall>,
    /// Whether one event reports a failed call.
    pub failed: fn(&Value) -> bool,
}

/// `line` as an event: a JSON object with a string `type`.
fn event(line: &str) -> Option<Value> {
    serde_json::from_str::<Value>(line)
        .ok()
        .filter(|v| v.get("type").is_some_and(Value::is_string))
}

/// The records of an event stdout, and whether every line parsed. A line
/// that is not an event (a torn last line, a JSON value without a `type`)
/// makes the view incomplete: it is not the machine form the view is read
/// as; a blank line is an empty record that loses nothing.
pub fn parse(text: &str, decoder: &Decoder) -> (Vec<Record>, bool) {
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
                    rec.tools = (decoder.calls)(&v).iter().map(ToolUse::from).collect();
                    rec.tool_errors = u32::from((decoder.failed)(&v));
                }
                None => whole = false,
            }
            rec
        })
        .collect();
    (records, whole)
}
