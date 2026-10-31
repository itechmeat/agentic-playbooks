//! Plain data about agents that detection, the catalog and the doctor share.

use serde::{Deserialize, Serialize};

/// Agent category (spec 7.1). Vendor is tied to its own provider (claude,
/// codex), Aggregator works with several (opencode, pi).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCategory {
    Vendor,
    Aggregator,
}

/// How authoritative the model list is (spec 8.4). `Full` - the agent itself
/// returned the exact list; `Partial` - partially extracted from a config;
/// `Display` - a list for display only, does not guarantee runnability;
/// `Static` - a list hardcoded into the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Full,
    Partial,
    Display,
    Static,
}

/// Type of detected authentication (no secrets). Best-effort hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    Oauth,
    ApiKey,
    None,
}

/// Inventory of an agent's models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsInventory {
    pub items: Vec<String>,
    pub authority: Authority,
}

/// Hint about the agent's authentication (type only, no values).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthHint {
    pub kind: AuthKind,
}

/// The variables one agent CLI reads: `always` kept, and each `when` group
/// kept only while its trigger variable is switched on (a cloud provider's
/// credentials only when the CLI is switched to that provider; see
/// `crate::process_env::is_truthy`).
#[derive(Debug, Clone, Copy)]
pub struct AgentEnvSet {
    pub always: &'static [&'static str],
    pub when: &'static [(&'static str, &'static [&'static str])],
}

impl AgentEnvSet {
    /// Nothing beyond the base set.
    pub const NONE: AgentEnvSet = AgentEnvSet {
        always: &[],
        when: &[],
    };
}

/// ZCode's `--mode` for a run that grants autonomy. In a headless run every
/// approval request is denied, so the mode is the whole permission set:
/// `yolo` allows everything (files, shell, network), `edit` allows file edits
/// and nothing that needs an approval (no shell commands). A run that grants
/// no autonomy always runs zcode in `build`, which refuses writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZcodeMode {
    #[default]
    Yolo,
    Edit,
}

impl ZcodeMode {
    /// The value ZCode's `--mode` flag takes.
    pub fn as_str(self) -> &'static str {
        match self {
            ZcodeMode::Yolo => "yolo",
            ZcodeMode::Edit => "edit",
        }
    }

    /// Parses `yolo` / `edit` (the surfaces' spelling).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "yolo" => Ok(ZcodeMode::Yolo),
            "edit" => Ok(ZcodeMode::Edit),
            other => Err(format!("zcode_mode `{other}`: expected `edit` or `yolo`")),
        }
    }
}
