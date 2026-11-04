//! The self-verifying passkey stamp on a review channel entry (the fix for
//! the forgeable `passkey` string).
//!
//! A dashboard decision on a person-only gate or an approval carries the
//! full WebAuthn assertion it was minted from: the credential id, the
//! `clientDataJSON` (base64url, exactly as the browser posted it), the
//! `authenticatorData` and the signature, all base64url. The challenge the
//! authenticator signed over is not random: it is derived deterministically
//! from the run, the gate, the decision, the request seq and the binding
//! nonce, so the drive can recompute it at consumption and no line can be
//! replayed onto another request, gate or option.
//!
//! At consumption the engine re-verifies the proof against the machine's
//! passkey store: the credential exists, the challenge matches the derived
//! one, user presence and user verification are set, and the signature
//! verifies over `authenticatorData || SHA-256(clientDataJSON bytes)` with
//! the stored public key. A forged or replayed stamp is refused.
//!
//! What this does NOT prove: that the browser talked to the dashboard under
//! the dashboard's own origin. The engine cannot see the request's Host,
//! so the origin and RP ID checks happen only where the assertion is first
//! accepted (`apb-server::webauthn`). The remaining trust is in a same-user
//! process that can read the machine's config dir but cannot sign with the
//! authenticator's private key, which never leaves the device.

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The proof a dashboard decision stamps on its channel entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasskeyProof {
    /// The credential id, base64url, naming the record in the store.
    pub credential_id: String,
    /// The clientDataJSON, base64url of the exact bytes the browser posted.
    pub client_data_json: String,
    /// The authenticator data, base64url.
    pub authenticator_data: String,
    /// The signature over `authenticatorData || SHA-256(clientDataJSON)`,
    /// base64url.
    pub signature: String,
}

/// The deterministic challenge an assertion for the decision `decision` on
/// gate `node` of run `run` (request seq `request`, binding nonce `nonce`)
/// must carry in its clientDataJSON. Both the dashboard's ceremony and this
/// verification derive the same value.
pub fn decision_challenge(
    run: &str,
    node: &str,
    decision: &str,
    request: Option<u64>,
    nonce: Option<&str>,
) -> String {
    let material =
        format!("apb-passkey-assertion\0{run}\0{node}\0{decision}\0{request:?}\0{nonce:?}");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(material.as_bytes()))
}

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_ATTESTED_CREDENTIAL_DATA: u8 = 0x40;
const ALG_ES256: i64 = -7;
const ALG_EDDSA: i64 = -8;

/// Why a channel entry's passkey stamp is not accepted.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
    #[error("the passkey store of this machine does not read: {0}")]
    Store(String),
    #[error("credential {0} is not enrolled on this machine")]
    UnknownCredential(String),
    #[error("the passkey assertion's clientDataJSON does not read: {0}")]
    ClientData(String),
    #[error(
        "the passkey assertion does not answer this decision: it was made for run {run}, gate {node}, decision {decision}, request {request:?}, nonce {nonce:?}"
    )]
    ChallengeMismatch {
        run: String,
        node: String,
        decision: String,
        request: Option<u64>,
        nonce: Option<String>,
    },
    #[error("the passkey assertion did not include user verification (UV) or user presence (UP)")]
    Flags,
    #[error("the passkey assertion signature does not verify: {0}")]
    Signature(String),
}

/// A parsed clientDataJSON of an assertion.
#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    typ: String,
    challenge: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: Option<bool>,
}

fn b64u(text: &str) -> Result<Vec<u8>, ProofError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|e| ProofError::ClientData(format!("invalid base64url: {e}")))
}

/// Whether `entry`'s passkey stamp lets the drive consume the decision.
///
/// The store that cannot read refuses every person decision (fail closed, it
/// is the reason the gate exists), the engine's own decisions and a
/// scenario run's scripted one are not a person's consent and need no
/// assertion, a machine without passkeys behaves as before, and everything
/// else must carry a proof that verifies against the store.
/// The facts of a channel entry this module judges, named without the
/// entry type itself (the entry lives above this module; taking the fields
/// keeps the dependency one way).
pub struct Stamp<'a> {
    pub node: &'a str,
    pub decision: &'a str,
    pub request: Option<u64>,
    pub nonce: Option<&'a str>,
    /// The assertion the decision carries, when any.
    pub proof: Option<&'a PasskeyProof>,
    /// The engine decided on its own (an unattended outcome, an enforced
    /// recommendation): never a person's consent, so no assertion.
    pub engine_own: bool,
    /// A scenario run's scripted decision, already confirmed to be inside
    /// a scenario run.
    pub scripted: bool,
}

