//! OAuth2 refresh helpers that touch no network: parsing a token endpoint
//! answer, scope arithmetic, PKCE (RFC 7636), the authorization URL, and the
//! consent callback target. The token source that uses them lives in the
//! engine (`apb_engine::connector::oauth`); nothing here holds state.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::secret::Secret;

/// A token endpoint's answer, parsed. `access_token` is sealed; the value
/// never appears in a log, an error message or a file.
#[derive(Debug)]
pub struct TokenGrant {
    pub access_token: Secret,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
    pub refresh_token: Option<Secret>,
}

/// Parses a 2xx token response body. `Err` names the missing field only,
/// never echoes the body.
pub fn parse_token_response(body: &Value) -> Result<TokenGrant, String> {
    let Some(access) = body.get("access_token").and_then(|v| v.as_str()) else {
        return Err("the token response carries no `access_token` field".to_string());
    };
    if access.is_empty() {
        return Err("the token response carries an empty `access_token` field".to_string());
    }
    Ok(TokenGrant {
        access_token: Secret::new(access),
        expires_in: body.get("expires_in").and_then(|v| v.as_u64()),
        scope: body
            .get("scope")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        refresh_token: body
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(Secret::new),
    })
}

/// The token endpoint's error code (`invalid_grant`, `invalid_client`, ...)
/// from a 4xx body. `error_description` is never returned: a provider may
/// fill it with request data.
pub fn token_error_code(body: &Value) -> Option<String> {
    body.get("error")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `Ok` when `required` is empty or one entry is in `granted`; else the
/// sorted required list for the message.
pub fn check_scopes(required: &[String], granted: &BTreeSet<String>) -> Result<(), Vec<String>> {
    if required.is_empty() || required.iter().any(|r| granted.contains(r)) {
        return Ok(());
    }
    let mut sorted = required.to_vec();
    sorted.sort();
    Err(sorted)
}

/// Splits a space-separated scope string into a set (empty tokens dropped).
pub fn scope_set(text: &str) -> BTreeSet<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// 32 random bytes as unpadded base64url (43 chars).
pub fn new_verifier() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system provides randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7636 S256: unpadded base64url of SHA-256 of the verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 16 random bytes as unpadded base64url.
pub fn new_state() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system provides randomness");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The fixed parameters of one authorization URL.
pub struct AuthorizeParams<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scopes: &'a str,
    pub state: &'a str,
    pub challenge: &'a str,
    pub extras: &'a BTreeMap<String, String>,
}

/// The authorization URL: `base` plus client_id, redirect_uri,
/// response_type=code, scope, state, code_challenge,
/// code_challenge_method=S256 and the extras, each value percent-encoded
/// with `crate::url_component::encode`; keys in that order, then the extras
/// in `BTreeMap` order.
pub fn authorize_url(base: &str, p: &AuthorizeParams<'_>) -> String {
    use crate::url_component::encode;
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut pairs: Vec<String> = vec![
        format!("client_id={}", encode(p.client_id)),
        format!("redirect_uri={}", encode(p.redirect_uri)),
        "response_type=code".to_string(),
        format!("scope={}", encode(p.scopes)),
        format!("state={}", encode(p.state)),
        format!("code_challenge={}", encode(p.challenge)),
        "code_challenge_method=S256".to_string(),
    ];
    for (k, v) in p.extras {
        pairs.push(format!("{k}={}", encode(v)));
    }
    format!("{base}{sep}{}", pairs.join("&"))
}

/// The `code`, `state` and `error` of a callback request target
/// (`/?code=...&state=...`), percent-decoded. `duplicate` is true when one
/// of the three is named more than once; the consent flow ends on it.
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub duplicate: bool,
}

