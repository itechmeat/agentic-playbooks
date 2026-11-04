//! Tests for the WebAuthn verifier: fixed-vector style, every key derived
//! from a fixed byte seed so the vectors are stable, every refusal case
//! asserting the exact error.

use super::*;

/// The rp the tests verify against.
fn test_rp() -> RpConfig {
    RpConfig {
        rp_id: "passkeys.example.com".to_string(),
        origin: "https://passkeys.example.com".to_string(),
    }
}

const ES256_SEED: [u8; 32] = *b"apb-webauthn-es256-test-seed----";
const EDDSA_SEED: [u8; 32] = *b"apb-webauthn-eddsa-test-seed----";
const CHALLENGE_BYTES: [u8; 32] = *b"apb-webauthn-challenge-vector---";

fn challenge() -> String {
    b64url_encode(&CHALLENGE_BYTES)
}

fn client_data(typ: &str, challenge: &str, origin: &str) -> String {
    format!(r#"{{"type":"{typ}","challenge":"{challenge}","origin":"{origin}"}}"#)
}

/// The fixed ES256 signing key of the vectors.
fn es256_signing_key() -> p256::ecdsa::SigningKey {
    let secret = p256::SecretKey::from_bytes(&ES256_SEED.into())
        .expect("fixed test seed is a valid P-256 secret");
    p256::ecdsa::SigningKey::from(&secret)
}

/// The ES256 public key coordinates of the fixed seed.
fn es256_xy() -> ([u8; 32], [u8; 32]) {
    let key = es256_signing_key();
    let vk = key.verifying_key();
    let public = elliptic_curve::PublicKey::<p256::NistP256>::from(*vk);
    let point = elliptic_curve::sec1::ToEncodedPoint::to_encoded_point(&public, false);
    let mut x = [0u8; 32];
    let mut y = [0u8; 32];
    x.copy_from_slice(point.x().expect("non-identity point").as_ref());
    y.copy_from_slice(point.y().expect("non-identity point").as_ref());
    (x, y)
}

/// The fixed Ed25519 signing key of the vectors.
fn eddsa_signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&EDDSA_SEED)
}

fn eddsa_public() -> [u8; 32] {
    eddsa_signing_key().verifying_key().to_bytes()
}

/// Builds authenticator data: rp id hash, flags, counter, and optionally the
/// attested credential data with the given COSE key.
fn auth_data(rp_id: &str, flags: u8, count: u32, attested: Option<(&[u8], &[u8])>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&Sha256::digest(rp_id.as_bytes()));
    out.push(flags);
    out.extend_from_slice(&count.to_be_bytes());
    if let Some((cred_id, cose_key)) = attested {
        out.extend_from_slice(&[0u8; 16]);
        out.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
        out.extend_from_slice(cred_id);
        out.extend_from_slice(cose_key);
    }
    out
}

fn attestation_object(auth_data_bytes: &[u8]) -> Vec<u8> {
    let map = Value::Map(vec![
        ("fmt".into(), "none".into()),
        ("attStmt".into(), Value::Map(vec![])),
        ("authData".into(), Value::Bytes(auth_data_bytes.to_vec())),
    ]);
    cbor_bytes(&map)
}

fn cbor_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(value, &mut out).expect("CBOR serialization cannot fail");
    out
}

const UP_UV_AT: u8 = FLAG_UP | FLAG_UV | FLAG_ATTESTED_CREDENTIAL_DATA;
const UP_UV: u8 = FLAG_UP | FLAG_UV;

/// The stored COSE key of the fixed ES256 vector, as registration returns it.
fn es256_stored_cose() -> Vec<u8> {
    let (x, y) = es256_xy();
    cose_es256(x, y)
}

fn valid_es256_registration() -> (String, Vec<u8>, RpConfig) {
    let (x, y) = es256_xy();
    let cose = cose_es256(x, y);
    let cred_id = b"apb-test-credential-id".to_vec();
    let ad = auth_data("passkeys.example.com", UP_UV_AT, 0, Some((&cred_id, &cose)));
    let origin = "https://passkeys.example.com";
    (
        client_data("webauthn.create", &challenge(), origin),
        attestation_object(&ad),
        test_rp(),
    )
}

