//! History discovery on the HTTP API, over the tool layer MCP and the CLI
//! use (`apb_mcp::tools::discover`):
//!
//! - `GET /api/discover/roots`: the configured history roots, by name;
//! - `GET /api/discover`: the stored results, newest first;
//! - `GET /api/discover/{id}`: one stored result;
//! - `POST /api/discover` with `{ workspaces, roots, since_days? }`: runs a
//!   discovery over registered projects and named roots;
//! - `POST /api/discover/{id}/{slug}/save` with `{ scope }`;
//! - `POST /api/discover/{id}/{slug}/dismiss` with `{ kind, scope? }`.
//!
//! Session history is meant for a person at this machine, so a route layer
//! ([`local_gate`]) refuses, before any body is read, a request that does
//! not look like one straight from a browser or a client here (403
//! `discover_local_only`): a peer off the loopback interface, a peer that
//! is a configured trusted proxy, any forwarding header, or a `Host` (and
//! an `Origin`, when sent) that is not a loopback name. That is as far as
//! the server can tell: a forwarder on this machine that adds no header
//! (an ssh tunnel, a raw TCP relay) looks like a local client, so exposing
//! the dashboard that way exposes this API too. The refusal from a run
//! step (409) checks the server's own environment: an agent of a run step
//! that reaches a keyless dashboard on loopback is not told apart from a
//! person, as it could read the history files directly anyway.
//!
//! A route never accepts a path: bodies deny unknown fields (422), and
//! roots and projects are named only. A discovery while another one runs
//! is refused (409, `busy`). All the work, the choice of the project a
//! save lands in included, runs off the async runtime.

use std::net::IpAddr;
use std::path::PathBuf;

use apb_core::step_env::StepCaller;
use apb_mcp::tools::ToolError;
use apb_mcp::tools::discover::{
    self, DiscoverArgs, ProjectPick, SaveRequest, discover_dismiss, discover_save,
};
use axum::Router;
use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use crate::auth::ClientCtx;
use crate::state::*;

/// Body of `POST /api/discover`.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoverBody {
    /// Registered workspace ids.
    #[serde(default)]
    workspaces: Vec<String>,
    /// History root names; empty: every configured root that is there.
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    since_days: Option<u32>,
}

/// Body of `POST /api/discover/{id}/{slug}/save`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveBody {
    /// `project` or `global`.
    scope: String,
}

/// Body of `POST /api/discover/{id}/{slug}/dismiss`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DismissBody {
    /// `soft` (not now) or `hard` (never).
    kind: String,
    /// `project` (default) or `global`.
    #[serde(default)]
    scope: Option<String>,
}

/// The discovery routes, behind [`local_gate`].
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/discover",
            get(list_results_handler).post(post_discover_handler),
        )
        .route("/api/discover/roots", get(get_roots_handler))
        .route("/api/discover/{id}", get(get_result_handler))
        .route("/api/discover/{id}/{slug}/save", post(post_save_handler))
        .route(
            "/api/discover/{id}/{slug}/dismiss",
            post(post_dismiss_handler),
        )
        .route_layer(axum::middleware::from_fn(local_gate))
}

/// Headers a proxy, a load balancer or a tunnel service adds.
const FORWARDING_HEADERS: [&str; 8] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
    "via",
    "cf-connecting-ip",
    "true-client-ip",
];

/// Whether an authority (`Host`, or an `Origin` without its scheme) names
/// this machine's loopback interface: `localhost`, a loopback address, on
/// any port.
fn loopback_authority(authority: &str) -> bool {
    let a = authority.trim().to_ascii_lowercase();
    let name = match a.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => a.rsplit_once(':').map_or(a.as_str(), |(n, _)| n),
    };
    let name = name.trim_end_matches('.');
    name == "localhost"
        || name
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// Whether the request came straight from this machine: a loopback peer
/// that is not a trusted proxy, no forwarding header, and a loopback
/// `Host` (and `Origin`, when sent).
fn local(ctx: &ClientCtx, headers: &HeaderMap) -> bool {
    let text = |name| headers.get(name).and_then(|v| v.to_str().ok());
    let origin_ok = match headers.get(header::ORIGIN) {
        None => true,
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://"))
            .is_some_and(|(_, authority)| loopback_authority(authority)),
    };
    ctx.ip.to_canonical().is_loopback()
        && !ctx.via_proxy
        && !FORWARDING_HEADERS.iter().any(|h| headers.contains_key(*h))
        && text(header::HOST).is_some_and(loopback_authority)
        && origin_ok
}

fn local_only() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "discover_local_only",
            "detail": "session history is read only for a client on this machine",
        })),
    )
        .into_response()
}

/// The route layer of every discovery route: refuses a client that is not
/// local, then a server that is a run step (the tool layer checks the
/// latter again, for every surface), before any extractor reads the body.
pub(crate) async fn local_gate(req: Request, next: Next) -> Response {
    let local = req
        .extensions()
        .get::<ClientCtx>()
        .is_some_and(|ctx| local(ctx, req.headers()));
    if !local {
        return local_only();
    }
    if let Err(e) = discover::guard(&StepCaller::from_env()) {
        return tool_refusal(e);
    }
    next.run(req).await
}

