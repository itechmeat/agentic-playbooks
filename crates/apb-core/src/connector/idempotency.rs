//! How a connector function that changes the world is kept from acting twice
//! (`idempotency`), and how the connector reads its own verified account
//! (`identity`).
//!
//! A function declares one of three modes:
//!
//! - `key`: the service deduplicates by a caller-chosen key apb sets on the
//!   request (a header or a top-level body field), for `window_sec` seconds.
//!   A repeat inside the window carries the same key, so the service acts at
//!   most once.
//! - `marker`: the service has no key, but its items can be listed back. apb
//!   appends a hidden marker ([`super::marker`]) to one text argument, and a
//!   later attempt reads the recent items to find it.
//! - `none`: neither is possible. An unknown outcome stays unknown until a
//!   person decides, unless the playbook allows a resend.
//!
//! The types are pure data. [`validate`] holds the manifest rules; `def`
//! hands it a [`FunctionView`] per function, so this module never imports the
//! manifest schema (which imports it).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::ConnectorError;
use super::marker::MarkerFormat;
use super::template::{Namespace, is_whole_args_body, placeholders};
use super::text_mode::TextMode;

/// A function's send-once declaration. The tag `mode` selects the variant.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Idempotency {
    Key {
        /// The request header that carries the key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        /// The top-level body field that carries the key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        /// How long the service honours a key, in seconds.
        window_sec: u64,
        /// Dot path in the response to the created item's id or link.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_ref: Option<String>,
    },
    Marker {
        /// The string argument the marker is appended to.
        arg: String,
        format: MarkerFormat,
        reconcile: ReconcileSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_ref: Option<String>,
    },
    None {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_ref: Option<String>,
    },
}

impl Idempotency {
    pub fn mode(&self) -> IdempotencyMode {
        match self {
            Idempotency::Key { .. } => IdempotencyMode::Key,
            Idempotency::Marker { .. } => IdempotencyMode::Marker,
            Idempotency::None { .. } => IdempotencyMode::None,
        }
    }

    /// The dot path to the effect reference in a success response.
    pub fn result_ref(&self) -> Option<&str> {
        match self {
            Idempotency::Key { result_ref, .. }
            | Idempotency::Marker { result_ref, .. }
            | Idempotency::None { result_ref } => result_ref.as_deref(),
        }
    }

    /// How long a key stays honoured; 0 for the modes without a key.
    pub fn window_sec(&self) -> u64 {
        match self {
            Idempotency::Key { window_sec, .. } => *window_sec,
            _ => 0,
        }
    }
}

/// The three modes, without their settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IdempotencyMode {
    Key,
    Marker,
    None,
}

impl IdempotencyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IdempotencyMode::Key => "key",
            IdempotencyMode::Marker => "marker",
            IdempotencyMode::None => "none",
        }
    }

    /// The mode a journal string names; unknown text reads as `none`, the
    /// mode that promises nothing.
    pub fn parse(s: &str) -> Self {
        match s {
            "key" => IdempotencyMode::Key,
            "marker" => IdempotencyMode::Marker,
            _ => IdempotencyMode::None,
        }
    }
}

/// How a `marker` send is looked up again: a read-only function of the same
/// connector that lists recent items, and where in its response the items,
/// their text, their author and their reference sit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileSpec {
    pub function: String,
    /// Arguments copied from the send's own arguments (`owner`, `repo`).
    #[serde(default)]
    pub args_from_send: Vec<String>,
    /// The listing's "created since" argument, filled with the intent time
    /// minus five minutes as an RFC-3339 timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_arg: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub fixed_args: serde_json::Map<String, Value>,
    /// Dot path to the list in the response; empty means the body is the
    /// list.
    #[serde(default)]
    pub items: String,
    /// Dot path to an item's text.
    pub text: String,
    /// Dot path to an item's author id.
    pub author: String,
    /// Dot path to an item's reference (link or id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_ref: Option<String>,
}

/// How the connector reads its own verified account: a read-only function
/// called with no arguments, and the dot path to the account id in its raw
/// response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySpec {
    pub function: String,
    pub path: String,
}

/// Which kind of function a [`FunctionView`] describes, as far as these
/// rules care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Http,
    Mock,
    Smtp,
    /// A native imap function: like smtp, it has no key or marker to
    /// reconcile with, so it may declare only `none`.
    Imap,
    Other,
}

impl FunctionKind {
    /// The native mail transport this kind is, by name.
    pub fn native_transport(self) -> Option<&'static str> {
        match self {
            FunctionKind::Smtp => Some("smtp"),
            FunctionKind::Imap => Some("imap"),
            FunctionKind::Http | FunctionKind::Mock | FunctionKind::Other => None,
        }
    }
}

/// The parts of one manifest function the rules read.
pub struct FunctionView<'a> {
    pub name: &'a str,
    pub kind: FunctionKind,
    pub read_only: bool,
    pub idempotency: Option<&'a Idempotency>,
    pub args_schema: Option<&'a Value>,
    pub body: Option<&'a Value>,
    pub headers: &'a BTreeMap<String, String>,
    pub text_args: &'a BTreeMap<String, TextMode>,
}

