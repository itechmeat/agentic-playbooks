//! The embedded svelte frontend and the HTTP cache policy around it.
//!
//! - `/assets/*` are content-hashed by vite, so an existing one never changes:
//!   `public, max-age=31536000, immutable`. A missing one is a real 404, never
//!   the shell served as JavaScript.
//! - The shell (`index.html`) is `no-cache`: revalidated on every load, so a
//!   reload always picks up a rebuilt bundle. It carries the build id in
//!   `<meta name="apb-build">`.
//! - Every other response names the build that served it (`x-apb-build`, see
//!   [`build_middleware`]); `/api/*` is `no-cache` unless a handler chose a
//!   stricter policy, and an unknown `/api/*` route is a JSON 404.
//!
//! The build id is the digest of the embedded shell. vite rewrites the shell
//! whenever any bundle file changes (it references them by hash), so the id
//! changes exactly when the frontend a tab is running is no longer the one the
//! server ships.

use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};

#[derive(rust_embed::Embed)]
#[folder = "../../web/dist"]
pub(crate) struct WebAssets;

/// Cache policy of a content-hashed asset.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Cache policy of the shell and the API: always revalidate.
const NO_CACHE: &str = "no-cache";
/// The response header naming the build that served a response.
pub const BUILD_HEADER: &str = "x-apb-build";

/// Identity of the embedded frontend: the first 16 hex chars of the shell's
/// digest, or `unbuilt` when the frontend was not built into this binary.
pub fn build_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| match WebAssets::get("index.html") {
        Some(f) => {
            let digest = apb_core::scope::digest_str(&String::from_utf8_lossy(&f.data));
            let hex = digest.trim_start_matches("sha256:");
            hex.chars().take(16).collect()
        }
        None => "unbuilt".to_string(),
    })
}

fn with_cache(mut res: Response, policy: &'static str) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    res
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "not_found" })),
    )
        .into_response()
}

/// Content-Security-Policy of the dashboard shell, for one page load: only
/// the server's own scripts, style sheets, fonts and API. The vite build has
/// no inline script and no eval. The one `<style>` element the UI creates is
/// CodeMirror's theme, admitted by `nonce` (a fresh CSPRNG value per load,
/// handed to the page in `<meta name="csp-nonce">`). Style attributes are
/// refused except the empty one: bits-ui restores `<body style="">` through
/// `setAttribute` when it releases its scroll lock (the hash below), and
/// without it a page stayed unscrollable and unclickable after the first
/// select or dialog closed. Other inline styles the UI sets go through the
/// CSSOM, which CSP does not govern. `data:` images cover the SVG icons the UI
/// libraries inline.
fn content_security_policy(nonce: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self' 'nonce-{nonce}'; \
         style-src-attr 'unsafe-hashes' 'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='; \
         img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; \
         base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    )
}

/// The shell with the build id and this load's CSP nonce injected before
/// `</head>`.
fn shell() -> Response {
    let Some(file) = WebAssets::get("index.html") else {
        return (StatusCode::NOT_FOUND, "web assets not built").into_response();
    };
    let Ok(nonce) = apb_core::server_auth::random_token() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "no randomness for the page nonce",
        )
            .into_response();
    };
    let html = String::from_utf8_lossy(&file.data).replacen(
        "</head>",
        &format!(
            "<meta name=\"apb-build\" content=\"{}\">\n  \
             <meta name=\"csp-nonce\" content=\"{nonce}\">\n  </head>",
            build_id()
        ),
        1,
    );
    let csp = content_security_policy(&nonce);
    with_cache(
        (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                (header::CONTENT_SECURITY_POLICY, csp),
            ],
            html,
        )
            .into_response(),
        NO_CACHE,
    )
}

/// The router fallback: every path no route matched.
pub(crate) async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path == "api" || path.starts_with("api/") {
        return not_found();
    }
    if path.is_empty() || path == "index.html" {
        return shell();
    }
    match WebAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let res = (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data,
            )
                .into_response();
            let policy = if path.starts_with("assets/") {
                IMMUTABLE
            } else {
                NO_CACHE
            };
            with_cache(res, policy)
        }
        // A missing hashed asset is gone for good: a 404, not the shell.
        None if path.starts_with("assets/") => not_found(),
        // Anything else is a client-side route: the shell renders it.
        None => shell(),
    }
}

/// Outermost layer: names the serving build on every response, and marks an
/// API response `no-cache` unless its handler already set a policy.
pub(crate) async fn build_middleware(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(build_id()) {
        headers.insert(BUILD_HEADER, v);
    }
    if api && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE));
    }
    res
}