fn valid_eddsa_registration() -> (String, Vec<u8>, RpConfig) {
    let cose = cose_eddsa(eddsa_public());
    let cred_id = b"apb-test-credential-id-ed".to_vec();
    let ad = auth_data("passkeys.example.com", UP_UV_AT, 0, Some((&cred_id, &cose)));
    let origin = "https://passkeys.example.com";
    (
        client_data("webauthn.create", &challenge(), origin),
        attestation_object(&ad),
        test_rp(),
    )
}

/// Signs `authenticator_data || sha256(client_data_json)` with the fixed
/// ES256 key and returns the DER signature.
fn es256_sign(ad: &[u8], client_data_json: &str) -> Vec<u8> {
    use p256::ecdsa::signature::Signer;
    let mut message = ad.to_vec();
    message.extend_from_slice(&Sha256::digest(client_data_json.as_bytes()));
    let signature: p256::ecdsa::Signature = es256_signing_key().sign(&message);
    signature.to_der().as_bytes().to_vec()
}

fn eddsa_sign(ad: &[u8], client_data_json: &str) -> Vec<u8> {
    use ed25519_dalek::Signer;
    let mut message = ad.to_vec();
    message.extend_from_slice(&Sha256::digest(client_data_json.as_bytes()));
    eddsa_signing_key().sign(&message).to_bytes().to_vec()
}

fn valid_es256_assertion() -> (String, Vec<u8>, Vec<u8>, RpConfig) {
    let origin = "https://passkeys.example.com";
    let cd = client_data("webauthn.get", &challenge(), origin);
    let ad = auth_data("passkeys.example.com", UP_UV, 5, None);
    let sig = es256_sign(&ad, &cd);
    (cd, ad, sig, test_rp())
}

fn valid_eddsa_assertion() -> (String, Vec<u8>, Vec<u8>, RpConfig) {
    let origin = "https://passkeys.example.com";
    let cd = client_data("webauthn.get", &challenge(), origin);
    let ad = auth_data("passkeys.example.com", UP_UV, 7, None);
    let sig = eddsa_sign(&ad, &cd);
    (cd, ad, sig, test_rp())
}

#[test]
fn es256_registration_verifies() {
    let (cd, att, rp) = valid_es256_registration();
    let cred = verify_registration(&cd, &att, &challenge(), &rp).expect("valid registration");
    assert_eq!(cred.alg, ALG_ES256);
    assert_eq!(cred.credential_id, b64url_encode(b"apb-test-credential-id"));
    assert_eq!(cred.sign_count, 0);
    // The canonical COSE key round-trips through the parser unchanged.
    let (canonical, _) = parse_and_canonicalize_cose_key(&cred.cose_key).expect("valid key");
    assert_eq!(canonical, cred.cose_key);
}

#[test]
fn eddsa_registration_verifies() {
    let (cd, att, rp) = valid_eddsa_registration();
    let cred = verify_registration(&cd, &att, &challenge(), &rp).expect("valid registration");
    assert_eq!(cred.alg, ALG_EDDSA);
    assert_eq!(cred.sign_count, 0);
}

#[test]
fn es256_assertion_verifies_and_returns_counter() {
    let (cd, ad, sig, rp) = valid_es256_assertion();
    let cose = es256_stored_cose();
    let new_count =
        verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 3).expect("valid assertion");
    assert_eq!(new_count, 5);
}

#[test]
fn eddsa_assertion_verifies_and_returns_counter() {
    let (cd, ad, sig, rp) = valid_eddsa_assertion();
    let cose = cose_eddsa(eddsa_public());
    let new_count =
        verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 7).expect("valid assertion");
    assert_eq!(new_count, 7);
}

#[test]
fn wrong_origin_refused() {
    let (_, cose, rp) = valid_es256_registration();
    let evil = "https://evil.example.net";
    let cd = client_data("webauthn.get", &challenge(), evil);
    let ad = auth_data("passkeys.example.com", UP_UV, 5, None);
    let sig = es256_sign(&ad, &cd);
    let err =
        verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 3).expect_err("origin mismatch");
    assert!(
        matches!(err, WebAuthnError::OriginMismatch(_)),
        "got {err:?}"
    );
    // The registration path refuses the same way.
    let (x, y) = es256_xy();
    let att_cose = cose_es256(x, y);
    let cred_id = b"cred".to_vec();
    let ad = auth_data(
        "passkeys.example.com",
        UP_UV_AT,
        0,
        Some((&cred_id, &att_cose)),
    );
    let cd = client_data("webauthn.create", &challenge(), evil);
    let err = verify_registration(&cd, &attestation_object(&ad), &challenge(), &rp)
        .expect_err("registration origin mismatch");
    assert!(
        matches!(err, WebAuthnError::OriginMismatch(_)),
        "got {err:?}"
    );
}

