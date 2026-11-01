//! The error of the version store.

use std::io;

use crate::validate::Issue;

#[derive(Debug, thiserror::Error)]
pub enum VersioningError {
    #[error("playbook `{0}` not found")]
    NotFound(String),
    #[error("{}", crate::validate::render_issues(.0))]
    Validation(Vec<Issue>),
    #[error("schema error: {0}")]
    Schema(String),
    #[error("version conflict: {0}")]
    Conflict(String),
    #[error("playbook `{0}` is frozen: definition changes are disabled")]
    Frozen(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}
