//! dsh's `--json` stdout as tool calls: a `tool_call {callId, tool, input}`
//! event is one call, named by dsh's own tool names (`bash`, `pwsh`, `read`,
//! `write`, `edit`, `apply_patch`, `grep`, `glob`, `skill`, `web_fetch`,
//! `web_search`, `subagent`, and `mcp__<server>__<tool>` for an MCP tool);
//! `apply_patch` takes its patch text as `input` and names its files in the
//! patch's add, update, move and delete headers. A `tool_result` whose
//! `status` is `error` is a failed call.

use serde_json::Value;

use crate::agent_actions::action::{Action, ToolCall, WebKind, patch_paths, str_at};
use crate::agent_actions::command::skill_of_path;

/// Tools that write the file they name.
const WRITE_TOOLS: [&str; 3] = ["write", "edit", "apply_patch"];
/// Shell tools, whose `command` is a command line.
const SHELL_TOOLS: [&str; 2] = ["bash", "pwsh"];
/// Input fields that name a file.
const PATH_FIELDS: [&str; 2] = ["file_path", "path"];

fn action(name: &str, input: &Value, paths: &[String]) -> Action {
    if let Some(rest) = name.strip_prefix("mcp__") {
        let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
        return Action::Mcp {
            server: server.to_string(),
            tool: tool.to_string(),
        };
    }
    match name {
        n if WRITE_TOOLS.contains(&n) => Action::Edit {
            paths: paths.to_vec(),
        },
        "read" => Action::Read {
            path: paths.first().cloned().unwrap_or_default(),
            skill: paths.iter().find_map(|p| skill_of_path(p)),
        },
        "grep" | "glob" => Action::Search,
        "web_fetch" => Action::Web {
            kind: WebKind::Fetch,
        },
        "web_search" => Action::Web {
            kind: WebKind::Search,
        },
        "subagent" | "subagent_fork" => Action::Agent,
        "skill" => match ["name", "skill"].iter().find_map(|f| str_at(input, f)) {
            Some(s) => Action::Skill {
                name: s.to_string(),
            },
            None => Action::Tool {
                name: name.to_string(),
            },
        },
        _ => Action::Tool {
            name: name.to_string(),
        },
    }
}

/// The calls of one stdout line: one for a `tool_call` event, none else.
pub fn calls_of_line(line: &Value) -> Vec<ToolCall> {
    if str_at(line, "type") != Some("tool_call") {
        return Vec::new();
    }
    let name = str_at(line, "tool").unwrap_or_default();
    let input = line.get("input").cloned().unwrap_or(Value::Null);
    if SHELL_TOOLS.contains(&name)
        && let Some(cmd) = str_at(&input, "command")
    {
        let cmd = cmd.to_string();
        return vec![crate::agent_actions::command::call(name, input, &cmd)];
    }
    // `apply_patch` names its files in the headers of its patch text.
    let paths: Vec<String> = match (name, str_at(&input, "input")) {
        ("apply_patch", Some(patch)) => patch_paths(patch),
        _ => PATH_FIELDS
            .iter()
            .filter_map(|f| str_at(&input, f))
            .map(str::to_string)
            .collect(),
    };
    vec![ToolCall {
        name: name.to_string(),
        actions: vec![action(name, &input, &paths)],
        input,
        paths,
        destructive: false,
    }]
}

/// Whether one stdout line reports a failed tool call.
pub fn failed_result(line: &Value) -> bool {
    str_at(line, "type") == Some("tool_result") && str_at(line, "status") == Some("error")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn each_tool_call_is_one_call_with_its_action() {
        let bash = calls_of_line(&line(
            r#"{"type":"tool_call","callId":"c","tool":"bash","input":{"command":"rm -rf build","description":"x"}}"#,
        ));
        assert_eq!(bash.len(), 1);
        assert!(bash[0].destructive);
        let write = calls_of_line(&line(
            r#"{"type":"tool_call","callId":"c","tool":"write","input":{"file_path":"a.md","content":"x"}}"#,
        ));
        assert!(write[0].writes());
        assert_eq!(write[0].paths, ["a.md"]);
        let read = calls_of_line(&line(
            r#"{"type":"tool_call","callId":"c","tool":"read","input":{"file_path":".agents/skills/demo/SKILL.md"}}"#,
        ));
        assert_eq!(read[0].skill(), Some("demo"));
        let mcp = calls_of_line(&line(
            r#"{"type":"tool_call","callId":"c","tool":"mcp__apb__playbook_list","input":{}}"#,
        ));
        assert_eq!(
            mcp[0].actions,
            [Action::Mcp {
                server: "apb".into(),
                tool: "playbook_list".into()
            }]
        );
        assert!(calls_of_line(&line(r#"{"type":"text","text":"hi"}"#)).is_empty());
    }

    /// dsh's `apply_patch` takes the patch text as `input`, its files named
    /// by the add, update, move and delete headers.
    #[test]
    fn an_apply_patch_names_the_files_of_its_headers() {
        let calls = calls_of_line(&line(
            r#"{"type":"tool_call","callId":"c","tool":"apply_patch","input":{"input":"*** Begin Patch\n*** Add File: docs/new.md\n+hi\n*** Update File: src/a.rs\n*** Move to: src/b.rs\n@@\n-x\n+y\n*** Delete File: old.txt\n*** End Patch"}}"#,
        ));
        assert!(calls[0].writes());
        assert_eq!(
            calls[0].paths,
            ["docs/new.md", "src/a.rs", "src/b.rs", "old.txt"]
        );
        assert_eq!(
            calls[0].actions,
            [Action::Edit {
                paths: vec![
                    "docs/new.md".into(),
                    "src/a.rs".into(),
                    "src/b.rs".into(),
                    "old.txt".into()
                ]
            }]
        );
    }

    /// A shell call without a string `command` carries no command facts: it
    /// stays one plain tool call, never destructive and naming no files.
    #[test]
    fn a_shell_call_without_a_command_is_a_plain_tool_call() {
        for input in [r#"{}"#, r#"{"command":["rm","-rf","build"]}"#] {
            for tool in ["bash", "pwsh"] {
                let calls = calls_of_line(&line(&format!(
                    r#"{{"type":"tool_call","callId":"c","tool":"{tool}","input":{input}}}"#
                )));
                assert_eq!(calls.len(), 1);
                assert!(!calls[0].destructive);
                assert!(calls[0].paths.is_empty());
                assert_eq!(calls[0].actions, [Action::Tool { name: tool.into() }]);
            }
        }
    }

    #[test]
    fn an_error_result_is_a_failed_call() {
        assert!(failed_result(&line(
            r#"{"type":"tool_result","callId":"c","status":"error"}"#
        )));
        assert!(!failed_result(&line(
            r#"{"type":"tool_result","callId":"c","status":"completed","result":"ok"}"#
        )));
    }
}
