//! opencode's machine output, checked against opencode 1.18.32 on
//! 2026-09-27.
//!
//! `run --format json` prints JSON lines; every event carries the session's
//! `sessionID`, `text` parts carry the reply, every `step_finish` part
//! carries that step's `tokens` `{input, output, reasoning, cache: {read,
//! write}}` and `cost`. `input` excludes the cache, `output` excludes
//! reasoning.

use serde_json::Value;

use crate::agents::output::{
    AgentUsage, OutputFormat, Promises, Reply, UsageSource, cost, json_lines, json_string_field,
    last_line_is, str_at, u64_at,
};

/// opencode's output format.
pub struct OpencodeOutput;

impl OutputFormat for OpencodeOutput {
    fn promises(&self) -> Promises {
        Promises::MACHINE
    }

    // Every text part. A run that finished steps without one (tool calls
    // only) is an empty reply, as for codex.
    fn reply(&self, stdout: &str) -> Option<Reply> {
        let mut stepped = false;
        let mut parts: Vec<String> = Vec::new();
        for v in json_lines(stdout) {
            match str_at(&v, "type") {
                Some("step_finish") => stepped = true,
                Some("text") => {
                    if let Some(t) = v.get("part").and_then(|p| str_at(p, "text")) {
                        parts.push(t.to_string());
                    }
                }
                _ => {}
            }
        }
        (stepped || !parts.is_empty()).then(|| Reply::ok(parts.join("\n")))
    }

    fn usage(&self, stdout: &str) -> Option<AgentUsage> {
        let steps: Vec<Value> = json_lines(stdout)
            .filter(|v| str_at(v, "type") == Some("step_finish"))
            .filter_map(|v| v.get("part").cloned())
            .filter(|p| p.get("tokens").is_some_and(Value::is_object))
            .collect();
        if steps.is_empty() {
            return None;
        }
        let tokens = |p: &Value, key: &str| p.get("tokens").map_or(0, |t| u64_at(t, key));
        let cache = |p: &Value, key: &str| {
            p.get("tokens")
                .and_then(|t| t.get("cache"))
                .map_or(0, |c| u64_at(c, key))
        };
        let costs: Vec<f64> = steps
            .iter()
            .filter_map(|p| p.get("cost").and_then(Value::as_f64))
            .collect();
        let sum =
            |f: &dyn Fn(&Value) -> u64| steps.iter().fold(0u64, |acc, p| acc.saturating_add(f(p)));
        Some(AgentUsage {
            input_tokens: sum(&|p| tokens(p, "input")),
            output_tokens: sum(&|p| tokens(p, "output").saturating_add(tokens(p, "reasoning"))),
            cache_read_tokens: sum(&|p| cache(p, "read")),
            cache_write_tokens: sum(&|p| cache(p, "write")),
            cost_usd: cost((!costs.is_empty()).then(|| costs.iter().sum())),
            source: UsageSource::Reported,
        })
    }

    // A `step_finish` part (its last line; earlier steps print one too) or
    // the `error` that ends a failed run.
    fn has_final_record(&self, stdout: &str) -> Option<bool> {
        Some(last_line_is(stdout, &["step_finish", "error"]))
    }

    fn session_id(&self, stdout: &str) -> Option<String> {
        json_string_field(stdout, &["session_id", "sessionID"])
    }
}
