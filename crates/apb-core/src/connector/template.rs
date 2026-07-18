//! Connector template renderer: minimal placeholder substitution over three
//! namespaces (spec 2026-07-18-connectors-design, section 3.1). A placeholder
//! is exactly `{{ns.name}}` (no spaces, no nesting, no filters); the bare
//! form `{{args}}` names the whole validated args object. `render_encoded`
//! percent-encodes substituted values for URL path/query context;
//! `render_raw` substitutes verbatim for auth header values and body string
//! leaves; `render_body` additionally special-cases a body that is exactly
//! `"{{args}}"` as the whole args object.

use std::collections::BTreeMap;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use super::common::{ConnectorError, validate_snake_name};

/// Percent-encoding set for substituted template values in URL path/query
/// context: every non-alphanumeric byte except the RFC 3986 unreserved marks
/// `-._~`.
const URL_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Which part of a `RenderCtx` a placeholder resolves against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Account,
    Args,
    Secret,
}

/// The values a template renders against: resolved account fields, the
/// validated call arguments, and resolved secrets. Kept as three separate
/// maps (rather than one merged map) so the secret-placement policy in
/// `validate_templates` can reason about namespaces without touching values.
pub struct RenderCtx<'a> {
    pub account: &'a BTreeMap<String, String>,
    pub args: &'a serde_json::Value,
    pub secrets: &'a BTreeMap<String, String>,
}

/// One piece of a scanned template: either a literal run of text or a
/// resolved placeholder. `scan` is the single pass shared by `placeholders`
/// (which keeps only the placeholders) and `render` (which keeps both, to
/// reassemble the output).
enum Token<'a> {
    Literal(&'a str),
    Placeholder(Namespace, String),
}

/// Scans `template` for `{{...}}` placeholders in one left-to-right pass.
/// Malformed braces (unterminated `{{`, nested `{{` before a matching `}}`)
/// and unknown namespaces or names are hard errors naming the template.
fn scan(template: &str) -> Result<Vec<Token<'_>>, ConnectorError> {
    let mut tokens = Vec::new();
    let mut rest = template;
    loop {
        match rest.find("{{") {
            None => {
                if !rest.is_empty() {
                    tokens.push(Token::Literal(rest));
                }
                break;
            }
            Some(start) => {
                if start > 0 {
                    tokens.push(Token::Literal(&rest[..start]));
                }
                let after_open = &rest[start + 2..];
                let end = after_open.find("}}").ok_or_else(|| {
                    ConnectorError::Invalid(format!(
                        "unterminated placeholder `{{{{` in template `{template}`"
                    ))
                })?;
                let inner = &after_open[..end];
                if inner.contains("{{") {
                    return Err(ConnectorError::Invalid(format!(
                        "nested placeholder braces in template `{template}`"
                    )));
                }
                let (ns, name) = parse_inner(inner, template)?;
                tokens.push(Token::Placeholder(ns, name));
                rest = &after_open[end + 2..];
            }
        }
    }
    Ok(tokens)
}

/// Parses the content between `{{` and `}}` (e.g. `account.base_url`, or the
/// bare `args`) into a namespace and name. No spaces, no extra dots: the name
/// itself follows the same snake_case shape as function and account field
/// names (`validate_snake_name`).
fn parse_inner(inner: &str, template: &str) -> Result<(Namespace, String), ConnectorError> {
    if inner.is_empty() || inner.trim() != inner {
        return Err(ConnectorError::Invalid(format!(
            "malformed placeholder `{{{{{inner}}}}}` in template `{template}`"
        )));
    }
    if inner == "args" {
        return Ok((Namespace::Args, String::new()));
    }
    let (ns_str, name) = inner.split_once('.').ok_or_else(|| {
        ConnectorError::Invalid(format!(
            "malformed placeholder `{{{{{inner}}}}}` in template `{template}`"
        ))
    })?;
    let ns = match ns_str {
        "account" => Namespace::Account,
        "args" => Namespace::Args,
        "secret" => Namespace::Secret,
        other => {
            return Err(ConnectorError::Invalid(format!(
                "unknown placeholder namespace `{other}` in template `{template}`"
            )));
        }
    };
    validate_snake_name(name).map_err(|e| {
        ConnectorError::Invalid(format!(
            "invalid placeholder name in `{{{{{inner}}}}}` in template `{template}`: {e}"
        ))
    })?;
    Ok((ns, name.to_string()))
}

/// Placeholders `{{ns.name}}` found in a template string. `{{args}}` bare
/// form is returned as `(Namespace::Args, "")`. Unknown namespace or
/// malformed braces is an error.
pub fn placeholders(template: &str) -> Result<Vec<(Namespace, String)>, ConnectorError> {
    Ok(scan(template)?
        .into_iter()
        .filter_map(|token| match token {
            Token::Placeholder(ns, name) => Some((ns, name)),
            Token::Literal(_) => None,
        })
        .collect())
}

