//! Connector definition schema: the content of `connector.yaml` (spec
//! 2026-07-18-connectors-design, section 3.1).
//!
//! A connector links a playbook node to an external HTTP service through a
//! declarative manifest: an auth block, the account fields the connector
//! needs, and a set of callable functions (HTTP or mock). This module only
//! parses and structurally validates that manifest; template placeholder
//! validation (e.g. `{{secret.*}}` only allowed in `auth`) is added by
//! `validate_templates` in a later task, once the template parser exists.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

/// How the connector authenticates outgoing requests. The tag `kind`
/// selects the variant; each variant only accepts the fields it needs, so
/// (for example) a `header` auth block cannot also carry a `param`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthSpec {
    Header {
        header: String,
        value_template: String,
    },
    Query {
        param: String,
        value_template: String,
    },
    Basic {
        username_template: String,
        password_template: String,
    },
}

/// One field of the schema of an account for this connector (e.g.
/// `base-url`, `token`). `secret` fields may only ever hold an
/// `{{env.VAR}}` reference in account config (enforced by the config
/// loader, not here).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountField {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
}

/// A canned response for a `mock` function: no network call is made, the
/// function simply returns this status and body. Mocks are a permanent part
/// of the format (used by the fake test connector and by connector authors).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MockSpec {
    pub status: u16,
    pub body: serde_json::Value,
}

fn default_timeout() -> u64 {
    30
}

/// Validates a machine-facing identifier (function name or account field
/// name): `[a-z0-9][a-z0-9_]*`, at most 64 chars. Snake_case is for these
/// API-style identifiers (matching template keys like
/// `{{account.base_url}}`); folder-level connector names stay hyphen slugs
/// via `crate::profile::validate_profile_name`.
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

/// One callable function of the connector: either an HTTP call (`method` +
/// `url`, optionally `query`/`body`) or a `mock` (canned response, no
/// network). `from_yaml` enforces that a function is exactly one of the two,
/// never both and never neither.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub deprecated: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub args_schema: Option<serde_json::Value>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
    #[serde(default)]
    pub mock: Option<MockSpec>,
}

impl FunctionSpec {
    /// A mock function returns a canned response instead of making an HTTP
    /// call.
    pub fn is_mock(&self) -> bool {
        self.mock.is_some()
    }
}

/// The content of `connector.yaml`: the whole connector manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorDoc {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub healthcheck: Option<String>,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
    #[serde(default)]
    pub account_fields: Vec<AccountField>,
    #[serde(default)]
    pub functions: Vec<FunctionSpec>,
}

impl ConnectorDoc {
    /// Parses and structurally validates a `connector.yaml` document.
    /// `expected_name` is the connector folder name; the document's `name`
    /// field must equal it (a mismatch, like a mismatched profile
    /// directory, is a validation error rather than a silent rename).
    ///
    /// Validation rules, each failing as `ConnectorError::Invalid` naming
    /// the offending item:
    /// 1. `name` passes `validate_profile_name` and equals `expected_name`.
    /// 2. `version` is non-empty.
    /// 3. Function names pass `validate_snake_name` and are unique.
    /// 4. Each function is HTTP xor mock.
    /// 5. `healthcheck`, when present, names an existing function.
    /// 6. Account field names pass `validate_snake_name` and are unique.
    pub fn from_yaml(yaml: &str, expected_name: &str) -> Result<Self, ConnectorError> {
        let doc: ConnectorDoc =
            serde_yaml_ng::from_str(yaml).map_err(|e| ConnectorError::Yaml(e.to_string()))?;

        crate::profile::validate_profile_name(&doc.name).map_err(ConnectorError::Invalid)?;
        if doc.name != expected_name {
            return Err(ConnectorError::Invalid(format!(
                "connector name `{}` does not match expected `{expected_name}`",
                doc.name
            )));
        }
        if doc.version.trim().is_empty() {
            return Err(ConnectorError::Invalid(format!(
                "connector `{}` has an empty version",
                doc.name
            )));
        }

        let mut seen_functions = std::collections::HashSet::new();
        for f in &doc.functions {
            validate_snake_name(&f.name).map_err(ConnectorError::Invalid)?;
            if !seen_functions.insert(f.name.as_str()) {
                return Err(ConnectorError::Invalid(format!(
                    "duplicate function name `{}`",
                    f.name
                )));
            }

            let is_http = f.method.is_some() || f.url.is_some();
            let is_mock = f.mock.is_some();
            match (is_http, is_mock) {
                (true, true) => {
                    return Err(ConnectorError::Invalid(format!(
                        "function `{}` cannot be both an HTTP call and a mock",
                        f.name
                    )));
                }
                (false, false) => {
                    return Err(ConnectorError::Invalid(format!(
                        "function `{}` is neither an HTTP call (method + url) nor a mock",
                        f.name
                    )));
                }
                (true, false) => {
                    if f.method.is_none() || f.url.is_none() {
                        return Err(ConnectorError::Invalid(format!(
                            "function `{}` must set both `method` and `url`",
                            f.name
                        )));
                    }
                }
                (false, true) => {
                    if !f.query.is_empty() || f.body.is_some() {
                        return Err(ConnectorError::Invalid(format!(
                            "mock function `{}` must not set `query` or `body`",
                            f.name
                        )));
                    }
                }
            }
        }

        if let Some(hc) = &doc.healthcheck
            && doc.function(hc).is_none()
        {
            return Err(ConnectorError::Invalid(format!(
                "healthcheck names unknown function `{hc}`"
            )));
        }

        let mut seen_fields = std::collections::HashSet::new();
        for field in &doc.account_fields {
            validate_snake_name(&field.name).map_err(ConnectorError::Invalid)?;
            if !seen_fields.insert(field.name.as_str()) {
                return Err(ConnectorError::Invalid(format!(
                    "duplicate account field name `{}`",
                    field.name
                )));
            }
        }

        Ok(doc)
    }

