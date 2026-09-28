//! Vercel AI Gateway's Evaluation route: `POST {base_url}/v1/evaluate`.
//!
//! The same questions as `/v1/systemone` with the gateway's own names:
//!
//! | Canonical | `/v1/evaluate` |
//! |---|---|
//! | question type `noul` | `boolean` |
//! | answer `{"type": "noul", "noul": p}` | `{"type": "boolean", "probability": p}` |
//! | `usage.input_tokens` / `output_tokens` | `usage.inputTokens` / `outputTokens` |
//! | `usage.cost` | `providerMetadata.gateway.cost` (a decimal string) |
//! | errors | `{"message", "error_type"}` |
//!
//! `choice` and `score` keep their names and shapes; their `confidence` may
//! be missing (the documented examples carry none), and is then recomputed
//! locally from the distribution by the shared validator, as for every
//! provider. Confidences recomputed this way are not comparable to a
//! vendor's own.
//!
//! `zero_data_retention` sends `providerOptions.gateway.zeroDataRetention:
//! true`, which asks the gateway to route only to providers that keep no
//! data (a request no such provider can serve fails).
//!
//! Calibration: only native evaluation models report calibrated
//! distributions; a language model answering through the gateway's
//! structured-output path (an evaluation fallback) does not. The response is
//! marked calibrated only when the answering model is TypeSafe's Jev
//! (`typesafe-ai/jev...`), whose vendor documents it as calibrated.
//!
//! Base URL `https://ai-gateway.vercel.sh`; model `typesafe-ai/jev`; key
//! `{{env.AI_GATEWAY_API_KEY}}`.
//!
//! Sources (read 2026-09-27):
//! - <https://vercel.com/docs/ai-gateway/modalities/evaluation>
//! - <https://vercel.com/docs/ai-gateway/sdks-and-apis/typesafe> (error shape)
//! - <https://vercel.com/docs/ai-gateway/models-and-providers/evaluation-fallbacks>
//!   (only native evaluation models report confidence)

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::{Map, Value, json};

use crate::http::Route;
use crate::systemone::{OrderedState, parse_systemone_reply, reply_json};
use crate::validate::check_limits;
use crate::{
    ApiKey, DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits, Question,
};

/// The answering-model prefix of a native (calibrated) evaluation model.
const CALIBRATED_PREFIX: &str = "typesafe-ai/jev";

/// A Vercel AI Gateway `/v1/evaluate` provider.
#[derive(Debug, Clone)]
pub struct VercelEvaluate {
    id: String,
    base_url: String,
    model: String,
    key: Option<ApiKey>,
    timeout: Duration,
    zero_data_retention: bool,
    limits: Limits,
}

impl VercelEvaluate {
    /// `base_url` is the gateway root (`https://ai-gateway.vercel.sh`);
    /// `timeout` bounds one decision, retries included.
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        key: Option<ApiKey>,
        timeout: Duration,
    ) -> Self {
        VercelEvaluate {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            key,
            timeout,
            zero_data_retention: false,
            // 32,000 tokens: the gateway's listed context for the model.
            limits: Limits::default(),
        }
    }

    /// Asks the gateway for zero data retention on every request.
    pub fn with_zero_data_retention(mut self, on: bool) -> Self {
        self.zero_data_retention = on;
        self
    }

    fn route(&self) -> Route {
        Route {
            url: format!("{}/v1/evaluate", self.base_url),
            key: self.key.clone(),
            invalid_statuses: &[400, 422],
        }
    }

    fn body(&self, req: &DecisionRequest) -> Result<Vec<u8>, DecideError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            model: &'a str,
            state: OrderedState<'a>,
            questions: Questions<'a>,
            #[serde(skip_serializing_if = "Option::is_none")]
            provider_options: Option<Value>,
        }
        serde_json::to_vec(&Body {
            model: &self.model,
            state: OrderedState(&req.state, &req.state_order),
            questions: Questions(&req.questions),
            provider_options: self
                .zero_data_retention
                .then(|| json!({"gateway": {"zeroDataRetention": true}})),
        })
        .map_err(|_| DecideError::Invalid("request does not serialize".into()))
    }

    fn parse(&self, req: &DecisionRequest, text: &str) -> Result<DecisionResponse, DecideError> {
        let reply = reply_json(text)?;
        let canonical = canonical_reply(&reply)?;
        let calibrated = canonical
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&self.model)
            .starts_with(CALIBRATED_PREFIX);
        parse_systemone_reply(&self.id, &self.model, calibrated, req, &canonical)
    }
}

/// The reply rewritten into the canonical shape: `boolean` items become
/// `noul` items, camelCase usage becomes snake_case, the gateway's cost
/// string becomes `usage.cost`.
fn canonical_reply(reply: &Value) -> Result<Value, DecideError> {
    let answers = reply
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| DecideError::Unavailable("reply has no answers".into()))?;
    let answers: Map<String, Value> = answers
        .iter()
        .map(|(id, item)| (id.clone(), canonical_item(item)))
        .collect();
    let usage = reply.get("usage");
    let count = |k: &str| usage.and_then(|u| u.get(k)).cloned();
    let cost = reply
        .pointer("/providerMetadata/gateway/cost")
        .and_then(|c| match c {
            Value::String(s) => s.trim().parse::<f64>().ok(),
            other => other.as_f64(),
        })
        .filter(|c| c.is_finite() && *c >= 0.0);
    let mut out = json!({
        "answers": answers,
        "usage": {"input_tokens": count("inputTokens"), "output_tokens": count("outputTokens")},
    });
    if let Some(model) = reply.get("model") {
        out["model"] = model.clone();
    }
    if let Some(c) = cost {
        out["usage"]["cost"] = json!(c);
    }
    Ok(out)
}

fn canonical_item(item: &Value) -> Value {
    match item.as_object() {
        Some(obj) if obj.get("type").and_then(Value::as_str) == Some("boolean") => {
            let mut out = Map::new();
            out.insert("type".into(), json!("noul"));
            if let Some(p) = obj.get("probability") {
                out.insert("noul".into(), p.clone());
            }
            Value::Object(out)
        }
        _ => item.clone(),
    }
}

/// The questions with `noul` renamed to `boolean`, choice options in their
/// given order.
struct Questions<'a>(&'a BTreeMap<String, Question>);

impl Serialize for Questions<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (id, q) in self.0 {
            match q {
                Question::Noul {
                    instructions,
                    criteria,
                } => {
                    #[derive(Serialize)]
                    struct Boolean<'a> {
                        #[serde(rename = "type")]
                        kind: &'static str,
                        instructions: &'a Value,
                        #[serde(skip_serializing_if = "Option::is_none")]
                        criteria: &'a Option<crate::NoulCriteria>,
                    }
                    map.serialize_entry(
                        id,
                        &Boolean {
                            kind: "boolean",
                            instructions,
                            criteria,
                        },
                    )?;
                }
                other => map.serialize_entry(id, other)?,
            }
        }
        map.end()
    }
}

impl DecisionProvider for VercelEvaluate {
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
        format!("vercel_evaluate:{}", self.model)
    }

    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        check_limits(req, &self.limits)?;
        let body = self.body(req)?;
        let (text, took) = self.route().exchange(&body, self.timeout)?;
        let mut response = self.parse(req, &text)?;
        response.latency_ms = took.as_millis() as u64;
        Ok(response)
    }
}