/// Resolves a single placeholder to its scalar string value. `Args` lookup
/// only supports top-level keys of the args object; a non-scalar arg (array,
/// object, null) cannot be substituted into a string template.
fn resolve(
    ns: Namespace,
    name: &str,
    ctx: &RenderCtx,
    template: &str,
) -> Result<String, ConnectorError> {
    match ns {
        Namespace::Account => ctx.account.get(name).cloned().ok_or_else(|| {
            ConnectorError::Invalid(format!(
                "unresolved placeholder `{{{{account.{name}}}}}` in template `{template}`"
            ))
        }),
        Namespace::Secret => ctx.secrets.get(name).cloned().ok_or_else(|| {
            ConnectorError::Invalid(format!(
                "unresolved placeholder `{{{{secret.{name}}}}}` in template `{template}`"
            ))
        }),
        Namespace::Args => {
            if name.is_empty() {
                return Err(ConnectorError::Invalid(format!(
                    "`{{{{args}}}}` is only valid as a whole body value, not inside template `{template}`"
                )));
            }
            let value = ctx.args.get(name).ok_or_else(|| {
                ConnectorError::Invalid(format!(
                    "unresolved placeholder `{{{{args.{name}}}}}` in template `{template}`"
                ))
            })?;
            scalar_to_string(value, name, template)
        }
    }
}

/// Renders a JSON scalar to its template substitution string. A string arg
/// is used as-is; number and bool render via `to_string`; anything else
/// (array, object, null) is not a scalar and is a hard error.
fn scalar_to_string(
    value: &serde_json::Value,
    name: &str,
    template: &str,
) -> Result<String, ConnectorError> {
    match value {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        _ => Err(ConnectorError::Invalid(format!(
            "placeholder `{{{{args.{name}}}}}` in template `{template}` resolves to a non-scalar value"
        ))),
    }
}

/// Shared render pass: reassembles `template` from its scanned tokens,
/// substituting each placeholder's resolved value, percent-encoded when
/// `encode` is set.
fn render(template: &str, ctx: &RenderCtx, encode: bool) -> Result<String, ConnectorError> {
    let mut out = String::new();
    for token in scan(template)? {
        match token {
            Token::Literal(s) => out.push_str(s),
            Token::Placeholder(ns, name) => {
                let value = resolve(ns, &name, ctx, template)?;
                if encode {
                    out.push_str(&utf8_percent_encode(&value, URL_VALUE).to_string());
                } else {
                    out.push_str(&value);
                }
            }
        }
    }
    Ok(out)
}

/// Renders with percent-encoding of substituted values (URL path/query
/// context).
pub fn render_encoded(template: &str, ctx: &RenderCtx) -> Result<String, ConnectorError> {
    render(template, ctx, true)
}

/// Renders raw (auth header values, body string leaves).
pub fn render_raw(template: &str, ctx: &RenderCtx) -> Result<String, ConnectorError> {
    render(template, ctx, false)
}

/// Renders a body value: a top-level `"{{args}}"` string renders as a clone
/// of `ctx.args`; otherwise the JSON value is walked and every string leaf is
/// rendered with `render_raw`.
pub fn render_body(
    body: &serde_json::Value,
    ctx: &RenderCtx,
) -> Result<serde_json::Value, ConnectorError> {
    if let serde_json::Value::String(s) = body
        && s == "{{args}}"
    {
        return Ok(ctx.args.clone());
    }
    render_body_walk(body, ctx)
}

