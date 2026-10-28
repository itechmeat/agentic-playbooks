//! The free local check of an installed agent CLI that `apb doctor` runs:
//! no model call, only what the binary prints about itself.
//!
//! - **Version:** the installed version against the one the agent module's
//!   built-in form and output reader were verified against ([`Verified`]).
//! - **Help flags:** every flag of the built-in launch, resume, autonomy,
//!   soul, fresh-session and profile-mode forms, derived from
//!   [`Agent::forms`] so nothing is listed twice, must appear in the text
//!   the agent's `help_args` print.
//! - **Custom form:** an `agents.<id>.invocation` in the config replaces the
//!   built-in form, whose output is then read as plain text when it is not
//!   the agent's machine format: allowed, so a note rather than a warning.
//!
//! Pure: the caller runs the binary (detection's sandboxed probe) and hands
//! in what it printed.

use super::agent::{Agent, Verified};
use super::forms::{Forms, FreshForm};
use super::types::ZcodeMode;

/// How a line of the local check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    /// A fact that is neither good nor bad (nothing recorded to compare).
    Info,
}

/// One line of the local check of one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub level: Level,
    pub detail: String,
}

impl Line {
    fn new(level: Level, detail: impl Into<String>) -> Self {
        Line {
            level,
            detail: detail.into(),
        }
    }
}

/// What the binary printed about itself.
#[derive(Debug, Clone, Default)]
pub struct CheckInput<'a> {
    /// The first line its version arguments printed.
    pub version: Option<&'a str>,
    /// What its help arguments printed, or why they failed; `None` when the
    /// help was not run.
    pub help: Option<Result<&'a str, &'a str>>,
    /// The config replaces the built-in form (`agents.<id>.invocation`).
    pub custom_form: bool,
}

/// The local check of `agent`: the version line, then the help-flags line
/// for the built-in form, or the custom-form note.
pub fn check(agent: &dyn Agent, input: &CheckInput<'_>) -> Vec<Line> {
    let id = agent.descriptor().id;
    let mut out = vec![version_line(id, agent.descriptor().verified, input.version)];
    if input.custom_form {
        out.push(Line::new(
            Level::Ok,
            format!(
                "custom invocation; output read as plain text when it is not {id}'s machine format"
            ),
        ));
    } else if let (Some(forms), Some(help)) = (agent.forms(), input.help) {
        out.push(help_line(&forms, help));
    }
    out
}

/// The installed version against the verified one.
pub fn version_line(id: &str, verified: Option<Verified>, installed: Option<&str>) -> Line {
    let Some(v) = verified else {
        return Line::new(
            Level::Info,
            "no verified version recorded: the built-in form was never checked against a recorded output",
        );
    };
    let Some(raw) = installed else {
        return Line::new(
            Level::Info,
            format!(
                "version unknown; verified against {} on {}",
                v.version, v.on
            ),
        );
    };
    let (Some(have), Some(want)) = (version_numbers(raw), version_numbers(v.version)) else {
        return Line::new(
            Level::Info,
            format!(
                "version `{raw}` cannot be compared with the verified {}",
                v.version
            ),
        );
    };
    match compare_versions(&have, &want) {
        std::cmp::Ordering::Equal => Line::new(
            Level::Ok,
            format!("version {} verified on {}", v.version, v.on),
        ),
        std::cmp::Ordering::Greater => Line::new(
            Level::Warn,
            format!(
                "version {} is newer than the verified {} ({}); `apb doctor --probe-agent {id}` checks its output format with one paid model call",
                dotted(&have),
                v.version,
                v.on
            ),
        ),
        std::cmp::Ordering::Less => Line::new(
            Level::Warn,
            format!(
                "version {} is older than the verified {} ({}); the built-in form may not match it",
                dotted(&have),
                v.version,
                v.on
            ),
        ),
    }
}

/// Whether every flag of the built-in form appears in the help text.
fn help_line(forms: &Forms, help: Result<&str, &str>) -> Line {
    let text = match help {
        Ok(text) => text,
        Err(why) => {
            return Line::new(Level::Warn, format!("help could not be read: {why}"));
        }
    };
    let missing = missing_flags(&form_flags(forms), text);
    if missing.is_empty() {
        Line::new(Level::Ok, "every flag of the built-in form is in its help")
    } else {
        Line::new(
            Level::Warn,
            format!(
                "help does not list {}: the built-in form may have changed",
                missing.join(", ")
            ),
        )
    }
}

/// Every flag the built-in forms pass, in order of first use, once each.
pub fn form_flags(forms: &Forms) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    let launch = &forms.launch;
    all.extend(launch.argv.iter().cloned());
    all.extend(launch.soul_flag.iter().cloned());
    all.extend(launch.autonomous_args.iter().cloned());
    all.extend(
        forms
            .resume
            .unwrap_or_default()
            .iter()
            .map(|s| s.to_string()),
    );
    match forms.fresh {
        FreshForm::Assigned { flag } | FreshForm::Titled { flag, .. } => all.push(flag.to_string()),
        FreshForm::Printed => {}
    }
    if let Some(mode) = forms.profile_mode {
        all.extend(mode(ZcodeMode::Yolo));
        all.extend(mode(ZcodeMode::Edit));
    }
    let mut out: Vec<String> = Vec::new();
    for a in all {
        if a.starts_with('-') && a != "--" && !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// The flags of `flags` that `help` does not name as a whole word.
pub fn missing_flags(flags: &[String], help: &str) -> Vec<String> {
    flags
        .iter()
        .filter(|f| !names_flag(help, f))
        .cloned()
        .collect()
}

/// Whether `text` holds `flag` not as part of a longer flag or word.
fn names_flag(text: &str, flag: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(flag).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + flag.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

/// The first dotted number in `raw` (`codex-cli 0.157.0` is `[0, 157, 0]`).
fn version_numbers(raw: &str) -> Option<Vec<u64>> {
    raw.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|t| t.trim_matches('.'))
        .find(|t| t.contains('.') && !t.is_empty())
        .and_then(|t| t.split('.').map(|n| n.parse().ok()).collect())
}

/// `have` against `want` segment by segment, a missing segment counting as
/// 0 (`2.1` is `2.1.0`).
fn compare_versions(have: &[u64], want: &[u64]) -> std::cmp::Ordering {
    let seg = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    (0..have.len().max(want.len()))
        .map(|i| seg(have, i).cmp(&seg(want, i)))
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn dotted(numbers: &[u64]) -> String {
    numbers
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}
