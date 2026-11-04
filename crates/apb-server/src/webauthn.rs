//! Self-contained WebAuthn passkey cryptography verification for the
//! dashboard: registration (`webauthn.create`) and assertion
//! (`webauthn.get`) verification over the two algorithm families the
//! dashboard accepts, ES256 (COSE alg -7) and EdDSA (COSE alg -8).
//!
//! The module is deliberately narrow. It does not own challenges: the
//! in-memory challenge store lives with the server, and every verification
//! entry point takes the expected challenge string it was handed and
//! compares it against the challenge decoded from `clientDataJSON`. It also
//! does not own user handling: the caller decides what a missing user
//! verification means for its route. Expiry
//! ([`WebAuthnError::ChallengeExpired`]) is raised by the challenge store,
//! never here.
//!
//! Every failure is a typed [`WebAuthnError`] with a Display message a route
//! can show as-is; no verification path panics on malformed input.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ciborium::Value;
use ed25519_dalek::Verifier;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The relying party identity passkey credentials bind to.
///
/// `rp_id` is the host the dashboard is served under (never an IP literal,
/// see [`rp_for_origin`]); `origin` is the exact origin string the routes
/// compare `clientDataJSON.origin` against after normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpConfig {
    /// The WebAuthn relying party id, the host name credentials bind to.
    pub rp_id: String,
    /// The expected origin string, compared after normalization.
    pub origin: String,
}

/// Everything that can go wrong while verifying a passkey registration or
/// assertion, with a message a route can show unchanged.
#[derive(Debug, thiserror::Error)]
pub enum WebAuthnError {
    /// The origin in clientDataJSON does not match the relying party origin.
    #[error("passkey origin does not match the relying party origin {0:?}")]
    OriginMismatch(String),
    /// The rpIdHash in the authenticator data does not hash the rp id.
    #[error("passkey authenticator data is bound to a different relying party id")]
    RpIdMismatch,
    /// The challenge in clientDataJSON is not the one the server issued.
    #[error("passkey challenge does not match the challenge this server issued")]
    ChallengeMismatch,
    /// The challenge has expired; raised by the caller's challenge store.
    #[error("passkey challenge has expired; start the ceremony again")]
    ChallengeExpired,
    /// The authenticator did not assert user verification where required.
    #[error("passkey ceremony did not include user verification (UV), which this server requires")]
    UvRequired,
    /// The authenticator did not assert user presence.
    #[error(
        "passkey ceremony did not include user presence (UP); the user did not interact with the authenticator"
    )]
    UserPresenceRequired,
    /// The attestation statement format is not `none`, or an attestation
    /// statement was carried where only `none` is accepted.
    #[error(
        "passkey attestation format is not \"none\", which is the only format this server accepts"
    )]
    AttestationNotNone,
    /// clientDataJSON could not be parsed or has the wrong ceremony type.
    #[error("passkey client data is invalid: {0}")]
    BadClientData(String),
    /// The authenticator data byte structure is invalid.
    #[error("passkey authenticator data is invalid: {0}")]
    BadAuthenticatorData(String),
    /// The attestation object could not be parsed or is not shaped as
    /// expected.
    #[error("passkey attestation object is invalid: {0}")]
    BadAttestation(String),
    /// The stored or attested COSE public key is invalid or unsupported.
    #[error("passkey public key is invalid: {0}")]
    BadPublicKey(String),
    /// The signature over the assertion did not verify with the stored key.
    #[error("passkey signature verification failed")]
    BadSignature,
    /// The authenticator reported a signature counter below the stored one.
    #[error(
        "passkey signature counter regressed from {stored} to {got}; possible cloned authenticator"
    )]
    CounterRegressed { stored: u32, got: u32 },
    /// The COSE key names an algorithm this server does not accept.
    #[error(
        "passkey public key algorithm {0} is not supported; this server accepts ES256 (-7) and EdDSA (-8)"
    )]
    UnsupportedAlgorithm(i64),
    /// The operator configured an IP-literal origin, which passkeys cannot
    /// bind to.
    #[error(
        "rp id {0} is an IP address; passkeys cannot bind to an IP address. Serve the dashboard under a loopback name (for example localhost) or an https name instead"
    )]
    IpLiteralRpId(String),
    /// The configured origin is not an acceptable WebAuthn origin.
    #[error(
        "origin {0:?} is not an acceptable WebAuthn origin; use https, or http on a loopback host"
    )]
    BadOrigin(String),
}

