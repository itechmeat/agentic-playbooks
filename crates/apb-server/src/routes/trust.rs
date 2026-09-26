//! The trust store: every approval, and their revocation. Thin over
//! `apb_core::trust::TrustStore::{entries, revoke}`, the one path `apb trust`
//! and the MCP trust tools use too.

use apb_core::trust::{Kind, TrustEntry, TrustSelector, TrustStore};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

/// GET /api/trust: every approval, ordered by kind, then id, then time.
pub(crate) async fn list_trust_handler() -> impl IntoResponse {
    Json(TrustStore::load().entries())
}

#[derive(Deserialize)]
pub(crate) struct RevokeBody {
    /// A digest (`sha256:...`) or an id.
    target: String,
    /// With an id: only approvals of this kind.
    #[serde(default)]
    kind: Option<String>,
}

/// What `POST /api/trust/revoke` removed.
#[derive(Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct TrustRevoked {
    pub revoked: Vec<TrustEntry>,
}

/// POST /api/trust/revoke `{target, kind?}`: revokes one digest, or every
/// approval under an id (optionally of one kind). 404 when nothing matches,
/// 400 for an unknown kind.
pub(crate) async fn revoke_trust_handler(Json(body): Json<RevokeBody>) -> Response {
    let kind = match body.kind.as_deref().filter(|k| !k.is_empty()) {
        None => None,
        Some(k) => match Kind::parse(k) {
            Some(k) => Some(k),
            None => {
                return (StatusCode::BAD_REQUEST, format!("unknown kind `{k}`")).into_response();
            }
        },
    };
    let selector = TrustSelector::parse(&body.target, kind);
    match TrustStore::load().revoke(&selector) {
        Ok(revoked) if revoked.is_empty() => (
            StatusCode::NOT_FOUND,
            format!("no approval matches `{}`", body.target),
        )
            .into_response(),
        Ok(revoked) => Json(TrustRevoked { revoked }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
