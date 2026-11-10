//! The tool layer: the operations the MCP server exposes, independent of the
//! rmcp plumbing in [`crate::server`]. One module per domain; this file holds
//! the shared error type and the two lookups every domain starts from.

pub mod actions;
pub mod budget;
pub mod candidate;
pub mod capture;
pub mod corrections;
pub mod decision;
pub mod discover;
pub mod evidence;
pub mod grade;
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
    /// The caller's channel may not take this ([`apb_engine::EngineError::
    /// ChannelRefused`]): a person-only gate refuses every channel that is
    /// not a person's, and a run step never decides for its own run tree.
    /// The message names how a person decides instead. HTTP answers 403.
    #[error("refused ({}): {message}", refusal.as_str())]
    Refused {
        refusal: apb_engine::decision_channel::CallerRefusal,
        message: String,
    },
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

impl From<apb_core::signed_store::StoreError> for ToolError {
    fn from(e: apb_core::signed_store::StoreError) -> Self {
        use apb_core::signed_store::StoreError;
        match e {
            StoreError::InvalidId(id) => ToolError::NotFound(format!("playbook `{id}`")),
            StoreError::NewerSchema(n) => ToolError::Engine(format!(
                "the file has schema {n}, newer than this apb understands; upgrade apb to read it"
            )),
            StoreError::Io(e) => ToolError::Engine(e.to_string()),
            e @ StoreError::Unverified(_) => ToolError::Conflict(e.to_string()),
        }
    }
}

impl From<EngineError> for ToolError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::NotFound(m) => ToolError::NotFound(m),
            EngineError::Registry(RegistryError::NotFound(w)) => ToolError::NotFound(w),
            EngineError::Conflict(m) => ToolError::Conflict(m),
            m @ EngineError::AlreadyAnswered(_) => ToolError::Conflict(m.to_string()),
            EngineError::AlreadyStarted(id) => {
                ToolError::Conflict(format!("already_started: {id}"))
            }
            // A workdir held by another live write-run is a retry-actionable
            // conflict, not a hard failure (#102.5): the caller can try again
            // shortly instead of treating this like any other engine error.
            EngineError::WorkdirBusy(m) => ToolError::Conflict(m),
            EngineError::ChannelRefused { refusal, message } => {
                ToolError::Refused { refusal, message }
            }
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
            // The message starts with its code and names the person's next
            // action (the dashboard editor, or `apb launch floor`).
            VersioningError::Launch(e) => ToolError::Conflict(e.to_string()),
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