/// Flags of the authenticator data byte at index 32.
const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_ATTESTED_CREDENTIAL_DATA: u8 = 0x40;

/// COSE algorithm identifiers this module verifies.
const ALG_ES256: i64 = -7;
const ALG_EDDSA: i64 = -8;

/// Decodes unpadded base64url text. Accepts padded input too; the padded
/// characters are ignored, matching what browsers put in clientDataJSON.
pub fn b64url_decode(text: &str) -> Result<Vec<u8>, WebAuthnError> {
    let trimmed: String = text.chars().filter(|c| *c != '=').collect();
    URL_SAFE_NO_PAD
        .decode(trimmed.as_bytes())
        .map_err(|e| WebAuthnError::BadClientData(format!("invalid base64url: {e}")))
}

/// Encodes bytes as unpadded base64url.
pub fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A parsed clientDataJSON.
#[derive(Debug, Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    typ: String,
    challenge: String,
    origin: String,
}

fn parse_client_data(json: &str) -> Result<ClientData, WebAuthnError> {
    serde_json::from_str(json)
        .map_err(|e| WebAuthnError::BadClientData(format!("clientDataJSON is not valid JSON: {e}")))
}

/// The parsed authenticator data of a registration or an assertion.
#[derive(Debug)]
struct AuthData {
    rp_id_hash: [u8; 32],
    flags: u8,
    sign_count: u32,
    /// Present only when the AT flag is set: the attested credential id and
    /// the COSE key bytes, canonicalized.
    attested: Option<AttestedCredentialData>,
}

/// The attested credential data of a registration authenticator.
#[derive(Debug)]
struct AttestedCredentialData {
    credential_id: Vec<u8>,
    /// The COSE key, canonicalized (re-serialized from its parsed fields).
    cose_key: Vec<u8>,
    /// The COSE `alg` of the parsed key.
    alg: i64,
}

fn parse_auth_data(bytes: &[u8]) -> Result<AuthData, WebAuthnError> {
    if bytes.len() < 37 {
        return Err(WebAuthnError::BadAuthenticatorData(format!(
            "authenticator data is {} bytes, at least 37 required",
            bytes.len()
        )));
    }
    let mut rp_id_hash = [0u8; 32];
    rp_id_hash.copy_from_slice(&bytes[..32]);
    let flags = bytes[32];
    let sign_count = u32::from_be_bytes([bytes[33], bytes[34], bytes[35], bytes[36]]);
    let attested = if flags & FLAG_ATTESTED_CREDENTIAL_DATA != 0 {
        let rest = &bytes[37..];
        if rest.len() < 18 {
            return Err(WebAuthnError::BadAuthenticatorData(
                "attested credential data truncated: AAGUID and credential id length missing"
                    .to_string(),
            ));
        }
        let cred_len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        let after_len = &rest[18..];
        if after_len.len() < cred_len {
            return Err(WebAuthnError::BadAuthenticatorData(format!(
                "attested credential data truncated: credential id length {cred_len} exceeds the remaining {} bytes",
                after_len.len()
            )));
        }
        let credential_id = after_len[..cred_len].to_vec();
        let cose_bytes = &after_len[cred_len..];
        let (cose_key, alg) = parse_and_canonicalize_cose_key(cose_bytes)?;
        Some(AttestedCredentialData {
            credential_id,
            cose_key,
            alg,
        })
    } else {
        None
    };
    Ok(AuthData {
        rp_id_hash,
        flags,
        sign_count,
        attested,
    })
}

