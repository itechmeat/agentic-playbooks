//! Which tools an agent really sees: what its streaming output reports at
//! start ([`ExposedTools`]), what apb handed it for one attempt
//! ([`ExpectedTools`]) and the difference ([`compare`]). Agent-neutral: an
//! agent module reads its own start record into an [`ExposedTools`] and names
//! its tools (`OutputFormat::shell_tool`, `OutputFormat::mcp_tool`).

/// The status an agent reports for an MCP server it could start.
pub const CONNECTED: &str = "connected";

/// The statuses of an MCP server that is still starting when the agent
/// reports: neither the server nor its tools count as missing yet. Every
/// other status that is not [`CONNECTED`] (`failed`, `needs-auth`, one the
/// agent adds later) counts the server as missing, so an unknown status
/// fails safe.
pub const PENDING: &[&str] = &["pending"];

/// The tools and MCP servers an agent reports it sees.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExposedTools {
    pub tools: Vec<String>,
    /// MCP server names with the status the agent reports.
    pub servers: Vec<(String, String)>,
}

/// One MCP server an attempt should see, with the tools it provides.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExpectedServer {
    pub name: String,
    /// The server's tools, as the agent names them.
    pub tools: Vec<String>,
}

/// What one attempt should see: the tools apb's own channels need, the MCP
/// servers apb passed, and whether those are the only servers allowed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExpectedTools {
    /// Tool names that must be present, outside any MCP server.
    pub tools: Vec<String>,
    /// MCP servers that must be present and connected, with their tools.
    pub servers: Vec<ExpectedServer>,
    /// No MCP server beyond `servers` may be present (a strict MCP config).
    pub only_these_servers: bool,
}

impl ExpectedTools {
    /// Whether nothing at all is expected, so there is nothing to compare.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.servers.is_empty() && !self.only_these_servers
    }
}

/// How what an agent saw differs from what it should see. Names only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExposureMismatch {
    pub missing_tools: Vec<String>,
    /// Expected servers that are absent (`name`) or not connected and not
    /// starting (`name (status)`, see [`PENDING`]).
    pub missing_servers: Vec<String>,
    pub unexpected_servers: Vec<String>,
}

impl ExposureMismatch {
    /// Whether the agent saw exactly what it should.
    pub fn is_empty(&self) -> bool {
        self.missing_tools.is_empty()
            && self.missing_servers.is_empty()
            && self.unexpected_servers.is_empty()
    }
}

/// The difference between `expected` and what the agent reported. A server
/// still starting ([`PENDING`]) is not judged,
/// and neither are its tools: they appear once it is up. Under a strict
/// config, a server `managed` names (one the agent lists whatever its MCP
/// config says, see `OutputFormat::managed_server`) is not unexpected.
pub fn compare(
    expected: &ExpectedTools,
    exposed: &ExposedTools,
    managed: &dyn Fn(&str) -> bool,
) -> ExposureMismatch {
    let mut missing_tools: Vec<String> = Vec::new();
    let mut missing_servers = Vec::new();
    let mut want_tool = |t: &String| {
        if !exposed.tools.contains(t) {
            missing_tools.push(t.clone());
        }
    };
    expected.tools.iter().for_each(&mut want_tool);
    for server in &expected.servers {
        match exposed.servers.iter().find(|(n, _)| *n == server.name) {
            None => missing_servers.push(server.name.clone()),
            Some((_, status)) if PENDING.contains(&status.as_str()) => continue,
            Some((_, status)) if status != CONNECTED => {
                missing_servers.push(format!("{} ({status})", server.name));
            }
            Some(_) => {}
        }
        server.tools.iter().for_each(&mut want_tool);
    }
    let unexpected_servers = if expected.only_these_servers {
        exposed
            .servers
            .iter()
            .map(|(n, _)| n)
            .filter(|n| !expected.servers.iter().any(|s| s.name == **n) && !managed(n))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    ExposureMismatch {
        missing_tools,
        missing_servers,
        unexpected_servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposed(tools: &[&str], servers: &[(&str, &str)]) -> ExposedTools {
        ExposedTools {
            tools: tools.iter().map(|s| s.to_string()).collect(),
            servers: servers
                .iter()
                .map(|(n, s)| (n.to_string(), s.to_string()))
                .collect(),
        }
    }

    fn none(_: &str) -> bool {
        false
    }

    fn apb_server() -> ExpectedServer {
        ExpectedServer {
            name: "apb".into(),
            tools: vec!["mcp__apb__ask_user".into()],
        }
    }

    #[test]
    /// Any status but connected or starting is missing, one the agent adds
    /// later included.
    fn a_server_that_is_not_connected_or_starting_is_missing_with_its_status() {
        let expected = ExpectedTools {
            tools: vec!["Bash".into()],
            servers: vec![apb_server()],
            only_these_servers: false,
        };
        for status in ["failed", "needs-auth", "disabled", "some-new-status"] {
            let m = compare(&expected, &exposed(&["Read"], &[("apb", status)]), &none);
            assert_eq!(m.missing_tools, ["Bash", "mcp__apb__ask_user"]);
            assert_eq!(m.missing_servers, [format!("apb ({status})")]);
            assert!(m.unexpected_servers.is_empty());
        }
    }

    /// A server still starting when the agent reported is not judged, and
    /// neither are its tools.
    #[test]
    fn a_pending_server_and_its_tools_are_not_missing() {
        let expected = ExpectedTools {
            servers: vec![apb_server()],
            ..Default::default()
        };
        assert!(compare(&expected, &exposed(&[], &[("apb", "pending")]), &none).is_empty());
        let absent = compare(&expected, &exposed(&[], &[]), &none);
        assert_eq!(absent.missing_servers, ["apb"]);
        assert_eq!(absent.missing_tools, ["mcp__apb__ask_user"]);
    }

    #[test]
    fn extra_servers_count_only_under_a_strict_config() {
        let mut expected = ExpectedTools {
            servers: vec![ExpectedServer {
                name: "apb".into(),
                tools: Vec::new(),
            }],
            ..Default::default()
        };
        let seen = exposed(&[], &[("apb", CONNECTED), ("extra", CONNECTED)]);
        assert!(compare(&expected, &seen, &none).is_empty());
        expected.only_these_servers = true;
        assert_eq!(
            compare(&expected, &seen, &none).unexpected_servers,
            ["extra"]
        );
        // A server the agent lists whatever its config says is no surprise.
        let managed = |n: &str| n == "extra";
        assert!(compare(&expected, &seen, &managed).is_empty());
    }
}
