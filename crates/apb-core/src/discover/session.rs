//! One session file as discovery reads it: its steps (argument-free tokens,
//! repeated steps collapsed) and the requests a person typed, each with
//! its line. Held in memory only; nothing here is stored.
//!
//! Only a session's own lines count: a claude sub-agent's lines
//! (`isSidechain`) are left out, so one session with ten sub-agents is one
//! session.

use std::collections::BTreeMap;

use serde_json::Value;

use super::token;
use super::types::HistoryAgent;
use crate::agent_actions::codex::RolloutFormat;
use crate::agent_actions::{ToolCall, claude, codex};

/// One step of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub token: String,
    /// The 1-based line it was logged on.
    pub line: u32,
    /// The step ran a destructive command.
    pub destructive: bool,
}

/// One session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The index of its root in the request.
    pub root: usize,
    /// `<project name>/<file name>`.
    pub rel: String,
    /// The index of its project in the request.
    pub project: usize,
    /// Days since the Unix epoch the file was last written on.
    pub day: i64,
    pub steps: Vec<Step>,
    /// What a person typed, with its line.
    pub requests: Vec<(u32, String)>,
}

/// A session file's lines as parsed: the steps, the requests and the
/// working directory it names first.
#[derive(Debug, Default)]
pub struct Parsed {
    pub steps: Vec<Step>,
    pub requests: Vec<(u32, String)>,
    pub cwd: Option<String>,
}

impl Parsed {
    fn push_calls(&mut self, line: u32, calls: &[ToolCall], max_steps: usize) {
        use crate::agent_actions::Action;
        let flagged = Action::is_destructive;
        for call in calls {
            // A whole command line on the destructive list that no single
            // command of it is: its last step carries the flag.
            let whole = call.destructive && !call.actions.iter().any(flagged);
            let last = call.actions.len().saturating_sub(1);
            for (i, action) in call.actions.iter().enumerate() {
                let destructive = flagged(action) || (whole && i == last);
                let token = token::of(action);
                let full = self.steps.len() >= max_steps;
                match self.steps.last_mut() {
                    // Consecutive identical steps are one step.
                    Some(last) if last.token == token => last.destructive |= destructive,
                    _ if full => return,
                    _ => self.steps.push(Step {
                        token,
                        line,
                        destructive,
                    }),
                }
            }
        }
    }
}

/// Calls `each` with every JSON line of `bytes` and its 1-based number,
/// one line parsed at a time. A line longer than `max_line_bytes` is
/// skipped and counted under `line_too_long`; a line that is not JSON (a
/// cut last line) is left out.
fn each_json_line(
    bytes: &[u8],
    max_line_bytes: u64,
    skipped: &mut BTreeMap<String, u32>,
    mut each: impl FnMut(u32, &Value),
) {
    for (i, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if line.len() as u64 > max_line_bytes {
            *skipped.entry("line_too_long".to_string()).or_default() += 1;
            continue;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(line) {
            each(i as u32 + 1, &v);
        }
    }
}

/// The form a rollout's work is read from ([`RolloutFormat::detect`]),
/// parsing only the lines that can hold the newer form.
fn rollout_format(bytes: &[u8], max_line_bytes: u64) -> RolloutFormat {
    const MARK: &[u8] = b"item_completed";
    RolloutFormat::detect(
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| {
                l.len() as u64 <= max_line_bytes && l.windows(MARK.len()).any(|w| w == MARK)
            })
            .filter_map(|l| serde_json::from_slice::<Value>(l).ok()),
    )
}

/// Parses one session file of `agent`.
pub fn parse(
    agent: HistoryAgent,
    bytes: &[u8],
    max_line_bytes: u64,
    max_steps: usize,
    skipped: &mut BTreeMap<String, u32>,
) -> Parsed {
    let mut out = Parsed::default();
    match agent {
        HistoryAgent::ClaudeCode => each_json_line(bytes, max_line_bytes, skipped, |n, v| {
            if claude::is_sidechain(v) {
                return;
            }
            if out.cwd.is_none() {
                out.cwd = claude::cwd(v).map(str::to_string);
            }
            if let Some(t) = claude::user_text(v) {
                out.requests.push((n, t));
            }
            out.push_calls(n, &claude::tool_calls(v), max_steps);
        }),
        HistoryAgent::Codex => {
            let format = rollout_format(bytes, max_line_bytes);
            each_json_line(bytes, max_line_bytes, skipped, |n, v| {
                if out.cwd.is_none() {
                    out.cwd = codex::cwd_of(v).map(str::to_string);
                }
                if let Some(t) = codex::user_text(v, format) {
                    out.requests.push((n, t));
                }
                out.push_calls(n, &codex::calls_of_rollout_line(v, format), max_steps);
            });
        }
    }
    out
}

/// The working directory a codex rollout's header names (its
/// `session_meta`, else its first `turn_context`).
pub fn codex_header_cwd(header: &[u8]) -> Option<String> {
    header
        .split(|b| *b == b'\n')
        .filter_map(|l| serde_json::from_slice::<Value>(l).ok())
        .find_map(|v| codex::cwd_of(&v).map(str::to_string))
}
