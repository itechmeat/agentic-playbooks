use crate::state::*;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// A request extractor that admits the WebSocket upgrade only when its `Origin`
/// is same-origin (or absent). It is listed BEFORE [`WebSocketUpgrade`] in the
/// handler so the origin gate runs first: `WebSocketUpgrade` itself rejects a
/// non-upgradable connection with 426, which would otherwise mask this check.
pub(crate) struct SameOrigin;

impl<S> FromRequestParts<S> for SameOrigin
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if origin_allowed(&parts.headers) {
            Ok(SameOrigin)
        } else {
            Err((StatusCode::FORBIDDEN, "cross-origin websocket refused").into_response())
        }
    }
}

pub(crate) async fn ws_handler(
    // Defense-in-depth against cross-site WebSocket hijacking. A cookie-
    // authenticated GET reaches this route as a safe method with no CSRF marker,
    // and the upgrade carries none either. SameSite=Lax already withholds the
    // session cookie on a cross-site handshake in every current browser; this
    // gate closes the gap on a legacy browser that does not honor it, and runs
    // before the upgrade is set up.
    _origin: SameOrigin,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
        .into_response()
}

/// Whether the upgrade's `Origin` is acceptable: absent, or naming the same
/// host as the request's own `Host` header. Only the authority (host and port)
/// is compared, not the scheme, matching how a browser frames a same-origin
/// socket. An `Origin` that is present while `Host` is not is refused rather
/// than trusted.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin_host = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    match headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        Some(host) => origin_host.eq_ignore_ascii_case(host),
        None => false,
    }
}

pub(crate) async fn ws_loop(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => {
                if incoming.is_none() { break; } // the client closed the connection
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::origin_allowed;
    use axum::http::{HeaderMap, header};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    #[test]
    fn absent_origin_is_allowed() {
        assert!(origin_allowed(&headers(&[("host", "example.com")])));
    }

    #[test]
    fn same_origin_is_allowed() {
        assert!(origin_allowed(&headers(&[
            ("host", "example.com"),
            (header::ORIGIN.as_str(), "https://example.com"),
        ])));
        // Host and port must both match.
        assert!(origin_allowed(&headers(&[
            ("host", "localhost:7321"),
            (header::ORIGIN.as_str(), "http://localhost:7321"),
        ])));
    }

    #[test]
    fn cross_origin_is_refused() {
        assert!(!origin_allowed(&headers(&[
            ("host", "example.com"),
            (header::ORIGIN.as_str(), "https://evil.example"),
        ])));
        // A different port is still a different origin.
        assert!(!origin_allowed(&headers(&[
            ("host", "localhost:7321"),
            (header::ORIGIN.as_str(), "http://localhost:9999"),
        ])));
    }

    #[test]
    fn an_origin_without_a_host_is_refused() {
        assert!(!origin_allowed(&headers(&[(
            header::ORIGIN.as_str(),
            "https://example.com",
        )])));
    }
}
