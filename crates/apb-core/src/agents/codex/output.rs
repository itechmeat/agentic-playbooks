//! codex's machine output, checked against codex 0.157.0 on 2026-09-27.
//!
//! `exec --json` prints JSON lines: `thread.started` carries the session's
//! `thread_id`, `item.completed` with an `agent_message` item carries the
//! reply, `turn.completed` carries `usage` `{input_tokens,
//! cached_input_tokens, cache_write_input_tokens, output_tokens,
//! reasoning_output_tokens}`. `input_tokens` include the cached ones and
//! `output_tokens` include the reasoning ones. No cost. `codex exec` also
//! opens stderr with a header block that holds a `session id: <uuid>` line.

use serde_json::Value;

use crate::agents::output::{
    AgentUsage, OutputFormat, Promises, Reply, UsageSource, json_lines, json_string_field,
    last_line_is, str_at, u64_at,
};

/// codex's output format.
pub struct CodexOutput;

impl OutputFormat for CodexOutput {
    fn promises(&self) -> Promises {
        Promises::MACHINE
    }

    // The last agent message. A completed turn that sent none (it only ran
    // tools) is an empty reply, as the plain form printed nothing for it,
    // not a stream to hand on as the reply.
    fn reply(&self, stdout: &str) -> Option<Reply> {
        let mut completed = false;
        let mut text = None;
        for v in json_lines(stdout) {
            match str_at(&v, "type") {
                Some("turn.completed") => completed = true,
                _ => {
                    if let Some(t) = agent_message(&v) {
                        text = Some(t.to_string());
                    }
                }
            }
        }
        text.or_else(|| completed.then(String::new)).map(Reply::ok)
    }

    fn usage(&self, stdout: &str) -> Option<AgentUsage> {
        let turns: Vec<Value> = json_lines(stdout)
            .filter(|v| str_at(v, "type") == Some("turn.completed"))
            .filter_map(|v| v.get("usage").filter(|u| u.is_object()).cloned())
            .collect();
        if turns.is_empty() {
            return None;
        }
        let sum = |key: &str| {
            turns
                .iter()
                .fold(0u64, |acc, u| acc.saturating_add(u64_at(u, key)))
        };
        let cached = sum("cached_input_tokens");
        let written = sum("cache_write_input_tokens");
        Some(AgentUsage {
            input_tokens: sum("input_tokens").saturating_sub(cached),
            output_tokens: sum("output_tokens"),
            cache_read_tokens: cached,
            cache_write_tokens: written,
            cost_usd: None,
            source: UsageSource::Reported,
        })
    }

    // `turn.completed`, or for a failed turn `turn.failed` or `error`.
    fn has_final_record(&self, stdout: &str) -> Option<bool> {
        Some(last_line_is(
            stdout,
            &["turn.completed", "turn.failed", "error"],
        ))
    }

    fn session_id(&self, stdout: &str) -> Option<String> {
        json_string_field(stdout, &["thread_id", "session_id", "conversation_id"])
    }

    fn stderr_session(&self, stderr: &str) -> Option<String> {
        stderr.lines().find_map(|line| {
            let id = line.trim().strip_prefix("session id:")?.trim();
            (!id.is_empty() && !id.contains(char::is_whitespace)).then(|| id.to_string())
        })
    }
}

/// The finished item of an `exec --json` stdout line (an `item.completed`).
pub fn completed_item(line: &Value) -> Option<&Value> {
    (str_at(line, "type") == Some("item.completed"))
        .then(|| line.get("item"))
        .flatten()
}

/// The reply text of an `exec --json` stdout line: an `agent_message`
/// item's `text`.
pub fn agent_message(line: &Value) -> Option<&str> {
    completed_item(line)
        .filter(|item| str_at(item, "type") == Some("agent_message"))
        .and_then(|item| str_at(item, "text"))
}
