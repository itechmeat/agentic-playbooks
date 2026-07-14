use apb_core::registry::RegistryError;
use apb_core::schema::SchemaError;
use apb_core::versioning::VersioningError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
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
