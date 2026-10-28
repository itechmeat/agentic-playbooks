//! claude's session transcript (`session.jsonl`, the CLI's own record of a
//! session): one JSON message per line, with every tool call and result.
//!
//! What a grader needs is kept as facts ([`ToolUse`]): the tool, a digest
//! of its input (to tell repeated identical calls), the files it names,
//! whether it writes them, whether it loads a skill (the `Skill` tool or a
//! `Read` of a `SKILL.md`), and whether it runs a destructive command
//! ([`is_destructive`]). A sub-agent's messages carry `isSidechain`.

use serde_json::Value;

use super::record::{Record, ToolUse};

pub use apb_core::agent_actions::command::{DESTRUCTIVE, is_destructive};

fn tool_use(item: &Value) -> ToolUse {
    ToolUse::from(&apb_core::agent_actions::claude::tool_call(item))
}

/// The text of a tool result's `content` (a string or a list of blocks).
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One session line as a record.
fn record(index: u64, line: &Value) -> Record {
    let mut rec = Record {
        index,
        sidechain: line
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Record::default()
    };
    let role = line.get("type").and_then(Value::as_str).unwrap_or("record");
    let mut parts = vec![format!("[{role}]")];
    let content = line.get("message").and_then(|m| m.get("content"));
    match content {
        Some(Value::String(s)) => parts.push(s.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        parts.extend(item.get("text").and_then(Value::as_str).map(str::to_string));
                    }
                    Some("tool_use") => {
                        let t = tool_use(item);
                        parts.push(format!("tool {} {}", t.name, t.paths.join(" ")));
                        rec.tools.push(t);
                    }
                    Some("tool_result") => {
                        let error = item
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        rec.tool_errors += u32::from(error);
                        let tag = if error { "error" } else { "result" };
                        parts.push(format!("{tag}: {}", result_text(item.get("content"))));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    rec.text = parts.join("\n").trim_end().to_string();
    rec
}

/// The records of a session transcript, and whether every line parsed. A
/// line that is not JSON (a torn last line) is kept as plain text and makes
/// the view incomplete; a blank line is an empty record that loses nothing.
pub fn parse(text: &str) -> (Vec<Record>, bool) {
    let mut whole = true;
    let records = text
        .lines()
        .enumerate()
        .map(|(i, line)| match serde_json::from_str::<Value>(line) {
            _ if line.trim().is_empty() => Record {
                index: i as u64,
                ..Record::default()
            },
            Ok(v) => record(i as u64, &v),
            Err(_) => {
                whole = false;
                Record {
                    index: i as u64,
                    text: line.to_string(),
                    ..Record::default()
                }
            }
        })
        .collect();
    (records, whole)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_read_of_a_skill_file_loads_the_skill() {
        let call = |name: &str| {
            tool_use(&serde_json::json!({
                "name": name,
                "input": { "file_path": "/s/rust-skills/SKILL.md" }
            }))
            .skill
        };
        assert_eq!(call("Read").as_deref(), Some("rust-skills"));
        assert_eq!(call("Write"), None);
        assert_eq!(call("Edit"), None);
    }

    #[test]
    fn a_blank_line_keeps_the_session_whole() {
        let (records, whole) = parse("{\"type\":\"user\"}\n\n   \n{\"type\":\"assistant\"}\n");
        assert!(whole);
        assert_eq!(records.len(), 4);
        let (_, torn) = parse("{\"type\":\"user\"}\n{\"type\":");
        assert!(!torn);
    }

    #[test]
    fn a_long_line_or_deep_nesting_is_one_record_that_does_not_read() {
        // serde_json stops at its recursion limit: the line is kept as text
        // and the view is incomplete, never a panic.
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        let long = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}",
            "x".repeat(1_000_000)
        );
        let (records, whole) = parse(&format!("{deep}\n{long}\n"));
        assert!(!whole);
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[1].text.chars().count(),
            "[user]\n".len() + 1_000_000
        );
    }
}
