//! The one HTTP exchange every hosted adapter shares: a JSON `POST` with an
//! optional bearer key, the status mapping onto [`DecideError`] and the retry
//! rules (issue #165 Part 1).
//!
//! - 2xx: the body text is returned for the adapter to parse.
//! - 401, 403: [`DecideError::Auth`]; 402: [`DecideError::Budget`].
//! - The adapter's invalid statuses (422 everywhere, 400 where a route
//!   documents it for validation): [`DecideError::Invalid`] with a short
//!   detail, truncated and scrubbed of the key.
//! - 408, 409, 429, 5xx and 529 are retried at most twice, honouring a
//!   `retry-after` (seconds) only when the wait fits into the timeout; an
//!   exhausted 429 is [`DecideError::RateLimited`], anything else
//!   [`DecideError::Unavailable`].
//! - A transport failure is never retried: once the body may have reached
//!   the server, a second request could be billed twice.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{ApiKey, DecideError};

/// Most retries after the first attempt.
const MAX_RETRIES: u32 = 2;

/// How much of an error detail an error keeps.
const DETAIL_CHARS: usize = 200;

/// Where one adapter posts, and how it reads refusals.
#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub(crate) url: String,
    pub(crate) key: Option<ApiKey>,
    /// Statuses the route uses for a refused request (never retried, and a
    /// provider chain does not move on: the request is invalid everywhere).
    pub(crate) invalid_statuses: &'static [u16],
}

impl Route {
    /// `text` cut to a short detail with the key scrubbed. A JSON error body
    /// is reduced to its message (`message`, `detail`, `error.message`
    /// or `errors[0].message`), so the detail never carries the rest of it.
    pub(crate) fn detail(&self, text: &str) -> String {
        let message = serde_json::from_str::<Value>(text).ok().and_then(|v| {
            v.get("message")
                .or_else(|| v.get("detail"))
                .or_else(|| v.pointer("/error/message"))
                .or_else(|| v.pointer("/errors/0/message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let raw = message.as_deref().unwrap_or(text);
        let cut: String = raw.chars().take(DETAIL_CHARS).collect();
        match &self.key {
            Some(k) => k.scrub(&cut),
            None => cut,
        }
    }

    /// One HTTP exchange. `Ok` carries the status, the `retry-after` header
    /// and the body text; `Err` is a transport failure.
    fn send(
        &self,
        body: &[u8],
        budget: Duration,
    ) -> Result<(u16, Option<u64>, String), DecideError> {
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(budget))
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut builder = ureq::http::Request::builder()
            .method("POST")
            .uri(&self.url)
            .header("content-type", "application/json");
        if let Some(key) = &self.key {
            builder = builder.header("authorization", format!("Bearer {}", key.expose()));
        }
        let request = builder
            .body(body.to_vec())
            .map_err(|_| DecideError::Unavailable("request could not be built".into()))?;
        let response = agent.run(request).map_err(|e| match e {
            ureq::Error::Timeout(_) => DecideError::Timeout,
            // A transport failure never carries ureq's own text: it can name
            // the URL, and nothing about the request belongs in an error.
            ureq::Error::HostNotFound => DecideError::Unavailable("host not found".into()),
            ureq::Error::ConnectionFailed => DecideError::Unavailable("connection failed".into()),
            _ => DecideError::Unavailable("transport error".into()),
        })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            // The header comes from the provider: bound it so that a huge
            // value can neither overflow the deadline arithmetic nor be
            // passed on as a wait.
            .map(|s| s.min(crate::http::MAX_RETRY_AFTER_SECS));
        let text = response
            .into_body()
            .read_to_string()
            .map_err(|_| DecideError::Unavailable("reply could not be read".into()))?;
        Ok((status, retry_after, text))
    }

    /// Posts `body` until a final answer, within `timeout` (retries and
    /// `retry-after` waits included). Returns the 2xx body text and the time
    /// the exchange took.
    pub(crate) fn exchange(
        &self,
        body: &[u8],
        timeout: Duration,
    ) -> Result<(String, Duration), DecideError> {
        let started = Instant::now();
        let deadline = started + timeout;
        let mut retries = 0;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(DecideError::Timeout);
            }
            let (status, retry_after, text) = self.send(body, left)?;
            match status {
                200..=299 => return Ok((text, started.elapsed())),
                401 | 403 => return Err(DecideError::Auth),
                402 => return Err(DecideError::Budget),
                s if self.invalid_statuses.contains(&s) => {
                    return Err(DecideError::Invalid(self.detail(&text)));
                }
                s if retryable(s) => {
                    let wait = Duration::from_secs(retry_after.unwrap_or(0));
                    let fits = wait < deadline.saturating_duration_since(Instant::now());
                    if retries >= MAX_RETRIES || !fits {
                        return Err(if s == 429 {
                            DecideError::RateLimited {
                                retry_after: retry_after.map(Duration::from_secs),
                            }
                        } else {
                            DecideError::Unavailable(format!("http {s}"))
                        });
                    }
                    retries += 1;
                    std::thread::sleep(wait);
                }
                s => return Err(DecideError::Unavailable(format!("http {s}"))),
            }
        }
    }
}

/// The longest `retry-after` a reply may ask for; a larger value is read as
/// this one.
pub(crate) const MAX_RETRY_AFTER_SECS: u64 = 3600;

/// Whether a status is worth another try: request timeout, conflict, rate
/// limit, server errors and the overload code.
fn retryable(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 529) || (500..600).contains(&status)
}
