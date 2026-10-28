//! The [`Agent`] trait every built-in agent module implements, and the data
//! that describes one agent.

use super::forms::Forms;
use super::output::OutputFormat;
use super::policy::{ModelPolicy, OpenModels};
use super::setup::{HostSetup, NoSetup};
use super::types::AgentCategory;

/// One built-in agent CLI: what it is called and where it is found, and the
/// facts apb relies on when it launches it.
pub trait Agent: Send + Sync + 'static {
    /// The agent's identity and capabilities.
    fn descriptor(&self) -> &'static Descriptor;

    /// The built-in launch and resume forms; `None` for an agent apb cannot
    /// launch without an `agents.<id>.invocation` in the config.
    fn forms(&self) -> Option<Forms> {
        None
    }

    /// How the agent's output reads.
    fn output(&self) -> &'static dyn OutputFormat;

    /// What the agent needs from the host, and what the host tells about it.
    fn setup(&self) -> &'static dyn HostSetup {
        &NoSetup
    }

    /// Which models the agent takes.
    fn models(&self) -> &'static dyn ModelPolicy {
        &OpenModels
    }

    /// Whether apb can continue the agent's sessions across attempts and
    /// nodes: it learns the session id of every attempt and the agent has a
    /// resume form. A warm session handoff (`continue_session`) needs both.
    fn continues_sessions(&self) -> bool {
        self.output().promises().session && self.forms().is_some_and(|f| f.resume.is_some())
    }
}

/// Who an agent is and what it can do, as data.
#[derive(Debug, Clone, Copy)]
pub struct Descriptor {
    /// The agent id profiles and the config name.
    pub id: &'static str,
    /// Legacy ids that stand for this agent (`claude-code` is `claude`).
    pub aliases: &'static [&'static str],
    /// The binaries the agent is found as on PATH, the launched one first.
    pub bins: &'static [&'static str],
    /// The arguments that print the installed version.
    pub version_args: &'static [&'static str],
    /// The arguments that print the help of the built-in form's command,
    /// whose flags a local check can look for.
    pub help_args: &'static [&'static str],
    /// Known install locations relative to `$HOME`, tried after PATH, for an
    /// agent whose installer does not put it on PATH.
    pub home_paths: &'static [&'static str],
    pub category: AgentCategory,
    pub capabilities: Capabilities,
    /// The CLI version the built-in forms and output readers were last
    /// checked against; `None` when they were never checked against a
    /// recorded output.
    pub verified: Option<Verified>,
}

/// What apb can do with an agent beyond launching it. Every flag is off for
/// an agent that does not set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// apb can hand the agent its own minimal settings for
    /// `environment: minimal` (claude's `--settings`).
    pub hermetic_settings: bool,
    /// The agent takes apb's ask-server as an MCP server, so a live node can
    /// ask a question mid-attempt.
    pub live_ask_mcp: bool,
    /// The agent's MCP tool calls may block for apb's longest wait, so a
    /// spawned supervisor can wait that long per call.
    pub long_tool_wait: bool,
    /// What the grader reads of an attempt.
    pub transcript: Transcript,
    /// The streaming form reports the tools and MCP servers the agent sees.
    pub tool_exposure: bool,
    /// apb can record the agent's sessions in the agent's own desktop history
    /// (`agents.<id>.ui_sync`).
    pub desktop_history: bool,
}

impl Capabilities {
    /// No capability at all.
    pub const NONE: Capabilities = Capabilities {
        hermetic_settings: false,
        live_ask_mcp: false,
        long_tool_wait: false,
        transcript: Transcript::Plain,
        tool_exposure: false,
        desktop_history: false,
    };
}

/// How the grader reads an attempt of the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transcript {
    /// Its stdout and stderr, line by line.
    Plain,
    /// Its stdout as a stream of JSON events with tool facts (codex's
    /// `exec --json`).
    JsonEvents,
    /// Its own session record, copied next to the output, plus stdout and
    /// stderr (claude's session JSONL).
    SessionRecord,
}

/// The CLI version a module's built-in forms and readers were checked
/// against, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    pub version: &'static str,
    /// The date of the check, `YYYY-MM-DD`.
    pub on: &'static str,
}
