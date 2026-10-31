//! claude's machine output, checked against claude 2.1.283 on 2026-09-27.
//!
//! `-p --output-format json` prints one object `{"type":"result",
//! "is_error", "result", "session_id", "total_cost_usd", "usage": {...},
//! "modelUsage": {<model>: {...}}}`; `--output-format stream-json` (the `acp`
//! transport) ends with a line of the same shape. `usage` and `modelUsage`
//! are cumulative over every turn of the attempt; `modelUsage` also covers
//! subagents running on other models, so it is preferred. Its `inputTokens`
//! exclude cache reads and cache writes.
//!
//! The stream opens with `{"type":"system","subtype":"init", "tools":
//! [<name>...], "mcp_servers": [{"name","status"}...], ...}`: the tools the
//! session sees, MCP tools named `mcp__<server>__<tool>`, and each MCP
//! server with its status (`connected`, `failed`, `needs-auth`, `pending`).
//! The headless `json` form prints no such record.
//!
//! Not checked against a captured init record: whether `--strict-mcp-config`
//! also drops the connectors of the user's claude.ai account. Those are
//! named `claude.ai <name>` and come from the account, not from any MCP
//! config, so the reader treats them as managed (`managed_server`) and the
//! exposure check never counts them as unexpected.

use serde_json::Value;

use crate::agents::exposure::ExposedTools;
use crate::agents::output::{
    AgentUsage, OutputFormat, Promises, Reply, UsageSource, cost, json_lines, json_string_field,
    str_at, u64_at,
};

/// The name prefix of an MCP server that a claude.ai account provides.
const ACCOUNT_SERVER_PREFIX: &str = "claude.ai ";

/// claude's output format.
pub struct ClaudeOutput;

impl OutputFormat for ClaudeOutput {
    fn promises(&self) -> Promises {
        Promises {
            error_flag: true,
            ..Promises::MACHINE
        }
    }

    fn reply(&self, stdout: &str) -> Option<Reply> {
        result(stdout).map(|r| Reply {
            text: str_at(&r, "result").unwrap_or_default().to_string(),
            is_error: r.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    fn usage(&self, stdout: &str) -> Option<AgentUsage> {
        usage_of(&result(stdout)?)
    }

    fn has_final_record(&self, stdout: &str) -> Option<bool> {
        Some(result(stdout).is_some())
    }

    fn session_id(&self, stdout: &str) -> Option<String> {
        json_string_field(stdout, &["session_id"])
    }

    fn exposed_tools(&self, stdout: &str) -> Option<ExposedTools> {
        let init = json_lines(stdout).find(|v| {
            str_at(v, "type") == Some("system") && str_at(v, "subtype") == Some("init")
        })?;
        let tools = init
            .get("tools")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let servers = init
            .get("mcp_servers")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        let name = str_at(s, "name")?;
                        let status = str_at(s, "status").unwrap_or_default();
                        Some((name.to_string(), status.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(ExposedTools { tools, servers })
    }

    fn shell_tool(&self) -> Option<&'static str> {
        Some("Bash")
    }

    fn mcp_tool(&self, server: &str, tool: &str) -> Option<String> {
        Some(format!("mcp__{server}__{tool}"))
    }

    fn managed_server(&self, name: &str) -> bool {
        name.starts_with(ACCOUNT_SERVER_PREFIX)
    }
}

/// claude's result object: the whole stdout (`--output-format json`) or the
/// last `result` line of a stream (`stream-json`).
fn result(stdout: &str) -> Option<Value> {
    let is_result = |v: &Value| str_at(v, "type") == Some("result");
    let trimmed = stdout.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed)
        && is_result(&v)
    {
        return Some(v);
    }
    json_lines(trimmed).filter(is_result).last()
}

fn usage_of(result: &Value) -> Option<AgentUsage> {
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
