//! The tool layer: the operations the MCP server exposes, independent of the
//! rmcp plumbing in [`crate::server`]. One module per domain; this file holds
//! the shared error type and the two lookups every domain starts from.

pub mod capture;
pub mod meta;
pub mod playbook;
pub mod run;
pub mod supervisor;
pub mod trial;

pub use capture::*;
pub use meta::*;
pub use playbook::*;
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