#[test]
fn wrong_rp_id_hash_refused() {
    let (_, cose, rp) = valid_es256_registration();
    let cd = client_data("webauthn.get", &challenge(), "https://passkeys.example.com");
    // Authenticator data bound to another rp id.
    let ad = auth_data("other.example.com", UP_UV, 5, None);
    let sig = es256_sign(&ad, &cd);
    let err =
        verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 3).expect_err("rp id mismatch");
    assert!(matches!(err, WebAuthnError::RpIdMismatch), "got {err:?}");
}

#[test]
fn replayed_challenge_refused() {
    let (cd, att, rp) = valid_es256_registration();
    let other = b64url_encode(b"apb-webauthn-challenge-vector-1\0\0");
    let err = verify_registration(&cd, &att, &other, &rp).expect_err("challenge mismatch");
    assert!(
        matches!(err, WebAuthnError::ChallengeMismatch),
        "got {err:?}"
    );
}

#[test]
fn uv_missing_refused() {
    let (x, y) = es256_xy();
    let cose = cose_es256(x, y);
    let cred_id = b"cred".to_vec();
    let ad = auth_data(
        "passkeys.example.com",
        FLAG_UP | FLAG_ATTESTED_CREDENTIAL_DATA,
        0,
        Some((&cred_id, &cose)),
    );
    let (cd, _, rp) = valid_es256_registration();
    let err = verify_registration(&cd, &attestation_object(&ad), &challenge(), &rp)
        .expect_err("UV missing");
    assert!(matches!(err, WebAuthnError::UvRequired), "got {err:?}");
}

#[test]
fn up_missing_refused() {
    let (x, y) = es256_xy();
    let cose = cose_es256(x, y);
    let cred_id = b"cred".to_vec();
    let ad = auth_data(
        "passkeys.example.com",
        FLAG_UV | FLAG_ATTESTED_CREDENTIAL_DATA,
        0,
        Some((&cred_id, &cose)),
    );
    let (cd, _, rp) = valid_es256_registration();
    let err = verify_registration(&cd, &attestation_object(&ad), &challenge(), &rp)
        .expect_err("UP missing");
    assert!(
        matches!(err, WebAuthnError::UserPresenceRequired),
        "got {err:?}"
    );
}

#[test]
fn counter_regressed_refused() {
    let (cd, ad, sig, rp) = valid_es256_assertion();
    let cose = es256_stored_cose();
    let err = verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 6)
        .expect_err("counter regression");
    match err {
        WebAuthnError::CounterRegressed { stored, got } => {
            assert_eq!(stored, 6);
            assert_eq!(got, 5);
        }
        other => panic!("expected CounterRegressed, got {other:?}"),
    }
}

#[test]
fn bad_signature_refused() {
    let (cd, ad, mut sig, rp) = valid_es256_assertion();
    let cose = es256_stored_cose();
    sig[0] ^= 0x01;
    let err = verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 3)
        .expect_err("flipped signature byte");
    assert!(matches!(err, WebAuthnError::BadSignature), "got {err:?}");
}

#[test]
fn signature_over_different_challenge_rejected() {
    // A valid ES256 DER signature produced once, then the challenge byte in
    // the client data differs: the same signature must be rejected.
    let (cd, ad, sig, rp) = valid_es256_assertion();
    let cose = es256_stored_cose();
    let mut other_challenge = CHALLENGE_BYTES;
    other_challenge[0] ^= 0x01;
    let tampered_challenge = b64url_encode(&other_challenge);
    let tampered = client_data("webauthn.get", &tampered_challenge, &rp.origin);
    let err = verify_assertion(&tampered, &ad, &sig, &tampered_challenge, &rp, &cose, 3)
        .expect_err("signature over a different challenge");
    assert!(matches!(err, WebAuthnError::BadSignature), "got {err:?}");
    // The untouched vector still verifies with the same signature.
    let _ = verify_assertion(&cd, &ad, &sig, &challenge(), &rp, &cose, 3)
        .expect("untouched vector verifies");
}

