//! Shared leaf primitives for the connector modules: the error type and the
//! identifier validator used by both `def` (manifest schema) and `template`
//! (placeholder rendering). Kept as a separate leaf so `def` and `template`
//! can depend on it without depending on each other (an `ADP` cycle:
//! `def::validate_templates` needs `template::{Namespace, placeholders}`,
//! and `template` needs an error type and a name validator - if either of
//! those lived in `def`, `template` would import back from `def`, forming a
//! cycle).

/// Error parsing or looking up a connector definition. Mirrors
/// `profile::ProfileError` in shape, but stays a separate type since
/// connectors are a distinct concept with their own failure modes (no
/// scope/case-fold concerns at this layer).
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("invalid connector: {0}")]
    Invalid(String),
    #[error("connector `{0}` not found")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(String),
}

/// Validates a machine-facing identifier (function name, account field
/// name, or template placeholder name): `[a-z0-9][a-z0-9_]*`, at most 64
/// chars. Snake_case is for these API-style identifiers (matching template
/// keys like `{{account.base_url}}`); folder-level connector names stay
/// hyphen slugs via `crate::profile::validate_profile_name`.
pub fn validate_snake_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name is empty".into());
    }
    if name.len() > 64 {
        return Err(format!("name `{name}` exceeds 64 chars"));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!("name `{name}` must start with [a-z0-9]"));
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' {
            return Err(format!("name `{name}` allows only [a-z0-9_]"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_snake_name_accepts_snake_case() {
        assert!(validate_snake_name("list_issues").is_ok());
        assert!(validate_snake_name("base_url").is_ok());
        assert!(validate_snake_name("ping").is_ok());
        assert!(validate_snake_name("a1_b2").is_ok());
    }

    #[test]
    fn validate_snake_name_rejects_hyphen_and_uppercase() {
        assert!(validate_snake_name("list-issues").is_err());
        assert!(validate_snake_name("ListIssues").is_err());
        assert!(validate_snake_name("").is_err());
        assert!(validate_snake_name(&"a".repeat(65)).is_err());
    }
}
