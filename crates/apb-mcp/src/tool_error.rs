//! The error type the tool layer and the modules it calls (`setup`,
//! `profile_tools`) share, with its conversions from the core and engine
//! errors. A leaf: it depends on no other module of this crate, so those
//! modules return it without depending on the tool layer
//! (`crate::tools` re-exports it as `ToolError`).

use apb_core::registry::RegistryError;
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
            e @ StoreError::TooLarge { .. } => ToolError::Conflict(e.to_string()),
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
