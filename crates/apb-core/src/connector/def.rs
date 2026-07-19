//! Connector definition schema: the content of `connector.yaml` (spec
//! 2026-07-18-connectors-design, section 3.1).
//!
//! A connector links a playbook node to an external HTTP service through a
//! declarative manifest: an auth block, the account fields the connector
//! needs, and a set of callable functions (HTTP or mock). Besides the
//! structural checks below, `from_yaml` also runs `validate_templates`,
//! which enforces the secret-placement policy over the template placeholders
//! parsed by `super::template` (e.g. `{{secret.*}}` only allowed in `auth`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::common::{ConnectorError, validate_snake_name};

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
    Path {
        value_template: String,
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

/// One authored example call for a function: `args` renders into the agent
/// instruction block after the description, and the validator checks it
/// against the function's `args_schema` (spec 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExampleSpec {
    pub args: serde_json::Value,
    pub note: String,
}

fn default_timeout() -> u64 {
    30
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
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub args_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub examples: Vec<ExampleSpec>,
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

        validate_templates(&doc)?;
        validate_examples(&doc)?;

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

/// The two `AccountField` names, split by secrecy: non-secret names that
/// `{{account.*}}` placeholders may reference, and secret names that
/// `{{secret.*}}` placeholders may reference.
struct FieldNames<'a> {
    account: std::collections::HashSet<&'a str>,
    secret: std::collections::HashSet<&'a str>,
}

impl<'a> FieldNames<'a> {
    fn from_fields(fields: &'a [AccountField]) -> Self {
        let mut account = std::collections::HashSet::new();
        let mut secret = std::collections::HashSet::new();
        for field in fields {
            if field.secret {
                secret.insert(field.name.as_str());
            } else {
                account.insert(field.name.as_str());
            }
        }
        FieldNames { account, secret }
    }

    /// Checks that an `Account` or `Secret` placeholder names a declared
    /// account field of the matching secrecy. `Args` is always allowed here;
    /// callers reject `Args` themselves where it is out of place (auth).
    fn check(
        &self,
        ns: crate::connector::template::Namespace,
        name: &str,
    ) -> Result<(), ConnectorError> {
        use crate::connector::template::Namespace;
        match ns {
            Namespace::Account => {
                if !self.account.contains(name) {
                    return Err(ConnectorError::Invalid(format!(
                        "placeholder `{{{{account.{name}}}}}` names an unknown or secret account field"
                    )));
                }
            }
            Namespace::Secret => {
                if !self.secret.contains(name) {
                    return Err(ConnectorError::Invalid(format!(
                        "placeholder `{{{{secret.{name}}}}}` names an unknown or non-secret account field"
                    )));
                }
            }
            Namespace::Args | Namespace::Auth => {}
        }
        Ok(())
    }
}

/// Validates every template placeholder in the connector against the
/// secret-placement policy: `{{secret.*}}` is allowed only inside `auth`
/// (any occurrence in a function's `url`, `query`, or `body` is rejected);
/// `{{args.*}}` and bare `{{args}}` are not allowed inside `auth`; every
/// `{{account.*}}` / `{{secret.*}}` placeholder must name a declared account
/// field of the matching secrecy. Called at the end of `ConnectorDoc::from_yaml`.
pub fn validate_templates(doc: &ConnectorDoc) -> Result<(), ConnectorError> {
    use crate::connector::template::{Namespace, placeholders};

    let fields = FieldNames::from_fields(&doc.account_fields);
    let is_path_auth = matches!(doc.auth, Some(AuthSpec::Path { .. }));

    for f in &doc.functions {
        if let Some(url) = &f.url {
            let mut auth_markers = 0;
            for (ns, name) in placeholders(url)? {
                if ns == Namespace::Auth {
                    auth_markers += 1;
                    continue;
                }
                reject_secret(ns, &format!("function `{}` url", f.name))?;
                fields.check(ns, &name)?;
            }
            if is_path_auth && auth_markers != 1 {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` url must contain the `{{{{auth}}}}` placeholder exactly once for path auth (found {auth_markers})",
                    f.name
                )));
            }
            if !is_path_auth && auth_markers > 0 {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` url uses `{{{{auth}}}}` but the connector does not use path auth",
                    f.name
                )));
            }
        }
        for value in f.query.values() {
            for (ns, name) in placeholders(value)? {
                reject_auth(ns, &format!("function `{}` query", f.name))?;
                reject_secret(ns, &format!("function `{}` query", f.name))?;
                fields.check(ns, &name)?;
            }
        }
        for value in f.headers.values() {
            for (ns, name) in placeholders(value)? {
                reject_auth(ns, &format!("function `{}` headers", f.name))?;
                reject_secret(ns, &format!("function `{}` headers", f.name))?;
                fields.check(ns, &name)?;
            }
        }
        if let Some(body) = &f.body {
            validate_body_templates(body, &f.name, &fields)?;
        }
    }

    if let Some(auth) = &doc.auth {
        for template in auth_templates(auth) {
            for (ns, name) in placeholders(template)? {
                reject_auth(ns, "auth template")?;
                if ns == Namespace::Args {
                    return Err(ConnectorError::Invalid(
                        "args placeholders are not allowed in auth templates".to_string(),
                    ));
                }
                fields.check(ns, &name)?;
            }
        }
    }

    Ok(())
}

