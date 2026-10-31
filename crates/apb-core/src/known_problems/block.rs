//! The playbook's `known_problems:` block: whether and how known problems
//! are shown to its agents. Kept apart from the rest of
//! [`super`] so the playbook schema depends on nothing else of it.

use serde::{Deserialize, Serialize};

/// The fewest distinct runs that make a problem significant, when the block
/// names none.
pub const DEFAULT_MIN_RUNS: u32 = 2;
/// The largest `min_runs`.
pub const MAX_MIN_RUNS: u32 = 20;
/// The most problems one attempt prompt shows, when the block names none.
pub const DEFAULT_MAX_SHOWN: u32 = 5;
/// The largest `max_shown`.
pub const MAX_MAX_SHOWN: u32 = 10;

/// The playbook's `known_problems:` block. Additive: an older apb ignores
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownProblemsBlock {
    #[serde(default)]
    pub show: ShowMode,
    #[serde(default = "default_min_runs")]
    pub min_runs: u32,
    #[serde(default = "default_max_shown")]
    pub max_shown: u32,
}

fn default_min_runs() -> u32 {
    DEFAULT_MIN_RUNS
}

fn default_max_shown() -> u32 {
    DEFAULT_MAX_SHOWN
}

impl Default for KnownProblemsBlock {
    fn default() -> Self {
        KnownProblemsBlock {
            show: ShowMode::default(),
            min_runs: DEFAULT_MIN_RUNS,
            max_shown: DEFAULT_MAX_SHOWN,
        }
    }
}

impl KnownProblemsBlock {
    /// Whether this block shows anything: `show` is a known mode other than
    /// `off`.
    pub fn shows(&self) -> bool {
        matches!(self.show, ShowMode::Significant | ShowMode::Confirmed)
    }
}

/// What the block shows: problems that are significant (seen in
/// `min_runs` runs, or confirmed by a person), confirmed ones only, or
/// nothing. Read from any string, so an unknown value is a validator error
/// (V108) rather than a definition that does not load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ShowMode {
    #[default]
    Significant,
    Confirmed,
    Off,
    Unknown(String),
}

impl From<String> for ShowMode {
    fn from(s: String) -> Self {
        match s.as_str() {
            "significant" => ShowMode::Significant,
            "confirmed" => ShowMode::Confirmed,
            "off" => ShowMode::Off,
            _ => ShowMode::Unknown(s),
        }
    }
}

impl From<ShowMode> for String {
    fn from(m: ShowMode) -> Self {
        match m {
            ShowMode::Significant => "significant".into(),
            ShowMode::Confirmed => "confirmed".into(),
            ShowMode::Off => "off".into(),
            ShowMode::Unknown(s) => s,
        }
    }
}
