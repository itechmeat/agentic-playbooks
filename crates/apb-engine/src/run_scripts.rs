//! The run's `scripts/` copy (script nodes, goal scripts and baseline
//! scripts run from it) and the digest pin that keeps an earlier node from
//! rewriting a script that later decides the run's goal or baseline.

use std::path::Path;

use crate::error::EngineError;
use crate::event::EventPayload;

/// The content digest of `run_dir/scripts` (the copy script nodes and goal
/// scripts run from), `none` when there is no such directory or it is
/// empty. Pinned in `run_provenance` at start.
pub(crate) fn digest(run_dir: &Path) -> Result<String, EngineError> {
    digest_or_why(run_dir)
        .map_err(|e| EngineError::Invalid(format!("run scripts cannot be digested: {e}")))
}

/// [`digest`], with the bare reason it failed.
fn digest_or_why(run_dir: &Path) -> Result<String, String> {
    let scripts = run_dir.join("scripts");
    let empty = match std::fs::read_dir(&scripts) {
        Ok(mut d) => d.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.to_string()),
    };
    if empty {
        return Ok("none".into());
    }
    apb_core::content::tree_digest(&scripts, &apb_core::content::TreeLimits::default())
        .map_err(|e| e.to_string())
}

/// Why the run's `scripts/` copy cannot be trusted for `what` (goal or
/// baseline scripts), or
/// `None` when it still matches what the run pinned at start. Any earlier
/// node could rewrite the copy (its path is in `APB_RUN_DIR`), which would
/// let it decide an enforced goal or a baseline. The pin is taken when the
/// playbook has goal or baseline scripts; a run journaled before the pin
/// existed is checked against its whole definition digest instead.
pub(crate) fn tampered(
    run_dir: &Path,
    events: &[crate::event::Event],
    what: &str,
) -> Option<String> {
    let (definition, pinned) = events.iter().find_map(|e| match &e.payload {
        EventPayload::RunProvenance {
            digest,
            scripts_digest,
            ..
        } => Some((digest.clone(), scripts_digest.clone())),
        _ => None,
    })?;
    let changed =
        format!("the run's scripts/ copy changed after the run started, so {what} do not run");
    let undigestable = |e: String| {
        format!("the run's scripts/ copy cannot be digested, so {what} do not run: {e}")
    };
    match (pinned, definition) {
        (Some(pinned), _) => match digest_or_why(run_dir) {
            Ok(now) if now == pinned => None,
            Ok(_) => Some(changed),
            Err(e) => Some(undigestable(e)),
        },
        (None, Some(definition)) => {
            let yaml = match std::fs::read_to_string(run_dir.join("playbook.yaml")) {
                Ok(y) => y,
                Err(e) => {
                    return Some(format!(
                        "the run's playbook snapshot cannot be read, so {what} do not run: {e}"
                    ));
                }
            };
            match apb_core::scope::definition_digest(&yaml, run_dir) {
                Ok(now) if now == definition => None,
                Ok(_) => Some(changed),
                Err(e) => Some(undigestable(e.to_string())),
            }
        }
        (None, None) => None,
    }
}
