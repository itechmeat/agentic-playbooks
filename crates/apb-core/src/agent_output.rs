//! What an agent CLI's machine-readable output says about one attempt: the
//! reply text and the token usage it reports.
//!
//! The built-in invocation forms ask each CLI for its machine output
//! (claude `--output-format json`, codex `exec --json`, opencode `run --format
//! json`, zcode `--json`). Every reader here is lenient in the same way: an
//! output that does not have the expected shape (plain text from a custom
//! invocation form, a stub agent, an older CLI) yields `None`, and the caller
//! keeps the raw stdout as the reply and records no usage. Usage is only ever
//! what the CLI printed; nothing here estimates tokens or prices them.
//!
//! Shapes, checked against the CLIs on 2026-09-27:
//!
//! - claude 2.1 `-p --output-format json`: one object `{"type":"result",
//!   "is_error", "result", "session_id", "total_cost_usd", "usage": {...},
//!   "modelUsage": {<model>: {...}}}`; `--output-format stream-json` (the
//!   `acp` transport) ends with a line of the same shape. `usage` and
//!   `modelUsage` are cumulative over every turn of the attempt;
//!   `modelUsage` also covers subagents running on other models, so it is
//!   preferred. Its `inputTokens` exclude cache reads and cache writes.
//! - codex 0.157 `exec --json`: JSON lines; `item.completed` with an
//!   `agent_message` item carries the reply, `turn.completed` carries `usage`
//!   `{input_tokens, cached_input_tokens, cache_write_input_tokens,
//!   output_tokens, reasoning_output_tokens}`. `input_tokens` include the
//!   cached ones and `output_tokens` include the reasoning ones. No cost.
//! - opencode 1.18 `run --format json`: JSON lines; `text` parts carry the
//!   reply, every `step_finish` part carries that step's `tokens` `{input,
//!   output, reasoning, cache: {read, write}}` and `cost`. `input` excludes
//!   the cache, `output` excludes reasoning.
//! - zcode 0.16 `--json`: one object with `response` and `usage`
//!   `{source, inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens}`;
//!   `inputTokens` include the cache reads. zcode also labels the numbers
//!   with a `source` of its own (`provider` or its own count); apb does not
//!   copy that label, since the count is still one the CLI printed.
//!
//! [`AgentUsage`] takes the numbers as the CLI reports them. The one
//! adjustment is the cache reads a CLI is documented to count inside its
//! input (codex, zcode), which are moved out of `input_tokens`; opencode's
//! separate reasoning count is added to `output_tokens`. Nothing else is
//! added or subtracted: whether codex's `cache_write_input_tokens` sit
//! inside its `input_tokens`, or zcode's `reasoningTokens` inside its
//! `outputTokens`, is not something apb can verify, so both are kept as
//! printed. The numbers may therefore not be comparable across agents.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where the numbers of an [`AgentUsage`] come from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// The agent CLI printed them, whatever it says about their origin.
    Reported,
    /// apb estimated them itself. Reserved: nothing records it yet, and a
    /// count the agent CLI printed is always [`UsageSource::Reported`].
    Estimated,
}

/// Token usage of one agent attempt, as the agent CLI reported it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUsage {
    /// Input tokens neither read from nor written to the prompt cache.
    pub input_tokens: u64,
    /// Output tokens, reasoning included.
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// The attempt's cost in US dollars, only when the CLI printed a
    /// non-zero one (a subscription plan prints `0`, which is not a price).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<f64>,
    pub source: UsageSource,
}

/// The reply an agent CLI's machine output carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// The CLI itself marked the attempt as failed (claude's `is_error`).
    pub is_error: bool,
}

