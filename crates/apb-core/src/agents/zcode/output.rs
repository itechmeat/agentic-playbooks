//! zcode's machine output, checked against zcode-agent 0.16.9 on 2026-09-25.
//!
//! `--json` prints ONE pretty-printed JSON object at the end: `{sessionId,
//! traceId, turnId, response, usage, eventCount, projection}`, with `usage`
//! `{source, inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens}`;
//! `inputTokens` include the cache reads. zcode also labels the numbers with
//! a `source` of its own (`provider` or its own count); apb does not copy
//! that label, since the count is still one the CLI printed.
//! `--output-format stream-json` prints NDJSON events ending in a
//! `{"type":"result",...}` line with the same fields. Failures exit 1 with
//! `Error: <message> (traceId: ...)` on stderr and nothing on stdout.

use serde_json::Value;

use crate::agents::output::{AgentUsage, OutputFormat, Promises, Reply, UsageSource, u64_at};

/// zcode's output format.
pub struct ZcodeOutput;

impl OutputFormat for ZcodeOutput {
    fn promises(&self) -> Promises {
        Promises::MACHINE
    }

    fn reply(&self, stdout: &str) -> Option<Reply> {
        response_text(stdout).map(Reply::ok)
    }

    fn usage(&self, stdout: &str) -> Option<AgentUsage> {
        let doc = final_doc(stdout, |v| v.get("usage").is_some_and(Value::is_object))?;
        let u = doc.get("usage")?;
        let cached = u64_at(u, "cacheReadTokens");
        Some(AgentUsage {
            input_tokens: u64_at(u, "inputTokens").saturating_sub(cached),
            output_tokens: u64_at(u, "outputTokens"),
            cache_read_tokens: cached,
            cache_write_tokens: u64_at(u, "cacheWriteTokens"),
            cost_usd: None,
            // Printed by the CLI, so reported, whatever zcode's own `source`
            // label says (see the module docs).
            source: UsageSource::Reported,
        })
    }

    // The response document.
    fn has_final_record(&self, stdout: &str) -> Option<bool> {
        Some(response_text(stdout).is_some())
    }

    // `--json` prints one PRETTY-printed object, so a per-line scan cannot
    // see it; this reads the whole document (and the stream-json `result`
    // line as well).
    fn session_id(&self, stdout: &str) -> Option<String> {
        session_id(stdout)
    }
}

/// zcode's final result object that `has` what the reader wants: the whole
/// `--json` document, or else the last `{"type":"result",...}` line of
/// `--output-format stream-json` that has it. `None` for plain text. Every
/// reader below goes through this one function, so the two shapes are read
/// alike, and a later result line without the field never hides an earlier
/// one that has it.
fn final_doc(stdout: &str, has: impl Fn(&Value) -> bool) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.starts_with('{')
        && let Ok(v) = serde_json::from_str::<Value>(trimmed)
        && has(&v)
    {
        return Some(v);
    }
    trimmed
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("type").and_then(Value::as_str) == Some("result") && has(v))
}

/// The string field `key` of the final result object that carries it,
/// non-empty when `non_empty`.
fn final_field(stdout: &str, key: &str, non_empty: bool) -> Option<String> {
    let field = |v: &Value| {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|s| !(non_empty && s.is_empty()))
            .map(str::to_string)
    };
    final_doc(stdout, |v| field(v).is_some()).and_then(|v| field(&v))
}

/// Extracts the final reply text from zcode's stdout (see [`final_doc`]).
/// `None` when neither shape is present (plain text output), so the caller
/// keeps the raw stdout.
pub fn response_text(stdout: &str) -> Option<String> {
    final_field(stdout, "response", false)
}

/// The session id (`sess_...`) from zcode's stdout, in either output shape.
pub fn session_id(stdout: &str) -> Option<String> {
    final_field(stdout, "sessionId", true)
}

/// The trace id (`traceId`) of the turn in zcode's stdout, `None` when the
/// output carries none.
pub fn trace_id(stdout: &str) -> Option<String> {
    final_field(stdout, "traceId", true)
}
