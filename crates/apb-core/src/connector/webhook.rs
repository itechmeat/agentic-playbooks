//! Inbound webhook verification (spec 2026-08-16-webhook-ingest-design).
//!
//! Two independent mechanisms, both owned here so no call site re-decides
//! them:
//!
//!   * the signature, HMAC-SHA256 over the exact raw request bytes, compared
//!     in constant time against the value a named header carried;
//!   * the challenge dialect, a one-time verification handshake some
//!     providers perform with a GET before they will deliver anything.
//!
//! There is no unsigned mode and no opt-out flag: an "unsigned for testing"
//! switch is how production ends up unsigned. A connector author who wants a
//! local test path uses a `mock` function instead.
//!
//! Nothing here logs, returns, or stores a secret or a body.

use std::collections::BTreeMap;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Query parameter names of the `meta_hub` challenge dialect.
pub const HUB_MODE: &str = "hub.mode";
pub const HUB_TOKEN: &str = "hub.verify_token";
pub const HUB_CHALLENGE: &str = "hub.challenge";
/// The only `hub.mode` value that is ever answered.
pub const SUBSCRIBE: &str = "subscribe";

/// What a challenge request earned. `Echo` carries the exact text to return
/// as `text/plain`; `Reject` is a flat refusal with no detail, so a caller
/// probing tokens learns nothing from the shape of the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    Echo(String),
    Reject,
}

/// HMAC-SHA256 of `body` under `secret`, as lowercase hex.
pub fn hmac_sha256_hex(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(body);
    crate::content::hex_lower(&mac.finalize().into_bytes())
}

/// Whether `presented` is even shaped like a signature this scheme could
/// accept: the declared prefix, then exactly 64 hex digits (case-insensitive,
/// surrounding whitespace tolerated). No secret is involved and nothing here
/// is secret-dependent, so a caller may use it to refuse obvious garbage
/// before paying to resolve a secret. Returns the normalized digest so the
/// verifier does not re-derive it.
fn well_formed_digest(presented: &str, prefix: &str) -> Option<String> {
    let hex = presented.strip_prefix(prefix)?;
    let hex = hex.trim().to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(hex)
}

/// Whether `presented` could possibly be a valid signature for `prefix`,
/// judged without any secret: the prefix is present and the remainder is a
/// 64-digit hex string. False means no secret could ever make it verify, so a
/// caller under load may refuse it before resolving one (the ingest listener
/// does exactly that for a client already over its failure budget).
pub fn signature_is_well_formed(presented: &str, prefix: &str) -> bool {
    well_formed_digest(presented, prefix).is_some()
}

/// Whether `presented` (the raw header value, `prefix` included) is the
/// correct signature for `body` under `secret`.
///
/// An empty `secret` never verifies. That guard is the whole reason this
/// function owns the decision: `HMAC-SHA256` accepts a zero-length key, so
/// without it a connector whose configured secret resolves to the empty
/// string (an env var that exists but is unset, an empty `KEY=` line in
/// `secrets.env`) would accept a signature any caller on the internet can
/// compute, since the key would be public by construction. The challenge path
/// (`meta_hub_challenge`) refuses an empty configured token for the same
/// reason; this is the path that actually accepts data, so it matters more.
///
/// The prefix is stripped literally and the remainder compared in constant
/// time through the workspace's single constant-time comparison. The digest
/// is lowercased first, because hex case carries no information and some
/// providers send uppercase; the comparison itself still leaks nothing about
/// which byte differed. A header that does not carry the prefix, or carries
/// a value of the wrong length, is refused without hashing anything further.
pub fn verify_signature_hex(secret: &str, body: &[u8], presented: &str, prefix: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    let Some(hex) = well_formed_digest(presented, prefix) else {
        return false;
    };
    let expected = hmac_sha256_hex(secret.as_bytes(), body);
    crate::server_auth::ct_eq_str(&expected, &hex)
}

/// The `meta_hub` challenge: echo `hub.challenge` when `hub.mode` is
/// `subscribe` and `hub.verify_token` matches, and refuse otherwise. The
/// token comparison is constant time; an empty configured token never
/// verifies, so a connector with the block but no configured secret cannot
/// be subscribed by anyone.
pub fn meta_hub_challenge(params: &BTreeMap<String, String>, verify_token: &str) -> Challenge {
    if verify_token.is_empty() {
        return Challenge::Reject;
    }
    if params.get(HUB_MODE).map(String::as_str) != Some(SUBSCRIBE) {
        return Challenge::Reject;
    }
    let Some(presented) = params.get(HUB_TOKEN) else {
        return Challenge::Reject;
    };
    if !crate::server_auth::ct_eq_str(presented, verify_token) {
        return Challenge::Reject;
    }
    match params.get(HUB_CHALLENGE) {
        Some(c) if !c.is_empty() => Challenge::Echo(c.clone()),
        _ => Challenge::Reject,
    }
}

/// The dedupe identity of one delivery: the scalar at `path` inside the
/// parsed `body` when the connector declares one and it resolves, otherwise
/// the SHA-256 of the raw bytes.
///
/// The fallback is deliberate: a declared path that does not resolve must
/// not collapse to a constant, which would make the second delivery of any
/// shape a duplicate of the first.
pub fn dedupe_id(body: &Value, raw: &[u8], path: Option<&str>) -> String {
    if let Some(path) = path
        && let Some(found) = lookup(body, path)
    {
        return found;
    }
    crate::content::sha256_hex(raw)
}

/// Resolves a dot path over objects and numeric array indices, returning the
/// scalar at the end as a string. Objects and arrays are not ids.
///
/// Deliberately not the engine's `connector::call::response::lookup_path`,
/// and deliberately not shared with it. That walker is maps-only by
/// documented design because it implements `response_pick`, whose semantics
/// must not change for connectors already relying on them; this one needs
/// numeric array indices, because `entry.0.id` is the shape real providers
/// deliver. `apb-core` also cannot depend on `apb-engine`, so sharing would
/// mean moving `response_pick`'s walker down a crate and widening it. The two
/// notations stay distinct on purpose (spec 2026-08-16-webhook-ingest-design,
/// webhook block).
fn lookup(body: &Value, path: &str) -> Option<String> {
    let mut cursor = body;
    for segment in path.split('.') {
        cursor = match cursor {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    match cursor {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