/// Whether a channel entry's passkey stamp lets the drive consume the
/// decision.
///
/// The store that cannot read refuses every person decision (fail closed, it
/// is the reason the gate exists), the engine's own decisions and a
/// scenario run's scripted one are not a person's consent and need no
/// assertion, a machine without passkeys behaves as before, and everything
/// else must carry a proof that verifies against the store.
pub(crate) fn stamp_ok(run_dir: &Path, stamp: Stamp<'_>) -> bool {
    match apb_core::passkey::count_strict() {
        Err(e) => {
            eprintln!(
                "apb: the passkey store does not read, so no passkey stamp is accepted ({e})"
            );
            false
        }
        Ok(0) => true,
        Ok(_) => {
            if stamp.engine_own || stamp.scripted {
                return true;
            }
            let Some(proof) = stamp.proof else {
                return false;
            };
            verify(
                run_dir,
                stamp.node,
                stamp.decision,
                stamp.request,
                stamp.nonce,
                proof,
            )
            .is_ok()
        }
    }
}

/// Re-verifies `proof` against the machine's passkey store and the stamped
/// decision. Used at consumption; see the module docs for what is and is
/// not covered.
pub fn verify(
    run_dir: &Path,
    node: &str,
    decision: &str,
    request: Option<u64>,
    nonce: Option<&str>,
    proof: &PasskeyProof,
) -> Result<(), ProofError> {
    let path =
        apb_core::passkey::passkey_file_path().map_err(|e| ProofError::Store(e.to_string()))?;
    let records =
        apb_core::passkey::list_from(&path).map_err(|e| ProofError::Store(e.to_string()))?;
    let record = records
        .iter()
        .find(|r| r.id == proof.credential_id)
        .ok_or_else(|| ProofError::UnknownCredential(proof.credential_id.clone()))?;

    let run = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let expected = decision_challenge(run, node, decision, request, nonce);

    let (client, raw_client) = {
        let raw = b64u(&proof.client_data_json)?;
        let parsed: ClientData = serde_json::from_slice(&raw).map_err(|e| {
            ProofError::ClientData(format!("clientDataJSON is not valid JSON: {e}"))
        })?;
        if parsed.cross_origin == Some(true) {
            return Err(ProofError::ClientData(
                "clientDataJSON names crossOrigin: true".to_string(),
            ));
        }
        (parsed, raw)
    };
    if client.typ != "webauthn.get" {
        return Err(ProofError::ClientData(format!(
            "expected ceremony type \"webauthn.get\", got {:?}",
            client.typ
        )));
    }
    let got = b64u(&client.challenge)?;
    let want = b64u(&expected)?;
    if got != want {
        return Err(ProofError::ChallengeMismatch {
            run: run.to_string(),
            node: node.to_string(),
            decision: decision.to_string(),
            request,
            nonce: nonce.map(str::to_string),
        });
    }

    let auth_data = b64u(&proof.authenticator_data)?;
    if auth_data.len() < 37 {
        return Err(ProofError::ClientData(format!(
            "authenticator data is {} bytes, at least 37 required",
            auth_data.len()
        )));
    }
    let flags = auth_data[32];
    if flags & FLAG_UP == 0 || flags & FLAG_UV == 0 || flags & FLAG_ATTESTED_CREDENTIAL_DATA != 0 {
        return Err(ProofError::Flags);
    }
    let count = u32::from_be_bytes([auth_data[33], auth_data[34], auth_data[35], auth_data[36]]);
    // The server bumped the store to this assertion's counter at verify
    // time, so a stamp whose counter does not match what the store now
    // holds was not the assertion the store accepted.
    if record.sign_count != 0 && count != record.sign_count {
        return Err(ProofError::Signature(format!(
            "counter {} does not match the store's {}",
            count, record.sign_count
        )));
    }

    let mut signed = auth_data;
    signed.extend_from_slice(&Sha256::digest(&raw_client));
    let signature = b64u(&proof.signature)
        .map_err(|e| ProofError::Signature(format!("signature is not base64url: {e}")))?;
    verify_signature(&record.public_key_cose, record.alg, &signed, &signature)
}