#[test]
fn unsupported_algorithm_refused() {
    // A CBOR map naming RS256 (-257) with a 256-byte modulus.
    let cose = cbor_bytes(&Value::Map(vec![
        (Value::from(1i64), Value::from(3i64)),
        (Value::from(3i64), Value::from(-257i64)),
        (Value::from(-1i64), Value::Bytes(vec![0u8; 256])),
        (Value::from(-2i64), Value::Bytes(vec![1u8; 3])),
    ]));
    let cred_id = b"cred".to_vec();
    let ad = auth_data("passkeys.example.com", UP_UV_AT, 0, Some((&cred_id, &cose)));
    let (cd, _, rp) = valid_es256_registration();
    let err = verify_registration(&cd, &attestation_object(&ad), &challenge(), &rp)
        .expect_err("unsupported algorithm");
    assert!(
        matches!(err, WebAuthnError::UnsupportedAlgorithm(-257)),
        "got {err:?}"
    );
}

#[test]
fn attestation_format_not_none_refused() {
    let (x, y) = es256_xy();
    let cose = cose_es256(x, y);
    let cred_id = b"cred".to_vec();
    let ad = auth_data("passkeys.example.com", UP_UV_AT, 0, Some((&cred_id, &cose)));
    let packed = cbor_bytes(&Value::Map(vec![
        ("fmt".into(), "packed".into()),
        (
            "attStmt".into(),
            Value::Map(vec![("sig".into(), Value::Bytes(vec![0u8]))]),
        ),
        ("authData".into(), Value::Bytes(ad)),
    ]));
    let cd = client_data(
        "webauthn.create",
        &challenge(),
        "https://passkeys.example.com",
    );
    let err = verify_registration(&cd, &packed, &challenge(), &test_rp())
        .expect_err("packed attestation");
    assert!(
        matches!(err, WebAuthnError::AttestationNotNone),
        "got {err:?}"
    );
}

#[test]
fn ip_literal_origin_refused() {
    let err = rp_for_origin("https://127.0.0.1:8443").expect_err("IPv4 literal");
    assert!(
        matches!(err, WebAuthnError::IpLiteralRpId(ref h) if h == "127.0.0.1"),
        "got {err:?}"
    );
    let err = rp_for_origin("http://127.0.0.1:8080").expect_err("loopback IPv4 literal");
    assert!(
        matches!(err, WebAuthnError::IpLiteralRpId(_)),
        "got {err:?}"
    );
    let err = rp_for_origin("https://[::1]").expect_err("bracketed IPv6 literal");
    assert!(
        matches!(err, WebAuthnError::IpLiteralRpId(_)),
        "got {err:?}"
    );
    assert!(
        err.to_string()
            .contains("passkeys cannot bind to an IP address")
    );
}

#[test]
fn http_non_loopback_origin_refused() {
    let err = rp_for_origin("http://dashboard.example.com").expect_err("plain http host");
    assert!(matches!(err, WebAuthnError::BadOrigin(_)), "got {err:?}");
}

#[test]
fn loopback_and_https_origins_accepted() {
    let rp = rp_for_origin("https://passkeys.example.com").expect("https origin");
    assert_eq!(rp.rp_id, "passkeys.example.com");
    let rp = rp_for_origin("https://passkeys.example.com:443").expect("default port");
    assert_eq!(rp.rp_id, "passkeys.example.com");
    let rp = rp_for_origin("http://localhost:8080").expect("http loopback name");
    assert_eq!(rp.rp_id, "localhost");
    let rp = rp_for_origin("http://apb.localhost").expect("http *.localhost name");
    assert_eq!(rp.rp_id, "apb.localhost");
}

