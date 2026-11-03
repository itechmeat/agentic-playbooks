//! How an agent CLI that is also an MCP host keeps its MCP servers, as pure
//! functions over the text of its config: where the config is, whether a
//! server is registered there, and either the host CLI's own register and
//! remove commands or an edit that inserts exactly one entry and takes
//! exactly those bytes back. `apb setup` does the reading, writing,
//! backups and process starts the same way for every host.

use std::path::PathBuf;

use serde::Serialize;

/// The server to register: its name and the command line that starts it.
#[derive(Debug, Clone, Copy)]
pub struct McpServer<'a> {
    pub name: &'a str,
    /// The program and its arguments, e.g. `["apb", "mcp"]`.
    pub command: &'a [&'a str],
}

/// Whether a server is registered in a host's config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRegistration {
    /// No entry of that name.
    Absent,
    /// An entry with exactly the server's command.
    Same,
    /// An entry of that name with another command: never touched.
    Different,
    /// The config could not be read or parsed: nothing is written.
    Unreadable,
}

/// How a host registers a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpMethod {
    /// The host CLI registers and removes it; its config is only read.
    Cli {
        add: Vec<String>,
        remove: Vec<String>,
    },
    /// apb edits the config file.
    EditFile,
}

/// An MCP host's config, as one agent module knows it.
pub trait McpHost: Sync {
    /// The file the host keeps its user-level MCP servers in; `None`
    /// without a home directory.
    fn config_path(&self) -> Option<PathBuf>;

    /// A path whose existence shows the host is set up on this machine even
    /// when its CLI is not on PATH.
    fn marker(&self) -> Option<PathBuf> {
        self.config_path()
    }

    /// Whether `server` is registered in the config `text`.
    fn registration(&self, text: &str, server: &McpServer) -> McpRegistration;

    /// How the server is registered.
    fn method(&self, server: &McpServer) -> McpMethod;

    /// For [`McpMethod::EditFile`]: the new text and the bytes inserted, for
    /// the config `text` (`None`: the file does not exist). `None` when the
    /// entry cannot be inserted without changing anything else.
    fn insert(&self, _text: Option<&str>, _server: &McpServer) -> Option<(String, String)> {
        None
    }

    /// For [`McpMethod::EditFile`]: `text` without the bytes [`Self::insert`]
    /// put there (`inserted`), `None` when they were changed. `created`:
    /// the insert created the file.
    fn remove(
        &self,
        _text: &str,
        _inserted: &str,
        _created: bool,
        _server: &McpServer,
    ) -> Option<String> {
        None
    }

    /// A note when the config is in a form apb does not edit.
    fn unsupported(&self) -> Option<String> {
        None
    }

    /// For [`McpMethod::EditFile`]: the exact text a person adds to the
    /// config by hand when [`Self::insert`] cannot place the entry safely.
    fn snippet(&self, _server: &McpServer) -> Option<String> {
        None
    }
}

/// The text after `text` once `needle` is cut out at its first occurrence.
pub(crate) fn cut_once(text: &str, needle: &str) -> Option<String> {
    let at = text.find(needle)?;
    let mut out = text.to_string();
    out.replace_range(at..at + needle.len(), "");
    Some(out)
}

/// What appending to a file holding `text` puts first: a blank line.
pub fn blank_line_before(text: &str) -> &'static str {
    if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    }
}
