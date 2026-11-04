//! The dashboard's passkey routes: listing, the enrollment ceremony (the
//! first passkey only with the one-time code `apb passkey enroll`
//! printed, later ones only with an assertion from an existing one), the
//! assertion ceremony that mints a short-lived token bound to one
//! purpose, and removal.
//!
//! The WebAuthn cryptography itself lives in [`crate::webauthn`]; this
//! module owns the ceremony state ([`crate::passkeys::PasskeyState`]) and
//! the rules around it: the origin must be the dashboard's own (a
//! loopback name or `server.public_base_url`, never an IP-literal RP ID),
//! `userVerification` is required, and nothing secret is stored or
//! logged.

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::passkeys;
use crate::webauthn::{self, RpConfig};

/// The purpose the assertion ceremonies name. Only these shapes are
/// accepted; anything else is refused before a challenge is issued.
#[derive(Deserialize, Serialize)]
pub(crate) struct AssertPurpose {
    /// Deciding one gate: run, node, the chosen option, the request seq
    /// and, for a publish consent or an approval, the binding nonce.
    #[serde(default)]
    decision: Option<DecisionPurpose>,
    /// Adding another passkey (an assertion from an existing one).
    #[serde(default)]
    enroll_new: Option<serde_json::Value>,
    /// Removing one passkey by label.
    #[serde(default)]
    remove: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DecisionPurpose {
    run: String,
    node: String,
    decision: String,
    #[serde(default)]
    request: Option<u64>,
    #[serde(default)]
    nonce: Option<String>,
}

impl AssertPurpose {
    fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    fn validate(&self) -> Result<(), String> {
        if self.decision.is_some() as u8
            + self.enroll_new.is_some() as u8
            + self.remove.is_some() as u8
            != 1
        {
            return Err("purpose names exactly one of decision, enroll_new or remove".to_string());
        }
        if let Some(d) = &self.decision
            && (d.run.is_empty() || d.node.is_empty() || d.decision.is_empty())
        {
            return Err("a decision purpose names run, node and decision".to_string());
        }
        Ok(())
    }

    fn digest(&self) -> String {
        passkeys::purpose_digest(&self.value())
    }
}

#[derive(Deserialize)]
pub(crate) struct EnrollStartBody {
    /// The one-time code `apb passkey enroll` printed, for the first
    /// passkey on this machine. Absent when an existing passkey asserts
    /// (then `assertion_token` names it).
    #[serde(default)]
    code: Option<String>,
    /// The token of an assertion from an existing passkey, purpose
    /// `enroll_new`.
    #[serde(default)]
    assertion_token: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct EnrollFinishBody {
    challenge: String,
    /// The credential id, base64url as the browser sent it. The store is
    /// keyed by it on every later assertion, so it is kept in the request
    /// for symmetry and checked against the attested id.
    id: String,
    /// The client data JSON, verbatim.
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    /// The attestation object, base64url.
    #[serde(rename = "attestationObject")]
    attestation_object: String,
    label: String,
}

#[derive(Deserialize)]
pub(crate) struct AssertChallengeBody {
    purpose: AssertPurpose,
}

#[derive(Deserialize)]
pub(crate) struct AssertVerifyBody {
    purpose: AssertPurpose,
    challenge: String,
    id: String,
    #[serde(rename = "clientDataJSON")]
    client_data_json: String,
    #[serde(rename = "authenticatorData")]
    authenticator_data: String,
    signature: String,
}

#[derive(Deserialize)]
pub(crate) struct RemoveBody {
    token: String,
}

/// The RP the dashboard presents to this browser: the request's own host,
/// which the auth layer has already checked against the server's names.
fn rp_for_request(headers: &HeaderMap) -> Result<RpConfig, String> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.trim().to_string())
        .unwrap_or_default();
    // The origin the browser will assert: `server.public_base_url` when
    // the operator set one, else the request's own host on http (a local
    // dashboard). The client's origin must normalize-match this at verify
    // time, so a dashboard reached on another name is refused there.
    let cfg = apb_core::config::GlobalConfig::load().ok();
    if let Some(origin) = cfg.as_ref().and_then(|c| c.server.public_base_url.clone()) {
        return webauthn::rp_for_origin(&origin).map_err(|e| e.to_string());
    }
    let (name, port) = host
        .rsplit_once(':')
        .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()) && *p != "80")
        .map_or((host.as_str(), ""), |(n, p)| (n, p));
    webauthn::rp_for_origin(&format!("http://{}{}", name, port)).map_err(|e| e.to_string())
}

/// The config dir the ceremony state reads the store through: the same
/// one `apb passkey enroll` wrote the code into.
fn enroll_code_dir() -> Option<std::path::PathBuf> {
    apb_core::config::config_dir()
}

