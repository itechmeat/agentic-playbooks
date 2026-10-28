//! What an agent did, read from its own log formats: one decoder per
//! format, shared by every reader (the engine's run transcript, history
//! discovery, the reply reader of [`crate::agent_output`]).
//!
//! - [`command`]: shell command lines as actions, and the destructive list;
//! - [`claude`]: claude's session lines (`projects/<cwd>/<session>.jsonl`);
//! - [`codex`]: codex's `exec --json` stdout and its session rollouts
//!   (`sessions/YYYY/MM/DD/rollout-*.jsonl`).
//!
//! An [`Action`] names what a call did, never its arguments: a command by
//! its program and subcommand, an edit by the files it names, an MCP call
//! by its server and tool. A [`ToolCall`] keeps the call's name and input
//! beside its actions, for a reader that tells repeated identical calls
//! apart (the input is never shown).

mod action;
pub mod claude;
pub mod codex;
pub mod command;
mod known;

pub use action::{Action, ToolCall, WebKind, typed_text};
pub use known::OTHER;
