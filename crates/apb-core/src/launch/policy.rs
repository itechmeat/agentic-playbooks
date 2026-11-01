//! The three launch policies, on their own so the settings reader and the
//! playbook schema name them without depending on how they combine
//! ([`crate::launch::resolve`]).

use serde::{Deserialize, Serialize};

/// Whether an agent may launch a playbook. Ordered from the loosest to the
/// strictest, so "the stricter of two" is `max`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LaunchPolicy {
    /// An agent launches it like a person (the behaviour before launch
    /// policies existed).
    #[default]
    Allow,
    /// An agent launches it only with the nonce of a refusal, after the
    /// host asked the person.
    Ask,
    /// Only a person launches it: the dashboard, or a terminal.
    Deny,
}

impl LaunchPolicy {
    /// Every policy, from the loosest.
    pub const ALL: [LaunchPolicy; 3] = [LaunchPolicy::Allow, LaunchPolicy::Ask, LaunchPolicy::Deny];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchPolicy::Allow => "allow",
            LaunchPolicy::Ask => "ask",
            LaunchPolicy::Deny => "deny",
        }
    }

    /// The policy a wire name names.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

impl std::fmt::Display for LaunchPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
