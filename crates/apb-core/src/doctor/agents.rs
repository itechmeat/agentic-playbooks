//! The agent rows of `apb doctor`: whether each agent is installed, its
//! login, and for a built-in agent the free local check of its CLI
//! (`crate::agents::check`): the version against the verified one, the
//! flags of its built-in form in its help, or the note for a custom form.
//!
//! The config is read under the agent name as given, the way a run reads a
//! profile's agent (`spec_for`, `program_for`), and the binary checked is
//! the one a run launches: `agents.<id>.program` when set.

use std::collections::BTreeSet;

use super::report::{CheckStatus, DoctorReport};
use crate::agents::check::{CheckInput, Level, check};
use crate::agents::{Agent, CONNECTED};
use crate::config::{GlobalConfig, Transport, program_in_path};
use crate::detect::AgentInfo;

/// The rows of the agents named by profiles. Detection spawns binaries, so
/// it runs only when at least one built-in agent is named (otherwise the
/// PATH fallback is enough, and tests stay fast).
pub(super) fn agent_checks(r: &mut DoctorReport, global: &GlobalConfig, agents: &BTreeSet<String>) {
    let want_detect = agents.iter().any(|a| crate::agents::lookup(a).is_some());
    let detected = if want_detect {
        crate::agent_catalog::agents(false)
    } else {
        Vec::new()
    };
    for agent in agents {
        let probe_id = crate::detect::canonical_agent_id(agent);
        // An agent program explicitly set in the config takes priority over
        // detect: the agent may point at a custom binary
        // (agents.<id>.program). A built-in agent's program is checked like
        // a detected one.
        if let Some(program) = global.agent_program(agent) {
            match crate::detect::probe_program(agent, &program) {
                Some(info) if info.installed => installed_rows(r, agent, &info, global),
                _ if program_in_path(&program) => r.push(
                    CheckStatus::Ok,
                    format!("agent {agent}"),
                    format!("program `{program}` found"),
                ),
                _ => r.push(
                    CheckStatus::Warn,
                    format!("agent {agent}"),
                    format!("program `{program}` not found in PATH"),
                ),
            }
        } else if let Some(info) = detected.iter().find(|a| a.agent == probe_id) {
            if info.installed {
                installed_rows(r, agent, info, global);
            } else {
                r.push(
                    CheckStatus::Warn,
                    format!("agent {agent}"),
                    "not installed (free detect)".to_string(),
                );
            }
        } else if program_in_path(agent) {
            r.push(
                CheckStatus::Ok,
                format!("agent {agent}"),
                format!("program `{agent}` found"),
            );
        } else {
            r.push(
                CheckStatus::Warn,
                format!("agent {agent}"),
                format!("program `{agent}` not found in PATH"),
            );
        }
    }
}

/// `apb doctor --agents`: every installed built-in agent, whether or not a
/// profile names it, with the same rows as the environment doctor.
pub fn diagnose_agents() -> DoctorReport {
    let mut r = DoctorReport::default();
    let global = match GlobalConfig::load() {
        Ok(g) => g,
        Err(e) => {
            r.push(CheckStatus::Fail, "global config", e);
            GlobalConfig::default()
        }
    };
    let detected = crate::agent_catalog::agents(false);
    let installed: Vec<&AgentInfo> = detected
        .iter()
        .filter(|i| i.installed && crate::agents::lookup(&i.agent).is_some())
        .collect();
    if installed.is_empty() {
        r.push(
            CheckStatus::Warn,
            "agents",
            "no built-in agent is installed (free detect)",
        );
    }
    for info in installed {
        installed_rows(&mut r, &info.agent, info, &global);
    }
    r
}

