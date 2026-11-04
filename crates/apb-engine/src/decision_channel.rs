//! The channel a gate's decision arrived through, and why a surface does
//! not offer an action to its caller.
//!
//! A gate marked `person_only` takes a decision only from a channel apb
//! counts as a person's: the CLI at an interactive terminal outside a run
//! step after a typed confirmation, or the dashboard with a signed-in
//! browser session. This does not prove that a person decided: a process
//! that controls a terminal, holds a session cookie or writes apb's channel
//! files is not stopped.

use serde::{Deserialize, Serialize};

/// Where a decision came from. Every decision surface states its own
/// channel; the engine stamps it on the channel entry (`reviews.jsonl`) and,
/// for a decision through a surface, on `review_decided.via`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionChannel {
    /// `apb review` at an interactive terminal outside a run step, after
    /// the typed confirmation.
    Terminal,
    /// `apb review` without a terminal (a script, CI).
    CliUnattended,
    /// The dashboard with a signed-in browser session (cookie and CSRF
    /// marker).
    DashboardSession,
    /// The dashboard without keys: anything on the machine reaches it.
    DashboardKeyless,
    /// The HTTP API with a bearer key.
    HttpBearer,
    /// The MCP server (`review_decide`).
    Mcp,
    /// Any surface called from inside a run step.
    RunStep,
    /// A scenario run's scripted decision.
    Scenario,
    /// The engine's enforced review recommendation.
    Engine,
    /// An unattended run's declared `decide` outcome.
    Unattended,
    /// A name a newer apb wrote; this binary never writes it and never
    /// counts it as a person's channel.
    Unknown,
}

impl DecisionChannel {
    /// Every channel this binary writes, in declaration order.
    pub const ALL: [DecisionChannel; 10] = [
        DecisionChannel::Terminal,
        DecisionChannel::CliUnattended,
        DecisionChannel::DashboardSession,
        DecisionChannel::DashboardKeyless,
        DecisionChannel::HttpBearer,
        DecisionChannel::Mcp,
        DecisionChannel::RunStep,
        DecisionChannel::Scenario,
        DecisionChannel::Engine,
        DecisionChannel::Unattended,
    ];

    /// The wire name (`dashboard_session`).
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionChannel::Terminal => "terminal",
            DecisionChannel::CliUnattended => "cli_unattended",
            DecisionChannel::DashboardSession => "dashboard_session",
            DecisionChannel::DashboardKeyless => "dashboard_keyless",
            DecisionChannel::HttpBearer => "http_bearer",
            DecisionChannel::Mcp => "mcp",
            DecisionChannel::RunStep => "run_step",
            DecisionChannel::Scenario => "scenario",
            DecisionChannel::Engine => "engine",
            DecisionChannel::Unattended => "unattended",
            DecisionChannel::Unknown => "unknown",
        }
    }

    /// Whether apb counts a decision through this channel as a person's.
    pub fn is_person_channel(self) -> bool {
        matches!(
            self,
            DecisionChannel::Terminal | DecisionChannel::DashboardSession
        )
    }

    /// Whether this is a decision surface rather than the engine deciding
    /// on its own. A surface's decision is journaled with `via`; the
    /// engine's keeps `decided_by` alone.
    pub fn is_surface(self) -> bool {
        !matches!(self, DecisionChannel::Engine | DecisionChannel::Unattended)
    }
}

impl DecisionChannel {
    /// Why a person-only gate refuses this channel. The engine's own
    /// channels, a scenario's outside a scenario run and a name this binary
    /// does not know are `not_this_caller`; a person's channel has none.
    pub fn person_only_refusal(self) -> Option<CallerRefusal> {
        Some(match self {
            DecisionChannel::Terminal | DecisionChannel::DashboardSession => return None,
            DecisionChannel::CliUnattended => CallerRefusal::NoTerminal,
            DecisionChannel::DashboardKeyless => CallerRefusal::KeylessDashboard,
            DecisionChannel::HttpBearer => CallerRefusal::BearerKey,
            DecisionChannel::Mcp => CallerRefusal::AgentChannel,
            DecisionChannel::RunStep => CallerRefusal::RunStep,
            DecisionChannel::Scenario
            | DecisionChannel::Engine
            | DecisionChannel::Unattended
            | DecisionChannel::Unknown => CallerRefusal::NotThisCaller,
        })
    }
}

impl<'de> Deserialize<'de> for DecisionChannel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(d)?;
        Ok(Self::ALL
            .into_iter()
            .find(|c| c.as_str() == wire)
            .unwrap_or(DecisionChannel::Unknown))
    }
}

/// Decodes an optional channel: absent or `null` is `None`, and any other
/// value this binary cannot read, in any shape, is
/// [`DecisionChannel::Unknown`], never a person's channel and never a
/// failed line.
pub(crate) fn lenient_channel<'de, D>(d: D) -> Result<Option<DecisionChannel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v).unwrap_or(DecisionChannel::Unknown)))
}

/// Why a surface does not offer an action to its caller
/// (`caller_refusal`). A closed list every facade renders.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerRefusal {
    /// A read-only MCP server takes no action that writes.
    ReadOnlyServer,
    /// Only the run's supervisor session takes this.
    SupervisorOnly,
    /// The run does not take this from its supervisor; relay it to the
    /// person.
    RelayToPerson,
    /// The run does not take this from this caller.
    NotThisCaller,
    /// A person-only gate refuses an agent's channel (MCP).
    AgentChannel,
    /// A person-only gate refuses a run step.
    RunStep,
    /// A person-only gate refuses the CLI without a terminal.
    NoTerminal,
    /// A person-only gate refuses a dashboard without keys.
    KeylessDashboard,
    /// A person-only gate refuses the HTTP API with a bearer key.
    BearerKey,
    /// A run step never decides a gate or answers a question of its own
    /// run, one of its ancestors or one of its descendants.
    RunStepOwnRun,
    /// The machine holds at least one passkey, so this decision needs a
    /// fresh WebAuthn assertion bound to the request and the chosen option,
    /// and this caller cannot give one.
    PasskeyRequired,
}

impl CallerRefusal {
    /// Every refusal, in declaration order.
    pub const ALL: [CallerRefusal; 11] = [
        CallerRefusal::ReadOnlyServer,
        CallerRefusal::SupervisorOnly,
        CallerRefusal::RelayToPerson,
        CallerRefusal::NotThisCaller,
        CallerRefusal::AgentChannel,
        CallerRefusal::RunStep,
        CallerRefusal::NoTerminal,
        CallerRefusal::KeylessDashboard,
        CallerRefusal::BearerKey,
        CallerRefusal::RunStepOwnRun,
        CallerRefusal::PasskeyRequired,
    ];

    /// The wire name (`agent_channel`).
    pub fn as_str(self) -> &'static str {
        match self {
            CallerRefusal::ReadOnlyServer => "read_only_server",
            CallerRefusal::SupervisorOnly => "supervisor_only",
            CallerRefusal::RelayToPerson => "relay_to_person",
            CallerRefusal::NotThisCaller => "not_this_caller",
            CallerRefusal::AgentChannel => "agent_channel",
            CallerRefusal::RunStep => "run_step",
            CallerRefusal::NoTerminal => "no_terminal",
            CallerRefusal::KeylessDashboard => "keyless_dashboard",
            CallerRefusal::BearerKey => "bearer_key",
            CallerRefusal::RunStepOwnRun => "run_step_own_run",
            CallerRefusal::PasskeyRequired => "passkey_required",
        }
    }
}
