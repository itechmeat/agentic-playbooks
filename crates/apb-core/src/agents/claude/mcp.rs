//! Claude Code as an MCP host: user-scope servers are registered and
//! removed through its own CLI (`mcp add --scope user <name> -- <command>`,
//! `mcp remove --scope user <name>`) and read, without starting it, from
//! `mcpServers` in its user config (`$CLAUDE_CONFIG_DIR/.claude.json`, else
//! `~/.claude.json`), which Claude Code itself rewrites all the time.

use std::path::PathBuf;

use serde_json::Value;

use crate::agents::mcp_host::{McpHost, McpMethod, McpRegistration, McpServer};

pub struct ClaudeMcp;

fn config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join(".claude.json"));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".claude.json"))
}

impl McpHost for ClaudeMcp {
    fn config_path(&self) -> Option<PathBuf> {
        config_path()
    }

    /// Only the CLI can register: the config alone does not make the host.
    fn marker(&self) -> Option<PathBuf> {
        None
    }

    fn registration(&self, text: &str, server: &McpServer) -> McpRegistration {
        let Ok(doc) = serde_json::from_str::<Value>(text) else {
            return McpRegistration::Unreadable;
        };
        let Some(entry) = doc.get("mcpServers").and_then(|m| m.get(server.name)) else {
            return McpRegistration::Absent;
        };
        let (program, args) = server.command.split_first().unwrap_or((&"", &[]));
        let same = entry.get("command").and_then(Value::as_str) == Some(*program)
            && entry
                .get("args")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()))
                == Value::from(args.to_vec());
        if same {
            McpRegistration::Same
        } else {
            McpRegistration::Different
        }
    }

    fn method(&self, server: &McpServer) -> McpMethod {
        let mut add: Vec<String> = ["mcp", "add", "--scope", "user", server.name, "--"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        add.extend(server.command.iter().map(|s| s.to_string()));
        let remove = ["mcp", "remove", "--scope", "user", server.name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        McpMethod::Cli { add, remove }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: McpServer = McpServer {
        name: "srv",
        command: &["prog", "serve"],
    };

    #[test]
    fn the_registration_reads_the_user_servers() {
        let same =
            r#"{"mcpServers":{"srv":{"type":"stdio","command":"prog","args":["serve"],"env":{}}}}"#;
        assert_eq!(ClaudeMcp.registration(same, &SERVER), McpRegistration::Same);
        let other = r#"{"mcpServers":{"srv":{"command":"/opt/prog","args":["serve"]}}}"#;
        assert_eq!(
            ClaudeMcp.registration(other, &SERVER),
            McpRegistration::Different
        );
        assert_eq!(
            ClaudeMcp.registration("{}", &SERVER),
            McpRegistration::Absent
        );
        assert_eq!(
            ClaudeMcp.registration("{", &SERVER),
            McpRegistration::Unreadable
        );
    }
}
