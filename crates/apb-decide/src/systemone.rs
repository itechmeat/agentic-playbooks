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

use std::time::Duration;

use serde_json::Value;

use crate::http::Route;
use crate::validate::{check_limits, validate_answers};
use crate::{
    ApiKey, DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits, Usage,
};

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

    fn route(&self) -> Route {
        Route {
            url: format!("{}/v1/systemone", self.base_url),
            key: self.key.clone(),
            invalid_statuses: &[422],
        }
    }
}

/// Parses a `/v1/systemone`-shaped reply (also OpenRouter's Decisions route
/// and Cloudflare's `result`): every item validated against its question,
/// `usage.cost` kept when the route reports one.
pub(crate) fn parse_systemone_reply(
    provider: &str,
    configured_model: &str,
    calibrated: bool,
    req: &DecisionRequest,
    reply: &Value,
) -> Result<DecisionResponse, DecideError> {
    let answers = reply
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| DecideError::Unavailable("reply has no answers".into()))?;
    let (answers, ignored_items) = validate_answers(&req.questions, answers);
    let usage = reply.get("usage");
    let count = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64);
    Ok(DecisionResponse {
        provider: provider.to_string(),
        model: crate::validate::reply_model(
            reply.get("model").and_then(Value::as_str),
            configured_model,
        ),
        calibrated,
        answers,
        usage: Usage {
            input_tokens: count("input_tokens"),
            output_tokens: count("output_tokens"),
            cost_usd: crate::validate::reply_cost(usage.and_then(|u| u.get("cost"))),
        },
        latency_ms: 0,
        cached: false,
        ignored_items,
    })
}

/// A reply body as JSON, or `Unavailable`.
pub(crate) fn reply_json(text: &str) -> Result<Value, DecideError> {
    serde_json::from_str(text).map_err(|_| DecideError::Unavailable("reply is not JSON".into()))
}

/// The `{"model", "state", "questions"}` body, serialized straight from the
/// types (not through a `serde_json::Value`, whose map would sort a
/// choice's options).
pub(crate) fn systemone_body(model: &str, req: &DecisionRequest) -> Result<Vec<u8>, DecideError> {
    serde_json::to_vec(&WireRequest::new(model, req))
        .map_err(|_| DecideError::Invalid("request does not serialize".into()))
}

/// The canonical request object, for adapters that wrap it.
#[derive(serde::Serialize)]
pub(crate) struct WireRequest<'a> {
    model: &'a str,
    state: OrderedState<'a>,
    questions: &'a std::collections::BTreeMap<String, crate::Question>,
}

impl<'a> WireRequest<'a> {
    pub(crate) fn new(model: &'a str, req: &'a DecisionRequest) -> Self {
        WireRequest {
            model,
            state: OrderedState(&req.state, &req.state_order),
            questions: &req.questions,
        }
    }
}

/// A state serialized with the caller's key order at the top level.
pub(crate) struct OrderedState<'a>(pub(crate) &'a Value, pub(crate) &'a [String]);

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

    fn threshold_profile(&self) -> String {
        format!("systemone:{}", self.model)
    }

    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        check_limits(req, &self.limits)?;
        let body = systemone_body(&self.model, req)?;
        let (text, took) = self.route().exchange(&body, self.timeout)?;
        let mut response =
            parse_systemone_reply(&self.id, &self.model, true, req, &reply_json(&text)?)?;
        response.latency_ms = took.as_millis() as u64;
        Ok(response)
    }
}
