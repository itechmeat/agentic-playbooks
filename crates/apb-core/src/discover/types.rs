//! The types of a discovery: what it is asked to read, and the result it
//! returns and stores.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::DiscoverSettings;

/// The agent whose history a root holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryAgent {
    ClaudeCode,
    Codex,
}

impl HistoryAgent {
    pub fn as_str(self) -> &'static str {
        match self {
            HistoryAgent::ClaudeCode => "claude_code",
            HistoryAgent::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude_code" | "claude-code" => Some(HistoryAgent::ClaudeCode),
            "codex" => Some(HistoryAgent::Codex),
            _ => None,
        }
    }

    /// The agent's own history directory on this machine.
    pub fn default_dir(self) -> Option<PathBuf> {
        match self {
            HistoryAgent::ClaudeCode => crate::agent_actions::claude::projects_dir(),
            HistoryAgent::Codex => crate::agent_actions::codex::sessions_dir(),
        }
    }
}

/// One named history root. Its path is never serialised: every surface
/// names a root by its name only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryRoot {
    pub name: String,
    pub agent: HistoryAgent,
    #[serde(skip)]
    pub path: PathBuf,
}

impl HistoryRoot {
    /// The roots `settings` names, or when it names none, each agent's own
    /// directory (named after the agent) where one can be told.
    pub fn configured(settings: &DiscoverSettings) -> Vec<HistoryRoot> {
        match &settings.roots {
            Some(roots) => roots
                .iter()
                .filter_map(|r| {
                    Some(HistoryRoot {
                        name: r.name.clone(),
                        agent: HistoryAgent::parse(&r.agent)?,
                        path: expand_home(&r.path),
                    })
                })
                .collect(),
            None => [HistoryAgent::ClaudeCode, HistoryAgent::Codex]
                .into_iter()
                .filter_map(|agent| {
                    Some(HistoryRoot {
                        name: agent.as_str().to_string(),
                        agent,
                        path: agent.default_dir()?,
                    })
                })
                .collect(),
        }
    }
}

/// `path` with a leading `~/` as the home directory.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if !home.is_empty() => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// One project whose sessions are read. Its root is never serialised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRef {
    /// Empty for a directory that is not a registered project.
    pub workspace_id: String,
    pub name: String,
    #[serde(skip)]
    pub root: PathBuf,
}

impl ProjectRef {
    /// The project as a result names it: by workspace id and name.
    pub fn seen(&self) -> ProjectSeen {
        ProjectSeen {
            workspace_id: self.workspace_id.clone(),
            name: self.name.clone(),
        }
    }
}

/// A project as a result names it: its workspace id (empty for a directory
/// that is not registered), which every surface resolves it by, and its
/// name, which a person reads. Names need not be unique; ids are.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSeen {
    pub workspace_id: String,
    pub name: String,
}

/// The names of `projects`, as a person reads them: `app, site`.
pub fn project_names(projects: &[ProjectSeen]) -> String {
    projects
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// How much of the history is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadLimits {
    /// Session files read in all.
    pub max_files: u32,
    /// The prefix read of one file; a longer file is marked cut.
    pub max_file_bytes: u64,
    /// Bytes read in all.
    pub max_total_bytes: u64,
    /// A longer line is skipped.
    pub max_line_bytes: u64,
    pub max_steps_per_session: u32,
    /// Steps kept in all: what bounds the miner's memory.
    pub max_total_steps: u32,
    /// Sessions older than this are not read, 1 to 365.
    pub since_days: u32,
}

impl Default for ReadLimits {
    fn default() -> Self {
        ReadLimits {
            max_files: 2000,
            max_file_bytes: 32 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_line_bytes: 4 * 1024 * 1024,
            max_steps_per_session: 2000,
            max_total_steps: 100_000,
            since_days: DEFAULT_SINCE_DAYS,
        }
    }
}

/// How far back sessions are read when nothing says otherwise.
pub const DEFAULT_SINCE_DAYS: u32 = 90;

/// What makes a step sequence a repeat (see `mine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MineParams {
    pub min_n: usize,
    pub max_n: usize,
    /// Distinct sessions it appears in.
    pub min_sessions: usize,
    /// Distinct days those sessions were written on.
    pub min_days: usize,
    /// Suggestions kept.
    pub top: usize,
}

impl Default for MineParams {
    fn default() -> Self {
        MineParams {
            min_n: 3,
            max_n: 8,
            min_sessions: 3,
            min_days: 2,
            top: 20,
        }
    }
}

/// One discovery.
#[derive(Debug, Clone, Default)]
pub struct DiscoverRequest {
    pub roots: Vec<HistoryRoot>,
    pub projects: Vec<ProjectRef>,
    /// The roots of every registered project, selected or not: a session is
    /// its deepest registered project's, so a project registered inside a
    /// selected one keeps its sessions (and its secrets) to itself.
    pub registered: Vec<PathBuf>,
    pub limits: ReadLimits,
    pub mine: MineParams,
}

