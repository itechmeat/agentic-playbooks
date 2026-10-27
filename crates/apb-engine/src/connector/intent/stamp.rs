//! Preparing the arguments and the rendered request of a send: text modes,
//! the neutralizing of forged markers, and the stamp that makes the send
//! recognisable (the marker in a text argument, or the key in a header or a
//! body field).
//!
//! Neutralizing runs for every function that is not `read_only`, stamped or
//! not (dry-run, the dashboard playground and contract tests included):
//! this is the one place that removes forged markers from what apb sends.

use std::borrow::Cow;
use std::collections::BTreeMap;

use apb_core::connector::def::FunctionSpec;
use apb_core::connector::idempotency::Idempotency;
use apb_core::connector::marker::{self, MarkerFields};
use apb_core::connector::text_mode::apply_text_args;
use serde_json::Value;

/// What a dry-run shows where the marker will go: a preview never carries a
/// real nonce, so none reaches the agent through it.
pub const MARKER_PREVIEW: &str = "[apb adds a hidden send marker here]";

/// What a dry-run shows as the key.
pub const KEY_PREVIEW: &str = "(set at send)";

/// How a request is stamped.
#[derive(Debug, Clone, Copy)]
pub enum Stamp<'a> {
    /// A real send: the nonce, and for a `marker` function the marker
    /// fields.
    Send {
        nonce: &'a str,
        marker: Option<&'a MarkerFields>,
    },
    /// A dry-run: placeholders instead of a nonce.
    Preview,
}

/// The arguments a body renders from: text modes applied, then forged
/// markers neutralized (functions that change something only), then the
/// marker appended to its argument when the function declares one.
pub fn prepare_body_args<'a>(
    function: &FunctionSpec,
    args: &'a Value,
    stamp: Option<&Stamp<'_>>,
) -> Cow<'a, Value> {
    let moded = apply_text_args(&function.text_args, args);
    if function.read_only {
        return moded;
    }
    let rewritten = match marker::neutralize_value(&moded) {
        Cow::Owned(v) => Some(v),
        Cow::Borrowed(_) => None,
    };
    let neutral = rewritten.map_or(moded, Cow::Owned);
    let (Some(Idempotency::Marker { arg, format, .. }), Some(stamp)) =
        (&function.idempotency, stamp)
    else {
        return neutral;
    };
    let tag = match stamp {
        Stamp::Send {
            marker: Some(fields),
            ..
        } => marker::render(*format, fields),
        Stamp::Send { marker: None, .. } => return neutral,
        Stamp::Preview => MARKER_PREVIEW.to_string(),
    };
    let mut out = neutral.into_owned();
    if let Value::Object(map) = &mut out {
        let text = map.get(arg).and_then(Value::as_str);
        let stamped = marker::append(text, &tag);
        map.insert(arg.clone(), Value::String(stamped));
    }
    Cow::Owned(out)
}