/// GET /api/passkeys: the enrolled passkeys, display facts only.
pub(crate) async fn list_handler(State(_state): State<AppState>) -> Response {
    let file = apb_core::passkey::try_load().unwrap_or_default();
    let passkeys: Vec<serde_json::Value> = file
        .passkeys
        .iter()
        .map(|p| {
            json!({
                "label": p.label,
                "alg": p.alg,
                "sign_count": p.sign_count,
                "created_at": p.created_at,
                "id_prefix": passkeys::credential_prefix(&p.id),
            })
        })
        .collect();
    Json(json!({ "passkeys": passkeys, "count": passkeys.len() })).into_response()
}

/// POST /api/passkeys/enroll/start: hands out the registration challenge.
/// The first passkey consumes a one-time code; any later one consumes an
/// assertion token from an existing passkey.
pub(crate) async fn enroll_start_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EnrollStartBody>,
) -> Response {
    let rp = match rp_for_request(&headers) {
        Ok(rp) => rp,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let enrolled = apb_core::passkey::count();
    let purpose = if enrolled == 0 {
        let Some(code) = body
            .code
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            return (
                StatusCode::FORBIDDEN,
                "the first passkey on this machine is enrolled with `apb passkey enroll`, which prints a one-time code",
            )
                .into_response();
        };
        let Some(dir) = enroll_code_dir() else {
            return (StatusCode::INTERNAL_SERVER_ERROR, "no config directory").into_response();
        };
        match apb_core::passkey::consume_enroll_code(&dir, code) {
            Ok(true) => "enroll-first".to_string(),
            Ok(false) => {
                return (
                    StatusCode::FORBIDDEN,
                    "that code is wrong, used or expired; run `apb passkey enroll` again",
                )
                    .into_response();
            }
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    } else {
        let Some(token) = body.assertion_token.as_deref() else {
            return (
                StatusCode::FORBIDDEN,
                "another passkey needs an assertion from one that exists; use the Passkeys page's Add, which runs the assertion first",
            )
                .into_response();
        };
        let digest = passkeys::purpose_digest(&json!({ "enroll_new": {} }));
        if state.passkeys.take_token(token, &digest).is_none() {
            return (
                StatusCode::FORBIDDEN,
                "that assertion is wrong, used or expired; run it again",
            )
                .into_response();
        }
        "enroll-asserted".to_string()
    };
    let challenge = match state.passkeys.issue_challenge(purpose) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    Json(json!({
        "challenge": challenge,
        "rpId": rp.rp_id,
        "userVerification": "required",
        "attestation": "none",
        "algorithms": [-7, -8],
    }))
    .into_response()
}

/// POST /api/passkeys/enroll/finish: verifies the registration and stores
/// the credential, then tells the person through the notify target.
pub(crate) async fn enroll_finish_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EnrollFinishBody>,
) -> Response {
    let rp = match rp_for_request(&headers) {
        Ok(rp) => rp,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    // The ceremony this challenge answers: first (a consumed code) or a
    // later passkey (a consumed assertion). The purpose digest ties the
    // challenge to what was consumed at start.
    let enrolled_before = apb_core::passkey::count();
    let purpose = if enrolled_before == 0 {
        "enroll-first"
    } else {
        "enroll-asserted"
    };
    if !state.passkeys.consume_challenge(&body.challenge, purpose) {
        return (
            StatusCode::FORBIDDEN,
            "no live enrollment ceremony for this machine; start again",
        )
            .into_response();
    }
    let attestation = match webauthn::b64url_decode(&body.attestation_object) {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "attestationObject is not base64url",
            )
                .into_response();
        }
    };
    let new = match webauthn::verify_registration(
        &body.client_data_json,
        &attestation,
        &body.challenge,
        &rp,
    ) {
        Ok(n) => n,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    if body.id != new.credential_id {
        return (
            StatusCode::BAD_REQUEST,
            "the credential id does not match the attestation",
        )
            .into_response();
    }
    let file_path = match apb_core::passkey::passkey_file_path() {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let label = body.label.trim();
    if label.is_empty() || label.len() > 100 || label.contains('\n') {
        return (
            StatusCode::BAD_REQUEST,
            "the label is one line of 1 to 100 characters",
        )
            .into_response();
    }
    let cose_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&new.cose_key)
    };
    let record = apb_core::passkey::add_into(
        &file_path,
        label,
        &new.credential_id,
        &cose_b64,
        new.alg,
        new.sign_count,
    );
    let record = match record {
        Ok(r) => r,
        Err(e) => return (StatusCode::CONFLICT, e.to_string()).into_response(),
    };
    notify_passkey(
        "a passkey was added",
        &format!(
            "a passkey was added on {}: label `{}`",
            machine_name(),
            record.label
        ),
    );
    Json(json!({
        "label": record.label,
        "created_at": record.created_at,
        "id_prefix": passkeys::credential_prefix(&record.id),
    }))
    .into_response()
}

