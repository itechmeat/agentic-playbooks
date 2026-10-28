//! The records a transcript view is made of, as every parser produces
//! them.

/// A tool the agent called, as facts: never its input text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolUse {
    pub name: String,
    /// The digest of the tool's input, to tell repeated identical calls.
    pub input_digest: String,
    /// The files the call names (`file_path` and the like).
    pub paths: Vec<String>,
    /// The call writes the files it names.
    pub writes: bool,
    /// The call runs a command from the destructive list
    /// ([`apb_core::agent_actions::command::is_destructive`]).
    pub destructive: bool,
    /// The skill the call loads (the `Skill` tool, or a read of a
    /// `SKILL.md`).
    pub skill: Option<String>,
}

/// One record of a view: a line of a stream, or one message of a session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    /// Zero-based, the `#<record>` of a citation.
    pub index: u64,
    /// A message of a sub-agent (claude's `isSidechain`).
    pub sidechain: bool,
    /// What a reader is shown (before redaction).
    pub text: String,
    pub tools: Vec<ToolUse>,
    /// Tool results that reported an error.
    pub tool_errors: u32,
}

impl From<&apb_core::agent_actions::ToolCall> for ToolUse {
    /// A logged call as facts: its own name, the digest of its input, the
    /// files it names, and what its actions say about writing, loading a
    /// skill (by name or a file read, never a shell `cat`) and destroying.
    fn from(call: &apb_core::agent_actions::ToolCall) -> Self {
        ToolUse {
            name: call.name.clone(),
            input_digest: apb_core::content::sha256_hex(call.input.to_string().as_bytes()),
            paths: call.paths.clone(),
            writes: call.writes(),
            destructive: call.destructive,
            skill: call.skill().map(str::to_string),
        }
    }
}
