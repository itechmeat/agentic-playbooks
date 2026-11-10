//! One response shaped: the raw bytes as they came off the wire
//! ([`RawResponse`]), the status mapping and the false-success
//! reclassification ([`shape`]), and the media-type guard
//! ([`check_content_type`]). Split out of `HttpCall::send_raw` so the page
//! walk (D2) and the offline contract runner drive the same half.

use crate::connector::call::response::{map_status, reclassify_error_when};
use apb_core::connector::def::ErrorWhen;
use serde_json::Value;

use crate::connector::completeness::BodyIncomplete;
use crate::connector::result::{CallError, CallErrorCode, CallOk};

/// One response as read off the wire, before any shaping. `bytes` is what
/// `read_body` actually read: it counts against the paging total.
pub(crate) struct RawResponse {
    pub status: u16,
    /// The response's Content-Type header; read by the `content_types`
    /// guard (D5) and by the contract runner.
    #[allow(dead_code)]
    pub content_type: Option<String>,
    pub link: Option<String>,
    pub retry_after: Option<u64>,
    pub body: Value,
    pub incomplete: Option<BodyIncomplete>,
    pub bytes: usize,
}

/// The rules `shape` applies. The pick stays with the caller: a paginated
/// call projects once, after every page merged.
pub(crate) struct ShapeRules<'a> {
    pub read_only: bool,
    pub error_when: Option<&'a ErrorWhen>,
    /// The function's `decode_base64url` paths, decoded after
    /// `error_when` on a 2xx (D4).
    pub decode_paths: &'a [String],
}

/// Maps one raw response into the result taxonomy: `map_status`, then the
/// connector's false-success predicate on a 2xx. No projection, no
/// redaction. The `link` value rides on the shaped result when the caller
/// sets it.
pub(crate) fn shape(resp: RawResponse, rules: &ShapeRules<'_>) -> Result<CallOk, CallError> {
    let RawResponse {
        status,
        body,
        incomplete,
        retry_after,
        link,
        ..
    } = resp;
    let mut mapped = map_status(status, body, incomplete, retry_after, rules.read_only);
    if let Ok(CallOk::Http { body, .. }) = &mapped
        && let Some(rule) = rules.error_when
        && let Some(err) = reclassify_error_when(rule, body, status)
    {
        mapped = Err(err);
    }
    if let Ok(CallOk::Http {
        link: link_slot,
        body,
        ..
    }) = &mut mapped
    {
        *link_slot = link;
        if !rules.decode_paths.is_empty() {
            decode_paths(body, rules.decode_paths);
        }
    }
    mapped
}

/// The media-type guard (D5): a 2xx answer whose media type matches none of
/// `allowed` fails `service` before its body is used. An entry ending in
/// `/` is a type prefix (`text/`); a missing header fails the same way.
pub(crate) fn check_content_type(
    function: &str,
    header: Option<&str>,
    allowed: &[String],
) -> Result<(), CallError> {
    if allowed.is_empty() {
        return Ok(());
    }
    let Some(raw) = header else {
        return Err(CallError::new(
            CallErrorCode::Service,
            format!(
                "the response has no content type; `{function}` reads only {}",
                allowed.join(", ")
            ),
        ));
    };
    let media = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let matched = allowed.iter().any(|entry| {
        // A prefix entry keeps its slash, so `text/` matches `text/plain`
        // from the start and never `textual/x`.
        if entry.ends_with('/') {
            media.starts_with(entry.as_str())
        } else {
            media == entry.as_str()
        }
    });
    if matched {
        return Ok(());
    }
    Err(CallError::new(
        CallErrorCode::Service,
        format!(
            "the response is `{media}`; `{function}` reads only {}",
            allowed.join(", ")
        ),
    ))
}

/// Decodes each string leaf at a `decode_base64url` path (D4): padded or
/// not, URL-safe alphabet. Valid UTF-8 replaces the value, capped at 256
/// KiB on a char boundary with a sibling `"<key>_truncated": true`; bytes
/// that are not UTF-8 replace it with `null` and a sibling
/// `"<key>_binary": true`; a string that is not base64url stays as is.
/// Array mapping like `response_pick`: a path through an array applies to
/// every element. Pure; runs on a 2xx after `error_when`.
pub(crate) fn decode_paths(body: &mut Value, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    for path in paths {
        let segments: Vec<&str> = path.split('.').collect();
        decode_at(body, &segments);
    }
}

/// What decoding a string leaf produced.
enum Decoded {
    /// Not base64url: the value stays.
    Untouched,
    Text(String),
    Truncated(String),
    Binary,
}

/// The sibling flag of a decoded leaf.
fn flag_key(_key: &str, decoded: &Decoded) -> Option<(&'static str, bool)> {
    match decoded {
        Decoded::Untouched => None,
        Decoded::Text(_) => None,
        Decoded::Truncated(_) => Some(("truncated", true)),
        Decoded::Binary => Some(("binary", true)),
    }
}

fn decode_at(value: &mut Value, segments: &[&str]) {
    let Some((first, rest)) = segments.split_first() else {
        return;
    };
    match value {
        Value::Object(map) => {
            let Some(child) = map.get_mut(*first) else {
                return;
            };
            if rest.is_empty() {
                // The child is the leaf; decode in place and set the flag
                // beside it, under the same parent map.
                if matches!(child, Value::String(_)) {
                    let taken = std::mem::replace(child, Value::Null);
                    let Value::String(s) = taken else {
                        unreachable!();
                    };
                    let decoded = decode_string(&s);
                    let flag = flag_key(first, &decoded);
                    *child = match decoded {
                        Decoded::Untouched => Value::String(s),
                        Decoded::Text(t) => Value::String(t),
                        Decoded::Truncated(t) => Value::String(t),
                        Decoded::Binary => Value::Null,
                    };
                    if let Some((suffix, on)) = flag {
                        map.insert(format!("{first}_{suffix}"), Value::Bool(on));
                    }
                }
                return;
            }
            decode_at(child, rest);
        }
        Value::Array(items) => {
            // An array consumes no segment: every element matches the same
            // remaining path.
            for item in items {
                decode_at(item, segments);
            }
        }
        _ => {}
    }
}

