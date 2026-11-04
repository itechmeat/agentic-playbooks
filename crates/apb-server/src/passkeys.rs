//! In-memory WebAuthn ceremony state: single-use challenges, the tokens a
//! verified assertion mints, and the canonical purpose digests binding
//! them to one decision, enrollment or removal.
//!
//! Nothing here is persisted on purpose, like the session store: a restart
//! hands out fresh challenges, and nothing secret is ever written down.
//! A challenge exists for two minutes, is consumed by its first use, and
//! only answers the purpose digest it was issued for, so a challenge the
//! browser obtained for one gate cannot decide another.

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// How long a ceremony challenge stays valid.
pub const CHALLENGE_TTL_MS: u128 = 120 * 1000;
/// How long the token a verified assertion mints stays valid.
pub const TOKEN_TTL_MS: u128 = 120 * 1000;

#[derive(Debug, Clone)]
struct Entry {
    purpose: String,
    expires: u128,
    /// The credential id prefix a verified assertion stamps on the
    /// decision (tokens only; challenges carry none).
    credential: Option<String>,
}

#[derive(Default)]
struct Inner {
    challenges: HashMap<String, Entry>,
    tokens: HashMap<String, Entry>,
}

/// The server's ceremony state, shared by the passkey routes and the
/// review decision route.
#[derive(Default)]
pub struct PasskeyState {
    inner: Mutex<Inner>,
}

fn now_ms() -> u128 {
    apb_core::clock::now_ms()
}

/// The canonical digest of a purpose value: the JSON re-serialized with
/// object keys sorted, so the challenge request and the verify request
/// produce the same digest however the client ordered the fields.
pub fn purpose_digest(v: &serde_json::Value) -> String {
    hex_lower(&Sha256::digest(canonical(v).to_string().as_bytes()))
}

fn canonical(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&map[k])))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical).collect())
        }
        other => other.clone(),
    }
}

/// First 8 hex chars of a string, the display prefix the store stamps.
pub fn credential_prefix(credential_id: &str) -> String {
    credential_id.chars().take(8).collect()
}

impl PasskeyState {
    /// Issues a single-use challenge for `purpose`, consumed by
    /// [`Self::consume_challenge`] within [`CHALLENGE_TTL_MS`].
    pub fn issue_challenge(&self, purpose: String) -> Result<String, String> {
        let challenge = apb_core::server_auth::random_token().map_err(|e| e.to_string())?;
        let mut inner = self.inner.lock().expect("passkey state lock");
        prune(&mut inner.challenges, now_ms());
        inner.challenges.insert(
            challenge.clone(),
            Entry {
                purpose,
                expires: now_ms() + CHALLENGE_TTL_MS,
                credential: None,
            },
        );
        Ok(challenge)
    }

    /// Issues `challenge` itself (already derived by the caller) as the
    /// single-use challenge for `purpose`.
    pub fn issue_challenge_for(
        &self,
        purpose: String,
        challenge: String,
    ) -> Result<String, String> {
        let mut inner = self.inner.lock().expect("passkey state lock");
        prune(&mut inner.challenges, now_ms());
        inner.challenges.insert(
            challenge.clone(),
            Entry {
                purpose,
                expires: now_ms() + CHALLENGE_TTL_MS,
                credential: None,
            },
        );
        Ok(challenge)
    }

    /// Whether `challenge` is the live, unconsumed challenge for
    /// `purpose`, consuming it. A second use, another purpose and an
    /// expired challenge all read `false`.
    pub fn consume_challenge(&self, challenge: &str, purpose: &str) -> bool {
        let mut inner = self.inner.lock().expect("passkey state lock");
        let live = inner
            .challenges
            .get(challenge)
            .is_some_and(|e| e.purpose == purpose && e.expires > now_ms());
        if live {
            inner.challenges.remove(challenge);
        }
        live
    }

    /// Mints the token a verified assertion hands back, bound to
    /// `purpose` and carrying `credential` (the id prefix the decision
    /// stamps). Single use, [`TOKEN_TTL_MS`].
    pub fn issue_token(&self, purpose: String, credential: String) -> Result<String, String> {
        let token = apb_core::server_auth::random_token().map_err(|e| e.to_string())?;
        let mut inner = self.inner.lock().expect("passkey state lock");
        prune(&mut inner.tokens, now_ms());
        inner.tokens.insert(
            token.clone(),
            Entry {
                purpose,
                expires: now_ms() + TOKEN_TTL_MS,
                credential: Some(credential),
            },
        );
        Ok(token)
    }

    /// Whether `token` is the live, unconsumed token for `purpose`,
    /// consuming it and returning the credential prefix it carries.
    pub fn take_token(&self, token: &str, purpose: &str) -> Option<String> {
        let mut inner = self.inner.lock().expect("passkey state lock");
        let live = inner
            .tokens
            .get(token)
            .is_some_and(|e| e.purpose == purpose && e.expires > now_ms());
        if !live {
            return None;
        }
        inner.tokens.remove(token).and_then(|e| e.credential)
    }
}

fn prune(map: &mut HashMap<String, Entry>, now: u128) {
    map.retain(|_, e| e.expires > now);
}

/// Lowercase hex of a digest (the shared content helper spells it out).
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_challenge_is_single_use_and_purpose_bound() {
        let state = PasskeyState::default();
        let ch = state.issue_challenge("a".into()).unwrap();
        assert!(!state.consume_challenge(&ch, "b"));
        assert!(state.consume_challenge(&ch, "a"));
        assert!(!state.consume_challenge(&ch, "a"), "single use");
    }

    #[test]
    fn a_token_carries_its_credential_once() {
        let state = PasskeyState::default();
        let t = state.issue_token("d".into(), "abcd1234".into()).unwrap();
        assert_eq!(state.take_token(&t, "other"), None);
        assert_eq!(state.take_token(&t, "d").as_deref(), Some("abcd1234"));
        assert_eq!(state.take_token(&t, "d"), None, "single use");
    }

    #[test]
    fn the_purpose_digest_is_key_order_stable() {
        let a = json!({"decision": {"run": "r1", "node": "g"}, "x": 1});
        let b = json!({"x": 1, "decision": {"node": "g", "run": "r1"}});
        assert_eq!(purpose_digest(&a), purpose_digest(&b));
        let c = json!({"x": 2, "decision": {"run": "r1", "node": "g"}});
        assert_ne!(purpose_digest(&a), purpose_digest(&c));
    }
}
