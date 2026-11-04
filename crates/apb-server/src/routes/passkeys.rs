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

use axum::extract::{Path as AxPath, Query, State};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<DecisionPurpose>,
    /// Adding another passkey (an assertion from an existing one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enroll_new: Option<serde_json::Value>,
    /// Removing one passkey by label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remove: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct DecisionPurpose {
    pub(crate) run: String,
    pub(crate) node: String,
    pub(crate) decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) request: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nonce: Option<String>,
}

impl AssertPurpose {
    /// The purpose of deciding gate `node` of run `run` with `decision`.
    /// Every digest over a decision purpose is taken from this one value,
    /// so the challenge request, the verify request and the review route
    /// can never disagree on the bytes.
    pub(crate) fn decision(
        run: &str,
        node: &str,
        decision: &str,
        request: Option<u64>,
        nonce: Option<&str>,
    ) -> AssertPurpose {
        AssertPurpose {
            decision: Some(DecisionPurpose {
                run: run.to_string(),
                node: node.to_string(),
                decision: decision.to_string(),
                request,
                nonce: nonce.map(str::to_string),
            }),
            enroll_new: None,
            remove: None,
        }
    }

    /// The purpose of adding another passkey, asserted from one that exists.
    pub(crate) fn enroll_new() -> AssertPurpose {
        AssertPurpose {
            decision: None,
            enroll_new: Some(serde_json::json!({})),
            remove: None,
        }
    }

    /// The purpose of removing the passkey labeled `label`.
    pub(crate) fn remove(label: &str) -> AssertPurpose {
        AssertPurpose {
            decision: None,
            enroll_new: None,
            remove: Some(label.to_string()),
        }
    }

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

    pub(crate) fn digest(&self) -> String {
        passkeys::purpose_digest(&self.value())
    }

    /// The request seq a resolved decision purpose names.
    pub(crate) fn request_of(&self) -> Option<u64> {
        self.decision.as_ref().and_then(|d| d.request)
    }

    /// The nonce a resolved decision purpose names.
    pub(crate) fn nonce_of(&self) -> Option<String> {
        self.decision.as_ref().and_then(|d| d.nonce.clone())
    }

    /// The decision purpose a dashboard decide flow names, resolved to the
    /// request and nonce the engine will stamp on the entry: the open
    /// request's seq and binding nonce when the client left them out. Both
    /// the challenge and the review route resolve the same way, so the
    /// assertion's challenge equals the derived challenge the drive
    /// re-verifies at consumption. `None` when the run or its journal does
    /// not read (500 territory, not a client fault).
    pub(crate) fn resolved_decision(
        root: &std::path::Path,
        d: &DecisionPurpose,
    ) -> Result<AssertPurpose, String> {
        let run_dir = root.join(".apb/runs").join(&d.run);
        let events = apb_engine::event::read_all(&run_dir)
            .map_err(|e| format!("the journal of run `{}` does not read: {e}", d.run))?;
        let open = apb_engine::review::channel::open_request(&events, &d.node);
        let request = d.request.or(open.map(|(seq, _)| seq));
        let nonce = d
            .nonce
            .clone()
            .or_else(|| open.and_then(|(_, b)| b.map(|b| b.nonce.clone())))
            .or_else(|| {
                apb_engine::publish::latest_plan(&events, &d.node).and_then(|e| match &e.payload {
                    apb_engine::event::EventPayload::PublishPlanned { nonce, .. } => {
                        Some(nonce.clone())
                    }
                    _ => None,
                })
            });
        Ok(AssertPurpose::decision(
            &d.run,
            &d.node,
            &d.decision,
            request,
            nonce.as_deref(),
        ))
    }

