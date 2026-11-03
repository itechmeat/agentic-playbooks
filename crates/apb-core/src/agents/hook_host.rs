//! How an agent CLI that runs a command when its turn ends (a Stop hook)
//! takes apb's end-of-turn check, as pure functions: where its settings
//! are, whether the hook is registered there, the edit that inserts exactly
//! one hook entry and takes exactly those bytes back, what the host hands
//! the hook on stdin and what the hook answers to keep the turn going.
//! `apb setup` does the reading, writing and backups the same way for
//! every host; `apb hook stop` runs the check
//! ([`crate::save_nudge`]).

use std::path::PathBuf;

use serde::Serialize;

use crate::discover::HistoryAgent;

/// Whether apb's hook command is registered in a host's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookRegistration {
    /// No hook entry runs apb's command.
    Absent,
    /// The entry apb writes is there.
    Same,
    /// An entry runs apb's hook in another form (another path, other
    /// options): never touched.
    Different,
    /// The settings could not be read, parsed, or have a shape apb does not
    /// edit: nothing is written.
    Unreadable,
}

/// What a host hands its Stop hook, as apb reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StopEvent {
    /// The host's session id.
    pub session_id: String,
    /// The session's history file.
    pub transcript: PathBuf,
    /// The session's working directory.
    pub cwd: Option<PathBuf>,
    /// The host is already continuing because a Stop hook blocked: a hook
    /// that blocks again would loop.
    pub hook_active: bool,
}

/// A host's Stop hook, as one agent module knows it.
pub trait StopHookHost: Sync {
    /// The user settings file the hook is registered in; `None` without a
    /// home directory.
    fn settings_path(&self) -> Option<PathBuf>;

    /// Whether `command` is registered as a Stop hook in `text`.
    fn registration(&self, text: &str, command: &str) -> HookRegistration;

    /// The new text and the bytes inserted for the settings `text` (`None`:
    /// the file does not exist). `None` when the entry cannot be inserted
    /// without changing anything else.
    fn insert(&self, text: Option<&str>, command: &str) -> Option<(String, String)>;

    /// `text` without the bytes [`Self::insert`] put there (`inserted`),
    /// `None` when they were changed. `created`: the insert created the
    /// file.
    fn remove(&self, text: &str, inserted: &str, created: bool, command: &str) -> Option<String>;

    /// The event the host wrote to the hook's stdin; `None` when it does
    /// not parse.
    fn read_event(&self, stdin: &[u8]) -> Option<StopEvent>;

    /// What the hook prints to keep the turn going with `reason` shown to
    /// the agent.
    fn block(&self, reason: &str) -> String;

    /// The format of the host's session history.
    fn history(&self) -> HistoryAgent;
}
