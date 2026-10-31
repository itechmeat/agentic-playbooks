//! Whose memory a file holds, and why memory could not be read or written.

use crate::scope::Scope;

/// Why remembered input could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("playbook id `{0}` is not a safe name")]
    InvalidId(String),
    #[error("`{0}` is not a scope (project or global)")]
    InvalidScope(String),
    #[error(
        "the remembered input has format {0}, newer than this apb understands; it is left untouched"
    )]
    NewerFormat(u32),
    #[error("remembered input already exists at the destination")]
    DestinationExists,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Whose memory: a playbook id in a scope. Built only through
/// [`MemoryKey::new`], so the id is always a safe path segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryKey {
    scope: Scope,
    id: String,
}

impl MemoryKey {
    pub fn new(scope: Scope, id: &str) -> Result<Self, MemoryError> {
        if !crate::registry::is_safe_segment(id) {
            return Err(MemoryError::InvalidId(id.to_string()));
        }
        Ok(MemoryKey {
            scope,
            id: id.to_string(),
        })
    }

    /// The key of the origin label the engine and the gate use
    /// (`project` or `global`); any other label is refused.
    pub fn for_label(label: &str, id: &str) -> Result<Self, MemoryError> {
        let scope = match label {
            "project" => Scope::Project,
            "global" => Scope::Global,
            other => return Err(MemoryError::InvalidScope(other.to_string())),
        };
        Self::new(scope, id)
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}
