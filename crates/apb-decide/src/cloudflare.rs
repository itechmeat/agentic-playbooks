//! Cloudflare Workers AI, model `typesafe/jev`, over the REST API:
//! `POST {base_url}/accounts/{account_id}/ai/run`.
//!
//! The REST form is the one the model page documents next to the Workers
//! binding (`env.AI.run('typesafe/jev', {state, questions})`):
//!
//! ```text
//! POST https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/run
//! Authorization: Bearer $CLOUDFLARE_API_TOKEN
//! {"model": "typesafe/jev", "input": {"state": ..., "questions": {...}}}
//! ```
//!
//! The reply is the canonical `{"model", "answers", "usage": {"input_tokens",
//! "output_tokens"}}` (the model page's output schema, with the answering
//! model as `jev-1.13.0`). Cloudflare's REST API usually wraps a result as
//! `{"result": ..., "success": true, "errors": [], "messages": []}`; both the
//! bare and the wrapped reply are accepted, and a wrapped `success: false` is
//! unavailable. No cost is reported; the model page lists USD 0.042 per
//! million input tokens and free output (the caller's price table applies).
//!
//! The token needs the Workers AI permission (the dashboard's "Workers AI"
//! token template, or `Workers AI - Read` and `Workers AI - Edit`). The
//! account id is a plain identifier, not a secret, but it is only accepted as
//! letters and digits so it cannot change the URL's path.
//!
//! Calibrated: the model page describes Jev's answers as calibrated, and it
//! lists the model as zero data retention.
//!
//! Status: the request shape is taken from Cloudflare's public documentation
//! and covered by fixture tests; it was not exercised against the live
//! service during development (no Cloudflare account was used).
//!
//! Sources (read 2026-09-27):
//! - <https://developers.cloudflare.com/ai/models/typesafe/jev/>
//! - <https://developers.cloudflare.com/ai/models/typesafe/jev/schema-output.json>
//! - <https://developers.cloudflare.com/workers-ai/get-started/rest-api/> (envelope, token)

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::http::Route;
use crate::systemone::{OrderedState, parse_systemone_reply, reply_json};
use crate::validate::check_limits;
use crate::{ApiKey, DecideError, DecisionProvider, DecisionRequest, DecisionResponse, Limits};

/// The default REST root.
pub const CLOUDFLARE_BASE_URL: &str = "https://api.cloudflare.com/client/v4";

/// A Cloudflare Workers AI provider.
#[derive(Debug, Clone)]
pub struct Cloudflare {
    id: String,
    base_url: String,
    account_id: String,
    model: String,
    key: Option<ApiKey>,
    timeout: Duration,
    limits: Limits,
}

impl Cloudflare {
    /// `base_url` is the REST root ([`CLOUDFLARE_BASE_URL`]); `timeout`
    /// bounds one decision, retries included.
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        model: impl Into<String>,
        key: Option<ApiKey>,
        timeout: Duration,
    ) -> Self {
        Cloudflare {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            account_id: account_id.into(),
            model: model.into(),
            key,
            timeout,
            // 32,000 tokens of context per the model page.
            limits: Limits::default(),
        }
    }

    /// Whether an account id is safe to put into the URL path.
    pub fn valid_account_id(account_id: &str) -> bool {
        (1..=64).contains(&account_id.len())
            && account_id.chars().all(|c| c.is_ascii_alphanumeric())
    }

    fn route(&self) -> Route {
        Route {
            url: format!("{}/accounts/{}/ai/run", self.base_url, self.account_id),
            key: self.key.clone(),
            invalid_statuses: &[400, 422],
        }
    }

    fn body(&self, req: &DecisionRequest) -> Result<Vec<u8>, DecideError> {
        #[derive(Serialize)]
        struct Input<'a> {
            state: OrderedState<'a>,
            questions: &'a std::collections::BTreeMap<String, crate::Question>,
        }
        #[derive(Serialize)]
        struct Body<'a> {
            model: &'a str,
            input: Input<'a>,
        }
        serde_json::to_vec(&Body {
            model: &self.model,
            input: Input {
                state: OrderedState(&req.state, &req.state_order),
                questions: &req.questions,
            },
        })
        .map_err(|_| DecideError::Invalid("request does not serialize".into()))
    }
}

/// The result inside Cloudflare's envelope, or the bare reply.
fn unwrap_envelope(reply: Value) -> Result<Value, DecideError> {
    if reply.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(DecideError::Unavailable("provider reported failure".into()));
    }
    match reply.get("result") {
        Some(inner) if inner.is_object() => Ok(inner.clone()),
        _ => Ok(reply),
    }
}

impl DecisionProvider for Cloudflare {
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
        format!("cloudflare:{}", self.model)
    }

    fn decide(&self, req: &DecisionRequest) -> Result<DecisionResponse, DecideError> {
        if !Self::valid_account_id(&self.account_id) {
            return Err(DecideError::Invalid(
                "the account id must be letters and digits".into(),
            ));
        }
        check_limits(req, &self.limits)?;
        let body = self.body(req)?;
        let (text, took) = self.route().exchange(&body, self.timeout)?;
        let reply = unwrap_envelope(reply_json(&text)?)?;
        let mut response = parse_systemone_reply(&self.id, &self.model, true, req, &reply)?;
        response.latency_ms = took.as_millis() as u64;
        Ok(response)
    }
}
