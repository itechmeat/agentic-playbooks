//! The `apb doctor` provider check (issue #165 Part 2, reworked): one request
//! per configured decision provider that costs nothing, instead of a bare TCP
//! connect. A connect proves only that the host accepts connections; a
//! provider can accept them and still hang every request, which a doctor
//! reporting "reachable" then hides.
//!
//! Where the route has a free authenticated endpoint the probe is a `GET` to
//! it with the provider's key, within the configured `timeout_ms`, and the
//! doctor reports the HTTP status and the latency. A kind without one (the
//! fake provider), or a key that only a run may produce (`{{cmd:...}}`), falls
//! back to the TCP connect and says so ("connect only").
//!
//! The key goes only to the provider's own configured base URL, exactly where
//! a run sends it, and never into the report.

use std::time::{Duration, Instant};

use crate::decisions::{KeyRef, ProviderKind, ProviderSpec};

/// The free request for one provider: `GET <url>` with an optional bearer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub url: String,
    /// The path shown in the report (the URL without the base).
    pub label: String,
    pub bearer: Option<String>,
}

/// What the probe found for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    /// The route answered: its status and how long the request took.
    Http { label: String, status: u16, ms: u64 },
    /// No answer within the timeout.
    TimedOut { label: String, ms: u64 },
    /// The request failed before any answer (DNS, connect, TLS).
    Failed { label: String, reason: &'static str },
    /// A TCP connect only: the kind has no free endpoint, or the key comes
    /// from a command only a run executes.
    ConnectOnly { reachable: bool },
}

impl ProbeResult {
    /// Whether the provider looks usable: a 2xx answer, or an open connect.
    pub fn ok(&self) -> bool {
        match self {
            ProbeResult::Http { status, .. } => (200..300).contains(status),
            ProbeResult::ConnectOnly { reachable } => *reachable,
            ProbeResult::TimedOut { .. } | ProbeResult::Failed { .. } => false,
        }
    }

    /// The doctor's words for it. Never contains the key.
    pub fn describe(&self) -> String {
        match self {
            ProbeResult::Http { label, status, ms } => {
                format!("GET {label}: HTTP {status} in {ms} ms")
            }
            ProbeResult::TimedOut { label, ms } => {
                format!("GET {label}: no answer within {ms} ms")
            }
            ProbeResult::Failed { label, reason } => format!("GET {label}: {reason}"),
            ProbeResult::ConnectOnly { reachable: true } => "reachable (connect only)".into(),
            ProbeResult::ConnectOnly { reachable: false } => "unreachable (connect only)".into(),
        }
    }
}

/// The free endpoint of a provider kind, relative to its base URL.
fn free_endpoint(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        // TypeSafe and the systemone-compatible routes list their models.
        ProviderKind::Systemone | ProviderKind::VercelEvaluate => Some("/v1/models"),
        // An OpenAI-compatible chat endpoint lists models next to it.
        ProviderKind::LlmEmulation => Some("/models"),
        // OpenRouter reports the key's own limits and usage, free.
        ProviderKind::OpenrouterDecisions => Some("/api/v1/key"),
        // Cloudflare verifies the API token, free.
        ProviderKind::Cloudflare => Some("/user/tokens/verify"),
        ProviderKind::Fake => None,
    }
}

/// The request to send for `spec`, or `None` when the doctor must fall back
/// to a connect: no free endpoint, no base URL, or a command-produced key.
/// `resolve_key` turns an `{{env.VAR}}` reference into its value.
pub fn probe_request(
    spec: &ProviderSpec,
    resolve_key: impl Fn(&str) -> Option<String>,
) -> Option<ProbeRequest> {
    let path = free_endpoint(spec.kind)?;
    let base = spec.base_url.as_deref()?.trim_end_matches('/');
    let bearer = match &spec.key {
        None => None,
        Some(KeyRef::Env(var)) => Some(resolve_key(var)?),
        Some(KeyRef::Cmd(_)) => return None,
    };
    Some(ProbeRequest {
        url: format!("{base}{path}"),
        label: path.to_string(),
        bearer,
    })
}

