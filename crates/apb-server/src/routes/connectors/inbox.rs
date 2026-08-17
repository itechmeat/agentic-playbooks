//! Read-only inbox endpoints behind the dashboard's inbox panel (spec
//! 2026-08-16-webhook-ingest-design).
//!
//! Machine-scoped like the store itself: the inbox lives under the global
//! config dir and carries no project, so neither endpoint takes a workspace.
//!
//! These are the only endpoints in the feature that return stored bodies, and
//! they do it only when a request explicitly asks for events. The summary
//! endpoint returns counts and timestamps. Neither ever returns a provider id
//! or anything from the account's secret fields. Both sit under `/api`, so
//! the dashboard's authentication covers them without a gate of their own.

use crate::state::*;
use std::path::Path;

use apb_core::connector::inbox::{Inbox, inbox_root, list_accounts};
use apb_core::connector::store;
use axum::extract::{Path as AxPath, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

/// Most events the panel will ever render in one request.
const EVENTS_CAP: usize = 200;
/// Events returned when the request names no limit.
const EVENTS_DEFAULT: usize = 20;

#[derive(Deserialize, Default)]
pub(crate) struct EventsQuery {
    limit: Option<usize>,
}

/// The base directory of the inbox store, or a 404-worthy `None` in a
/// config-less environment.
fn base() -> Option<std::path::PathBuf> {
    inbox_root()
}

/// GET /api/connectors/{name}/inbox: per-account pending depth, last
/// received timestamp and the exact callback URL to register, plus whether
/// this connector can receive at all. A connector without a webhook block is
/// a 200 with `has_webhook: false`, not an error: the panel simply hides.
pub(crate) async fn inbox_handler(AxPath(name): AxPath<String>) -> impl IntoResponse {
    if !is_safe_id(&name) || apb_core::profile::validate_profile_name(&name).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(loaded) = store::load(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let ingest = apb_core::config::GlobalConfig::load()
        .map(|c| c.ingest)
        .unwrap_or_default();
    if loaded.doc.webhook.is_none() {
        return Json(serde_json::json!({
            "connector": name,
            "has_webhook": false,
            "public_base_url_set": ingest.public_base_url.is_some(),
            "accounts": [],
        }))
        .into_response();
    }
    let Some(base) = base() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rows: Vec<serde_json::Value> = list_accounts(&base, &name)
        .into_iter()
        .map(|account| account_row(&base, &name, &account, &ingest))
        .collect();
    Json(serde_json::json!({
        "connector": name,
        "has_webhook": true,
        "public_base_url_set": ingest.public_base_url.is_some(),
        "accounts": rows,
    }))
    .into_response()
}

/// Counts for one account. An unreadable inbox reports zeroes rather than
/// failing the whole listing, matching how the connector listing tolerates
/// one broken entry.
fn account_row(
    base: &Path,
    connector: &str,
    account: &str,
    ingest: &apb_core::config::IngestConfig,
) -> serde_json::Value {
    let depth = Inbox::at(base, connector, account)
        .and_then(|inbox| inbox.depth(apb_engine::connector::inbox::DEFAULT_CONSUMER))
        .unwrap_or_default();
    serde_json::json!({
        "account": account,
        "pending": depth.pending,
        "total": depth.total,
        "cursor": depth.cursor,
        "last_received_at": depth.last_received_at,
        "dropped": depth.dropped,
        "callback_url": ingest.callback_url(connector, account).ok(),
    })
}

/// GET /api/connectors/{name}/inbox/{account}/events: the stored events,
/// oldest first, capped at `limit`. This is the deliberate exception to
/// "bodies are never returned": an operator inspecting what a provider
/// actually sent needs to see it, and the dashboard marks it as untrusted
/// content when it renders it. Filtered to `seq > min_cursor` (the lowest
/// cursor across every consumer of the account, the same basis retention
/// uses to decide what is safe to drop) so a long-acked event that retention
/// simply has not gotten around to deleting yet cannot show up here while
/// the summary route reports it as no longer pending.
pub(crate) async fn inbox_events_handler(
    AxPath((name, account)): AxPath<(String, String)>,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    for segment in [&name, &account] {
        if !is_safe_id(segment) || apb_core::profile::validate_profile_name(segment).is_err() {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    let Ok(loaded) = store::load(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if loaded.doc.webhook.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(base) = base() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !list_accounts(&base, &name).iter().any(|a| a == &account) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(inbox) = Inbox::at(&base, &name, &account) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let limit = q.limit.unwrap_or(EVENTS_DEFAULT).clamp(1, EVENTS_CAP);
    let events = inbox.read_events().unwrap_or_default();
    let depth = inbox
        .depth(apb_engine::connector::inbox::DEFAULT_CONSUMER)
        .unwrap_or_default();
    // No cursors yet (a fresh inbox nobody has read from) means `min_cursor`
    // is 0, so every retained event is still pending - the same case the
    // summary route treats as "nothing acknowledged yet".
    let min_cursor = inbox.min_cursor().unwrap_or(0);
    let rows: Vec<serde_json::Value> = events
        .iter()
        .filter(|e| e.seq > min_cursor)
        .take(limit)
        .map(|e| {
            // The provider id is a dedupe identity, not information the
            // operator needs, and leaving it out keeps one less
            // provider-controlled string flowing into the page.
            serde_json::json!({
                "seq": e.seq,
                "received_at": e.received_at,
                "body": e.body,
            })
        })
        .collect();
    Json(serde_json::json!({
        "connector": name,
        "account": account,
        "cursor": depth.cursor,
        "events": rows,
    }))
    .into_response()
}
