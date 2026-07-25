//! Wire encoding helpers: percent-encoding for paths and query components,
//! form bodies, and base64 for basic auth. Pure functions over strings.

use super::*;

/// Percent-encodes the top-level string values of an args object so they can
/// be substituted RAW into a URL template without letting an argument inject
/// path traversal or extra query structure. Non-string scalars pass through
/// (their `to_string` form carries no reserved bytes); nested values are not
/// reachable by a URL `{{args.name}}` placeholder (only top-level scalar args
/// are), so they are left untouched.
pub(crate) fn encode_args_for_url(args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => Value::String(encode_component(s)),
                        other => other.clone(),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Projects `body` down to the dot-separated field chains in `paths`
/// (spec 4.5). Objects keep the named field; arrays (top-level or midway
/// through a chain) map the projection over their elements; a path absent
/// from the body is silently dropped.
pub(crate) fn project(body: &Value, paths: &[String]) -> Value {
    let split: Vec<Vec<&str>> = paths.iter().map(|p| p.split('.').collect()).collect();
    let refs: Vec<&[&str]> = split.iter().map(Vec::as_slice).collect();
    project_value(body, &refs)
}

pub(crate) fn project_value(source: &Value, paths: &[&[&str]]) -> Value {
    match source {
        Value::Array(items) => {
            Value::Array(items.iter().map(|it| project_value(it, paths)).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, val) in map {
                let mut sub: Vec<&[&str]> = Vec::new();
                let mut terminal = false;
                for p in paths {
                    if p.first() == Some(&key.as_str()) {
                        if p.len() == 1 {
                            terminal = true;
                        } else {
                            sub.push(&p[1..]);
                        }
                    }
                }
                if terminal {
                    out.insert(key.clone(), val.clone());
                } else if !sub.is_empty() {
                    out.insert(key.clone(), project_value(val, &sub));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Percent-encodes a rendered auth value as one URL path segment per RFC 3986
/// pchar rules, keeping ':' literal (Telegram tokens embed a colon, spec 4.3).
/// pchar = unreserved / sub-delims / ':' / '@'.
pub(crate) fn encode_path_segment(s: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const PCHAR: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~')
        .remove(b'!')
        .remove(b'$')
        .remove(b'&')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')')
        .remove(b'*')
        .remove(b'+')
        .remove(b',')
        .remove(b';')
        .remove(b'=')
        .remove(b':')
        .remove(b'@');
    utf8_percent_encode(s, PCHAR).to_string()
}

/// Percent-encodes a whole string as a single URL component (same set the
/// renderer uses for substituted values). Applied to literal query keys and
/// auth param names so a stray reserved byte cannot restructure the URL.
pub(crate) fn encode_component(s: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(s, SET).to_string()
}

/// Encodes DECODED form pairs into the `application/x-www-form-urlencoded`
/// wire body, percent-encoding keys and values with the same component set
/// query rendering uses so encoding stays uniform (spec
/// 2026-07-22-official-connectors-wave-3, section 4.1).
pub(crate) fn encode_form_body(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Standard RFC 4648 base64 (with padding). Local to keep the engine lean: the
/// only use is HTTP Basic auth, and pulling a base64 crate for `user:pass`
/// encoding is not worth the dependency.
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(n >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}