impl FunctionView<'_> {
    fn property_type(&self, arg: &str) -> Option<&str> {
        self.args_schema?
            .get("properties")?
            .get(arg)?
            .get("type")?
            .as_str()
    }

    /// The arguments its `args_schema` requires.
    fn required(&self) -> Vec<&str> {
        self.args_schema
            .and_then(|s| s.get("required"))
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    fn has_property(&self, arg: &str) -> bool {
        self.args_schema
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get(arg))
            .is_some()
    }

    /// Whether the body always renders as a JSON object, so a top-level
    /// field can carry a key: an object template, or exactly `{{args}}` with
    /// an `args_schema` of type `object` (the arguments are checked against
    /// it before any render).
    fn body_is_object(&self) -> bool {
        match self.body {
            Some(Value::Object(_)) => true,
            Some(Value::String(s)) => {
                is_whole_args_body(s)
                    && self
                        .args_schema
                        .and_then(|a| a.get("type"))
                        .and_then(Value::as_str)
                        == Some("object")
            }
            _ => false,
        }
    }

    /// Whether `arg` reaches the body, through `{{args}}` or
    /// `{{args.<arg>}}` in any string leaf.
    fn body_uses(&self, arg: &str) -> bool {
        fn walk(v: &Value, arg: &str) -> bool {
            match v {
                Value::String(s) => placeholders(s).is_ok_and(|found| {
                    found.iter().any(|(ns, name)| {
                        *ns == Namespace::Args && (name.is_empty() || name == arg)
                    })
                }),
                Value::Array(items) => items.iter().any(|i| walk(i, arg)),
                Value::Object(map) => map.values().any(|i| walk(i, arg)),
                _ => false,
            }
        }
        self.body.is_some_and(|b| walk(b, arg))
    }
}

/// The manifest rules for `idempotency` and `identity`. `functions` lists
/// every function of the connector.
pub fn validate(
    identity: Option<&IdentitySpec>,
    functions: &[FunctionView<'_>],
) -> Result<(), ConnectorError> {
    if let Some(id) = identity {
        let f = find(functions, &id.function)
            .ok_or_else(|| invalid(format!("identity names unknown function `{}`", id.function)))?;
        if !f.read_only || !matches!(f.kind, FunctionKind::Http | FunctionKind::Mock) {
            return Err(invalid(format!(
                "identity function `{}` must be a read_only HTTP or mock function",
                id.function
            )));
        }
        if !f.required().is_empty() {
            return Err(invalid(format!(
                "identity function `{}` must take no required arguments: apb calls it with none",
                id.function
            )));
        }
        if id.path.trim().is_empty() {
            return Err(invalid("identity requires a non-empty `path`".to_string()));
        }
    }
    for f in functions {
        if let Some(idem) = f.idempotency {
            check_function(f, idem, identity.is_some(), functions)?;
        }
    }
    Ok(())
}

fn find<'a, 'b>(functions: &'a [FunctionView<'b>], name: &str) -> Option<&'a FunctionView<'b>> {
    functions.iter().find(|f| f.name == name)
}

