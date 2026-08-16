//! The dashboard HTTP API: an axum router over the local `.apb` state, with
//! the built svelte frontend embedded as static assets.
//!
//! The surface is split by resource. [`routes`] holds one module per API
//! family (playbooks, runs, profiles, connectors, and the small read-only
//! `meta` endpoints); [`state`] holds the shared [`AppState`] plus the
//! request-scoped project resolution every handler starts from; [`ws`] is the
//! event stream the dashboard subscribes to; [`assets`] serves the embedded
//! frontend. This module wires them into a router and runs the server.

pub mod assets;
pub mod auth;
pub mod lock;
pub mod routes;
pub mod state;
pub mod watch;
pub mod ws;

use axum::Router;
use axum::routing::{get, post, put};
use std::net::{IpAddr, SocketAddr};

pub use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(routes::meta::health))
        .route("/api/projects", get(routes::meta::list_projects_handler))
        .route(
            "/api/playbooks",
            get(routes::playbooks::list_playbooks).post(routes::playbooks::create_playbook),
        )
        .route(
            "/api/playbooks/{id}",
            get(routes::playbooks::get_playbook)
                .put(routes::playbooks::update_playbook)
                .delete(routes::playbooks::delete_playbook_handler),
        )
        .route(
            "/api/playbooks/{id}/layout",
            put(routes::playbooks::put_layout),
        )
        .route("/api/playbooks/{id}/diff", get(routes::playbooks::get_diff))
        .route(
            "/api/playbooks/{id}/versions",
            get(routes::playbooks::list_versions_handler),
        )
        .route(
            "/api/playbooks/{id}/versions/{version}/promote",
            post(routes::playbooks::promote_version_handler),
        )
        .route(
            "/api/playbooks/{id}/frozen",
            put(routes::playbooks::set_frozen_handler),
        )
        .route(
            "/api/playbooks/{id}/input-draft",
            get(routes::playbooks::get_input_draft_handler)
                .put(routes::playbooks::put_input_draft_handler),
        )
        .route(
            "/api/playbooks/{id}/run",
            post(routes::playbooks::run_playbook_handler),
        )
        .route(
            "/api/profiles",
            get(routes::profiles::list_profiles).post(routes::profiles::write_profile),
        )
        .route(
            "/api/profiles/{name}",
            get(routes::profiles::get_profile).delete(routes::profiles::delete_profile),
        )
        .route(
            "/api/connectors",
            get(routes::connectors::list_connectors_handler),
        )
        .route(
            "/api/connectors/approve",
            post(routes::connectors::approve_connector_handler),
        )
        .route(
            "/api/connectors/available",
            get(routes::connectors::available_connectors_handler),
        )
        .route(
            "/api/connectors/{name}",
            get(routes::connectors::get_connector_handler),
        )
        .route(
            "/api/connectors/{name}/install",
            post(routes::connectors::install_connector_handler),
        )
        .route(
            "/api/connectors/{name}/uninstall",
            post(routes::connectors::uninstall_connector_handler),
        )
        .route(
            "/api/connectors/{name}/stats",
            get(routes::connectors::connector_stats_handler),
        )
        .route(
            "/api/connectors/{name}/healthcheck/{account}",
            post(routes::connectors::healthcheck_connector_handler),
        )
        .route(
            "/api/connectors/{name}/call",
            post(routes::connectors::call_connector_handler),
        )
        .route("/api/agents", get(routes::meta::list_agents_handler))
        .route("/api/models", get(routes::meta::list_models_handler))
        .route("/api/skills", get(routes::profiles::list_skills_handler))
        .route(
            "/api/suggestions",
            get(routes::suggestions::list_suggestions_handler),
        )
        .route(
            "/api/suggestions/{pattern}",
            axum::routing::delete(routes::suggestions::delete_suggestion_handler),
        )
        .route("/api/runs", get(routes::runs::list_runs_handler))
        .route("/api/runs/{id}", get(routes::runs::get_run_handler))
        .route(
            "/api/runs/{id}/report",
            get(routes::runs::get_run_report_handler),
        )
        .route(
            "/api/runs/{id}/review",
            post(routes::runs::post_review_handler),
        )
        .route(
            "/api/runs/{id}/answer",
            post(routes::runs::post_answer_handler),
        )
        .route(
            "/api/hooks/{run_id}/{secret}",
            post(routes::runs::post_hook_handler),
        )
        .route("/api/ws", get(ws::ws_handler))
        .fallback(assets::static_handler)
        // The gate wraps everything, including the static fallback, so that
        // ClientCtx is present on every request. Exempt paths are decided
        // inside the middleware, not by leaving routes outside the layer.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .with_state(state)
}