    /// The challenge an assertion for this purpose is issued with: a
    /// decision's challenge is derived deterministically from the run, the
    /// gate, the decision, the request and the nonce, so the drive can
    /// recompute it from the stamped entry alone.
    pub(crate) fn challenge_for(&self) -> Result<String, String> {
        match &self.decision {
            Some(d) => Ok(apb_engine::review::passkey_proof::decision_challenge(
                &d.run,
                &d.node,
                &d.decision,
                d.request,
                d.nonce.as_deref(),
            )),
            None => apb_core::server_auth::random_token().map_err(|e| e.to_string()),
        }
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
    /// The client data JSON, base64url of the exact bytes the browser's
    /// authenticator produced.
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
    let cfg = apb_core::config::GlobalConfig::load().map_err(|e| {
        format!("the global config does not read, so the relying party is unknown: {e}")
    })?;
    if let Some(origin) = cfg.server.public_base_url.clone() {
        return webauthn::rp_for_origin(&origin).map_err(|e| e.to_string());
    }
    let (name, port) = host
        .rsplit_once(':')
        .filter(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && *p != "80")
        .map_or((host.as_str(), None), |(n, p)| (n, Some(p.to_string())));
    let origin = match port {
        Some(p) => format!("http://{name}:{p}"),
        None => format!("http://{name}"),
    };
    webauthn::rp_for_origin(&origin).map_err(|e| e.to_string())
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
        let digest = AssertPurpose::enroll_new().digest();
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
        state.root.as_deref().map(|p| p.as_path()),
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
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<AssertChallengeBody>,
) -> Response {
    if let Err(why) = body.purpose.validate() {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    let purpose = match &body.purpose.decision {
        Some(d) => {
            // A decision names its run's project like the review post does;
            // enroll and remove purposes stay workspace-independent.
            let root = match resolve_root(&state, q.workspace.as_deref()) {
                Ok(root) => root,
                Err(response) => return response,
            };
            match AssertPurpose::resolved_decision(&root, d) {
                Ok(p) => p,
                Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
            }
        }
        None => body.purpose,
    };
    match apb_core::passkey::count_strict() {
        Ok(0) => {
            return (
                StatusCode::CONFLICT,
                "no passkey is enrolled on this machine; nothing can assert",
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "the passkey store of this machine does not read, so no assertion is issued: {e}"
                ),
            )
                .into_response();
        }
        Ok(_) => {}
    }
    let challenge = match purpose.challenge_for() {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    match state
        .passkeys
        .issue_challenge_for(purpose.digest(), challenge)
    {
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
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<AssertVerifyBody>,
) -> Response {
    if let Err(why) = body.purpose.validate() {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    let purpose = match &body.purpose.decision {
        Some(d) => {
            // A decision names its run's project like the review post does;
            // enroll and remove purposes stay workspace-independent.
            let root = match resolve_root(&state, q.workspace.as_deref()) {
                Ok(root) => root,
                Err(response) => return response,
            };
            match AssertPurpose::resolved_decision(&root, d) {
                Ok(p) => p,
                Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
            }
        }
        None => body.purpose,
    };
    let rp = match rp_for_request(&headers) {
        Ok(rp) => rp,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let file_path = match apb_core::passkey::passkey_file_path() {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let all = match apb_core::passkey::list_from(&file_path) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "the passkey store of this machine does not read, so no assertion is verified: {e}"
                ),
            )
                .into_response();
        }
    };
    let Some(record) = all.iter().find(|p| p.id == body.id) else {
        return (
            StatusCode::FORBIDDEN,
            "that credential is not enrolled on this machine",
        )
            .into_response();
    };
    if !state
        .passkeys
        .consume_challenge(&body.challenge, &purpose.digest())
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
    let token = match state
        .passkeys
        .issue_token(purpose.digest(), passkeys::credential_prefix(&record.id))
    {
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
    let digest = AssertPurpose::remove(&label).digest();
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
    // The never-the-last rule lives in the locked core function, so two
    // concurrent removals cannot race the count down to zero.
    match apb_core::passkey::remove_by_label_into(&file_path, &label, false) {
        Ok(_) => {
            notify_passkey(
                "a passkey was removed",
                &format!(
                    "a passkey was removed on {}: label `{}`",
                    machine_name(),
                    label
                ),
                state.root.as_deref().map(|p| p.as_path()),
            );
            StatusCode::OK.into_response()
        }
        Err(apb_core::passkey::PasskeyError::LastPasskey) => (
            StatusCode::FORBIDDEN,
            "the last passkey is removed with `apb passkey remove`, which asks for a one-time code at the terminal",
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

use crate::state::{AppState, WorkspaceQuery, resolve_root};

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
fn notify_passkey(subject: &str, text: &str, root: Option<&std::path::Path>) {
    // The notify target resolves the way every other notify use does: from
    // the project root the server was started for when there is one.
    let fall_back = std::path::PathBuf::from(".");
    let root = root.unwrap_or(&fall_back);
    if let Err(why) = apb_engine::notify::trial::send_notice(root, subject, text) {
        eprintln!("apb: passkey notice not sent: {why}");
    }
}