/// POST /api/passkeys/assert/challenge: issues the assertion challenge for
/// one validated purpose.
pub(crate) async fn assert_challenge_handler(
    State(state): State<AppState>,
    Json(body): Json<AssertChallengeBody>,
) -> Response {
    if let Err(why) = body.purpose.validate() {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    if apb_core::passkey::count() == 0 {
        return (
            StatusCode::CONFLICT,
            "no passkey is enrolled on this machine; nothing can assert",
        )
            .into_response();
    }
    match state.passkeys.issue_challenge(body.purpose.digest()) {
        Ok(challenge) => Json(json!({ "challenge": challenge })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// POST /api/passkeys/assert/verify: verifies the assertion against the
/// named credential, bumps its sign count, and mints the single-use
/// token the purpose consumes.
pub(crate) async fn assert_verify_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AssertVerifyBody>,
) -> Response {
    if let Err(why) = body.purpose.validate() {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    let rp = match rp_for_request(&headers) {
        Ok(rp) => rp,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let file_path = match apb_core::passkey::passkey_file_path() {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let all = apb_core::passkey::list_from(&file_path).unwrap_or_default();
    let Some(record) = all.iter().find(|p| p.id == body.id) else {
        return (
            StatusCode::FORBIDDEN,
            "that credential is not enrolled on this machine",
        )
            .into_response();
    };
    if !state
        .passkeys
        .consume_challenge(&body.challenge, &body.purpose.digest())
    {
        return (
            StatusCode::FORBIDDEN,
            "no live assertion challenge for this purpose; request one again",
        )
            .into_response();
    }
    let auth_data = match webauthn::b64url_decode(&body.authenticator_data) {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "authenticatorData is not base64url",
            )
                .into_response();
        }
    };
    let signature = match webauthn::b64url_decode(&body.signature) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "signature is not base64url").into_response(),
    };
    let cose = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(&record.public_key_cose)
            .unwrap_or_default()
    };
    let verify = webauthn::verify_assertion(
        &body.client_data_json,
        &auth_data,
        &signature,
        &body.challenge,
        &rp,
        &cose,
        record.sign_count,
    );
    let new_count = match verify {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    // The sign count goes back into the store, so a cloned authenticator
    // replaying an older counter is refused next time.
    if new_count != record.sign_count
        && let Err(e) = apb_core::passkey::bump_sign_count(&file_path, &record.id, new_count)
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let token = match state.passkeys.issue_token(
        body.purpose.digest(),
        passkeys::credential_prefix(&record.id),
    ) {
        Ok(t) => t,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    Json(json!({ "token": token, "credential_prefix": passkeys::credential_prefix(&record.id) }))
        .into_response()
}

/// DELETE /api/passkeys/{label}: removes one passkey, with the token of an
/// assertion for purpose `remove`. The last one goes only through the CLI,
/// where the person types the one-time code on a terminal.
pub(crate) async fn remove_handler(
    State(state): State<AppState>,
    AxPath(label): AxPath<String>,
    Json(body): Json<RemoveBody>,
) -> Response {
    let all =
        apb_core::passkey::list_from(&apb_core::passkey::passkey_file_path().unwrap_or_default())
            .unwrap_or_default();
    if all.len() <= 1 {
        return (
            StatusCode::FORBIDDEN,
            "the last passkey is removed with `apb passkey remove`, which asks for a one-time code at the terminal",
        )
            .into_response();
    }
    let digest = passkeys::purpose_digest(&json!({ "remove": label }));
    if state.passkeys.take_token(&body.token, &digest).is_none() {
        return (
            StatusCode::FORBIDDEN,
            "that assertion is wrong, used or expired; run it again",
        )
            .into_response();
    }
    let file_path = match apb_core::passkey::passkey_file_path() {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match apb_core::passkey::remove_by_label_into(&file_path, &label) {
        Ok(_) => {
            notify_passkey(
                "a passkey was removed",
                &format!(
                    "a passkey was removed on {}: label `{}`",
                    machine_name(),
                    label
                ),
            );
            StatusCode::OK.into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

use crate::state::AppState;

/// The machine's host name, for the enrollment and removal notices.
pub(crate) fn machine_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".to_string())
}

/// Best-effort notice through the machine's notify target: quiet when
/// notifications are off, never a fault of the enrollment or the removal
/// itself. Journaled nowhere: no run owns it.
fn notify_passkey(subject: &str, text: &str) {
    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Err(why) = apb_engine::notify::trial::send_notice(&root, subject, text) {
        eprintln!("apb: passkey notice not sent: {why}");
    }
}