/// Startup interlock (spec 2026-08-16-server-mode-design): binding anywhere
/// but the loopback interface requires at least one issued API key. This is a
/// hard error and not a warning, because the API can start runs and make
/// authenticated connector calls, which makes an open bind equivalent to
/// remote code execution.
pub fn check_bind_allowed(bind: IpAddr, key_count: usize) -> Result<(), String> {
    if bind.is_loopback() || key_count > 0 {
        return Ok(());
    }
    Err(format!(
        "refusing to bind {bind}: no API key is configured, so the dashboard would be reachable from the network without authentication. Issue one with `apb server key issue`, or keep the default 127.0.0.1 bind"
    ))
}

/// The host part of the dashboard URL for a bind address, with IPv6 bracketed.
fn display_host(bind: IpAddr) -> String {
    match bind {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// Runs the global, machine-wide dashboard: one server, no project binding.
/// Playbooks and runs are aggregated across every reachable project in the
/// registry; project-specific requests carry `?workspace=<id>`. A single
/// instance lock lives in the config dir so two global dashboards cannot race
/// on the same port.
pub async fn run_server(bind: IpAddr, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // The key file decides two things at once: whether the requested bind is
    // permitted at all, and (from Task 4 on) whether auth is enforced. A
    // malformed file is a startup error rather than a silent "no keys", so a
    // typo can never quietly open the server.
    let global_cfg = apb_core::config::GlobalConfig::load().map_err(std::io::Error::other)?;
    let auth_path = apb_core::server_auth::auth_file_path()?;
    let auth_file = apb_core::server_auth::load_from(&auth_path)?;
    check_bind_allowed(bind, auth_file.keys.len()).map_err(std::io::Error::other)?;
    // The path is handed to the auth state so it can notice the file changing:
    // issuing a first key or revoking a compromised one takes effect on a
    // running dashboard without a restart.
    let auth = std::sync::Arc::new(
        auth::AuthState::new(Some(auth_path), auth_file.keys, &global_cfg.server)
            .map_err(std::io::Error::other)?,
    );
    let auth_enabled = auth.enabled();
    let state = AppState::new_global_with_auth(auth);
    let cfg = apb_core::config::config_dir()
        .ok_or_else(|| std::io::Error::other("no config dir for the global server lock"))?;
    std::fs::create_dir_all(&cfg)?;
    // Bind the port BEFORE writing the lock file: the port bind is the real
    // mutual exclusion (a second server on the same port fails here), so if it
    // fails we must return without having written a lock that no cleanup path
    // would then remove.
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    let _lock = lock::write_global_lock(&cfg, port)?;
    // Real-time updates across all projects: a filesystem watcher broadcasts
    // change pings on the shared channel that the dashboard's WebSocket relays.
    // Best-effort: if it cannot start, the server still serves (the UI just
    // falls back to refetch-on-navigation).
    let _watcher = match watch::spawn_global_watcher(state.events.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("apb dashboard: real-time watcher unavailable: {e}");
            None
        }
    };
    let app = build_router(state);
    println!(
        "apb dashboard (global): http://{}:{port}",
        display_host(bind)
    );
    if auth_enabled {
        println!("authentication is on: sign in with a key from `apb server key issue`");
    }
    if let Some(url) = global_cfg.server.public_base_url.as_deref() {
        println!("public address: {url}");
    }
    // Behind a proxy with no trusted peer configured, every client arrives as
    // the proxy's own address, so all of them share one rate-limit key and a
    // single attacker can exhaust the failure budget for everyone.
    if global_cfg.server.public_base_url.is_some() && global_cfg.server.trusted_proxies.is_empty() {
        eprintln!(
            "apb dashboard: server.public_base_url is set but server.trusted_proxies is empty; every client will share the proxy's IP as one rate-limit key. Add the proxy's address to server.trusted_proxies (see docs/DEPLOYMENT.md)"
        );
    }
    // ConnectInfo carries the socket peer address into every request, which the
    // auth layer needs for rate-limit keying and for deciding whether a
    // forwarded header came from a trusted proxy.
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;
    // Remove the lock both on normal shutdown and after catching a signal.
    lock::remove_global_lock(&cfg)?;
    result?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
