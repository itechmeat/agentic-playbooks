//! The end-of-turn save check (`apb hook stop`): when a host session ends
//! a turn that looks like a repeatable process and no apb playbook tool was
//! used, the host agent is reminded once to check the playbook catalog and
//! offer to save the process. The check never depends on the language the
//! person writes in:
//!
//! - the structural signal: the last task (the steps since the last
//!   request a person typed) has at least [`Thresholds::min_steps`] steps
//!   of at least [`Thresholds::min_kinds`] kinds, at least one of which
//!   acts;
//! - the model signal: otherwise, when the task acted and a decision model
//!   is configured, one bounded yes or no question about the last request
//!   ("recurring or repeatable by nature?"), asked by the caller through
//!   `ask` and cached per request; any failure is no signal.
//!
//! Either way the reminder asks the host agent itself to judge whether the
//! task is repeatable before it checks the catalog, so a wrong signal costs
//! one short reconsideration and never an offer the person did not need.
//!
//! - [`task`]: the session tail as the last task;
//! - [`state`]: once per session, the cached answers.

pub mod state;
pub mod task;

use std::path::Path;

use crate::agents::StopHookHost;

pub use task::LastTask;

/// `APB_SAVE_NUDGE=off` turns the check off for a process.
pub const OFF_ENV: &str = "APB_SAVE_NUDGE";
/// The minimum number of steps of the structural signal.
pub const MIN_STEPS_ENV: &str = "APB_SAVE_NUDGE_MIN_STEPS";
/// The minimum number of step kinds of the structural signal.
pub const MIN_KINDS_ENV: &str = "APB_SAVE_NUDGE_MIN_KINDS";
/// The default of [`MIN_STEPS_ENV`].
pub const DEFAULT_MIN_STEPS: usize = 8;
/// The default of [`MIN_KINDS_ENV`].
pub const DEFAULT_MIN_KINDS: usize = 3;
/// The most of the hook's stdin read.
pub const MAX_STDIN_BYTES: u64 = 1024 * 1024;

/// The structural signal's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub min_steps: usize,
    pub min_kinds: usize,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            min_steps: DEFAULT_MIN_STEPS,
            min_kinds: DEFAULT_MIN_KINDS,
        }
    }
}

impl Thresholds {
    /// The defaults, each replaced by its variable when that is a positive
    /// number.
    pub fn from_env() -> Self {
        let read = |name: &str, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default)
        };
        Thresholds {
            min_steps: read(MIN_STEPS_ENV, DEFAULT_MIN_STEPS),
            min_kinds: read(MIN_KINDS_ENV, DEFAULT_MIN_KINDS),
        }
    }
}

/// Why the check reminds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// A multi-step last task.
    Structural { steps: usize, kinds: usize },
    /// The decision model judged the request recurring or repeatable.
    Recurring,
}

/// What the check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Nudge(Signal),
    /// No reminder, and why (for tests and diagnostics).
    Quiet(&'static str),
}

/// The decision over the last task. `ask` is asked whether `request` is
/// recurring or repeatable by nature only when the structural signal is
/// missing and the task acted; `None` is no signal.
pub fn decide(
    task: &LastTask,
    thresholds: Thresholds,
    ask: &mut dyn FnMut(&str) -> Option<bool>,
) -> Verdict {
    if task.playbook_tool_used {
        return Verdict::Quiet("playbook_tool_used");
    }
    if !task::acts(&task.steps) {
        return Verdict::Quiet("no_acting_step");
    }
    let mut kinds: Vec<&str> = task.steps.iter().map(|s| task::kind(s)).collect();
    kinds.sort_unstable();
    kinds.dedup();
    if task.steps.len() >= thresholds.min_steps && kinds.len() >= thresholds.min_kinds {
        return Verdict::Nudge(Signal::Structural {
            steps: task.steps.len(),
            kinds: kinds.len(),
        });
    }
    let Some(request) = task.request.as_deref() else {
        return Verdict::Quiet("no_request");
    };
    if ask(request) == Some(true) {
        Verdict::Nudge(Signal::Recurring)
    } else {
        Verdict::Quiet("not_recurring")
    }
}