/// Parses a COSE key from raw bytes and re-serializes it canonically.
///
/// Rejects trailing garbage after the key, keys that are not maps, and keys
/// that name an algorithm other than ES256 or EdDSA or whose EC2/OKP
/// structure does not match the accepted parameter set.
fn parse_and_canonicalize_cose_key(bytes: &[u8]) -> Result<(Vec<u8>, i64), WebAuthnError> {
    let mut reader = CountingReader::new(bytes);
    let value: Value = ciborium::from_reader(&mut reader)
        .map_err(|e| WebAuthnError::BadPublicKey(format!("COSE key is not valid CBOR: {e}")))?;
    if reader.position() != bytes.len() {
        return Err(WebAuthnError::BadPublicKey(
            "trailing bytes after the COSE key".to_string(),
        ));
    }
    let map = value
        .into_map()
        .map_err(|_| WebAuthnError::BadPublicKey("COSE key is not a CBOR map".to_string()))?;
    let entry =
        |key: i64| -> Option<&Value> { map.iter().find(|(k, _)| is_int(k, key)).map(|(_, v)| v) };
    let get_int = |key: i64| -> Option<i64> {
        entry(key).and_then(|v| match v {
            Value::Integer(n) => i64::try_from(*n).ok(),
            _ => None,
        })
    };
    let get_bytes = |key: i64| -> Option<Vec<u8>> {
        entry(key).and_then(|v| match v {
            Value::Bytes(b) => Some(b.clone()),
            _ => None,
        })
    };
    let kty = get_int(1).ok_or_else(|| bad_key("kty (1) missing"))?;
    let alg = get_int(3).ok_or_else(|| bad_key("alg (3) missing"))?;
    let canonical = match alg {
        ALG_ES256 => {
            if kty != 2 {
                return Err(bad_key("ES256 key is not EC2 (kty 2)"));
            }
            let crv = get_int(-1).ok_or_else(|| bad_key("crv (-1) missing"))?;
            if crv != 1 {
                return Err(bad_key("ES256 curve is not P-256 (crv 1)"));
            }
            let x = get_bytes(-2).ok_or_else(|| bad_key("x coordinate (-2) missing"))?;
            let y = get_bytes(-3).ok_or_else(|| bad_key("y coordinate (-3) missing"))?;
            if x.len() != 32 || y.len() != 32 {
                return Err(bad_key("P-256 coordinates must be 32 bytes each"));
            }
            canonical_cose_key(&[
                (1, Value::from(2i64)),
                (3, Value::from(ALG_ES256)),
                (-1, Value::from(1i64)),
                (-2, Value::Bytes(x)),
                (-3, Value::Bytes(y)),
            ])
        }
        ALG_EDDSA => {
            if kty != 1 {
                return Err(bad_key("EdDSA key is not OKP (kty 1)"));
            }
            let crv = get_int(-1).ok_or_else(|| bad_key("crv (-1) missing"))?;
            if crv != 6 {
                return Err(bad_key("EdDSA curve is not Ed25519 (crv 6)"));
            }
            let x = get_bytes(-2).ok_or_else(|| bad_key("public key (-2) missing"))?;
            if x.len() != 32 {
                return Err(bad_key("Ed25519 public key must be 32 bytes"));
            }
            canonical_cose_key(&[
                (1, Value::from(1i64)),
                (3, Value::from(ALG_EDDSA)),
                (-1, Value::from(6i64)),
                (-2, Value::Bytes(x)),
            ])
        }
        other => return Err(WebAuthnError::UnsupportedAlgorithm(other)),
    };
    Ok((canonical, alg))
}

/// A reader that counts how many bytes ciborium consumed, so trailing
/// garbage after a CBOR item is detectable.
struct CountingReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> CountingReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn position(&self) -> usize {
        self.position
    }
}

impl std::io::Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = &self.bytes[self.position.min(self.bytes.len())..];
        let n = remaining.len().min(buf.len());
        buf[..n].copy_from_slice(&remaining[..n]);
        self.position += n;
        Ok(n)
    }
}

