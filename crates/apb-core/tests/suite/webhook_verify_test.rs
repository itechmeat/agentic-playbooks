//! `apb_core::connector::webhook`: the inbound verification primitives.
//!
//! The pinned digests are RFC 4231 section 4 test vectors for HMAC-SHA256.
//! They are the standard's own published values, so they check the helper
//! against the algorithm rather than against a third party's documentation
//! sample that this suite could not verify offline.

use apb_core::connector::webhook::{
    self, Challenge, HUB_CHALLENGE, HUB_MODE, HUB_TOKEN, SUBSCRIBE,
};
use std::collections::BTreeMap;

#[test]
fn hmac_matches_the_rfc_4231_vectors() {
    // RFC 4231, section 4.2: key = 20 bytes of 0x0b, data = "Hi There".
    let key = vec![0x0bu8; 20];
    assert_eq!(
        webhook::hmac_sha256_hex(&key, b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    // RFC 4231, section 4.3: key = "Jefe".
    assert_eq!(
        webhook::hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn verify_accepts_the_prefixed_header_and_rejects_everything_else() {
    let secret = "app-secret";
    let body = br#"{"object":"whatsapp_business_account","entry":[{"id":"1"}]}"#;
    let digest = webhook::hmac_sha256_hex(secret.as_bytes(), body);
    let header = format!("sha256={digest}");

    assert!(webhook::verify_signature_hex(
        secret, body, &header, "sha256="
    ));
    // The prefix is part of the contract: a bare digest is not accepted when
    // the connector declares one.
    assert!(!webhook::verify_signature_hex(
        secret, body, &digest, "sha256="
    ));
    // An empty prefix means the header carries the bare digest.
    assert!(webhook::verify_signature_hex(secret, body, &digest, ""));

    assert!(!webhook::verify_signature_hex(
        "wrong-secret",
        body,
        &header,
        "sha256="
    ));
    assert!(!webhook::verify_signature_hex(
        secret,
        b"tampered",
        &header,
        "sha256="
    ));
    assert!(!webhook::verify_signature_hex(
        secret, body, "sha256=", "sha256="
    ));
    assert!(!webhook::verify_signature_hex(secret, body, "", "sha256="));
    assert!(
        !webhook::verify_signature_hex(
            secret,
            body,
            &format!("sha256={}", &digest[..40]),
            "sha256="
        ),
        "a truncated digest must not match a prefix of the real one"
    );
    assert!(
        webhook::verify_signature_hex(
            secret,
            body,
            &header.to_uppercase().replace("SHA256=", "sha256="),
            "sha256="
        ),
        "hex comparison is case-insensitive on the digest itself"
    );
    // One byte flipped anywhere in the body changes the verdict.
    let mut tampered = body.to_vec();
    tampered[10] ^= 0x01;
    assert!(!webhook::verify_signature_hex(
        secret, &tampered, &header, "sha256="
    ));
}

/// An empty resolved secret must verify nothing at all.
///
/// `HMAC-SHA256` accepts a zero-length key, so without an explicit guard the
/// expected digest is one any caller can compute: the key is public by
/// construction. An operator reaches that state by ordinary means (an
/// `APP_SECRET=` line in `secrets.env`, `Environment=APP_SECRET=` in a
/// systemd unit, `export APP_SECRET="$SOMETHING_UNSET"` in a wrapper), and
/// `required: true` on the account field is satisfied by a present-but-empty
/// value, so nothing else in the chain refuses it. The challenge path already
/// refuses an empty configured token for the same reason.
#[test]
fn an_empty_secret_verifies_nothing() {
    let body = br#"{"id":"evt-1"}"#;
    // The signature an attacker would send: the correct HMAC under the empty
    // key, which is exactly what a naive implementation would compute and
    // accept.
    let forged = format!("sha256={}", webhook::hmac_sha256_hex(b"", body));
    assert!(
        !webhook::verify_signature_hex("", body, &forged, "sha256="),
        "an empty secret must not accept the digest of the empty key"
    );
    // And nothing else verifies under it either, well-formed or not.
    for presented in [
        "sha256=0000000000000000000000000000000000000000000000000000000000000000",
        "sha256=",
        "",
        "deadbeef",
    ] {
        assert!(
            !webhook::verify_signature_hex("", body, presented, "sha256="),
            "empty secret, presented {presented:?}"
        );
    }
    // The shape check is secret-free, so it is unaffected: it says only that
    // this value could be a digest, never that it verifies.
    assert!(webhook::signature_is_well_formed(&forged, "sha256="));
    assert!(!webhook::signature_is_well_formed(
        "sha256=nothex",
        "sha256="
    ));
    assert!(!webhook::signature_is_well_formed("", "sha256="));
}

fn hub(mode: &str, token: &str, challenge: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (HUB_MODE.to_string(), mode.to_string()),
        (HUB_TOKEN.to_string(), token.to_string()),
        (HUB_CHALLENGE.to_string(), challenge.to_string()),
    ])
}

#[test]
fn meta_hub_echoes_the_challenge_only_on_an_exact_token_match() {
    let token = "the-verify-token";
    assert_eq!(
        webhook::meta_hub_challenge(&hub(SUBSCRIBE, token, "1158201444"), token),
        Challenge::Echo("1158201444".to_string())
    );
    assert_eq!(
        webhook::meta_hub_challenge(&hub(SUBSCRIBE, "other", "1158201444"), token),
        Challenge::Reject,
        "a wrong token is refused"
    );
    assert_eq!(
        webhook::meta_hub_challenge(&hub("unsubscribe", token, "1158201444"), token),
        Challenge::Reject,
        "only hub.mode=subscribe is answered"
    );
    assert_eq!(
        webhook::meta_hub_challenge(&BTreeMap::new(), token),
        Challenge::Reject,
        "missing params are refused, not treated as empty matches"
    );
    assert_eq!(
        webhook::meta_hub_challenge(&hub(SUBSCRIBE, token, ""), token),
        Challenge::Reject,
        "an empty challenge has nothing to echo"
    );
    assert_eq!(
        webhook::meta_hub_challenge(&hub(SUBSCRIBE, "", ""), ""),
        Challenge::Reject,
        "an empty configured token never verifies anything"
    );
}

#[test]
fn dedupe_id_uses_the_path_when_it_resolves_and_the_body_hash_otherwise() {
    let body = serde_json::json!({
        "entry": [{ "id": "wamid.HBg", "changes": [] }]
    });
    let raw = serde_json::to_vec(&body).unwrap();
    assert_eq!(
        webhook::dedupe_id(&body, &raw, Some("entry.0.id")),
        "wamid.HBg"
    );
    // A path that does not resolve, or resolves to a non-scalar, falls back
    // to the body hash rather than silently deduplicating everything to one
    // constant.
    let fallback = webhook::dedupe_id(&body, &raw, Some("entry.0.missing"));
    assert!(fallback.starts_with("sha256:"), "was: {fallback}");
    assert_eq!(fallback, webhook::dedupe_id(&body, &raw, None));
    assert_eq!(
        webhook::dedupe_id(&body, &raw, Some("entry")),
        fallback,
        "an array is not an id"
    );
    // Numbers and booleans are legitimate ids on some providers.
    let numeric = serde_json::json!({ "id": 42 });
    assert_eq!(
        webhook::dedupe_id(&numeric, b"{\"id\":42}", Some("id")),
        "42"
    );
    // Two different bodies hash differently, one body hashes stably.
    assert_ne!(
        webhook::dedupe_id(&body, b"a", None),
        webhook::dedupe_id(&body, b"b", None)
    );
    assert_eq!(
        webhook::dedupe_id(&body, b"a", None),
        webhook::dedupe_id(&body, b"a", None)
    );
}