/// Sets the key of a `key` function on the rendered request: the header, or
/// the top-level body field. Does nothing for the other modes or without a
/// stamp. A body that did not render as a JSON object has no field to carry
/// the key: that is an error, never a send without its key.
pub fn apply_key(
    function: &FunctionSpec,
    stamp: Option<&Stamp<'_>>,
    body: &mut Option<Value>,
    headers: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let (Some(Idempotency::Key { header, field, .. }), Some(stamp)) =
        (&function.idempotency, stamp)
    else {
        return Ok(());
    };
    let value = match stamp {
        Stamp::Send { nonce, .. } => nonce.to_string(),
        Stamp::Preview => KEY_PREVIEW.to_string(),
    };
    match (header, field, body) {
        (Some(name), _, _) => {
            headers.insert(name.clone(), value);
        }
        (None, Some(name), Some(Value::Object(map))) => {
            map.insert(name.clone(), Value::String(value));
        }
        (None, Some(name), _) => {
            return Err(format!(
                "function `{}` carries its idempotency key in the body field `{name}`, but its body did not render as a JSON object",
                function.name
            ));
        }
        (None, None, _) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::connector::def::ConnectorDoc;
    use serde_json::json;

    const DOC: &str = r#"
name: x
version: 0.1.0
identity: { function: me, path: login }
functions:
  - name: me
    description: d
    read_only: true
    method: GET
    url: "https://api.example.com/me"
  - name: list
    description: d
    read_only: true
    method: GET
    url: "https://api.example.com/items"
    args_schema: { type: object, properties: { since: { type: string } } }
  - name: post
    description: d
    method: POST
    url: "https://api.example.com/items"
    body: "{{args}}"
    text_args: { text: markdown_no_commands }
    args_schema: { type: object, properties: { text: { type: string }, meta: { type: object } } }
    idempotency:
      mode: marker
      arg: text
      format: html_comment
      reconcile: { function: list, since_arg: since, text: text, author: user }
  - name: keyed
    description: d
    method: POST
    url: "https://api.example.com/items"
    headers: { Accept: application/json }
    idempotency: { mode: key, header: Idempotency-Key, window_sec: 60 }
  - name: noted
    description: d
    method: POST
    url: "https://api.example.com/notes"
    body: { content: "{{args.content}}" }
    idempotency: { mode: key, field: nonce, window_sec: 60 }
  - name: read
    description: d
    read_only: true
    method: POST
    url: "https://api.example.com/search"
    body: "{{args}}"
"#;

    fn function(name: &str) -> FunctionSpec {
        ConnectorDoc::from_yaml(DOC, "x")
            .unwrap()
            .function(name)
            .unwrap()
            .clone()
    }

    fn fields() -> MarkerFields {
        MarkerFields {
            run: "r".into(),
            node: "n".into(),
            attempt: Some(1),
            nonce: "abc".into(),
        }
    }

    const MARKER: &str = "<!-- apb-intent v1 run=r node=n attempt=1 nonce=abc -->";

    #[test]
    fn the_marker_follows_the_text_mode_and_the_neutralized_text() {
        let f = function("post");
        let fields = fields();
        let stamp = Stamp::Send {
            nonce: "abc",
            marker: Some(&fields),
        };
        let args = json!({ "text": "/close apb-intent here", "meta": { "k": ["APB-INTENT"] } });
        let out = prepare_body_args(&f, &args, Some(&stamp)).into_owned();
        assert_eq!(
            out["text"],
            format!("\\/close apb-quoted-intent here\n\n{MARKER}")
        );
        assert_eq!(out["meta"]["k"][0], "apb-quoted-intent");
    }

    #[test]
    fn an_absent_marker_arg_becomes_the_marker_alone() {
        let f = function("post");
        let fields = fields();
        let stamp = Stamp::Send {
            nonce: "abc",
            marker: Some(&fields),
        };
        let args = json!({});
        let out = prepare_body_args(&f, &args, Some(&stamp)).into_owned();
        assert_eq!(out["text"], MARKER);
    }

    #[test]
    fn a_preview_shows_placeholders_and_no_nonce() {
        let args = json!({ "text": "hi" });
        let out = prepare_body_args(&function("post"), &args, Some(&Stamp::Preview));
        assert_eq!(out["text"], format!("hi\n\n{MARKER_PREVIEW}"));
        let noted = function("noted");
        let mut body = Some(json!({ "content": "hi" }));
        let mut headers = BTreeMap::new();
        apply_key(&noted, Some(&Stamp::Preview), &mut body, &mut headers).unwrap();
        assert_eq!(body.unwrap()["nonce"], KEY_PREVIEW);
    }

    #[test]
    fn a_key_goes_into_its_header_or_its_body_field() {
        let stamp = Stamp::Send {
            nonce: "abc",
            marker: None,
        };
        let mut body = None;
        let mut headers = BTreeMap::new();
        apply_key(&function("keyed"), Some(&stamp), &mut body, &mut headers).unwrap();
        assert_eq!(
            headers.get("Idempotency-Key").map(String::as_str),
            Some("abc")
        );
        assert!(body.is_none());

        let mut body = Some(json!({ "content": "hi" }));
        let mut headers = BTreeMap::new();
        apply_key(&function("noted"), Some(&stamp), &mut body, &mut headers).unwrap();
        assert_eq!(body.unwrap()["nonce"], "abc");
        assert!(headers.is_empty());
    }

    /// A body that did not render as an object cannot carry the key: the
    /// send is refused rather than sent without it.
    #[test]
    fn a_key_field_without_an_object_body_is_an_error() {
        let stamp = Stamp::Send {
            nonce: "abc",
            marker: None,
        };
        for rendered in [None, Some(json!("text")), Some(json!([1]))] {
            let mut body = rendered;
            let mut headers = BTreeMap::new();
            let err =
                apply_key(&function("noted"), Some(&stamp), &mut body, &mut headers).unwrap_err();
            assert!(err.contains("did not render as a JSON object"));
        }
    }

    /// Neutralizing applies to every write, stamped or not, and never to a
    /// read.
    #[test]
    fn writes_are_neutralized_without_a_stamp_and_reads_are_left_alone() {
        let args = json!({ "content": "x apb-intent" });
        let out = prepare_body_args(&function("noted"), &args, None);
        assert_eq!(out["content"], "x apb-quoted-intent");
        let read = prepare_body_args(&function("read"), &args, None);
        assert!(matches!(read, Cow::Borrowed(_)));
    }
}