/// Sends `req` within `timeout`. Redirects are not followed (a key must not
/// travel to another host), and no error text from the client is kept: it
/// can name the URL.
pub fn send(req: &ProbeRequest, timeout: Duration) -> ProbeResult {
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut request = agent.get(&req.url).header("accept", "application/json");
    if let Some(key) = &req.bearer {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let started = Instant::now();
    let label = req.label.clone();
    match request.call() {
        Ok(response) => ProbeResult::Http {
            label,
            status: response.status().as_u16(),
            ms: started.elapsed().as_millis() as u64,
        },
        Err(ureq::Error::Timeout(_)) => ProbeResult::TimedOut {
            label,
            ms: timeout.as_millis() as u64,
        },
        Err(ureq::Error::HostNotFound) => ProbeResult::Failed {
            label,
            reason: "host not found",
        },
        Err(ureq::Error::ConnectionFailed) => ProbeResult::Failed {
            label,
            reason: "connection failed",
        },
        Err(_) => ProbeResult::Failed {
            label,
            reason: "transport error",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn spec(kind: ProviderKind, base: &str, key: Option<KeyRef>) -> ProviderSpec {
        ProviderSpec {
            id: "p".into(),
            kind,
            base_url: Some(base.into()),
            model: Some("m".into()),
            data_class: crate::decisions::DataClass::Hosted,
            key,
            answers: Default::default(),
            account_id: None,
            zero_data_retention: false,
            structured_output: None,
        }
    }

    #[test]
    fn each_kind_probes_its_free_endpoint_with_its_key() {
        let key = |v: &str| (v == "K").then(|| "secret".to_string());
        let env = || Some(KeyRef::Env("K".into()));
        for (kind, base, url) in [
            (
                ProviderKind::Systemone,
                "https://api.typesafe.ai",
                "https://api.typesafe.ai/v1/models",
            ),
            (
                ProviderKind::VercelEvaluate,
                "https://ai-gateway.vercel.sh",
                "https://ai-gateway.vercel.sh/v1/models",
            ),
            (
                ProviderKind::LlmEmulation,
                "http://127.0.0.1:8080/v1/",
                "http://127.0.0.1:8080/v1/models",
            ),
            (
                ProviderKind::OpenrouterDecisions,
                "https://openrouter.ai",
                "https://openrouter.ai/api/v1/key",
            ),
            (
                ProviderKind::Cloudflare,
                "https://api.cloudflare.com/client/v4",
                "https://api.cloudflare.com/client/v4/user/tokens/verify",
            ),
        ] {
            let req = probe_request(&spec(kind, base, env()), key).unwrap();
            assert_eq!(req.url, url, "{kind:?}");
            assert_eq!(req.bearer.as_deref(), Some("secret"));
        }
        // No free endpoint, a command key, or an unset key: connect only.
        assert!(probe_request(&spec(ProviderKind::Fake, "http://x", None), key).is_none());
        let cmd = Some(KeyRef::Cmd("pass show k".into()));
        assert!(probe_request(&spec(ProviderKind::Systemone, "https://x", cmd), key).is_none());
        let unset = Some(KeyRef::Env("UNSET".into()));
        assert!(probe_request(&spec(ProviderKind::Systemone, "https://x", unset), key).is_none());
        // A keyless local server is asked without a key.
        let local = probe_request(
            &spec(ProviderKind::Systemone, "http://127.0.0.1:9", None),
            key,
        );
        assert_eq!(local.unwrap().bearer, None);
    }

    /// A one-shot server on the loopback that records the request head and
    /// answers `reply` (or never answers when `reply` is `None`). Accepts with
    /// a deadline, so a test that never connects cannot hang the thread.
    fn serve_once(reply: Option<&'static str>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut conn = loop {
                match listener.accept() {
                    Ok((c, _)) => break c,
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return,
                }
            };
            conn.set_nonblocking(false).unwrap();
            conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            match reply {
                Some(r) => {
                    let _ = conn.write_all(r.as_bytes());
                }
                // Holds the connection open past the client's timeout.
                None => std::thread::sleep(Duration::from_secs(3)),
            }
        });
        (base, rx)
    }

    #[test]
    fn an_answer_reports_status_and_latency_and_the_key_is_sent() {
        let (base, head) = serve_once(Some(
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
        ));
        let req = probe_request(
            &spec(
                ProviderKind::Systemone,
                &base,
                Some(KeyRef::Env("K".into())),
            ),
            |_| Some("secret".into()),
        )
        .unwrap();
        let out = send(&req, Duration::from_secs(5));
        assert!(
            matches!(out, ProbeResult::Http { status: 401, .. }),
            "{out:?}"
        );
        assert!(!out.ok());
        assert!(!out.describe().contains("secret"));
        let head = head.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(head.starts_with("GET /v1/models "), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer secret")
        );
    }

    /// The case the connect check missed: the host accepts the connection
    /// and never answers.
    #[test]
    fn a_host_that_accepts_and_hangs_is_reported_within_the_timeout() {
        let (base, _head) = serve_once(None);
        let req = probe_request(&spec(ProviderKind::Systemone, &base, None), |_| None).unwrap();
        let started = Instant::now();
        let out = send(&req, Duration::from_millis(300));
        assert!(matches!(out, ProbeResult::TimedOut { .. }), "{out:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(out.describe(), "GET /v1/models: no answer within 300 ms");
    }
}