/// Whether a CBOR value is the integer `key`.
fn is_int(value: &Value, key: i64) -> bool {
    matches!(value, Value::Integer(n) if i64::try_from(*n).is_ok_and(|k| k == key))
}

fn bad_key(detail: &str) -> WebAuthnError {
    WebAuthnError::BadPublicKey(detail.to_string())
}

fn canonical_cose_key(entries: &[(i64, Value)]) -> Vec<u8> {
    let map: Vec<(Value, Value)> = entries
        .iter()
        .map(|(k, v)| (Value::from(*k), v.clone()))
        .collect();
    let mut out = Vec::new();
    ciborium::ser::into_writer(&Value::Map(map), &mut out)
        .expect("canonical CBOR key serialization cannot fail");
    out
}

/// The credential a registration verification returns to the caller, ready
/// for storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    /// The credential id, unpadded base64url of the raw id bytes.
    pub credential_id: String,
    /// The attested public key, canonical CBOR.
    pub cose_key: Vec<u8>,
    /// The COSE algorithm of the key, -7 (ES256) or -8 (EdDSA).
    pub alg: i64,
    /// The signature counter the authenticator reported at registration.
    pub sign_count: u32,
}

/// Verifies a `webauthn.create` ceremony end to end.
///
/// Checks, in order: the client data ceremony type, the challenge against
/// `expected_challenge` (compared as canonical unpadded base64url), the
/// origin against `rp.origin` after normalization, the attestation object
/// (format `none`, empty or absent attestation statement), the rp id hash,
/// and the authenticator data flags (user presence and user verification
/// required, attested credential data present). On success returns the
/// credential to store.
pub fn verify_registration(
    client_data_json: &str,
    attestation_object: &[u8],
    expected_challenge: &str,
    rp: &RpConfig,
) -> Result<NewCredential, WebAuthnError> {
    let client = parse_client_data(client_data_json)?;
    if client.typ != "webauthn.create" {
        return Err(WebAuthnError::BadClientData(format!(
            "expected ceremony type \"webauthn.create\", got {:?}",
            client.typ
        )));
    }
    check_challenge(&client.challenge, expected_challenge)?;
    check_origin(&client.origin, rp)?;
    let attestation = parse_attestation_object(attestation_object)?;
    let auth_data = parse_auth_data(&attestation.auth_data)?;
    check_rp_id_hash(&auth_data.rp_id_hash, rp)?;
    if auth_data.flags & FLAG_UP == 0 {
        return Err(WebAuthnError::UserPresenceRequired);
    }
    if auth_data.flags & FLAG_UV == 0 {
        return Err(WebAuthnError::UvRequired);
    }
    let attested = auth_data
        .attested
        .ok_or(WebAuthnError::AttestationNotNone)?;
    Ok(NewCredential {
        credential_id: b64url_encode(&attested.credential_id),
        cose_key: attested.cose_key,
        alg: attested.alg,
        sign_count: auth_data.sign_count,
    })
}

