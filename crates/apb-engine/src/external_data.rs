//! External data runs (capability `run-external-data`): a run started with
//! `external_data: true` processes data that comes from outside the
//! operator, so apb keeps everything it produces in its run directory and
//! nothing in a store that later runs or learning tools read.
//!
//! The one rule lives here. Four sources mark a run, any one is enough and
//! none reads as cleared: the manifest's `external_data`, its origin
//! [`RunOrigin::ExternalData`], the `+external` suffix of its execution mode
//! and `run_started.external_data` in its journal ([`is_flagged`]). Every
//! reader that learns from earlier runs asks [`learns_from`], never the
//! origin alone; every tool that learns from one named run asks
//! [`refuse_if_flagged`]; every start surface asks [`for_start`] before its
//! own checks.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::EngineError;
use crate::event::EventPayload;
use crate::manifest::RunOrigin;

pub(crate) mod flag;

pub use flag::is_flagged;
use flag::{journal_flag, manifest_flag};

/// A resume or a successor that asks for another value than the run's own.
pub const IMMUTABLE: &str = "external_data_immutable";
/// A flagged start that also asks to remember its input.
pub const REMEMBER_REFUSED: &str = "external_data_remember_refused";
/// A tool that would learn from a flagged run (or a version one created).
pub const RUN: &str = "external_data_run";

/// The reason a candidate was skipped for a flagged start
/// (`manifest.candidate_skipped`).
pub const CANDIDATE_SKIPPED: &str = "external_data";

/// A start refused by the external data rule: its code and a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

impl Refusal {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// The JSON answer a surface returns: `{ "error": code, "detail": ... }`.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({ "error": self.code, "detail": self.detail })
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl From<Refusal> for EngineError {
    fn from(r: Refusal) -> Self {
        EngineError::Invalid(r.to_string())
    }
}

/// Whether a reader of earlier runs may learn from the run in `run_dir`:
/// its manifest reads (or there is none, an older run's layout), its origin
/// is production and it is not flagged. The one predicate every learning
/// reader asks.
pub fn learns_from(run_dir: &Path) -> bool {
    let read = crate::manifest::read(run_dir);
    if read.is_err() {
        return false;
    }
    let flagged_manifest = read
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .is_some_and(manifest_flag);
    RunOrigin::of_read(&read).is_production() && !flagged_manifest && !journal_flag(run_dir)
}

/// Refuses `what` (a sentence naming the tool) for a flagged run with
/// `external_data_run`, failing closed when the manifest does not read.
pub fn refuse_if_flagged(run_dir: &Path, what: &str) -> Result<(), EngineError> {
    if crate::manifest::read(run_dir).is_err() {
        return Err(EngineError::Invalid(format!(
            "{RUN}: {what}: the run's manifest does not read, so whether it processed external data cannot be told"
        )));
    }
    if is_flagged(run_dir) {
        return Err(EngineError::Invalid(format!(
            "{RUN}: {what}: the run processed external data, and nothing is kept from it for learning"
        )));
    }
    Ok(())
}

/// Refuses `what` for a step of a flagged run: the step's run is the one
/// `APB_RUN_DIR` names when its directory name is the step's `run_id`,
/// else the run of that id under the project `root`, whatever workspace
/// the step's request targets. A step whose run is found nowhere is
/// refused too (fail-closed), since whether it processes external data
/// cannot be told. A process that is no run step passes.
pub fn refuse_step_if_flagged(
    root: &Path,
    step: &apb_core::step_env::StepCaller,
    what: &str,
) -> Result<(), EngineError> {
    let Some(run_id) = step.run_id.as_deref() else {
        return Ok(());
    };
    let named = std::env::var_os("APB_RUN_DIR")
        .map(PathBuf::from)
        .filter(|d| d.file_name().is_some_and(|n| n == run_id) && d.is_dir());
    match named.or_else(|| run_dir_in(root, run_id)) {
        Some(dir) => refuse_if_flagged(&dir, what),
        None => Err(EngineError::Invalid(format!(
            "{RUN}: {what}: the step's run `{run_id}` is not found, so whether it processes external data cannot be told"
        ))),
    }
}

/// The run directory of `run_id` under the project `root`: a production
/// run, else a scenario run. `None` for an id that is no safe path segment
/// or names no directory.
pub fn run_dir_in(root: &Path, run_id: &str) -> Option<PathBuf> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return None;
    }
    [".apb/runs", ".apb/scenario-runs"]
        .iter()
        .map(|d| root.join(d).join(run_id))
        .find(|d| d.is_dir())
}