/// The form-decoding of one callback value: percent escapes and `+` for a
/// space. Invalid UTF-8 stays as replacement characters.
fn decode_form(s: &str) -> String {
    percent_encoding::percent_decode_str(&s.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// Parses a consent callback request target. A pair without `=` or outside
/// the three known keys is ignored, so a provider appending something else
/// does not break the flow.
pub fn parse_callback(target: &str) -> CallbackParams {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut out = CallbackParams {
        code: None,
        state: None,
        error: None,
        duplicate: false,
    };
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let value = decode_form(v);
        let slot = match k {
            "code" => &mut out.code,
            "state" => &mut out.state,
            "error" => &mut out.error,
            _ => continue,
        };
        if slot.is_some() {
            out.duplicate = true;
        }
        *slot = Some(value);
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn pkce_challenge_matches_rfc_7636_appendix_b() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_43_base64url_chars_and_two_differ() {
        let v1 = new_verifier();
        let v2 = new_verifier();
        assert_eq!(v1.len(), 43, "{v1}");
        assert!(
            v1.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(v1, v2);
    }

    #[test]
    fn a_state_is_url_safe_and_two_differ() {
        let s1 = new_state();
        let s2 = new_state();
        assert!(
            s1.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_ne!(s1, s2);
    }

    #[test]
    fn scopes_are_any_of() {
        let required = vec!["https://a".to_string(), "https://b".to_string()];
        let granted: BTreeSet<String> = ["https://b".to_string()].into_iter().collect();
        assert!(check_scopes(&required, &granted).is_ok());
        let none: BTreeSet<String> = BTreeSet::new();
        let err = check_scopes(&required, &none).unwrap_err();
        assert_eq!(err, vec!["https://a".to_string(), "https://b".to_string()]);
        // An empty required list needs nothing.
        assert!(check_scopes(&[], &none).is_ok());
    }

    #[test]
    fn a_scope_set_drops_empty_tokens() {
        let set = scope_set("  a   b  ");
        assert_eq!(
            set,
            ["a".to_string(), "b".to_string()].into_iter().collect()
        );
        assert!(scope_set("").is_empty());
    }

    #[test]
    fn a_token_answer_without_access_token_names_the_field_only() {
        let body = serde_json::json!({
            "error": "oops the refresh token abcdefgh leaked here",
            "token_type": "bearer"
        });
        let err = parse_token_response(&body).unwrap_err();
        assert!(err.contains("access_token"), "{err}");
        assert!(
            !err.contains("abcdefgh"),
            "the error echoed the body: {err}"
        );
    }

    #[test]
    fn a_token_answer_parses_into_a_grant() {
        let body = serde_json::json!({
            "access_token": "at1",
            "expires_in": 3600,
            "scope": "scope.a scope.b",
            "refresh_token": "rt1"
        });
        let grant = parse_token_response(&body).unwrap();
        assert_eq!(grant.access_token.expose_secret(), "at1");
        assert_eq!(grant.expires_in, Some(3600));
        assert_eq!(grant.scope.as_deref(), Some("scope.a scope.b"));
        assert_eq!(grant.refresh_token.unwrap().expose_secret(), "rt1");

        let minimal = serde_json::json!({ "access_token": "at2" });
        let grant = parse_token_response(&minimal).unwrap();
        assert_eq!(grant.expires_in, None);
        assert!(grant.scope.is_none());
        assert!(grant.refresh_token.is_none());
    }

    #[test]
    fn the_token_error_code_comes_from_the_body() {
        assert_eq!(
            token_error_code(&serde_json::json!({ "error": "invalid_grant" })),
            Some("invalid_grant".to_string())
        );
        assert_eq!(token_error_code(&serde_json::json!({ "nope": 1 })), None);
    }

    #[test]
    fn the_authorize_url_orders_and_encodes_its_parameters() {
        let extras: BTreeMap<String, String> = [
            ("access_type".to_string(), "offline".to_string()),
            ("prompt".to_string(), "consent".to_string()),
        ]
        .into_iter()
        .collect();
        let p = AuthorizeParams {
            client_id: "c;id",
            redirect_uri: "http://127.0.0.1:8080/",
            scopes: "scope.a scope.b",
            state: "st/ate",
            challenge: "ch",
            extras: &extras,
        };
        let url = authorize_url("https://accounts.example.com/o/oauth2/v2/auth", &p);
        assert_eq!(
            url,
            "https://accounts.example.com/o/oauth2/v2/auth?client_id=c%3Bid\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2F\
             &response_type=code\
             &scope=scope.a%20scope.b\
             &state=st%2Fate\
             &code_challenge=ch\
             &code_challenge_method=S256\
             &access_type=offline\
             &prompt=consent"
        );
    }

    #[test]
    fn a_callback_target_decodes_percent_and_plus() {
        let cb = parse_callback("/?code=c%2F1+2&state=s%20t&error=access_denied");
        assert_eq!(cb.code.as_deref(), Some("c/1 2"));
        assert_eq!(cb.state.as_deref(), Some("s t"));
        assert_eq!(cb.error.as_deref(), Some("access_denied"));
        assert!(!cb.duplicate);

        let plain = parse_callback("/?code=c1&state=s1");
        assert_eq!(plain.code.as_deref(), Some("c1"));
        assert!(plain.error.is_none());
    }

    #[test]
    fn a_repeated_callback_parameter_is_ambiguous() {
        let cb = parse_callback("/?code=c1&code=c2&state=s1");
        assert!(cb.duplicate);
        let cb = parse_callback("/?code=c1&state=s1&error=x&error=y");
        assert!(cb.duplicate);
        let cb = parse_callback("/?code=c1&state=s1");
        assert!(!cb.duplicate);
    }
}