/// Decodes one base64url string.
fn decode_string(s: &str) -> Decoded {
    use base64::Engine as _;
    // Providers differ on padding (Gmail sends unpadded); the canonical
    // check either engine applies is the same.
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(s)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s));
    let Ok(bytes) = bytes else {
        return Decoded::Untouched;
    };
    match String::from_utf8(bytes) {
        Ok(mut text) => {
            const DECODE_CAP: usize = 256 * 1024;
            if text.len() > DECODE_CAP {
                let mut cut = DECODE_CAP;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                Decoded::Truncated(text)
            } else {
                Decoded::Text(text)
            }
        }
        Err(_) => Decoded::Binary,
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use serde_json::json;

    // -- decode_base64url (D4) --

    #[test]
    fn decode_paths_decodes_padded_and_unpadded_utf8() {
        let mut body = json!({
            "payload": {
                "body": { "data": "SGVsbG8=" },
                "parts": [ { "body": { "data": "V29ybGQ" } }, { "body": { "data": "aGVsbG8=" } } ],
                "keep": "SGVsbG8="
            }
        });
        let spec_paths = vec![
            "payload.body.data".to_string(),
            "payload.parts.body.data".to_string(),
        ];
        decode_paths(&mut body, &spec_paths);
        assert_eq!(body["payload"]["body"]["data"], json!("Hello"));
        assert_eq!(body["payload"]["parts"][0]["body"]["data"], json!("World"));
        assert_eq!(body["payload"]["parts"][1]["body"]["data"], json!("hello"));
        // A path not declared stays as is.
        assert_eq!(body["payload"]["keep"], json!("SGVsbG8="));
    }

    #[test]
    fn decode_paths_leaves_non_base64_strings_as_is() {
        let mut body = json!({ "data": "not base64 at all!" });
        decode_paths(&mut body, &["data".to_string()]);
        assert_eq!(body["data"], json!("not base64 at all!"));
    }

    #[test]
    fn decode_paths_marks_binary_and_truncates() {
        // Bytes that are not UTF-8.
        use base64::Engine as _;
        let binary = base64::engine::general_purpose::URL_SAFE.encode([0xff, 0xfe, 0x00]);
        let mut body = json!({ "data": binary, "list": [ { "data": binary } ] });
        decode_paths(&mut body, &["data".to_string(), "list.data".to_string()]);
        assert_eq!(body["data"], Value::Null);
        assert_eq!(body["data_binary"], json!(true));
        assert_eq!(body["list"][0]["data"], Value::Null);
        assert_eq!(body["list"][0]["data_binary"], json!(true));

        // Over the 256 KiB cap: cut on a char boundary, flag it.
        let long = "a".repeat(300_000);
        let encoded = base64::engine::general_purpose::URL_SAFE.encode(long.as_bytes());
        let mut body = json!({ "data": encoded });
        decode_paths(&mut body, &["data".to_string()]);
        let out = body["data"].as_str().unwrap();
        assert!(out.chars().count() <= 262_144, "{}", out.chars().count());
        assert_eq!(body["data_truncated"], json!(true));
        // A multi-byte char is not cut in half.
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn decode_paths_arrays_and_missing_paths_are_safe() {
        let mut body = json!({ "items": [ { "data": "not base64" } ], "other": 1 });
        decode_paths(
            &mut body,
            &["items.data".to_string(), "missing.deep".to_string()],
        );
        // A non-base64 leaf stays; the walk over a missing path is a no-op.
        assert_eq!(body["items"][0]["data"], json!("not base64"));
        assert_eq!(body["other"], json!(1));
    }
}

#[cfg(test)]
mod content_type_tests {
    use super::*;

    // -- check_content_type (D5) --

    #[test]
    fn the_media_type_guard_matches_prefix_and_exact_entries() {
        // A prefix entry matches from the start only.
        let allowed = vec!["text/".to_string(), "application/json".to_string()];
        assert!(
            check_content_type(
                "read_text_file",
                Some("text/plain; charset=utf-8"),
                &allowed
            )
            .is_ok()
        );
        assert!(check_content_type("read_text_file", Some("text/markdown"), &allowed).is_ok());
        assert!(check_content_type("read_text_file", Some("application/json"), &allowed).is_ok());
        // `text/` does not match `application/text` nor `textual/x`: a prefix
        // entry matches from the start only.
        let err =
            check_content_type("read_text_file", Some("application/text"), &allowed).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Service);
        let err = check_content_type("read_text_file", Some("textual/x"), &allowed).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Service);
        assert!(err.message.contains("reads only"), "{}", err.message);
        // Case and parameters are normalized on the answer side.
        assert!(
            check_content_type("read_text_file", Some("Application/JSON; q=1"), &allowed).is_ok()
        );
        // A missing header fails.
        let err = check_content_type("read_text_file", None, &allowed).unwrap_err();
        assert!(err.message.contains("no content type"), "{}", err.message);
        // An empty allowlist guards nothing.
        assert!(check_content_type("read_text_file", Some("image/png"), &[]).is_ok());
    }
}
