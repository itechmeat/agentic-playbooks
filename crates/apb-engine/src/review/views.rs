//! The view types a review request journals and every surface shows
//! (people-in-the-loop D2/D3): the scope counts, the rendered `show:`
//! entries and the typed fields. A leaf: serde and the core field kind
//! only, so the journal (`event.rs`) can name them without a cycle.

use serde::{Deserialize, Serialize};

/// What a review decision covers, counted since the gate's previous
/// decision (see [`super::scope::window_start`]). Counts only, never content.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewScope {
    /// The journal seq the window starts after (0: the run's start).
    #[serde(default)]
    pub since_seq: u64,
    /// Commits the run's nodes made in the window (`artifacts_committed`).
    #[serde(default)]
    pub commits: u32,
    /// Files those commits changed; absent when git could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub files: Option<u32>,
    /// Lines added and removed by those commits; absent when git could not
    /// tell. A binary file counts as a file with no lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub lines_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub lines_removed: Option<u32>,
    /// List items settled `passed` or `needs_look` (carried ones left out).
    #[serde(default)]
    pub items_settled: u32,
    /// Connector calls that wrote (or may have written), apb's own
    /// notifications left out.
    #[serde(default)]
    pub writes: u32,
    /// Of those, writes whose effect nobody can tell.
    #[serde(default)]
    pub writes_unknown: u32,
    /// Send intents recorded, apb's own notifications left out.
    #[serde(default)]
    pub sends: u32,
    /// Of those, sends a person still has to resolve.
    #[serde(default)]
    pub sends_open: u32,
    /// Node executions that took a result snapshot (`results:`).
    #[serde(default)]
    pub results: u32,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShownEntry {
    pub label: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untrusted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clipped: bool,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldView {
    pub name: String,
    pub kind: apb_core::review_form::FieldKind,
    /// The options a value is required for (every option for `required: true`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_problem: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untrusted: bool,
}