/// Validates that every function example's `args` conform to that function's
/// `args_schema` (spec 4.4), so an example cannot drift from the schema
/// silently. Functions without an `args_schema` are skipped (nothing to
/// check against).
fn validate_examples(doc: &ConnectorDoc) -> Result<(), ConnectorError> {
    for f in &doc.functions {
        if f.examples.is_empty() {
            continue;
        }
        let Some(schema) = &f.args_schema else {
            continue;
        };
        let validator = jsonschema::validator_for(schema).map_err(|e| {
            ConnectorError::Invalid(format!(
                "function `{}` args_schema is not a valid JSON schema: {e}",
                f.name
            ))
        })?;
        for (i, ex) in f.examples.iter().enumerate() {
            if let Some(err) = validator.iter_errors(&ex.args).next() {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` example {} args fail its args_schema: {err}",
                    f.name,
                    i + 1
                )));
            }
        }
    }
    Ok(())
}

/// Errors if `ns` is `Auth`: the `{{auth}}` marker is confined to a
/// function `url`, so any occurrence found while walking a query, body, or
/// auth template is a hard error naming where it was found (spec 4.3).
fn reject_auth(
    ns: crate::connector::template::Namespace,
    where_: &str,
) -> Result<(), ConnectorError> {
    if ns == crate::connector::template::Namespace::Auth {
        return Err(ConnectorError::Invalid(format!(
            "the `{{{{auth}}}}` placeholder is allowed only in a function url ({where_})"
        )));
    }
    Ok(())
}

/// Errors if `ns` is `Secret`: secret placeholders are confined to `auth`, so
/// any occurrence found while walking a function's `url`/`query`/`body` is a
/// hard error naming where it was found.
fn reject_secret(
    ns: crate::connector::template::Namespace,
    where_: &str,
) -> Result<(), ConnectorError> {
    if ns == crate::connector::template::Namespace::Secret {
        return Err(ConnectorError::Invalid(format!(
            "secret placeholders are allowed only in auth ({where_})"
        )));
    }
    Ok(())
}

/// Walks a `body` JSON value, validating the placeholders in every string
/// leaf (arrays and objects recurse; non-string scalars carry no
/// placeholders).
fn validate_body_templates(
    value: &serde_json::Value,
    function_name: &str,
    fields: &FieldNames,
) -> Result<(), ConnectorError> {
    use crate::connector::template::placeholders;

    match value {
        serde_json::Value::String(s) => {
            for (ns, name) in placeholders(s)? {
                reject_auth(ns, &format!("function `{function_name}` body"))?;
                reject_secret(ns, &format!("function `{function_name}` body"))?;
                fields.check(ns, &name)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate_body_templates(item, function_name, fields)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                validate_body_templates(v, function_name, fields)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The template strings carried by an `AuthSpec`, in a uniform list
/// regardless of variant.
fn auth_templates(auth: &AuthSpec) -> Vec<&str> {
    match auth {
        AuthSpec::Header { value_template, .. } => vec![value_template.as_str()],
        AuthSpec::Query { value_template, .. } => vec![value_template.as_str()],
        AuthSpec::Basic {
            username_template,
            password_template,
        } => vec![username_template.as_str(), password_template.as_str()],
        AuthSpec::Path { value_template } => vec![value_template.as_str()],
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
    fn examples_validate_against_args_schema() {
        let ok = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }\n    examples:\n      - args: { title: hi }\n        note: minimal create\n";
        assert!(ConnectorDoc::from_yaml(ok, "x").is_ok());

        let bad = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }\n    examples:\n      - args: { nope: 1 }\n        note: missing required title\n";
        let err = ConnectorDoc::from_yaml(bad, "x").unwrap_err().to_string();
        assert!(err.contains("f") && err.contains("example"), "was: {err}");
    }

    #[test]
    fn headers_forbid_secret_and_auth_allow_account_and_args() {
        let ok = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/x\"\n    headers:\n      X-Api-Version: \"2024-01\"\n      X-From: \"{{account.base_url}}\"\n      X-Q: \"{{args.q}}\"\n";
        assert!(ConnectorDoc::from_yaml(ok, "x").is_ok());

        let sec = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    headers:\n      X-Leak: \"{{secret.token}}\"\n";
        let err = ConnectorDoc::from_yaml(sec, "x").unwrap_err().to_string();
        assert!(err.contains("secret") && err.contains("auth"), "was: {err}");
    }

    #[test]
    fn path_auth_requires_auth_placeholder_exactly_once() {
        let ok = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/getMe\"\n";
        assert!(ConnectorDoc::from_yaml(ok, "t").is_ok());

        let missing = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    method: GET\n    url: \"{{account.base_url}}/getMe\"\n";
        let err = ConnectorDoc::from_yaml(missing, "t")
            .unwrap_err()
            .to_string();
        assert!(err.contains("get_me") && err.contains("auth"), "was: {err}");

        let twice = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/{{auth}}/getMe\"\n";
        assert!(ConnectorDoc::from_yaml(twice, "t").is_err());
    }

    #[test]
    fn auth_placeholder_without_path_auth_is_rejected() {
        let y = "name: t\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/x\"\n";
        let err = ConnectorDoc::from_yaml(y, "t").unwrap_err().to_string();
        assert!(err.contains("auth") && err.contains("path"), "was: {err}");
    }

    #[test]
    fn auth_placeholder_in_query_or_body_is_rejected() {
        let y = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/x\"\n    query: { k: \"{{auth}}\" }\n";
        assert!(ConnectorDoc::from_yaml(y, "t").is_err());
    }

    #[test]
    fn auth_variants_reject_foreign_fields() {
        let y = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: t\n  param: extra\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }
}
