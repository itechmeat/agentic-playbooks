//! Who paused a run and how (issue #200). A leaf module: the journal
//! (`event.rs`) names these types, so they depend on nothing else in the
//! crate.

use serde::{Deserialize, Serialize};

/// Who asked for a pause.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseBy {
    /// A person, through `apb pause`, the `run_pause` MCP tool or the dashboard.
    Operator,
    /// The run's supervisor, through `supervisor_run_pause`.
    Supervisor,
    /// The engine itself (an interrupted node, a quota error, a driverless run).
    Engine,
}

impl PauseBy {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseBy::Operator => "operator",
            PauseBy::Supervisor => "supervisor",
            PauseBy::Engine => "engine",
        }
    }

    /// The inverse of [`PauseBy::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "operator" => Some(PauseBy::Operator),
            "supervisor" => Some(PauseBy::Supervisor),
            "engine" => Some(PauseBy::Engine),
            _ => None,
        }
    }
}

/// How a pause treats the nodes already running.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseMode {
    /// Admit no new node; let the running ones finish (the default).
    #[default]
    Drain,
    /// Interrupt the running nodes; they run again on resume.
    Now,
}

impl PauseMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseMode::Drain => "drain",
            PauseMode::Now => "now",
        }
    }
}

/// Why the engine paused a run, read back from the pause's reason: never
/// journaled, derived by [`crate::pause::cause_of`].
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseCause {
    /// A provider quota or rate limit (see `pause::quota`).
    Quota,
    /// The run's budget (see `pause::budget`).
    Budget,
}

impl PauseCause {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseCause::Quota => "quota",
            PauseCause::Budget => "budget",
        }
    }
}
