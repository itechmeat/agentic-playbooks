//! Turning an HTTP response into a call result: reading a capped body,
//! mapping status codes through the manifest's table, and the `error_when`
//! reclassification that catches envelope errors hidden inside a 2xx.

use super::*;

/// Recursively redacts secret values in a JSON value's string leaves.
pub(crate) fn redact_value(value: Value, redactions: &[(String, String)]) -> Value {
    match value {
        Value::String(mut s) => {
            for (secret, var) in redactions {
                if s.contains(secret.as_str()) {
                    s = s.replace(secret.as_str(), &format!("[redacted:{var}]"));
                }
            }
            Value::String(s)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| redact_value(v, redactions))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, redact_value(v, redactions)))
                .collect(),
        ),
        other => other,
    }
}

/// Reads a response body up to `BODY_CAP` (+1 to detect overflow), returning
/// the parsed-or-string body and whether it was truncated. A JSON content type
/// yields a parsed value; anything else yields a lossy-UTF8 string.
pub(crate) fn read_body(mut response: ureq::http::Response<ureq::Body>) -> (Value, bool) {
    let is_json = response.body().mime_type() == Some("application/json");
    let mut buf = Vec::new();
    // A read error yields whatever was collected so far rather than failing the
    // whole call after a response was already obtained. `as_reader()` is not
    // capped by ureq itself, so the `.take(BODY_CAP + 1)` below is what bounds
    // memory (same as before).
    let _ = response
        .body_mut()
        .as_reader()
        .take(BODY_CAP as u64 + 1)
        .read_to_end(&mut buf);
    let truncated = buf.len() > BODY_CAP;
    if truncated {
        buf.truncate(BODY_CAP);
    }
    let body = if is_json {
        serde_json::from_slice(&buf)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&buf).into_owned()))
    } else {
        Value::String(String::from_utf8_lossy(&buf).into_owned())
    };
    (body, truncated)
}

/// The HTTP-status -> result mapping (spec section 8). 2xx is success; 3xx is a
/// `service` error (redirects are not followed); 401/403 -> `auth`; 404 ->
/// `not_found`; 429 -> `rate_limited` (with `retry_after` when the service
/// gave a `Retry-After`); every other non-2xx -> `service`.
pub(crate) fn map_status(
    status: u16,
    body: Value,
    truncated: bool,
    retry_after: Option<u64>,
) -> Result<CallOk, CallError> {
    if (200..300).contains(&status) {
        return Ok(CallOk::Http {
            status,
            body,
            truncated,
            link: None,
            picked: false,
        });
    }
    let mut err = match status {
        300..=399 => CallError::new(
            CallErrorCode::Service,
            format!("the service returned a redirect (HTTP {status}); redirects are not followed"),
        ),
        401 | 403 => CallError::new(
            CallErrorCode::Auth,
            format!("the service rejected the credentials (HTTP {status})"),
        ),
        404 => CallError::new(
            CallErrorCode::NotFound,
            "the service returned 404 not found".to_string(),
        ),
        429 => {
            let mut e = CallError::new(
                CallErrorCode::RateLimited,
                "the service rate-limited the request (HTTP 429)".to_string(),
            );
            e.retry_after_sec = retry_after;
            e
        }
        _ => CallError::new(
            CallErrorCode::Service,
            format!("the service returned HTTP {status}"),
        ),
    };
    err.http_status = Some(status);
    Err(err)
}

/// Evaluates the connector's `error_when` predicate (spec
/// 2026-07-22-official-connectors-wave-3, section 4.2) against a 2xx response
/// body: when the value at `path` equals `equals` (JSON equality), returns
/// the `service` error the call reclassifies to, carrying the real HTTP
/// status (e.g. 200) and the message extracted at `message_path` - or a
/// fixed fallback when `message_path` is absent, resolves to nothing, or the
/// value there is not a string. A `path` that resolves to nothing means no
/// match (success passes through).
pub(crate) fn reclassify_error_when(
    rule: &ErrorWhen,
    body: &Value,
    status: u16,
) -> Option<CallError> {
    if lookup_path(body, &rule.path) != Some(&rule.equals) {
        return None;
    }
    let message = rule
        .message_path
        .as_deref()
        .and_then(|p| lookup_path(body, p))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "the service reported an error in the response body".to_string());
    let mut err = CallError::new(CallErrorCode::Service, message);
    err.http_status = Some(status);
    Some(err)
}

/// Resolves a dot-separated field chain over JSON objects (like
/// `response_pick` paths, spec 4.5, without array semantics). `None` when any
/// segment is missing or a value midway through the chain is not an object.
pub(crate) fn lookup_path<'a>(body: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(body, |value, segment| {
        value.as_object().and_then(|map| map.get(segment))
    })
}
