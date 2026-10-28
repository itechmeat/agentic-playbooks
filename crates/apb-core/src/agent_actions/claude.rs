//! claude's session lines: one JSON message per line, `message.content` a
//! string or a list of blocks (`text`, `tool_use`, `tool_result`). A line
//! also carries `type`, `cwd`, `isSidechain` (a sub-agent's message),
//! `isMeta` (a turn the CLI added) and `isCompactSummary` (the summary that
//! replaces a compacted conversation); many lines are not messages at all.

use std::path::PathBuf;

use serde_json::Value;

use super::action::{Action, ToolCall, WebKind, str_at};
use super::command::skill_of_path;

/// Tools that write the file they name.
const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];
/// Input fields that name a file.
const PATH_FIELDS: [&str; 3] = ["file_path", "notebook_path", "path"];

/// The action of a claude tool, by its name and input.
fn action(name: &str, input: &Value, paths: &[String]) -> Vec<Action> {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
        return vec![Action::Mcp {
            server: server.to_string(),
            tool: tool.to_string(),
        }];
    }
    let one = match name {
        "Bash" => {
            return super::command::actions(str_at(input, "command").unwrap_or_default());
        }
        n if WRITE_TOOLS.contains(&n) => Action::Edit {
            paths: paths.to_vec(),
        },
        // Only a read loads a skill: writing or searching a `SKILL.md`
        // does not.
        "Read" => Action::Read {
            path: paths.first().cloned().unwrap_or_default(),
            skill: paths.iter().find_map(|p| skill_of_path(p)),
        },
        "Grep" | "Glob" => Action::Search,
        "WebFetch" => Action::Web {
            kind: WebKind::Fetch,
        },
        "WebSearch" => Action::Web {
            kind: WebKind::Search,
        },
        "Task" | "Agent" => Action::Agent,
        "Skill" => match ["skill", "command", "name"]
            .iter()
            .find_map(|f| str_at(input, f))
        {
            Some(s) => Action::Skill {
                name: s.trim_start_matches('/').to_string(),
            },
            None => Action::Tool {
                name: name.to_string(),
            },
        },
        _ => Action::Tool {
            name: name.to_string(),
        },
    };
    vec![one]
}

/// One `tool_use` block as a call.
pub fn tool_call(item: &Value) -> ToolCall {
    let name = str_at(item, "name").unwrap_or_default().to_string();
    let input = item.get("input").cloned().unwrap_or(Value::Null);
    let paths: Vec<String> = PATH_FIELDS
        .iter()
        .filter_map(|f| str_at(&input, f))
        .map(str::to_string)
        .collect();
    ToolCall {
        actions: action(&name, &input, &paths),
        destructive: str_at(&input, "command").is_some_and(super::command::is_destructive),
        name,
        input,
        paths,
    }
}

/// The content blocks of a session line (`message.content` as a list).
fn blocks(line: &Value) -> &[Value] {
    line.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The tool calls of a session line, in order.
pub fn tool_calls(line: &Value) -> Vec<ToolCall> {
    blocks(line)
        .iter()
        .filter(|b| str_at(b, "type") == Some("tool_use"))
        .map(tool_call)
        .collect()
}

/// What a person typed in this line: a `user` message that the CLI did not
/// add (`isMeta`, a summary of a compacted conversation
/// `isCompactSummary`) and that is not a tool result, its wrappers
/// stripped.
pub fn user_text(line: &Value) -> Option<String> {
    let flag = |key: &str| line.get(key).and_then(Value::as_bool) == Some(true);
    if str_at(line, "type") != Some("user") || flag("isMeta") || flag("isCompactSummary") {
        return None;
    }
    let content = line.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            if items
                .iter()
                .any(|b| str_at(b, "type") == Some("tool_result"))
            {
                return None;
            }
            items
                .iter()
                .filter(|b| str_at(b, "type") == Some("text"))
                .filter_map(|b| str_at(b, "text"))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    super::action::typed_text(&text)
}

/// The working directory a line was written in.
pub fn cwd(line: &Value) -> Option<&str> {
    str_at(line, "cwd")
}

/// A sub-agent's line.
pub fn is_sidechain(line: &Value) -> bool {
    line.get("isSidechain").and_then(Value::as_bool) == Some(true)
}

/// claude's config directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    super::action::config_dir("CLAUDE_CONFIG_DIR", ".claude")
}

/// Where claude keeps its sessions: `projects/` of [`config_dir`].
pub fn projects_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("projects"))
}

/// The directory name claude keeps a working directory's sessions under:
/// every character that is not ASCII alphanumeric becomes `-`.
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
