//! codex's two log formats.
//!
//! - `exec --json` stdout: one event per line; `item.completed` carries a
//!   finished item whose lowercase `type` says what it was
//!   (`command_execution` with its `command`, `file_change` with its
//!   `changes`, `mcp_tool_call` with `server` and `tool`, `web_search`,
//!   `agent_message` with the reply's `text`).
//! - A session rollout (`sessions/YYYY/MM/DD/rollout-*.jsonl`): `{type,
//!   payload}` lines. `session_meta` and `turn_context` name the working
//!   directory. The newer form reports finished work as `event_msg`
//!   payloads of type `item_completed` (item types `CommandExecution`,
//!   `FileChange`, `McpToolCall`, `UserMessage`); the older one as
//!   `response_item` payloads (`function_call`, `custom_tool_call`,
//!   `local_shell_call`, a user `message`). Both forms can describe the same
//!   work in one file, so one form is chosen per file ([`RolloutFormat`])
//!   and nothing is counted twice.

use std::path::PathBuf;

use serde_json::Value;

use crate::agent_actions::action::{Action, ToolCall, WebKind, str_at};

/// Item fields that report what came of a call, not what it asked: left out
/// of the input a repeated call is told by.
const RESULT_FIELDS: [&str; 6] = [
    "id",
    "status",
    "aggregated_output",
    "exit_code",
    "result",
    "error",
];

/// `item` without its [`RESULT_FIELDS`]: what a call asked, built without
/// copying a command's output first.
fn asked(item: &Value) -> Value {
    match item {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !RESULT_FIELDS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Shell tools of the older rollout form.
const SHELL_TOOLS: [&str; 4] = ["shell", "exec_command", "container.exec", "local_shell"];

/// A command given as a list of words or as one line, as one line.
fn command_line(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(words) => {
            let words: Vec<&str> = words.iter().filter_map(Value::as_str).collect();
            (!words.is_empty()).then(|| shell_words::join(words))
        }
        _ => None,
    }
}

/// The paths of a file change: a list of `{path}` or a map keyed by path.
fn change_paths(changes: Option<&Value>) -> Vec<String> {
    match changes {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|c| str_at(c, "path").map(str::to_string))
            .collect(),
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The files an `apply_patch` names in its headers.
fn patch_paths(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| {
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|h| l.strip_prefix(h))
        })
        .map(|p| p.trim().to_string())
        .collect()
}

fn edit(name: &str, input: Value, paths: Vec<String>) -> ToolCall {
    ToolCall {
        name: name.to_string(),
        input,
        actions: vec![Action::Edit {
            paths: paths.clone(),
        }],
        paths,
        destructive: false,
    }
}

fn one(name: &str, input: Value, action: Action) -> ToolCall {
    ToolCall {
        name: name.to_string(),
        input,
        paths: Vec::new(),
        destructive: false,
        actions: vec![action],
    }
}

/// A finished item (either case of its type) as a call; `None` for an item
/// that is not a call (a message, reasoning, a plan).
pub fn item_call(item: &Value) -> Option<ToolCall> {
    let raw = str_at(item, "type")?;
    let kind: String = raw
        .chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let input = asked(item);
    match kind.as_str() {
        "commandexecution" => {
            let line = command_line(item.get("command"))?;
            Some(crate::agent_actions::command::call(raw, input, &line))
        }
        "filechange" => {
            let paths = change_paths(item.get("changes"));
            Some(edit(raw, input, paths))
        }
        "mcptoolcall" => {
            let action = Action::Mcp {
                server: str_at(item, "server").unwrap_or_default().to_string(),
                tool: str_at(item, "tool").unwrap_or_default().to_string(),
            };
            Some(one(raw, input, action))
        }
        "websearch" => Some(one(
            raw,
            input,
            Action::Web {
                kind: WebKind::Search,
            },
        )),
        "collabagenttoolcall" | "spawnagent" => Some(one(raw, input, Action::Agent)),
        _ => None,
    }
}

pub use super::output::{agent_message, completed_item};

/// The calls of one `exec --json` stdout line (an `item.completed`).
pub fn calls_of_exec_line(line: &Value) -> Vec<ToolCall> {
    completed_item(line)
        .and_then(item_call)
        .into_iter()
        .collect()
}

/// Which form of a rollout a file's work is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutFormat {
    /// `event_msg` payloads of type `item_completed`.
    ItemCompleted,
    /// `response_item` payloads.
    ResponseItem,
}

fn payload(line: &Value) -> Option<&Value> {
    line.get("payload")
}

