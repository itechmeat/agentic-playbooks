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
        let authority = parts.uri.authority().map(|a| a.as_str());
        if origin_allowed(&parts.headers, authority) {
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
/// host as the request's own authority. Only the authority (host and port) is
/// compared, not the scheme, matching how a browser frames a same-origin
/// socket.
///
/// The authority comes from the `Host` header, falling back to `uri_authority`
/// (the request URI's own authority). HTTP/2 carries `:authority` rather than a
/// `Host` header and hyper does not synthesize one, so an h2 extended-CONNECT
/// upgrade would otherwise present an `Origin` with no `Host` and be refused.
/// With neither available, an `Origin` that cannot be checked is still refused
/// rather than trusted.
fn origin_allowed(headers: &HeaderMap, uri_authority: Option<&str>) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin_host = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    let own = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or(uri_authority);
    match own {
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
        assert!(origin_allowed(&headers(&[("host", "example.com")]), None));
    }

    #[test]
    fn same_origin_is_allowed() {
        assert!(origin_allowed(
            &headers(&[
                ("host", "example.com"),
                (header::ORIGIN.as_str(), "https://example.com"),
            ]),
            None
        ));
        // Host and port must both match.
        assert!(origin_allowed(
            &headers(&[
                ("host", "localhost:7321"),
                (header::ORIGIN.as_str(), "http://localhost:7321"),
            ]),
            None
        ));
    }

    #[test]
    fn cross_origin_is_refused() {
        assert!(!origin_allowed(
            &headers(&[
                ("host", "example.com"),
                (header::ORIGIN.as_str(), "https://evil.example"),
            ]),
            None
        ));
        // A different port is still a different origin.
        assert!(!origin_allowed(
            &headers(&[
                ("host", "localhost:7321"),
                (header::ORIGIN.as_str(), "http://localhost:9999"),
            ]),
            None
        ));
    }

    /// HTTP/2 carries `:authority` and no `Host` header. The gate must compare
    /// against that authority rather than refusing every h2 upgrade outright,
    /// while still refusing a genuinely cross-origin one.
    #[test]
    fn the_uri_authority_stands_in_for_an_absent_host() {
        let h = headers(&[(header::ORIGIN.as_str(), "https://example.com")]);
        assert!(
            origin_allowed(&h, Some("example.com")),
            "an h2 upgrade whose :authority matches Origin is allowed"
        );
        assert!(
            !origin_allowed(&h, Some("evil.example")),
            "a mismatched :authority is still refused"
        );
    }

    /// A `Host` header wins over the URI authority when both are present, so
    /// the h2 fallback cannot be used to sidestep the HTTP/1.1 check.
    #[test]
    fn the_host_header_wins_over_the_uri_authority() {
        let h = headers(&[
            ("host", "example.com"),
            (header::ORIGIN.as_str(), "https://evil.example"),
        ]);
        assert!(
            !origin_allowed(&h, Some("evil.example")),
            "a matching authority must not override a mismatched Host"
        );
    }

    #[test]
    fn an_origin_with_neither_host_nor_authority_is_refused() {
        assert!(!origin_allowed(
            &headers(&[(header::ORIGIN.as_str(), "https://example.com")]),
            None
        ));
    }
}
