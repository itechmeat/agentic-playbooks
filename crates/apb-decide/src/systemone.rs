//! The `/v1/systemone` adapter: TypeSafe's wire format, also served by the
//! gateways and by self-hosted servers.
//!
//! | Route | `base_url` | Pinned model id |
//! |---|---|---|
//! | TypeSafe | `https://api.typesafe.ai` | `jev-1.13.0` |
//! | OpenRouter | `https://openrouter.ai/api` | `typesafe/jev-1.13` |
//! | Vercel AI Gateway | `https://ai-gateway.vercel.sh/typesafe` | `typesafe-ai/jev` |
//! | OpenCode Zen | `https://opencode.ai/zen` | `jev-1.13` |
//! | Self-hosted (for example Laya) | `http://127.0.0.1:<port>` | the server's own id |
//!
//! Pin a versioned id: aliases such as `jev-latest` move with each release,
//! and thresholds tuned on one version do not transfer to the next.
//!
//! Request: `POST {base_url}/v1/systemone` with `{"model", "state",
//! "questions"}`, `Authorization: Bearer <key>` only when a key is set.
//! Reply: `{"model", "answers": {id: {...}}, "usage": {"input_tokens",
//! "output_tokens", "cost"?}}`.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::validate::{check_limits, validate_answers};
use crate::{
    ApiKey, DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits, Usage,
};

/// Most retries after the first attempt.
const MAX_RETRIES: u32 = 2;

/// How much of a 422 detail an error keeps.
const DETAIL_CHARS: usize = 200;

/// A `/v1/systemone` provider.
#[derive(Debug, Clone)]
pub struct SystemOne {
    id: String,
    base_url: String,
    model: String,
    key: Option<ApiKey>,
    timeout: Duration,
    limits: Limits,
}

impl SystemOne {
    /// `timeout` bounds one decision, retries and `retry-after` waits
    /// included.
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        key: Option<ApiKey>,
        timeout: Duration,
    ) -> Self {
        SystemOne {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            key,
            timeout,
            limits: Limits::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The request body, serialized straight from the types (not through a
    /// `serde_json::Value`, whose map would sort a choice's options).
    fn body(&self, req: &DecisionRequest) -> Result<Vec<u8>, DecideError> {
        #[derive(serde::Serialize)]
        struct Body<'a> {
            model: &'a str,
            state: OrderedState<'a>,
            questions: &'a std::collections::BTreeMap<String, crate::Question>,
        }
        serde_json::to_vec(&Body {
            model: &self.model,
            state: OrderedState(&req.state, &req.state_order),
            questions: &req.questions,
        })
        .map_err(|_| DecideError::Invalid("request does not serialize".into()))
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
            .uri(format!("{}/v1/systemone", self.base_url))
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
            .and_then(|s| s.trim().parse::<u64>().ok());
        let text = response
            .into_body()
            .read_to_string()
            .map_err(|_| DecideError::Unavailable("reply could not be read".into()))?;
        Ok((status, retry_after, text))
    }

    fn scrub(&self, text: &str) -> String {
        let cut: String = text.chars().take(DETAIL_CHARS).collect();
        match &self.key {
            Some(k) => k.scrub(&cut),
            None => cut,
        }
    }

    fn parse_reply(
        &self,
        req: &DecisionRequest,
        text: &str,
    ) -> Result<DecisionResponse, DecideError> {
        let reply: Value = serde_json::from_str(text)
            .map_err(|_| DecideError::Unavailable("reply is not JSON".into()))?;
        let answers = reply
            .get("answers")
            .and_then(Value::as_object)
            .ok_or_else(|| DecideError::Unavailable("reply has no answers".into()))?;
        let (answers, ignored_items) = validate_answers(&req.questions, answers);
        let usage = reply.get("usage");
        let count = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64);
        Ok(DecisionResponse {
            provider: self.id.clone(),
            model: reply
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&self.model)
                .to_string(),
            calibrated: true,
            answers,
            usage: Usage {
                input_tokens: count("input_tokens"),
                output_tokens: count("output_tokens"),
                cost_usd: usage.and_then(|u| u.get("cost")).and_then(Value::as_f64),
            },
            latency_ms: 0,
            cached: false,
            ignored_items,
        })
    }
}

/// A state serialized with the caller's key order at the top level.
struct OrderedState<'a>(&'a Value, &'a [String]);

impl serde::Serialize for OrderedState<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let Value::Object(map) = self.0 else {
            return self.0.serialize(s);
        };
        let mut out = s.serialize_map(Some(map.len()))?;
        for k in self.1.iter().filter(|k| map.contains_key(*k)) {
            out.serialize_entry(k, &map[k])?;
        }
        for (k, v) in map.iter().filter(|(k, _)| !self.1.contains(k)) {
            out.serialize_entry(k, v)?;
        }
        out.end()
    }
}

/// Whether a status is worth another try: request timeout, conflict, rate
/// limit, server errors and the overload code.
fn retryable(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 529) || (500..600).contains(&status)
}

impl DecisionProvider for SystemOne {
    fn id(&self) -> &str {
        &self.id
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn limits(&self) -> Limits {
        self.limits
    }

    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        check_limits(req, &self.limits)?;
        let body = self.body(req)?;
        let started = Instant::now();
        let deadline = started + self.timeout;
        let mut retries = 0;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(DecideError::Timeout);
            }
            // A transport failure is never retried: once the body may have
            // reached the server, a second request could be billed twice.
            let (status, retry_after, text) = self.send(&body, left)?;
            match status {
                200..=299 => {
                    let mut response = self.parse_reply(req, &text)?;
                    response.latency_ms = started.elapsed().as_millis() as u64;
                    return Ok(response);
                }
                401 | 403 => return Err(DecideError::Auth),
                402 => return Err(DecideError::Budget),
                422 => return Err(DecideError::Invalid(self.scrub(&text))),
                s if retryable(s) => {
                    let wait = Duration::from_secs(retry_after.unwrap_or(0));
                    let fits = Instant::now() + wait < deadline;
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