/// Verifies an assertion signature with a stored COSE key: ES256 with SHA-256
/// over the signed message with a DER signature, or EdDSA over the signed
/// message.
fn verify_signature(
    public_key_cose_b64: &str,
    alg: i64,
    message: &[u8],
    signature: &[u8],
) -> Result<(), ProofError> {
    use base64::engine::general_purpose::STANDARD;
    let cose = STANDARD
        .decode(public_key_cose_b64)
        .map_err(|e| ProofError::Store(format!("the stored COSE key does not decode: {e}")))?;
    let coords = cose_coordinates(&cose).map_err(|e| ProofError::Store(e.to_string()))?;
    match alg {
        ALG_ES256 => {
            let x: [u8; 32] =
                coords.x.as_slice().try_into().map_err(|_| {
                    ProofError::Store("P-256 x coordinate is not 32 bytes".to_string())
                })?;
            let y: [u8; 32] =
                coords.y.as_slice().try_into().map_err(|_| {
                    ProofError::Store("P-256 y coordinate is not 32 bytes".to_string())
                })?;
            let point =
                p256::elliptic_curve::sec1::EncodedPoint::<p256::NistP256>::from_affine_coordinates(
                    (&x).into(),
                    (&y).into(),
                    false,
                );
            let vk = p256::ecdsa::VerifyingKey::from_encoded_point(&point)
                .map_err(|_| ProofError::Store("P-256 point not on curve".to_string()))?;
            let sig = p256::ecdsa::Signature::from_der(signature)
                .map_err(|_| ProofError::Signature("the signature is not DER".to_string()))?;
            use p256::ecdsa::signature::Verifier as _;
            vk.verify(message, &sig)
                .map_err(|_| ProofError::Signature("the signature does not verify".to_string()))
        }
        ALG_EDDSA => {
            let key: [u8; 32] =
                coords.x.as_slice().try_into().map_err(|_| {
                    ProofError::Store("the Ed25519 key is not 32 bytes".to_string())
                })?;
            let vk = ed25519_dalek::VerifyingKey::from_bytes(&key)
                .map_err(|_| ProofError::Store("the Ed25519 key is rejected".to_string()))?;
            let sig = ed25519_dalek::Signature::from_slice(signature)
                .map_err(|_| ProofError::Signature("the signature is not 64 bytes".to_string()))?;
            vk.verify_strict(message, &sig)
                .map_err(|_| ProofError::Signature("the signature does not verify".to_string()))
        }
        other => Err(ProofError::Store(format!(
            "the stored COSE key names alg {other}, only -7 and -8 are supported"
        ))),
    }
}

/// The x (and for EC2 the y) coordinate of a stored canonical COSE key.
struct Coordinates {
    x: Vec<u8>,
    y: Vec<u8>,
}

fn cose_coordinates(cose: &[u8]) -> Result<Coordinates, String> {
    let value: ciborium::Value = ciborium::from_reader(cose)
        .map_err(|e| format!("the stored COSE key is not valid CBOR: {e}"))?;
    let map = value
        .into_map()
        .map_err(|_| "the stored COSE key is not a CBOR map".to_string())?;
    let bytes = |key: i64| -> Option<Vec<u8>> {
        map.iter().find_map(|(k, v)| match (k, v) {
            (ciborium::Value::Integer(n), ciborium::Value::Bytes(b))
                if i64::try_from(*n).ok() == Some(key) =>
            {
                Some(b.clone())
            }
            _ => None,
        })
    };
    let x = bytes(-2).ok_or("the stored COSE key has no x coordinate")?;
    let y = bytes(-3).unwrap_or_default();
    Ok(Coordinates { x, y })
}
