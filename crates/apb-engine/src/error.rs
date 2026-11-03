use apb_core::registry::RegistryError;
use apb_core::schema::SchemaError;
use apb_core::versioning::VersioningError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    /// A state conflict (for example, continuing from an already-superseded run).
    /// Surfaces that map HTTP status codes treat this as 409 Conflict.
    #[error("conflict: {0}")]
    Conflict(String),
    /// The request an answer or a decision names already has one, queued or
    /// journaled: a second one would answer nothing (or a later round).
    #[error("already answered: {0}")]
    AlreadyAnswered(String),
    /// A decision surface the gate does not take a decision from: a
    /// person-only gate and a channel apb does not count as a person's, or a
    /// run step deciding its own run. The message names how a person
    /// decides instead. Surfaces map it to HTTP 403 and an MCP tool error.
    #[error("{message}")]
    ChannelRefused {
        refusal: crate::decision_channel::CallerRefusal,
        message: String,
    },
    #[error("workdir busy: {0}")]
    WorkdirBusy(String),
    #[error("agent adapter error: {0}")]
    Adapter(String),
    #[error("script error: {0}")]
    Script(String),
    #[error("anomaly: {0}")]
    Anomaly(String),
    #[error("yaml error: {0}")]
    Yaml(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Versioning(#[from] VersioningError),
}