/// Verifies a `webauthn.get` ceremony end to end and returns the new
/// signature counter to store.
///
/// Checks the client data ceremony type, the challenge, the origin, the rp
/// id hash of the authenticator data, the user presence and user
/// verification flags, the signature counter against `stored_sign_count`
/// (equality allowed), and the signature over
/// `authenticator_data || sha256(client_data_json)` with the stored COSE
/// key.
pub fn verify_assertion(
    client_data_json: &str,
    authenticator_data: &[u8],
    signature: &[u8],
    expected_challenge: &str,
    rp: &RpConfig,
    stored_cose_key: &[u8],
    stored_sign_count: u32,
) -> Result<u32, WebAuthnError> {
    let client = parse_client_data(client_data_json)?;
    if client.typ != "webauthn.get" {
        return Err(WebAuthnError::BadClientData(format!(
            "expected ceremony type \"webauthn.get\", got {:?}",
            client.typ
        )));
    }
    check_challenge(&client.challenge, expected_challenge)?;
    check_origin(&client.origin, rp)?;
    let auth_data = parse_auth_data(authenticator_data)?;
    check_rp_id_hash(&auth_data.rp_id_hash, rp)?;
    if auth_data.flags & FLAG_UP == 0 {
        return Err(WebAuthnError::UserPresenceRequired);
    }
    if auth_data.flags & FLAG_UV == 0 {
        return Err(WebAuthnError::UvRequired);
    }
    if auth_data.sign_count < stored_sign_count {
        return Err(WebAuthnError::CounterRegressed {
            stored: stored_sign_count,
            got: auth_data.sign_count,
        });
    }
    let mut signed = Vec::with_capacity(authenticator_data.len() + 32);
    signed.extend_from_slice(authenticator_data);
    signed.extend_from_slice(&Sha256::digest(client_data_json.as_bytes()));
    verify_signature(stored_cose_key, &signed, signature)?;
    Ok(auth_data.sign_count)
}

/// Compares the challenge decoded from clientDataJSON with the expected
/// challenge, both normalized to canonical unpadded base64url.
fn check_challenge(got: &str, expected: &str) -> Result<(), WebAuthnError> {
    let got = b64url_decode(got)?;
    let expected = b64url_decode(expected)?;
    if b64url_encode(&got) != b64url_encode(&expected) {
        return Err(WebAuthnError::ChallengeMismatch);
    }
    Ok(())
}

/// Compares the client data origin with the configured origin after
/// normalization: lowercase scheme and host, default port stripped.
fn check_origin(got: &str, rp: &RpConfig) -> Result<(), WebAuthnError> {
    let got = normalize_origin(got).ok_or_else(|| WebAuthnError::BadOrigin(got.to_string()))?;
    let expected =
        normalize_origin(&rp.origin).ok_or_else(|| WebAuthnError::BadOrigin(rp.origin.clone()))?;
    if got != expected {
        return Err(WebAuthnError::OriginMismatch(rp.origin.clone()));
    }
    Ok(())
}

fn check_rp_id_hash(got: &[u8; 32], rp: &RpConfig) -> Result<(), WebAuthnError> {
    let expected: [u8; 32] = Sha256::digest(rp.rp_id.as_bytes()).into();
    if got != &expected {
        return Err(WebAuthnError::RpIdMismatch);
    }
    Ok(())
}

/// The parsed attestation object: format `none`, no or an empty attestation
/// statement, and the authenticator data bytes.
struct AttestationObject {
    auth_data: Vec<u8>,
}