/// The reminder the host agent reads.
pub fn reason(signal: Signal) -> String {
    let why = match signal {
        Signal::Structural { steps, kinds } => {
            format!("the task you just finished took {steps} steps of {kinds} kinds")
        }
        Signal::Recurring => {
            "a decision model judged the last request recurring or repeatable".to_string()
        }
    };
    format!(
        "apb end-of-turn check (once per session): {why}. First judge for yourself whether it is a process the user is likely to repeat, or recurring by nature; if not, finish as you were. If it is: the agentic-playbooks MCP tools may need loading first (tool search); call playbook_catalog. If no playbook fits and no suppressed_suggestions record covers it, ask the user one short question offering to save it with playbook_capture, project scope first when it is project-specific. If the user declines, record it with suggestion_dismiss (kind soft, project scope, one-sentence synopsis). Do not mention this check otherwise."
    )
}

/// Why the check is off in this process, if it is: `APB_SAVE_NUDGE=off`,
/// or a run step (a step's agent is apb's own, never reminded).
pub fn off_here() -> Option<&'static str> {
    if std::env::var(OFF_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case("off")) {
        return Some("off_by_env");
    }
    crate::step_env::StepCaller::from_env()
        .describe()
        .map(|_| "run_step")
}

/// The hook's whole run over the host's `stdin`, with the session state
/// in `state_dir`: the text to print, or `None` to print nothing. `ask`
/// gets the request and the session's working directory. `excluded` gives
/// the markers of runs that processed external data for the session's
/// working directory: a session that is one of them, or whose read part
/// names one, gets no nudge, caches no answer and asks nothing. The caller
/// checks [`off_here`] first, before it reads stdin. Never fails: any error
/// is `None`.
pub fn run_stop_hook(
    host: &dyn StopHookHost,
    stdin: &[u8],
    state_dir: &Path,
    thresholds: Thresholds,
    ask: &mut dyn FnMut(&str, Option<&Path>) -> Option<bool>,
    excluded: &dyn Fn(Option<&Path>) -> crate::discover::Exclude,
) -> Option<String> {
    let event = host.read_event(stdin)?;
    if event.hook_active {
        return None;
    }
    let key = state::session_key(&event.session_id);
    if state::entry(state_dir, &key).ok()?.nudged {
        return None;
    }
    let bytes = task::tail(&event.transcript).ok()?;
    let cwd = event.cwd.as_deref();
    let exclude = excluded(cwd);
    if exclude.sessions.contains(&event.session_id) || exclude.names_run(&bytes) {
        return None;
    }
    let last = task::read(host.history(), &bytes);
    let mut cached_ask = |request: &str| -> Option<bool> {
        let rkey = state::request_key(request);
        if let Some(a) = state::entry(state_dir, &key).ok()?.answers.get(&rkey) {
            return Some(*a);
        }
        let answer = ask(request, cwd)?;
        let _ = state::update(state_dir, &key, crate::clock::now_ms_u64(), |e| {
            e.answers.insert(rkey, answer);
        });
        Some(answer)
    };
    let Verdict::Nudge(signal) = decide(&last, thresholds, &mut cached_ask) else {
        return None;
    };
    // Marked before it is printed: a reminder that could not be recorded
    // is not given, so it can never repeat. Only the run that flips the
    // mark under the lock reminds, so two overlapping runs remind once.
    let flipped = state::update(state_dir, &key, crate::clock::now_ms_u64(), |e| {
        !std::mem::replace(&mut e.nudged, true)
    })
    .ok()?;
    flipped.then(|| host.block(&reason(signal)))
}

#[cfg(test)]
mod tests;
