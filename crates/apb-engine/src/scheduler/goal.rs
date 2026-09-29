//! The playbook's goal criteria, checked when a run ends (C1).
//!
//! When the drive reaches a finish node, after every earlier node ran and
//! after the finish answer is composed, each `goal.criteria` entry is
//! checked and journaled as one `goal_checked` event, before the finish
//! node's `node_finished` (its checkpoint):
//!
//! - `script`: the script under the version's `scripts/` (covered by the
//!   trust digest, copied into the run directory) runs with `sh` in the
//!   run's working tree with the run context env; exit 0 passes.
//! - `marker`: the literal marker must appear in the finish answer or in the
//!   latest output of any node.
//! - `manual`: never checked by the engine; journaled `manual` so every
//!   surface shows it as an item for a person to confirm.
//!
//! Because they run at the finish node, criteria see the tree after every
//! node before it, cleanup nodes included: write them against persistent
//! outcomes (a merged branch, a published file, a passing test suite), not
//! scratch state a cleanup step removes. A run that ends before a finish
//! node (a failure no route handles, a stop) checks nothing.
//!
//! By default the results are only reported. With `goal.enforce: true` a
//! failed `script` or `marker` criterion fails a run that would otherwise
//! succeed, with a `run_error` naming it.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use apb_core::schema::{GoalCheck, Playbook};

use crate::error::EngineError;
use crate::event::{EventLog, EventPayload, read_all};
use crate::state::{NodeStatus, RunState};

/// How long one goal script may run.
pub(crate) const GOAL_SCRIPT_TIMEOUT: Duration = Duration::from_secs(600);
/// The tail of a failed script's output kept in the event.
const DETAIL_TAIL: usize = 1024;

/// `goal_checked.status` values.
pub(crate) mod status {
    pub const PASSED: &str = "passed";
    pub const FAILED: &str = "failed";
    pub const MANUAL: &str = "manual";
    /// The check could not run (a missing script, no runtime, cancelled).
    pub const ERROR: &str = "error";
}

fn check_name(c: &GoalCheck) -> &'static str {
    match c {
        GoalCheck::Manual => "manual",
        GoalCheck::Marker { .. } => "marker",
        GoalCheck::Script { .. } => "script",
    }
}

fn tail(text: &str) -> String {
    let text = text.trim();
    let chars = text.chars().count();
    text.chars()
        .skip(chars.saturating_sub(DETAIL_TAIL))
        .collect()
}

/// Checks every criterion, journals one `goal_checked` each, and returns
/// the reason the run must fail when `goal.enforce` is set and a script or
/// marker criterion did not pass. `None` without a goal, without criteria,
/// or when nothing enforced failed.
pub(crate) fn check(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    answer: &str,
    cancel: &AtomicBool,
    log: &mut EventLog,
) -> Result<Option<String>, EngineError> {
    let Some(goal) = &playbook.goal else {
        return Ok(None);
    };
    if goal.criteria.is_empty() {
        return Ok(None);
    }
    let outputs = RunState::fold(&read_all(run_dir)?).outputs;
    let mut enforced_failure: Option<String> = None;
    for (index, c) in goal.criteria.iter().enumerate() {
        let (status, detail) = match &c.check {
            GoalCheck::Manual => (status::MANUAL, None),
            GoalCheck::Marker { marker } => {
                let found = answer.contains(marker.as_str())
                    || outputs.values().any(|o| o.contains(marker.as_str()));
                if found {
                    (status::PASSED, None)
                } else {
                    (
                        status::FAILED,
                        Some(format!(
                            "marker `{marker}` not found in the finish answer or any node output"
                        )),
                    )
                }
            }
            GoalCheck::Script { path } => {
                let env = crate::script::run_env(run_dir, None);
                match crate::script::run_script_with_env(
                    run_dir,
                    workdir,
                    path,
                    "sh",
                    Some(GOAL_SCRIPT_TIMEOUT),
                    Some(cancel),
                    &env,
                ) {
                    Ok(r) if r.status == NodeStatus::Succeeded => (status::PASSED, None),
                    Ok(r) if r.status == NodeStatus::Failed => {
                        let out = tail(&r.stdout);
                        (
                            status::FAILED,
                            Some(if out.is_empty() {
                                format!("`{path}` exited non-zero")
                            } else {
                                format!("`{path}` exited non-zero: {out}")
                            }),
                        )
                    }
                    Ok(r) => (
                        status::ERROR,
                        Some(format!("`{path}` ended {}", r.status.as_str())),
                    ),
                    Err(e) => (status::ERROR, Some(e.to_string())),
                }
            }
        };
        let enforced = goal.enforce && !matches!(c.check, GoalCheck::Manual);
        if enforced && status != status::PASSED && enforced_failure.is_none() {
            enforced_failure = Some(format!(
                "goal criterion {} (`{}`) {status}{}",
                index + 1,
                c.description,
                detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            ));
        }
        log.append(EventPayload::GoalChecked {
            index,
            description: c.description.clone(),
            check: check_name(&c.check).to_string(),
            status: status.to_string(),
            detail,
            enforced,
        })?;
    }
    Ok(enforced_failure)
}