fn parse_attestation_object(bytes: &[u8]) -> Result<AttestationObject, WebAuthnError> {
    let value: Value = ciborium::from_reader(&mut CountingReader::new(bytes))
        .map_err(|e| WebAuthnError::BadAttestation(format!("not valid CBOR: {e}")))?;
    let map = value
        .into_map()
        .map_err(|_| WebAuthnError::BadAttestation("not a CBOR map".to_string()))?;
    let entry = |name: &str| -> Option<&Value> {
        map.iter()
            .find(|(k, _)| matches!(k, Value::Text(t) if t == name))
            .map(|(_, v)| v)
    };
    let fmt = entry("fmt")
        .and_then(|v| match v {
            Value::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .ok_or_else(|| WebAuthnError::BadAttestation("fmt missing".to_string()))?;
    if fmt != "none" {
        return Err(WebAuthnError::AttestationNotNone);
    }
    if let Some(stmt) = entry("attStmt") {
        let empty = matches!(stmt, Value::Map(m) if m.is_empty());
        if !empty {
            return Err(WebAuthnError::AttestationNotNone);
        }
    }
    let auth_data = match entry("authData") {
        Some(Value::Bytes(b)) => b.clone(),
        _ => {
            return Err(WebAuthnError::BadAttestation(
                "authData missing or not bytes".to_string(),
            ));
        }
    };
    Ok(AttestationObject { auth_data })
}

/// Verifies an assertion signature with a stored COSE key: ES256 with SHA-256
/// over the signed message with a DER signature, or EdDSA over the signed
/// message. Never panics; every malformed key or signature is
/// [`WebAuthnError::BadSignature`].
fn verify_signature(
    cose_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), WebAuthnError> {
    // Full structural validation of the stored key first (trailing bytes,
    // accepted algorithm, parameter shapes), then the raw coordinates.
    let (_, alg) = parse_and_canonicalize_cose_key(cose_key)?;
    let map = parse_cose_key_fields(cose_key)?;
    match alg {
        ALG_ES256 => {
            let x: &[u8; 32] = map.x.as_slice().try_into().map_err(|_| {
                WebAuthnError::BadPublicKey("P-256 x coordinate must be 32 bytes".to_string())
            })?;
            let y: &[u8; 32] = map.y.as_slice().try_into().map_err(|_| {
                WebAuthnError::BadPublicKey("P-256 y coordinate must be 32 bytes".to_string())
            })?;
            let point =
                elliptic_curve::sec1::EncodedPoint::<p256::NistP256>::from_affine_coordinates(
                    x.into(),
                    y.into(),
                    false,
                );
            let verifying_key = p256::ecdsa::VerifyingKey::from_encoded_point(&point)
                .map_err(|_| WebAuthnError::BadPublicKey("P-256 point not on curve".to_string()))?;
            let signature = p256::ecdsa::Signature::from_der(signature)
                .map_err(|_| WebAuthnError::BadSignature)?;
            use p256::ecdsa::signature::Verifier as _;
            verifying_key
                .verify(message, &signature)
                .map_err(|_| WebAuthnError::BadSignature)
        }
        ALG_EDDSA => {
            let key: &[u8; 32] = map.x.as_slice().try_into().map_err(|_| {
                WebAuthnError::BadPublicKey("Ed25519 public key must be 32 bytes".to_string())
            })?;
            let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(key)
                .map_err(|_| WebAuthnError::BadPublicKey("Ed25519 key rejected".to_string()))?;
            let signature = ed25519_dalek::Signature::from_slice(signature)
                .map_err(|_| WebAuthnError::BadSignature)?;
            verifying_key
                .verify(message, &signature)
                .map_err(|_| WebAuthnError::BadSignature)
        }
        other => Err(WebAuthnError::UnsupportedAlgorithm(other)),
    }
}

/// The raw coordinate fields of a parsed COSE key, read back from the stored
/// bytes for signature verification.
struct CoseKeyFields {
    x: Vec<u8>,
    y: Vec<u8>,
}

fn parse_cose_key_fields(cose_key: &[u8]) -> Result<CoseKeyFields, WebAuthnError> {
    let value: Value = ciborium::from_reader(&mut CountingReader::new(cose_key)).map_err(|e| {
        WebAuthnError::BadPublicKey(format!("stored COSE key is not valid CBOR: {e}"))
    })?;
    let map = value.into_map().map_err(|_| {
        WebAuthnError::BadPublicKey("stored COSE key is not a CBOR map".to_string())
    })?;
    let field = |key: i64| -> Option<Vec<u8>> {
        map.iter()
            .find(|(k, _)| is_int(k, key))
            .and_then(|(_, v)| match v {
                Value::Bytes(b) => Some(b.clone()),
                _ => None,
            })
    };
    Ok(CoseKeyFields {
        x: field(-2).ok_or_else(|| bad_key("x (-2) missing"))?,
        y: field(-3).unwrap_or_default(),
    })
}

/// Resolves the relying party configuration for an origin the operator
/// serves the dashboard under.
///
/// The scheme must be https, or http with a loopback host (`localhost`,
/// `127.0.0.1`, `[::1]`, or any `*.localhost` name). The rp id is the host
/// with a default port stripped. An IP-literal host is refused with
/// [`WebAuthnError::IpLiteralRpId`] in every case, including
/// `127.0.0.1`: passkeys cannot bind to an IP address, so the operator must
/// serve the dashboard under a loopback name or an https name.
pub fn rp_for_origin(origin: &str) -> Result<RpConfig, WebAuthnError> {
    let (scheme, host, port) =
        parse_origin(origin).ok_or_else(|| WebAuthnError::BadOrigin(origin.to_string()))?;
    let loopback =
        matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") || host.ends_with(".localhost");
    match scheme.as_str() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(WebAuthnError::BadOrigin(format!(
                "{origin}; https is required except on a loopback http host"
            )));
        }
    }
    let default_port =
        port == Some(80) && scheme == "http" || port == Some(443) && scheme == "https";
    let rp_id_host = if default_port { host.clone() } else { host };
    if is_ip_literal(&rp_id_host) {
        return Err(WebAuthnError::IpLiteralRpId(rp_id_host));
    }
    Ok(RpConfig {
        rp_id: rp_id_host,
        origin: origin.to_string(),
    })
}