    /// Looks up a function by name.
    pub fn function(&self, name: &str) -> Option<&FunctionSpec> {
        self.functions.iter().find(|f| f.name == name)
    }

    /// Names of functions marked `read_only: true`, in manifest order. Used
    /// to resolve the `functions: read_only` grant shorthand (spec section
    /// 5).
    pub fn read_only_functions(&self) -> Vec<String> {
        self.functions
            .iter()
            .filter(|f| f.read_only)
            .map(|f| f.name.clone())
            .collect()
    }

    /// Names of account fields marked `secret: true`, in manifest order.
    pub fn secret_fields(&self) -> Vec<String> {
        self.account_fields
            .iter()
            .filter(|f| f.secret)
            .map(|f| f.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JIRA_YAML: &str = r#"
name: jira
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: list_issues
    description: Search issues by JQL
    read_only: true
    method: GET
    url: "{{account.base_url}}/rest/api/3/search"
    query: { jql: "{{args.jql}}" }
    args_schema: { type: object, properties: { jql: { type: string } }, required: [jql] }
  - name: create_issue
    description: Create an issue
    method: POST
    url: "{{account.base_url}}/rest/api/3/issue"
    body: "{{args}}"
    args_schema: { type: object }
  - name: ping
    description: Fake reachability check
    mock: { status: 200, body: { ok: true } }
"#;

    #[test]
    fn parses_valid_doc_with_defaults() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        assert_eq!(doc.functions.len(), 3);
        let f = doc.function("list_issues").unwrap();
        assert!(f.read_only);
        assert_eq!(f.timeout_sec, 30);
        assert!(doc.function("ping").unwrap().is_mock());
        assert_eq!(doc.read_only_functions(), vec!["list_issues".to_string()]);
    }

    #[test]
    fn defaults_apply_to_functions_without_explicit_values() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        let create = doc.function("create_issue").unwrap();
        assert!(!create.read_only);
        assert_eq!(create.timeout_sec, 30);
        assert!(create.query.is_empty());
        assert!(!doc.function("ping").unwrap().read_only);
    }

    #[test]
    fn secret_fields_lists_secret_account_fields() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        assert_eq!(doc.secret_fields(), vec!["token".to_string()]);
    }

    #[test]
    fn name_mismatch_is_rejected() {
        let err = ConnectorDoc::from_yaml(JIRA_YAML, "not-jira").unwrap_err();
        assert!(matches!(err, ConnectorError::Invalid(_)));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = format!("{JIRA_YAML}bogus: 1\n");
        assert!(ConnectorDoc::from_yaml(&bad, "jira").is_err());
    }

    #[test]
    fn rejects_function_that_is_both_http_and_mock() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    mock: { status: 200, body: {} }\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn rejects_function_that_is_neither_http_nor_mock() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn duplicate_function_name_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n  - name: f\n    description: b\n    method: GET\n    url: http://b\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn healthcheck_naming_missing_function_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nhealthcheck: missing\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn empty_version_is_rejected() {
        let y = "name: x\nversion: \"\"\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn duplicate_account_field_name_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\n  - name: base_url\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn invalid_function_name_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: Bad_Name\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn function_name_with_hyphen_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list-issues\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn function_name_with_uppercase_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: ListIssues\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn function_name_with_underscore_is_accepted() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list_issues\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_ok());
    }

    #[test]
    fn account_field_name_with_hyphen_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base-url\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn account_field_name_with_uppercase_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: BaseUrl\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn account_field_name_with_underscore_is_accepted() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_ok());
    }

    #[test]
    fn auth_variants_reject_foreign_fields() {
        let y = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: t\n  param: extra\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

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
