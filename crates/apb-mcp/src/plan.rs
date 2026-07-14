//! The two-phase contract for cross-workspace runs (spec 7). `prepare_run`
//! resolves the target and issues a signed `plan_token`; `execute_plan` checks
//! the signature, expiry, single-use, and plan immutability (digest/params), then
//! runs the playbook in the target workspace.
//!
//! The token guarantees plan integrity (no drift between what was shown to the user and
//! execution - TOCTOU), single use, and audit. It does NOT prove the user's
//! consent: the caller is the same agent. Semantic consent lives on the host.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The plan payload. Signed with the process's server-side key. Parameters
/// are baked into the signed plan, so `execute_plan` does not ask for them again and
/// parameter drift between the plan shown to the user and execution is impossible.
/// Binding a profile to the plan: `<scope>/<name>` + its bundle_digest at the moment of
/// prepare. Profile or skill drift between prepare and execute breaks the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanProfile {
    pub key: String,
    pub bundle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPayload {
    pub workspace_id: String,
    pub id: String,
    pub version: String,
    pub digest: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    pub effects: Vec<String>,
    /// Profiles the playbook references, with their bundle_digest (spec 3.6).
    #[serde(default)]
    pub profiles: Vec<PlanProfile>,
    pub exp_ms: u64,
    pub nonce: String,
}

/// The process signing key: 32 random bytes (two uuid v4s). Lives in the
/// process's memory, not accessible to the model. Tokens do not survive a server restart - this
/// is correct (short TTL, the plan needs to be rebuilt anyway).
fn process_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut k = [0u8; 32];
        k[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        k[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        k
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    // ASCII only: otherwise the byte slicing `&s[i..i+2]` could land in the
    // middle of a multi-byte character and panic.
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn sign(msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(process_key()).expect("hmac key");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Encodes the plan into a token `hex(json).hex(hmac)`.
pub fn encode(payload: &PlanPayload) -> String {
    let json = serde_json::to_vec(payload).unwrap_or_default();
    let sig = sign(&json);
    format!("{}.{}", hex_encode(&json), hex_encode(&sig))
}

/// Decodes and verifies the token's signature. `None` - the token is malformed or the signature
/// does not match.
pub fn decode(token: &str) -> Option<PlanPayload> {
    let (json_hex, sig_hex) = token.split_once('.')?;
    let json = hex_decode(json_hex)?;
    let sig = hex_decode(sig_hex)?;
    // Constant-time comparison via verify_slice.
    let mut mac = HmacSha256::new_from_slice(process_key()).ok()?;
    mac.update(&json);
    mac.verify_slice(&sig).ok()?;
    serde_json::from_slice(&json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> PlanPayload {
        PlanPayload {
            workspace_id: "ws-1".into(),
            id: "x".into(),
            version: "1.0.0".into(),
            digest: "sha256:aa".into(),
            params: BTreeMap::new(),
            effects: vec!["fs_write".into()],
            profiles: vec![],
            exp_ms: 999,
            nonce: "n1".into(),
        }
    }

    #[test]
    fn roundtrip_ok() {
        let t = encode(&payload());
        let back = decode(&t).unwrap();
        assert_eq!(back.id, "x");
        assert_eq!(back.digest, "sha256:aa");
    }

    #[test]
    fn tampered_signature_rejected() {
        let t = encode(&payload());
        let (json_hex, _sig) = t.split_once('.').unwrap();
        let forged = format!("{json_hex}.{}", "00".repeat(32));
        assert!(decode(&forged).is_none());
    }

    #[test]
    fn tampered_payload_rejected() {
        let t = encode(&payload());
        let (_json, sig_hex) = t.split_once('.').unwrap();
        // Swap the payload while keeping the old signature.
        let other = encode(&PlanPayload {
            id: "y".into(),
            ..payload()
        });
        let (other_json, _) = other.split_once('.').unwrap();
        let forged = format!("{other_json}.{sig_hex}");
        assert!(decode(&forged).is_none());
    }
}