/// Splits an origin URL into `(scheme, host, port)`, with IPv6 brackets
/// stripped from the host. Returns None on anything that is not
/// `scheme://host[:port]` with no userinfo and a non-empty host.
fn parse_origin(origin: &str) -> Option<(String, String, Option<u16>)> {
    let (scheme, rest) = origin.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+')
    {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map(|(_, a)| a)
        .unwrap_or(authority);
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (v6, after) = stripped.split_once(']')?;
        if v6.is_empty() {
            return None;
        }
        let port = after
            .strip_prefix(':')
            .map(str::parse)
            .transpose()
            .ok()
            .flatten();
        (v6.to_string(), port)
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((h, p)) => (h.to_string(), Some(p.parse().ok()?)),
            None => (authority.to_string(), None),
        };
        (host, port)
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase(), port))
}

/// Normalizes an origin for comparison: lowercase scheme and host, default
/// port for the scheme stripped, path and query dropped.
fn normalize_origin(origin: &str) -> Option<String> {
    let (scheme, host, port) = parse_origin(origin)?;
    let default =
        (scheme == "http" && port == Some(80)) || (scheme == "https" && port == Some(443));
    let host_part = if default {
        host
    } else if let Some(port) = port {
        format!("{host}:{port}")
    } else {
        host
    };
    Some(format!("{scheme}://{host_part}"))
}

/// Whether a host is an IP literal: an IPv4 dotted quad or a (bracket
/// stripped) IPv6 address.
fn is_ip_literal(host: &str) -> bool {
    if host.contains(':') {
        return host.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let segments = host.split('.').collect::<Vec<_>>();
    segments.len() == 4
        && segments
            .iter()
            .all(|s| !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()))
}

/// Builds an ES256 COSE key from the raw P-256 coordinates, in canonical
/// form. A test-only helper shared by the vectors below and future key
/// handling that wants to build keys from raw coordinates.
#[cfg(test)]
pub(crate) fn cose_es256(x: [u8; 32], y: [u8; 32]) -> Vec<u8> {
    canonical_cose_key(&[
        (1, Value::from(2i64)),
        (3, Value::from(ALG_ES256)),
        (-1, Value::from(1i64)),
        (-2, Value::Bytes(x.to_vec())),
        (-3, Value::Bytes(y.to_vec())),
    ])
}

/// Builds an EdDSA (Ed25519) COSE key from the raw public key, in canonical
/// form. A test-only helper shared by the vectors below and future key
/// handling that wants to build keys from raw coordinates.
#[cfg(test)]
pub(crate) fn cose_eddsa(public_key: [u8; 32]) -> Vec<u8> {
    canonical_cose_key(&[
        (1, Value::from(1i64)),
        (3, Value::from(ALG_EDDSA)),
        (-1, Value::from(6i64)),
        (-2, Value::Bytes(public_key.to_vec())),
    ])
}

#[cfg(test)]
mod tests;
