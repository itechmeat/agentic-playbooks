//! The shapes of a start action: what a start refused before any run
//! existed needs. Kept apart from the rules that build them
//! ([`super::start`]), so a report that carries start actions (the fit
//! report) does not depend on those rules.

use serde::Serialize;

use super::types::{ActionArg, Principal};

/// What a refused start needs. A closed set.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartActionCode {
    ConfirmIrreversibleStart,
    FixRequirement,
    AcknowledgeUntrusted,
    ConfirmLaunch,
    LaunchByPerson,
}

impl StartActionCode {
    /// Every code, in declaration order.
    pub const ALL: [StartActionCode; 5] = [
        StartActionCode::ConfirmIrreversibleStart,
        StartActionCode::FixRequirement,
        StartActionCode::AcknowledgeUntrusted,
        StartActionCode::ConfirmLaunch,
        StartActionCode::LaunchByPerson,
    ];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            StartActionCode::ConfirmIrreversibleStart => "confirm_irreversible_start",
            StartActionCode::FixRequirement => "fix_requirement",
            StartActionCode::AcknowledgeUntrusted => "acknowledge_untrusted",
            StartActionCode::ConfirmLaunch => "confirm_launch",
            StartActionCode::LaunchByPerson => "launch_by_person",
        }
    }
}

/// How a requirement is met. Never carries a value or a token.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "remedy", rename_all = "snake_case")]
pub enum Remedy {
    /// Approve this exact content by its digest (`apb trust approve`);
    /// `kind` is the kind of content (`playbook`, `profile_bundle`, ...).
    ApproveDigest { kind: String, digest: String },
    /// Pass a value for this param at start.
    ProvideParam { name: String },
    /// Install this agent's CLI.
    InstallAgent { agent: String },
    /// Set this environment variable.
    SetEnv { name: String },
    /// `apb doctor` says what this machine lacks.
    RunDoctor,
    /// Something a person changes by hand.
    Manual { detail: String },
}

/// One thing a refused start needs.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartAction {
    pub code: StartActionCode,
    pub playbook: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub version: Option<String>,
    pub args: Vec<ActionArg>,
    pub principals: Vec<Principal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub remedy: Option<Remedy>,
    /// One English line.
    pub reason: String,
}
