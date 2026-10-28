//! The types every decoder of an agent log produces: an [`Action`] and the
//! [`ToolCall`] that carries it, and the helpers the decoders share.

use serde_json::Value;

/// What a web call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebKind {
    Fetch,
    Search,
}

impl WebKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WebKind::Fetch => "fetch",
            WebKind::Search => "search",
        }
    }
}

/// One thing an agent did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// One simple shell command: its program, the subcommand of the tools
    /// that have one, whether it is on the destructive list, and the skill
    /// it reads (`cat .../SKILL.md`).
    Command {
        program: String,
        sub: Option<String>,
        destructive: bool,
        skill: Option<String>,
    },
    /// A write of the files it names (`Write`, `Edit`, a codex file
    /// change, an `apply_patch`).
    Edit {
        paths: Vec<String>,
    },
    /// A read of one file; a `SKILL.md` names its skill.
    Read {
        path: String,
        skill: Option<String>,
    },
    /// A search of the tree (`Grep`, `Glob`).
    Search,
    /// A call of an MCP tool.
    Mcp {
        server: String,
        tool: String,
    },
    Web {
        kind: WebKind,
    },
    /// A sub-agent started.
    Agent,
    /// A skill loaded by name.
    Skill {
        name: String,
    },
    /// Anything else, by its tool name.
    Tool {
        name: String,
    },
}

impl Action {
    /// A shell command on the destructive list.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Action::Command {
                destructive: true,
                ..
            }
        )
    }

    /// The skill the action loads by name or by reading its `SKILL.md`
    /// through a file tool (not a shell command).
    pub fn loaded_skill(&self) -> Option<&str> {
        match self {
            Action::Skill { name } => Some(name),
            Action::Read { skill, .. } => skill.as_deref(),
            _ => None,
        }
    }
}

/// One tool call as its agent logged it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// The tool's own name (`Bash`, `command_execution`).
    pub name: String,
    /// The call's input as logged, for telling repeated identical calls
    /// apart; never shown.
    pub input: Value,
    /// The files the call names.
    pub paths: Vec<String>,
    /// The call's command line is on the destructive list, as written or
    /// in any command it runs ([`super::command::is_destructive`]).
    pub destructive: bool,
    pub actions: Vec<Action>,
}

impl ToolCall {
    /// Whether the call writes the files it names.
    pub fn writes(&self) -> bool {
        self.actions
            .iter()
            .any(|a| matches!(a, Action::Edit { .. }))
    }

    /// The skill the call loads ([`Action::loaded_skill`]).
    pub fn skill(&self) -> Option<&str> {
        self.actions.iter().find_map(Action::loaded_skill)
    }
}

/// The tags an agent CLI wraps around what it adds to a user turn
/// (reminders, environment notes, instruction files, slash-command and
/// shell-mode echoes with their output): the text inside is not what the
/// person typed.
const WRAPPERS: [&str; 14] = [
    "system-reminder",
    "environment_context",
    "user_instructions",
    "INSTRUCTIONS",
    "command-name",
    "command-message",
    "command-args",
    "local-command-stdout",
    "local-command-stderr",
    "local-command-caveat",
    "user-prompt-submit-hook",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
];

/// How a message an agent CLI adds from an instruction file begins (codex
/// sends a project's `AGENTS.md` as a user message): the whole message is
/// not typed.
const INSTRUCTION_FILE_PREFIXES: [&str; 1] = ["# AGENTS.md instructions for"];

/// How the line an agent CLI writes when a person interrupts it begins.
const INTERRUPT_MARKER: &str = "[Request interrupted by user";

/// `text` without the [`WRAPPERS`] blocks and the interrupt markers,
/// trimmed; `None` when nothing typed is left or the message is an
/// instruction file's text.
pub fn typed_text(text: &str) -> Option<String> {
    if INSTRUCTION_FILE_PREFIXES
        .iter()
        .any(|p| text.trim_start().starts_with(p))
    {
        return None;
    }
    let mut out = text.to_string();
    for tag in WRAPPERS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = out.find(&open) {
            let end = out[start..]
                .find(&close)
                .map_or(out.len(), |e| start + e + close.len());
            out.replace_range(start..end, "");
        }
    }
    let out: Vec<&str> = out
        .lines()
        .filter(|l| !l.trim_start().starts_with(INTERRUPT_MARKER))
        .collect();
    let out = out.join("\n");
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_string())
}

pub(super) fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The config directory of an agent: `env` when set and not empty, else
/// `~/<home_dir>`.
pub(super) fn config_dir(env: &str, home_dir: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(env)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(home_dir))
        })
}