/// Runs `work` off the runtime and answers what it answers.
async fn off_runtime<F>(work: F) -> Response
where
    F: FnOnce(&StepCaller) -> Response + Send + 'static,
{
    tokio::task::spawn_blocking(move || work(&StepCaller::from_env()))
        .await
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
}

/// A tool-layer answer: its value as JSON, or its refusal.
fn reply<T: serde::Serialize>(out: Result<T, ToolError>) -> Response {
    match out {
        Ok(v) => Json(v).into_response(),
        Err(e) => tool_refusal(e),
    }
}

pub(crate) async fn get_roots_handler() -> Response {
    off_runtime(|_| reply(discover::roots())).await
}

pub(crate) async fn list_results_handler() -> Response {
    off_runtime(|step| reply(discover::results_list(step))).await
}

pub(crate) async fn get_result_handler(AxPath(id): AxPath<String>) -> Response {
    off_runtime(move |step| reply(discover::result_get(&id, step))).await
}

pub(crate) async fn post_discover_handler(
    State(state): State<AppState>,
    Json(body): Json<DiscoverBody>,
) -> Response {
    // A pinned-root harness discovers over its own project when the body
    // names none; the machine-wide dashboard always names its projects.
    let mut projects: Vec<ProjectPick> = body
        .workspaces
        .into_iter()
        .map(ProjectPick::Workspace)
        .collect();
    if projects.is_empty()
        && let Some(root) = &state.root
    {
        projects.push(ProjectPick::Root(root.as_ref().clone()));
    }
    let args = DiscoverArgs {
        projects,
        roots: body.roots,
        extra_root: None,
        since_days: body.since_days,
    };
    off_runtime(move |step| reply(discover::run_discover(&args, step))).await
}

/// Where a save or a dismissal lands: the project the query names, else
/// the pinned root; on the machine-wide dashboard, any root for a global
/// one (it never reads the project), else the registered project the
/// suggestion was seen in first, by workspace id.
#[allow(clippy::result_large_err)]
fn target_root(
    state: &AppState,
    workspace: Option<&str>,
    id: &str,
    slug: &str,
    global: bool,
) -> Result<PathBuf, Response> {
    if workspace.is_some() || state.root.is_some() {
        return resolve_root(state, workspace);
    }
    if global {
        return resolve_root_for_scope(state, None, "global");
    }
    discover::home_project(id, slug).map_err(tool_refusal)
}

pub(crate) async fn post_save_handler(
    State(state): State<AppState>,
    AxPath((id, slug)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<SaveBody>,
) -> Response {
    off_runtime(move |step| {
        let global = body.scope == "global";
        let root = match target_root(&state, q.workspace.as_deref(), &id, &slug, global) {
            Ok(root) => root,
            Err(refused) => return refused,
        };
        let req = SaveRequest {
            id: &id,
            slug: &slug,
            scope: &body.scope,
            yaml: None,
        };
        reply(discover_save(&root, req, step))
    })
    .await
}

pub(crate) async fn post_dismiss_handler(
    State(state): State<AppState>,
    AxPath((id, slug)): AxPath<(String, String)>,
    Query(q): Query<WorkspaceQuery>,
    Json(body): Json<DismissBody>,
) -> Response {
    off_runtime(move |step| {
        let global = body.scope.as_deref() == Some("global");
        let root = match target_root(&state, q.workspace.as_deref(), &id, &slug, global) {
            Ok(root) => root,
            Err(refused) => return refused,
        };
        reply(discover_dismiss(
            &root,
            &id,
            &slug,
            &body.kind,
            body.scope.as_deref(),
            step,
        ))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{local, loopback_authority};
    use crate::auth::ClientCtx;
    use axum::http::HeaderMap;

    #[test]
    fn a_trusted_proxy_or_a_mapped_address_is_told_apart() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "localhost:7357".parse().unwrap());
        let ctx = |ip: &str, via_proxy| ClientCtx {
            ip: ip.parse().unwrap(),
            https: false,
            via_proxy,
        };
        assert!(local(&ctx("127.0.0.1", false), &headers));
        assert!(local(&ctx("::ffff:127.0.0.1", false), &headers));
        // A configured proxy on this machine forwarded it.
        assert!(!local(&ctx("127.0.0.1", true), &headers));
        assert!(!local(&ctx("::ffff:10.0.0.1", false), &headers));
    }

    #[test]
    fn only_a_loopback_name_is_a_local_authority() {
        for ok in [
            "localhost",
            "localhost:8080",
            "127.0.0.1:3000",
            "127.0.0.2",
            "[::1]:3000",
            "[::ffff:127.0.0.1]:3000",
            "LOCALHOST.",
        ] {
            assert!(loopback_authority(ok), "{ok}");
        }
        for bad in [
            "example.com",
            "localhost.example.com:80",
            "10.0.0.1:3000",
            "[2001:db8::1]:80",
            "",
        ] {
            assert!(!loopback_authority(bad), "{bad}");
        }
    }
}