/// The effective flag of a start in the project `root`: what the caller
/// asked (`requested`, `None` when it said nothing), inherited from a
/// flagged parent run or `continued_from` predecessor. An explicit `false`
/// under a flagged parent or predecessor is refused with
/// `external_data_immutable`. Called by every start surface before its own
/// checks; preparation applies the same inheritance as a backstop.
pub fn for_start(
    root: &Path,
    requested: Option<bool>,
    parent_run: Option<&str>,
    continued_from: Option<&str>,
) -> Result<bool, Refusal> {
    let inherited = [parent_run, continued_from]
        .into_iter()
        .flatten()
        .filter_map(|id| run_dir_in(root, id).map(|d| (id, d)))
        .find(|(_, d)| is_flagged(d));
    match (inherited, requested) {
        (Some((id, _)), Some(false)) => Err(Refusal::new(
            IMMUTABLE,
            format!(
                "run `{id}` processed external data, so a run that continues it does too; leave external_data out or pass true"
            ),
        )),
        (Some(_), _) => Ok(true),
        (None, r) => Ok(r.unwrap_or(false)),
    }
}

/// Refuses a flagged start that also asks to remember its input, with
/// `external_data_remember_refused`: nothing from such a start may be kept.
pub fn check_remember(external_data: bool, remember: bool) -> Result<(), Refusal> {
    if external_data && remember {
        return Err(Refusal::new(
            REMEMBER_REFUSED,
            "a run that processes external data never remembers its input; start it without remember",
        ));
    }
    Ok(())
}

/// The check of a resume: a value the caller passed must equal the run's
/// own flag (`external_data_immutable` otherwise); nothing passed is
/// accepted. A resume never writes the flag.
pub fn check_resume(run_dir: &Path, requested: Option<bool>) -> Result<(), Refusal> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let flagged = is_flagged(run_dir);
    if requested != flagged {
        return Err(Refusal::new(
            IMMUTABLE,
            format!(
                "the run was started with external_data {flagged}, which is fixed for its lifetime"
            ),
        ));
    }
    Ok(())
}

/// The newest `cap` run directories under `roots` that [`learns_from`],
/// newest first: the filter runs before the cap, so flagged runs, however
/// many, never crowd earlier history out of a capped window.
pub fn learnable_run_dirs(roots: &[PathBuf], cap: usize) -> Vec<PathBuf> {
    crate::decision::report::newest_run_dirs_where(roots, cap, learns_from)
}

/// Every run directory under the project `roots`, production and scenario
/// runs both, that is flagged.
fn flagged_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        for sub in [".apb/runs", ".apb/scenario-runs"] {
            let Ok(entries) = std::fs::read_dir(root.join(sub)) else {
                continue;
            };
            out.extend(
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .filter(|d| is_flagged(d)),
            );
        }
    }
    out.sort();
    out
}

/// The agent session ids (`attempt_finished.session`) and the run ids of
/// the flagged runs under the project `roots`: what history discovery and
/// the end-of-turn save hook leave out.
pub fn flagged_markers(roots: &[PathBuf]) -> apb_core::discover::Exclude {
    let mut run_ids = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for dir in flagged_dirs(roots) {
        if let Some(name) = dir.file_name() {
            run_ids.insert(name.to_string_lossy().into_owned());
        }
        if let Ok(journal) = crate::event::read_journal(&dir) {
            for e in journal.events {
                if let EventPayload::AttemptFinished {
                    session: Some(s), ..
                } = e.payload
                    && !s.is_empty()
                {
                    sessions.insert(s);
                }
            }
        }
    }
    apb_core::discover::Exclude { run_ids, sessions }
}

#[cfg(test)]
mod tests;