/// Where a suggestion was seen: a root by its name, a file as
/// `<project name>/<file name>`, the project's workspace id (empty for a
/// directory that is not registered) and a 1-based, inclusive line range.
/// Never an absolute path.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub root: String,
    pub file: String,
    pub workspace_id: String,
    pub lines: [u32; 2],
}

/// A request a person typed right before a repeat began: scrubbed, at most
/// `draft::EXAMPLE_MAX_CHARS` characters.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    pub text: String,
    pub source: SourceSpan,
}

/// How widely a repeat was seen.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Support {
    pub sessions: u32,
    pub days: u32,
    /// The projects, in the order of the request.
    pub projects: Vec<ProjectSeen>,
}

/// What became of a suggestion.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SuggestionStatus {
    New,
    Saved {
        playbook: String,
        version: String,
        scope: String,
    },
    Dismissed,
}

/// One repeated step sequence, offered as a playbook draft.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// `seq-` and 10 hex characters of the steps.
    pub slug: String,
    pub steps: Vec<String>,
    pub support: Support,
    pub score: u32,
    pub title: String,
    /// The synopsis `playbook_capture` takes.
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub synopsis: serde_json::Value,
    pub yaml: String,
    pub effects: Vec<String>,
    pub examples: Vec<Example>,
    pub sources: Vec<SourceSpan>,
    /// Things to settle before saving (`needs_profile`).
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub status: SuggestionStatus,
}

/// What was read of one root.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootReport {
    pub name: String,
    pub agent: String,
    pub files_seen: u32,
    pub files_read: u32,
    pub bytes_read: u64,
    /// Files left unread, by why: `symlink`, `not_regular`, `too_old`,
    /// `other_project`, `file_cap`, `byte_cap`, `step_cap`, `unreadable`,
    /// and lines skipped as `line_too_long`.
    pub skipped: BTreeMap<String, u32>,
    /// Files read by their prefix only.
    pub cut_files: u32,
}

impl RootReport {
    pub(crate) fn skip(&mut self, why: &str) {
        *self.skipped.entry(why.to_string()).or_default() += 1;
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadReport {
    pub roots: Vec<RootReport>,
    /// Sessions with at least one step.
    pub sessions: u32,
}

/// What the request asked for, by name.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEcho {
    pub roots: Vec<String>,
    pub projects: Vec<ProjectSeen>,
    pub since_days: u32,
}

/// A suggestion left out: a person declined it (`kind` `soft` or
/// `hard`), or it was saved as a playbook before (`kind` `saved`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuppressedSlug {
    pub slug: String,
    pub scope: String,
    pub kind: String,
}

/// The schema of a stored result.
pub const RESULT_SCHEMA: u32 = 1;
/// The readers a result was made with.
pub const READER: &str = "claude_code/1+codex/1";
/// The miner a result was made with.
pub const MINER: &str = "ngram/1";

/// One discovery's result, as returned and stored.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoverResult {
    pub schema: u32,
    /// `disc-` and 10 hex characters.
    pub id: String,
    /// Unix milliseconds.
    pub created_at: u64,
    pub reader: String,
    pub miner: String,
    pub request: RequestEcho,
    pub read: ReadReport,
    pub suggestions: Vec<Suggestion>,
    pub suppressed: Vec<SuppressedSlug>,
    /// Drafts dropped because their text still looked like it held a
    /// secret.
    #[serde(default)]
    pub dropped_drafts: u32,
    /// The installation's MAC over the rest (see `store`).
    #[cfg_attr(feature = "ts", ts(optional))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// Why a discovery did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoverError {
    /// A root is a symbolic link: it is never followed.
    RootIsSymlink(String),
    /// A root is not a directory there.
    RootMissing(String),
    /// Another discovery is running.
    Busy,
    /// The machine's discovery lock cannot be taken (not because another
    /// discovery holds it): the reason, never a path.
    Lock(String),
    Invalid(String),
}

impl DiscoverError {
    /// A stable machine code for the surfaces.
    pub fn code(&self) -> &'static str {
        match self {
            DiscoverError::RootIsSymlink(_) => "root_is_symlink",
            DiscoverError::RootMissing(_) => "root_missing",
            DiscoverError::Busy => "busy",
            DiscoverError::Lock(_) => "lock_unavailable",
            DiscoverError::Invalid(_) => "invalid",
        }
    }
}

impl std::fmt::Display for DiscoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoverError::RootIsSymlink(n) => {
                write!(
                    f,
                    "history root `{n}` is a symbolic link and is not followed"
                )
            }
            DiscoverError::RootMissing(n) => write!(f, "history root `{n}` is not a directory"),
            DiscoverError::Busy => write!(f, "another discovery is running"),
            DiscoverError::Lock(why) => write!(f, "the discovery lock cannot be taken: {why}"),
            DiscoverError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for DiscoverError {}
