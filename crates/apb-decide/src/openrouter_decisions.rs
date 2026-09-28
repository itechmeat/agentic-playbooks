//! OpenRouter's Decisions API (**alpha**): `POST {base_url}/api/alpha/decisions`.
//!
//! The route is marked alpha by OpenRouter and may change or go away without
//! notice; the `systemone` kind against `https://openrouter.ai/api` (the
//! compatible `/v1/systemone` path) is the stable way to reach the same model.
//!
//! Wire shape, per the OpenAPI reference: the request is the canonical
//! `{"model", "state", "questions"}` (noul, choice, score), the reply
//! `{"id", "model", "provider", "answers", "usage": {"input_tokens",
//! "output_tokens", "cost"}}`, with the answering model's dated id in
//! `model` (for example `typesafe/jev-1.13-20260917`). Refusals are
//! `{"error": {"code", "message"}}`; 400 is a validation refusal, 402 out of
//! credits, 429 rate limited, 502, 503, 524 and 529 upstream failures.
//!
//! Pin a versioned model id (`typesafe/jev-1.13`): aliases move with each
//! release and one community report had the `~typesafe/jev-latest` alias
//! fail on this route; `apb doctor` flags an alias model id.
//!
//! Base URL `https://openrouter.ai`; key `{{env.OPENROUTER_API_KEY}}`.
//! Calibrated: the model is TypeSafe's Jev, which its vendor documents as
//! calibrated, and OpenRouter routes the request to it unchanged.
//!
//! Sources (read 2026-09-27):
//! - <https://openrouter.ai/docs/guides/community/jev.md>
//! - <https://openrouter.ai/docs/api/api-reference/alphadecisions/submit-a-decisions-questions-and-answers-request.md>

use std::time::Duration;

use crate::http::Route;
use crate::systemone::{parse_systemone_reply, reply_json, systemone_body};
use crate::validate::check_limits;
use crate::{ApiKey, DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits};

/// An OpenRouter Decisions provider (alpha route).
#[derive(Debug, Clone)]
pub struct OpenRouterDecisions {
    id: String,
    base_url: String,
    model: String,
    key: Option<ApiKey>,
    timeout: Duration,
    limits: Limits,
}

impl OpenRouterDecisions {
    /// `base_url` is the site root (`https://openrouter.ai`); `timeout`
    /// bounds one decision, retries included.
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        key: Option<ApiKey>,
        timeout: Duration,
    ) -> Self {
        OpenRouterDecisions {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            key,
            timeout,
            // 32,000 tokens of context per the model page.
            limits: Limits::default(),
        }
    }

    fn route(&self) -> Route {
        Route {
            url: format!("{}/api/alpha/decisions", self.base_url),
            key: self.key.clone(),
            invalid_statuses: &[400, 422],
        }
    }
}

impl DecisionProvider for OpenRouterDecisions {
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
        format!("openrouter_decisions:{}", self.model)
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
