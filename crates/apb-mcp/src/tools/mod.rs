//! The tool layer: the operations the MCP server exposes, independent of the
//! rmcp plumbing in [`crate::server`]. One module per domain; this file holds
//! the shared error type and the two lookups every domain starts from.

pub mod budget;
pub mod candidate;
pub mod capture;
pub mod decision;
pub mod grade;
pub mod intent;
pub mod memory;
pub mod meta;
pub mod notify;
pub mod playbook;
pub mod problems;
pub mod retro;
pub mod run;
pub mod supervisor;
pub mod trial;

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

use apb_core::registry::{Registry, RegistryError, is_safe_segment};
use apb_core::validate::Issue;
use apb_core::versioning::VersioningError;
use apb_engine::EngineError;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("engine error: {0}")]
    Engine(String),
    /// An optimistic-concurrency conflict (CAS): the profile already exists / does not
    /// exist / `expected_digest` did not match. Typed separately so that
    /// surfaces can map it to 409 without a fragile substring search in the text.
    #[error("conflict: {0}")]
    Conflict(String),
}

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

impl From<RegistryError> for ToolError {
    fn from(e: RegistryError) -> Self {
        match e {
            RegistryError::NotFound(w) => ToolError::NotFound(w),
            other => ToolError::Engine(other.to_string()),
        }
    }
}

impl From<EngineError> for ToolError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::NotFound(m) => ToolError::NotFound(m),
            EngineError::Registry(RegistryError::NotFound(w)) => ToolError::NotFound(w),
            EngineError::Conflict(m) => ToolError::Conflict(m),
            // A workdir held by another live write-run is a retry-actionable
            // conflict, not a hard failure (#102.5): the caller can try again
            // shortly instead of treating this like any other engine error.
            EngineError::WorkdirBusy(m) => ToolError::Conflict(m),
            other => ToolError::Engine(other.to_string()),
        }
    }
}

impl From<VersioningError> for ToolError {
    fn from(e: VersioningError) -> Self {
        match e {
            VersioningError::NotFound(w) => ToolError::NotFound(w),
            VersioningError::Validation(issues) => {
                ToolError::Engine(render_validation_issues(&issues))
            }
            VersioningError::Conflict(m) => ToolError::Conflict(m),
            other => ToolError::Engine(other.to_string()),
        }
    }
}

/// Renders a validation failure as `validation failed:` followed by one line
/// per issue. Delegates to `apb_core::validate::render_issues`, the single
/// canonical rendering shared with `VersioningError::Validation`'s own
/// `Display` impl, so this surface can never drift from any other consumer
/// of the same `Vec<Issue>`.
fn render_validation_issues(issues: &[Issue]) -> String {
    apb_core::validate::render_issues(issues)
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