fn is_item_completed(line: &Value) -> bool {
    str_at(line, "type") == Some("event_msg")
        && payload(line).and_then(|p| str_at(p, "type")) == Some("item_completed")
}

impl RolloutFormat {
    /// The newer form when any line has it, else the older one.
    pub fn detect<V: std::borrow::Borrow<Value>>(lines: impl IntoIterator<Item = V>) -> Self {
        if lines.into_iter().any(|l| is_item_completed(l.borrow())) {
            RolloutFormat::ItemCompleted
        } else {
            RolloutFormat::ResponseItem
        }
    }
}

/// The arguments of an older-form `function_call`: a JSON object as text.
fn arguments(p: &Value) -> Value {
    match p.get("arguments") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

/// An older-form `response_item` payload as a call.
fn response_call(p: &Value) -> Option<ToolCall> {
    let name = str_at(p, "name").unwrap_or_default();
    match str_at(p, "type")? {
        "function_call" => {
            let args = arguments(p);
            if SHELL_TOOLS.contains(&name) {
                let line = command_line(args.get("command").or_else(|| args.get("cmd")))?;
                return Some(crate::agent_actions::command::call(name, args, &line));
            }
            Some(match name {
                "apply_patch" => {
                    let patch = str_at(&args, "input").unwrap_or_default().to_string();
                    edit(name, args, patch_paths(&patch))
                }
                "spawn_agent" => one(name, args, Action::Agent),
                _ => one(
                    name,
                    args,
                    Action::Tool {
                        name: name.to_string(),
                    },
                ),
            })
        }
        "custom_tool_call" => {
            let input = p.get("input").cloned().unwrap_or(Value::Null);
            Some(if name == "apply_patch" {
                let paths = patch_paths(input.as_str().unwrap_or_default());
                edit(name, input, paths)
            } else {
                one(
                    name,
                    input,
                    Action::Tool {
                        name: name.to_string(),
                    },
                )
            })
        }
        "local_shell_call" => {
            let action = p.get("action").cloned().unwrap_or(Value::Null);
            let line = command_line(action.get("command"))?;
            Some(crate::agent_actions::command::call(
                "local_shell_call",
                action,
                &line,
            ))
        }
        "web_search_call" => Some(one(
            "web_search_call",
            asked(p),
            Action::Web {
                kind: WebKind::Search,
            },
        )),
        _ => None,
    }
}

/// The calls of one rollout line under the file's `format`.
pub fn calls_of_rollout_line(line: &Value, format: RolloutFormat) -> Vec<ToolCall> {
    let call = match format {
        RolloutFormat::ItemCompleted if is_item_completed(line) => payload(line)
            .and_then(|p| p.get("item"))
            .and_then(item_call),
        RolloutFormat::ResponseItem if str_at(line, "type") == Some("response_item") => {
            payload(line).and_then(response_call)
        }
        _ => None,
    };
    call.into_iter().collect()
}

/// The text of a message's content: a string, or the `text` of its parts.
fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| str_at(p, "text"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// What a person typed in this rollout line, under the file's `format`,
/// its wrappers stripped.
pub fn user_text(line: &Value, format: RolloutFormat) -> Option<String> {
    let p = payload(line)?;
    let text = match format {
        RolloutFormat::ItemCompleted => {
            let item = is_item_completed(line).then(|| p.get("item")).flatten()?;
            let kind = str_at(item, "type")?.replace('_', "").to_ascii_lowercase();
            (kind == "usermessage")
                .then(|| {
                    content_text(item.get("content")).or_else(|| content_text(item.get("text")))
                })
                .flatten()?
        }
        RolloutFormat::ResponseItem => (str_at(line, "type") == Some("response_item")
            && str_at(p, "type") == Some("message")
            && str_at(p, "role") == Some("user"))
        .then(|| content_text(p.get("content")))
        .flatten()?,
    };
    crate::agent_actions::action::typed_text(&text)
}

/// The working directory a `session_meta` or `turn_context` line names.
pub fn cwd_of(line: &Value) -> Option<&str> {
    match str_at(line, "type")? {
        "session_meta" | "turn_context" => payload(line).and_then(|p| str_at(p, "cwd")),
        _ => None,
    }
}

/// codex's home: `CODEX_HOME`, else `~/.codex`.
pub fn home_dir() -> Option<PathBuf> {
    crate::agent_actions::action::config_dir("CODEX_HOME", ".codex")
}

/// Where codex keeps its session rollouts: `sessions/` of [`home_dir`].
pub fn sessions_dir() -> Option<PathBuf> {
    home_dir().map(|d| d.join("sessions"))
}
