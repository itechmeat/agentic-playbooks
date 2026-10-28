//! Which tools an agent attempt should see, and the `tool_exposure_mismatch`
//! an attempt journals when what the agent reported differs.
//!
//! What apb can expect is only what it hands over itself:
//! - the shell, when the node holds connector grants: an agent calls a
//!   connector through `apb connector call`, never as an MCP tool;
//! - apb's ask-server, connected, and its `ask_user` tool on a live node (a
//!   server still starting when the agent reported is not judged);
//! - under the minimal environment's strict MCP config, no other server
//!   beyond the ones the agent's account provides whatever its config says.
//!
//! Checked only for an agent whose streaming form reports its tools
//! (`Capabilities::tool_exposure`) and only when the attempt's output did
//! (the stream transport): the headless form prints no tool list.

use apb_core::agents::{ExpectedServer, ExpectedTools, ExposedTools, compare};

use crate::adapter::{ASK_SERVER_NAME, ASK_TOOL};
use crate::error::EngineError;
use crate::event::EventPayload;
use crate::scheduler::journal::Journal;

/// What apb handed one attempt.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Handed {
    /// The node holds connector grants.
    pub connectors: bool,
    /// The attempt is live: apb's ask-server was passed as an MCP server.
    pub live: bool,
    /// The minimal environment's strict MCP config: only apb's servers.
    pub strict_mcp: bool,
}

/// What an attempt of `agent` handed `handed` should see; `None` for an
/// agent that does not report its tools, or when nothing is expected.
pub(crate) fn expected(agent: &str, handed: Handed) -> Option<ExpectedTools> {
    let module = apb_core::agents::lookup(agent)?;
    let capabilities = module.descriptor().capabilities;
    if !capabilities.tool_exposure {
        return None;
    }
    let output = module.output();
    let mut want = ExpectedTools {
        only_these_servers: handed.strict_mcp,
        ..ExpectedTools::default()
    };
    if handed.connectors {
        want.tools.extend(output.shell_tool().map(str::to_string));
    }
    if handed.live && capabilities.live_ask_mcp {
        want.servers.push(ExpectedServer {
            name: ASK_SERVER_NAME.to_string(),
            tools: output
                .mcp_tool(ASK_SERVER_NAME, ASK_TOOL)
                .into_iter()
                .collect(),
        });
    }
    (!want.is_empty()).then_some(want)
}

/// The `tool_exposure_mismatch` of attempt `attempt` of `node` on `agent`,
/// when the agent reported its tools and they differ from what it was
/// handed; `None` otherwise.
pub(crate) fn mismatch(
    node: &str,
    attempt: u32,
    agent: &str,
    handed: Handed,
    exposure: Option<&ExposedTools>,
) -> Option<EventPayload> {
    let exposed = exposure?;
    let want = expected(agent, handed)?;
    let output = apb_core::agents::lookup(agent)?.output();
    let m = compare(&want, exposed, &|name| output.managed_server(name));
    (!m.is_empty()).then(|| EventPayload::ToolExposureMismatch {
        node: node.to_string(),
        attempt,
        agent: agent.to_string(),
        missing_tools: m.missing_tools,
        missing_servers: m.missing_servers,
        unexpected_servers: m.unexpected_servers,
        item: None,
    })
}

/// Journals the attempt's [`mismatch`], if any. Written before the
/// attempt's `attempt_finished`.
pub(crate) fn journal(
    journal: &Journal<'_>,
    node: &str,
    attempt: u32,
    agent: &str,
    handed: Handed,
    exposure: Option<&ExposedTools>,
) -> Result<(), EngineError> {
    match mismatch(node, attempt, agent, handed, exposure) {
        Some(event) => journal.append(event),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::agents::CONNECTED;

    fn seen(tools: &[&str], servers: &[(&str, &str)]) -> ExposedTools {
        ExposedTools {
            tools: tools.iter().map(|s| s.to_string()).collect(),
            servers: servers
                .iter()
                .map(|(n, s)| (n.to_string(), s.to_string()))
                .collect(),
        }
    }

    fn any_reporting_agent() -> &'static str {
        apb_core::agents::all()
            .iter()
            .find(|a| a.descriptor().capabilities.tool_exposure)
            .map(|a| a.descriptor().id)
            .expect("an agent reports its tools")
    }

    /// An agent that reports its tools and takes apb's ask-server.
    fn a_reporting_live_agent() -> &'static str {
        apb_core::agents::all()
            .iter()
            .map(|a| a.descriptor())
            .find(|d| d.capabilities.tool_exposure && d.capabilities.live_ask_mcp)
            .map(|d| d.id)
            .expect("an agent reports its tools and takes the ask-server")
    }

    #[test]
    fn nothing_is_expected_of_an_agent_that_reports_no_tools() {
        let quiet = apb_core::agents::all()
            .iter()
            .find(|a| !a.descriptor().capabilities.tool_exposure)
            .unwrap()
            .descriptor()
            .id;
        let handed = Handed {
            connectors: true,
            live: true,
            strict_mcp: true,
        };
        assert_eq!(expected(quiet, handed), None);
        assert!(mismatch("n", 1, quiet, handed, Some(&seen(&[], &[]))).is_none());
    }

    #[test]
    fn a_live_attempt_without_a_connected_ask_server_is_a_mismatch() {
        let agent = a_reporting_live_agent();
        let handed = Handed {
            connectors: false,
            live: true,
            strict_mcp: true,
        };
        let ev = mismatch(
            "ask",
            2,
            agent,
            handed,
            Some(&seen(&["Read"], &[("apb", "failed"), ("other", CONNECTED)])),
        )
        .expect("a mismatch");
        let EventPayload::ToolExposureMismatch {
            missing_servers,
            unexpected_servers,
            missing_tools,
            attempt,
            ..
        } = ev
        else {
            panic!("wrong event")
        };
        assert_eq!(attempt, 2);
        assert_eq!(missing_servers, ["apb (failed)"]);
        assert_eq!(unexpected_servers, ["other"]);
        assert_eq!(missing_tools.len(), 1);
    }

    /// A server still starting is not missing, and an account's own
    /// connector is not unexpected under a strict config. claude by name:
    /// the account connector's naming is claude's own.
    #[test]
    fn a_pending_ask_server_and_an_account_connector_are_no_mismatch() {
        let agent = apb_core::agents::claude::ID;
        let output = apb_core::agents::lookup(agent).unwrap().output();
        let handed = Handed {
            connectors: false,
            live: true,
            strict_mcp: true,
        };
        let account = "claude.ai Calendar";
        assert!(output.managed_server(account));
        let seen = seen(
            &["Read"],
            &[(ASK_SERVER_NAME, "pending"), (account, CONNECTED)],
        );
        assert!(mismatch("ask", 1, agent, handed, Some(&seen)).is_none());
    }

    #[test]
    fn an_output_without_a_tool_list_is_not_checked() {
        let handed = Handed {
            connectors: true,
            ..Handed::default()
        };
        assert!(mismatch("n", 1, any_reporting_agent(), handed, None).is_none());
    }
}