fn render_body_walk(
    value: &serde_json::Value,
    ctx: &RenderCtx,
) -> Result<serde_json::Value, ConnectorError> {
    match value {
        serde_json::Value::String(s) => Ok(serde_json::Value::String(render_raw(s, ctx)?)),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(render_body_walk(item, ctx)?);
            }
            Ok(serde_json::Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), render_body_walk(v, ctx)?);
            }
            Ok(serde_json::Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ConnectorDoc;

    fn empty_ctx<'a>(
        account: &'a BTreeMap<String, String>,
        args: &'a serde_json::Value,
        secrets: &'a BTreeMap<String, String>,
    ) -> RenderCtx<'a> {
        RenderCtx {
            account,
            args,
            secrets,
        }
    }

    #[test]
    fn encodes_url_substitutions() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"jql": "project = APB"});
        let secrets = BTreeMap::new();
        let ctx = RenderCtx {
            account: &account,
            args: &args,
            secrets: &secrets,
        };
        assert_eq!(
            render_encoded("q={{args.jql}}", &ctx).unwrap(),
            "q=project%20%3D%20APB"
        );
    }

    #[test]
    fn render_raw_leaves_value_unencoded() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"path": "a b/c"});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        assert_eq!(render_raw("{{args.path}}", &ctx).unwrap(), "a b/c");
        assert_eq!(render_encoded("{{args.path}}", &ctx).unwrap(), "a%20b%2Fc");
    }

    #[test]
    fn render_body_passes_through_bare_args() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"title": "hello", "count": 3});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        let body = serde_json::json!("{{args}}");
        assert_eq!(render_body(&body, &ctx).unwrap(), args);
    }

    #[test]
    fn render_body_renders_string_leaves() {
        let mut account = BTreeMap::new();
        account.insert("base_url".to_string(), "https://x.test".to_string());
        let args = serde_json::json!({"title": "hello"});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        let body = serde_json::json!({
            "summary": "{{args.title}}",
            "links": ["{{account.base_url}}"],
            "count": 3,
        });
        let rendered = render_body(&body, &ctx).unwrap();
        assert_eq!(
            rendered,
            serde_json::json!({
                "summary": "hello",
                "links": ["https://x.test"],
                "count": 3,
            })
        );
    }

    #[test]
    fn unresolved_placeholder_is_an_error() {
        let account = BTreeMap::new();
        let args = serde_json::json!({});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        let err = render_raw("{{account.missing}}", &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("account.missing"), "message was: {msg}");
    }

    #[test]
    fn secret_in_url_is_rejected_by_from_yaml() {
        let yaml = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: \"http://a/{{secret.token}}\"\n";
        let err = ConnectorDoc::from_yaml(yaml, "x").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("secret") && msg.contains("auth"),
            "message was: {msg}"
        );
    }

    #[test]
    fn args_in_auth_is_rejected() {
        let yaml = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{args.token}}\"\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        let err = ConnectorDoc::from_yaml(yaml, "x").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("args") && msg.contains("auth"),
            "message was: {msg}"
        );
    }

    #[test]
    fn secret_in_auth_is_accepted() {
        let yaml = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(yaml, "x").is_ok());
    }

    // -- Malformed-placeholder edge cases (fix round) --

    #[test]
    fn unclosed_braces_is_an_error() {
        let err = placeholders("{{args.x").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unterminated") && msg.contains("{{args.x"),
            "message was: {msg}"
        );
    }

    #[test]
    fn unclosed_braces_in_render_is_an_error() {
        let account = BTreeMap::new();
        let args = serde_json::json!({});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        assert!(render_raw("{{args.x", &ctx).is_err());
        assert!(render_encoded("{{args.x", &ctx).is_err());
    }

    #[test]
    fn unknown_namespace_is_an_error_naming_the_namespace() {
        // `env` is not one of the three template namespaces (account/args/
        // secret); this exercises the same code path a function template
        // (url/query/body) would hit.
        let err = placeholders("{{env.FOO}}").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown placeholder namespace") && msg.contains("env"),
            "message was: {msg}"
        );
    }

    #[test]
    fn unknown_namespace_via_from_yaml_is_rejected() {
        let yaml = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: \"http://a/{{env.FOO}}\"\n";
        let err = ConnectorDoc::from_yaml(yaml, "x").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("env"), "message was: {msg}");
    }

    #[test]
    fn malformed_placeholder_name_is_an_error() {
        let err = placeholders("{{args.Bad-Name}}").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Bad-Name") && msg.contains("invalid placeholder name"),
            "message was: {msg}"
        );
    }

    #[test]
    fn bare_args_is_an_error_as_a_string_placeholder() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"title": "hello"});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        // The whole-body form `{{args}}` is only valid as an entire body
        // value (see `render_body_passes_through_bare_args`); embedded in a
        // larger string it must fail both render paths.
        let raw_err = render_raw("{{args}}", &ctx).unwrap_err();
        assert!(
            raw_err
                .to_string()
                .contains("only valid as a whole body value"),
            "message was: {raw_err}"
        );
        let encoded_err = render_encoded("{{args}}", &ctx).unwrap_err();
        assert!(
            encoded_err
                .to_string()
                .contains("only valid as a whole body value"),
            "message was: {encoded_err}"
        );
        let embedded_err = render_raw("prefix {{args}} suffix", &ctx).unwrap_err();
        assert!(
            embedded_err
                .to_string()
                .contains("only valid as a whole body value"),
            "message was: {embedded_err}"
        );
    }

    #[test]
    fn non_scalar_object_arg_in_string_template_is_an_error() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"obj": {"a": 1}});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        let err = render_raw("{{args.obj}}", &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("non-scalar") && msg.contains("args.obj"),
            "message was: {msg}"
        );
    }

    #[test]
    fn non_scalar_array_arg_in_string_template_is_an_error() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"items": [1, 2, 3]});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        let err = render_encoded("{{args.items}}", &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("non-scalar"), "message was: {msg}");
    }

    #[test]
    fn null_arg_in_string_template_is_an_error() {
        let account = BTreeMap::new();
        let args = serde_json::json!({"n": null});
        let secrets = BTreeMap::new();
        let ctx = empty_ctx(&account, &args, &secrets);
        let err = render_raw("{{args.n}}", &ctx).unwrap_err();
        assert!(err.to_string().contains("non-scalar"));
    }
}