fn check_function(
    f: &FunctionView<'_>,
    idem: &Idempotency,
    has_identity: bool,
    functions: &[FunctionView<'_>],
) -> Result<(), ConnectorError> {
    let name = f.name;
    if f.read_only {
        return Err(invalid(format!(
            "function `{name}` is read_only; idempotency applies only to functions that change something"
        )));
    }
    match (f.kind, idem) {
        (FunctionKind::Http | FunctionKind::Mock, _) => {}
        (FunctionKind::Smtp | FunctionKind::Imap, Idempotency::None { .. }) => {}
        (FunctionKind::Smtp | FunctionKind::Imap, _) => {
            let transport = f.kind.native_transport().unwrap_or("native");
            return Err(invalid(format!(
                "{transport} function `{name}` may only declare idempotency mode `none`"
            )));
        }
        (FunctionKind::Other, _) => {
            return Err(invalid(format!(
                "function `{name}` declares idempotency, which only HTTP, mock, smtp and imap functions support"
            )));
        }
    }
    match idem {
        Idempotency::Key {
            header,
            field,
            window_sec,
            ..
        } => check_key(f, header.as_deref(), field.as_deref(), *window_sec),
        Idempotency::Marker { arg, reconcile, .. } => {
            check_marker(f, arg, reconcile, has_identity, functions)
        }
        Idempotency::None { .. } => Ok(()),
    }
}

fn check_key(
    f: &FunctionView<'_>,
    header: Option<&str>,
    field: Option<&str>,
    window_sec: u64,
) -> Result<(), ConnectorError> {
    let name = f.name;
    match (header, field) {
        (Some(h), None) => {
            if h.trim().is_empty() {
                return Err(invalid(format!(
                    "function `{name}`: idempotency header is empty"
                )));
            }
            if f.headers.keys().any(|k| k.eq_ignore_ascii_case(h)) {
                return Err(invalid(format!(
                    "function `{name}`: idempotency header `{h}` collides with a header the function sets"
                )));
            }
        }
        (None, Some(field)) => {
            if field.trim().is_empty() {
                return Err(invalid(format!(
                    "function `{name}`: idempotency field is empty"
                )));
            }
            if f.has_property(field) {
                return Err(invalid(format!(
                    "function `{name}`: idempotency field `{field}` is an argument; apb sets that field itself"
                )));
            }
            if !f.body_is_object() {
                return Err(invalid(format!(
                    "function `{name}`: idempotency field `{field}` needs a body that renders as a JSON object: an object, or `{{{{args}}}}` with an object args_schema"
                )));
            }
        }
        _ => {
            return Err(invalid(format!(
                "function `{name}`: idempotency mode `key` needs exactly one of `header` or `field`"
            )));
        }
    }
    if window_sec == 0 {
        return Err(invalid(format!(
            "function `{name}`: idempotency window_sec must be at least 1"
        )));
    }
    Ok(())
}

fn check_marker(
    f: &FunctionView<'_>,
    arg: &str,
    spec: &ReconcileSpec,
    has_identity: bool,
    functions: &[FunctionView<'_>],
) -> Result<(), ConnectorError> {
    let name = f.name;
    if f.property_type(arg) != Some("string") {
        return Err(invalid(format!(
            "function `{name}`: marker arg `{arg}` must be a string property of its args_schema"
        )));
    }
    if !f.body_uses(arg) {
        return Err(invalid(format!(
            "function `{name}`: marker arg `{arg}` is not used in the body"
        )));
    }
    if f.text_args.get(arg) == Some(&TextMode::Literal) {
        return Err(invalid(format!(
            "function `{name}`: marker arg `{arg}` has text mode `literal`, which would escape the marker"
        )));
    }
    if !has_identity {
        return Err(invalid(format!(
            "function `{name}` declares a marker, which needs a connector-level `identity` block"
        )));
    }
    let r = find(functions, &spec.function).ok_or_else(|| {
        invalid(format!(
            "function `{name}`: reconcile names unknown function `{}`",
            spec.function
        ))
    })?;
    if !r.read_only || r.kind != FunctionKind::Http {
        return Err(invalid(format!(
            "function `{name}`: reconcile function `{}` must be a read_only HTTP function",
            spec.function
        )));
    }
    for copied in &spec.args_from_send {
        if !f.has_property(copied) || !r.has_property(copied) {
            return Err(invalid(format!(
                "function `{name}`: reconcile arg `{copied}` must be an argument of both `{name}` and `{}`",
                spec.function
            )));
        }
    }
    if let Some(fixed) = spec.fixed_args.keys().find(|k| !r.has_property(k)) {
        return Err(invalid(format!(
            "function `{name}`: reconcile fixed arg `{fixed}` is not an argument of `{}`",
            spec.function
        )));
    }
    let given = |arg: &str| {
        spec.args_from_send.iter().any(|a| a == arg)
            || spec.since_arg.as_deref() == Some(arg)
            || spec.fixed_args.contains_key(arg)
    };
    if let Some(missing) = r.required().into_iter().find(|arg| !given(arg)) {
        return Err(invalid(format!(
            "function `{name}`: reconcile does not give `{}` its required argument `{missing}`",
            spec.function
        )));
    }
    if let Some(since) = &spec.since_arg
        && r.property_type(since) != Some("string")
    {
        return Err(invalid(format!(
            "function `{name}`: reconcile since_arg `{since}` must be a string argument of `{}`",
            spec.function
        )));
    }
    if spec.text.trim().is_empty() || spec.author.trim().is_empty() {
        return Err(invalid(format!(
            "function `{name}`: reconcile needs non-empty `text` and `author` paths"
        )));
    }
    Ok(())
}

fn invalid(msg: String) -> ConnectorError {
    ConnectorError::Invalid(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse_with_their_settings() {
        let key: Idempotency =
            yaml_serde::from_str("{ mode: key, field: nonce, window_sec: 120, result_ref: id }")
                .unwrap();
        assert_eq!(key.mode(), IdempotencyMode::Key);
        assert_eq!(key.window_sec(), 120);
        assert_eq!(key.result_ref(), Some("id"));
        let none: Idempotency = yaml_serde::from_str("{ mode: none }").unwrap();
        assert_eq!(none.mode(), IdempotencyMode::None);
        assert!(yaml_serde::from_str::<Idempotency>("{ mode: none, bogus: 1 }").is_err());
        assert!(yaml_serde::from_str::<Idempotency>("{ mode: maybe }").is_err());
    }

    #[test]
    fn journal_mode_strings_round_trip() {
        for m in [
            IdempotencyMode::Key,
            IdempotencyMode::Marker,
            IdempotencyMode::None,
        ] {
            assert_eq!(IdempotencyMode::parse(m.as_str()), m);
        }
        assert_eq!(IdempotencyMode::parse("future"), IdempotencyMode::None);
    }
}