/// The reply in `stdout`, `None` when the output is not the machine format
/// of `agent` (see the module docs).
pub fn reply(agent: &str, stdout: &str) -> Option<Reply> {
    match crate::detect::canonical_agent_id(agent) {
        "claude" => claude_result(stdout).map(|r| Reply {
            text: str_at(&r, "result").unwrap_or_default().to_string(),
            is_error: r.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        }),
        // The last agent message. A completed turn that sent none (it only
        // ran tools) is an empty reply, as the plain form printed nothing for
        // it, not a stream to hand on as the reply.
        "codex" => {
            let mut completed = false;
            let mut text = None;
            for v in json_lines(stdout) {
                match str_at(&v, "type") {
                    Some("turn.completed") => completed = true,
                    Some("item.completed") => {
                        if let Some(item) = v.get("item")
                            && str_at(item, "type") == Some("agent_message")
                            && let Some(t) = str_at(item, "text")
                        {
                            text = Some(t.to_string());
                        }
                    }
                    _ => {}
                }
            }
            text.or_else(|| completed.then(String::new))
                .map(|text| Reply {
                    text,
                    is_error: false,
                })
        }
        // Every text part. A run that finished steps without one (tool calls
        // only) is an empty reply, as for codex above.
        "opencode" => {
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
            (stepped || !parts.is_empty()).then(|| Reply {
                text: parts.join("\n"),
                is_error: false,
            })
        }
        "zcode" => crate::zcode::response_text(stdout).map(|text| Reply {
            text,
            is_error: false,
        }),
        _ => None,
    }
}

/// The token usage in `stdout`, `None` when the output reports none (see
/// the module docs). Never estimated.
pub fn usage(agent: &str, stdout: &str) -> Option<AgentUsage> {
    match crate::detect::canonical_agent_id(agent) {
        "claude" => claude_usage(&claude_result(stdout)?),
        "codex" => codex_usage(stdout),
        "opencode" => opencode_usage(stdout),
        "zcode" => zcode_usage(stdout),
        _ => None,
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn u64_at(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// A reported cost, dropped when it is zero (not a price, see
/// [`AgentUsage::cost_usd`]).
fn cost(v: Option<f64>) -> Option<f64> {
    v.filter(|c| c.is_finite() && *c > 0.0)
}

/// Every line of `stdout` that is a JSON object.
fn json_lines(stdout: &str) -> impl Iterator<Item = Value> + '_ {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
}

/// claude's result object: the whole stdout (`--output-format json`) or the
/// last `result` line of a stream (`stream-json`).
fn claude_result(stdout: &str) -> Option<Value> {
    let is_result = |v: &Value| str_at(v, "type") == Some("result");
    let trimmed = stdout.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed)
        && is_result(&v)
    {
        return Some(v);
    }
    json_lines(trimmed).filter(is_result).last()
}

fn claude_usage(result: &Value) -> Option<AgentUsage> {
    let total_cost = cost(result.get("total_cost_usd").and_then(Value::as_f64));
    // Per model, subagents included, when the CLI breaks it down.
    if let Some(models) = result.get("modelUsage").and_then(Value::as_object)
        && !models.is_empty()
    {
        let sum = |key: &str| {
            models
                .values()
                .fold(0u64, |acc, m| acc.saturating_add(u64_at(m, key)))
        };
        return Some(AgentUsage {
            input_tokens: sum("inputTokens"),
            output_tokens: sum("outputTokens"),
            cache_read_tokens: sum("cacheReadInputTokens"),
            cache_write_tokens: sum("cacheCreationInputTokens"),
            cost_usd: total_cost,
            source: UsageSource::Reported,
        });
    }
    let u = result.get("usage").filter(|u| u.is_object())?;
    Some(AgentUsage {
        input_tokens: u64_at(u, "input_tokens"),
        output_tokens: u64_at(u, "output_tokens"),
        cache_read_tokens: u64_at(u, "cache_read_input_tokens"),
        cache_write_tokens: u64_at(u, "cache_creation_input_tokens"),
        cost_usd: total_cost,
        source: UsageSource::Reported,
    })
}

fn codex_usage(stdout: &str) -> Option<AgentUsage> {
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

fn opencode_usage(stdout: &str) -> Option<AgentUsage> {
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

fn zcode_usage(stdout: &str) -> Option<AgentUsage> {
    let doc = serde_json::from_str::<Value>(stdout.trim()).ok()?;
    let u = doc.get("usage").filter(|u| u.is_object())?;
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
