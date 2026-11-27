//! The tool layer: the operations the MCP server exposes, independent of the
//! rmcp plumbing in [`crate::server`]. One module per domain; this file holds
//! the two lookups every domain starts from and re-exports the shared error
//! type, which lives in the leaf module `tool_error` so the modules this
//! layer calls (`setup`, `profile_tools`) can return it without depending on
//! the tool layer.

pub mod actions;
pub mod budget;
pub mod candidate;
pub mod capture;
pub mod corrections;
pub mod decision;
pub mod discover;
pub mod evidence;
pub mod grade;
#[cfg(feature = "testing")]
pub mod install_faults;
pub mod intent;
pub mod judge;
pub mod memory;
pub mod meta;
pub mod notify;
pub mod playbook;
pub mod problems;
pub mod results;
pub mod retro;
pub mod run;
pub mod scenario;
pub mod scenario_capture;
pub mod stats;
pub mod supervisor;
pub mod templates;
pub mod trial;
pub mod trigger;

pub use capture::*;
pub use decision::*;
pub use intent::*;
pub use meta::*;
pub use playbook::*;
pub use retro::*;
pub use run::*;
pub use supervisor::*;
pub use trial::*;

use std::path::Path;

use apb_core::registry::{Registry, is_safe_segment};

pub use crate::tool_error::ToolError;

/// Refuses `what` when `step` is a run step (see
/// [`apb_core::step_env::StepCaller::refusal`]): grading a run, deciding
/// a candidate and deciding, rechecking or collecting known problems are
/// never done by a step of a run. The one guard every surface (MCP, the
/// CLI, the HTTP API) reaches through this tool layer.
pub(crate) fn outside_run(
    step: &apb_core::step_env::StepCaller,
    what: &str,
) -> Result<(), ToolError> {
    match step.refusal(what) {
        Some(why) => Err(ToolError::Conflict(why)),
        None => Ok(()),
    }
}

/// A playbook scope argument (`project` or `global`) and the definition
/// parent it names: `<root>/.apb` or the global store's parent.
pub(crate) fn scope_parent(
    root: &Path,
    scope: &str,
) -> Result<(apb_core::scope::Scope, std::path::PathBuf), ToolError> {
    use apb_core::scope::Scope;
    match Scope::parse(scope) {
        Some(Scope::Project) => Ok((Scope::Project, root.join(".apb"))),
        Some(Scope::Global) => apb_core::store::global_playbooks_parent()
            .map(|p| (Scope::Global, p))
            .ok_or_else(|| ToolError::Engine("no global config dir".into())),
        None => Err(ToolError::Engine(format!("unknown scope `{scope}`"))),
    }
}

fn open(root: &Path) -> Result<Registry, ToolError> {
    Registry::open(root).map_err(ToolError::from)
}

/// A run surface's answer about the run at `dir` as it leaves apb: every
/// secret value of the run, its ancestors and the workspace replaced, the
/// host tasks it delivers to the executor kept
/// ([`apb_engine::run_redact::RunRedactor::redact_answer`]).
pub(crate) fn redacted_answer(
    root: &Path,
    dir: &Path,
    answer: serde_json::Value,
) -> serde_json::Value {
    redacted_with(
        &apb_engine::run_redact::WorkspaceRedaction::new(root),
        dir,
        answer,
    )
}

/// [`redacted_answer`] through a workspace redaction the caller already
/// resolved (and used for the answer's next actions).
pub(crate) fn redacted_with(
    redaction: &apb_engine::run_redact::WorkspaceRedaction,
    dir: &Path,
    mut answer: serde_json::Value,
) -> serde_json::Value {
    redaction.run_dir(dir).redact_answer(&mut answer);
    answer
}

/// Resolves the run directory, uniformly rejecting an unsafe run_id (path traversal)
/// and a missing run as NotFound.
fn resolve_run_dir(root: &Path, run_id: &str) -> Result<std::path::PathBuf, ToolError> {
    if !is_safe_segment(run_id) {
        return Err(ToolError::NotFound(format!("run `{run_id}`")));
    }
    let dir = root.join(".apb/runs").join(run_id);
    if !dir.is_dir() {
        return Err(ToolError::NotFound(format!("run `{run_id}`")));
    }
    Ok(dir)
}