/// The rows of an installed agent: installed with its version and the
/// authority of its model list, its login warning, and for a built-in
/// agent the local check and the tool exposure note.
fn installed_rows(r: &mut DoctorReport, agent: &str, info: &AgentInfo, global: &GlobalConfig) {
    let ver = info.version.as_deref().unwrap_or("unknown version");
    // The models list authority is the boundary of what detect can confirm
    // about launchability (spec 8.4).
    let authority = info
        .models
        .as_ref()
        .map(|m| format!(", models authority: {:?}", m.authority))
        .unwrap_or_default();
    r.push(
        CheckStatus::Ok,
        format!("agent {agent}"),
        format!("installed ({ver}){authority}"),
    );
    let Some(module) = crate::agents::lookup(agent) else {
        return;
    };
    // An agent that cannot run headless without a login of its own (zcode's
    // standalone CLI) says so.
    if let Some(warning) = module.setup().login_warning(info.auth.as_ref()) {
        r.push(
            CheckStatus::Warn,
            format!("agent {agent} login"),
            warning.to_string(),
        );
    }
    local_check_rows(r, agent, module, info, global);
    if let Some(line) = exposure_note(agent, module, global) {
        r.push(CheckStatus::Ok, format!("agent {agent} exposure"), line);
    }
}

/// The local check of an installed built-in agent, one row per line.
fn local_check_rows(
    r: &mut DoctorReport,
    agent: &str,
    module: &dyn Agent,
    info: &AgentInfo,
    global: &GlobalConfig,
) {
    let custom_form = global.has_custom_form(agent);
    // The help is read only when the built-in form is the one in use.
    let help = match (
        &info.canonical_path,
        custom_form || module.forms().is_none(),
    ) {
        (Some(path), false) => {
            let args: Vec<String> = module
                .descriptor()
                .help_args
                .iter()
                .map(|a| a.to_string())
                .collect();
            Some(crate::detect::probe_output(path, &args))
        }
        _ => None,
    };
    let input = CheckInput {
        version: info.version.as_deref(),
        help: help.as_ref().map(|h| h.as_deref().map_err(String::as_str)),
        custom_form,
    };
    for line in check(module, &input) {
        let status = match line.level {
            Level::Warn => CheckStatus::Warn,
            Level::Ok | Level::Info => CheckStatus::Ok,
        };
        r.push(status, format!("agent {agent} form"), line.detail);
    }
}

/// For an agent whose streaming form reports the tools it sees: where that
/// check runs. Only the stream transport prints the tool list, so on the
/// headless default nothing is checked. The transport is read under
/// `agent`, as a run reads it.
fn exposure_note(agent: &str, module: &dyn Agent, global: &GlobalConfig) -> Option<String> {
    let d = module.descriptor();
    if !d.capabilities.tool_exposure {
        return None;
    }
    Some(if global.agent_transport(agent) == Transport::Acp {
        format!(
            "the tools and MCP servers each attempt sees are checked (stream transport); a server that is not `{CONNECTED}` or a missing tool journals tool_exposure_mismatch"
        )
    } else {
        format!(
            "tool exposure is checked only on the stream transport (agents.{agent}.transport: acp); the headless form prints no tool list"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> GlobalConfig {
        yaml_serde::from_str(yaml).unwrap()
    }

    /// The config is read under the name a profile uses, as a run reads it:
    /// an entry under the alias does not change the canonical id, and the
    /// other way round.
    #[test]
    fn the_config_is_read_under_the_agent_name_as_given() {
        let alias_form = config(
            "agents:\n  claude-code:\n    transport: acp\n    invocation:\n      argv: [\"{prompt}\"]\n",
        );
        assert!(alias_form.has_custom_form("claude-code"));
        assert!(!alias_form.has_custom_form("claude"));
        let module = crate::agents::lookup("claude").unwrap();
        let on_alias = exposure_note("claude-code", module, &alias_form).unwrap();
        assert!(on_alias.contains("are checked"), "{on_alias}");
        let on_id = exposure_note("claude", module, &alias_form).unwrap();
        assert!(on_id.contains("agents.claude.transport: acp"), "{on_id}");
    }
}