#[test]
fn origin_comparison_normalizes() {
    // The comparison normalizes scheme and host case and strips the default
    // port; the origin string is kept exactly as configured.
    let rp = RpConfig {
        rp_id: "passkeys.example.com".to_string(),
        origin: "https://passkeys.example.com".to_string(),
    };
    let cd = client_data(
        "webauthn.create",
        &challenge(),
        "HTTPS://PASSKEYS.EXAMPLE.COM:443",
    );
    let (_, att, _) = valid_es256_registration();
    let cred = verify_registration(&cd, &att, &challenge(), &rp).expect("normalized origin");
    assert_eq!(cred.alg, ALG_ES256);
    assert_eq!(
        rp_for_origin("https://Passkeys.Example.COM")
            .expect("rp")
            .origin,
        "https://Passkeys.Example.COM"
    );
}

#[test]
fn wrong_ceremony_type_refused() {
    let (_, att, rp) = valid_es256_registration();
    let cd = client_data("webauthn.get", &challenge(), "https://passkeys.example.com");
    let err = verify_registration(&cd, &att, &challenge(), &rp).expect_err("wrong type");
    assert!(
        matches!(err, WebAuthnError::BadClientData(_)),
        "got {err:?}"
    );
}

#[test]
fn truncated_and_garbage_auth_data_refused() {
    let cose = es256_stored_cose();
    let cred_id = b"cred".to_vec();
    let full = auth_data("passkeys.example.com", UP_UV_AT, 0, Some((&cred_id, &cose)));
    let rp = test_rp();
    let cd = client_data(
        "webauthn.create",
        &challenge(),
        "https://passkeys.example.com",
    );
    // Truncated below the fixed header.
    let err = verify_registration(&cd, &attestation_object(&full[..20]), &challenge(), &rp)
        .expect_err("too short");
    assert!(
        matches!(err, WebAuthnError::BadAuthenticatorData(_)),
        "got {err:?}"
    );
    // Credential id length beyond the remaining bytes.
    let mut cut = full[..37 + 18].to_vec();
    cut[37 + 16..37 + 18].copy_from_slice(&0xFFFFu16.to_be_bytes());
    let err = verify_registration(&cd, &attestation_object(&cut), &challenge(), &rp)
        .expect_err("credential id overruns");
    assert!(
        matches!(err, WebAuthnError::BadAuthenticatorData(_)),
        "got {err:?}"
    );
    // Trailing garbage after the COSE key.
    let mut garbage = full;
    garbage.push(0xFF);
    let err = verify_registration(&cd, &attestation_object(&garbage), &challenge(), &rp)
        .expect_err("trailing garbage");
    assert!(matches!(err, WebAuthnError::BadPublicKey(_)), "got {err:?}");
}

#[test]
fn missing_at_flag_refused() {
    let (cd, _, rp) = valid_es256_registration();
    let ad = auth_data("passkeys.example.com", UP_UV, 0, None);
    let err = verify_registration(&cd, &attestation_object(&ad), &challenge(), &rp)
        .expect_err("no attested credential data");
    assert!(
        matches!(err, WebAuthnError::AttestationNotNone),
        "got {err:?}"
    );
}

#[test]
fn b64url_helpers_round_trip() {
    let bytes = (0u8..=255).collect::<Vec<u8>>();
    let encoded = b64url_encode(&bytes);
    assert!(!encoded.contains('='));
    assert!(!encoded.contains('+'));
    assert!(!encoded.contains('/'));
    assert_eq!(b64url_decode(&encoded).expect("decodes"), bytes);
    // Padded input decodes too.
    assert_eq!(b64url_decode("YQ==").expect("padded decodes"), b"a");
    assert_eq!(b64url_decode("YQ").expect("unpadded decodes"), b"a");
    assert!(b64url_decode("not base64").is_err());
}

#[test]
fn cose_helpers_produce_verifiable_keys() {
    let (x, y) = es256_xy();
    let cose = cose_es256(x, y);
    let (canonical, alg) = parse_and_canonicalize_cose_key(&cose).expect("valid ES256 key");
    assert_eq!(canonical, cose, "the helper already emits canonical CBOR");
    assert_eq!(alg, ALG_ES256);
    let cose = cose_eddsa(eddsa_public());
    let (canonical, alg) = parse_and_canonicalize_cose_key(&cose).expect("valid EdDSA key");
    assert_eq!(canonical, cose);
    assert_eq!(alg, ALG_EDDSA);
}
