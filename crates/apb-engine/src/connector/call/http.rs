//! An HTTP call ready to send, and the sending itself: auth injected after
//! the pre-auth URL is fixed, redirects off, the per-function timeout, the
//! body read under its cap, the status mapped into the result taxonomy, the
//! connector's false-success predicate, the `response_pick` projection, and
//! the interim literal secret redaction of everything that comes back.

use std::collections::BTreeMap;
use std::time::Duration;

use apb_core::connector::def::{AuthSpec, ErrorWhen};
use apb_core::connector::template::{RenderCtx, render_encoded, render_raw};
use apb_core::secret::{Redaction, Secret};
use serde_json::Value;

use super::encode::{
    assemble_url, base64_encode, encode_component, encode_form_body, encode_path_segment, project,
};
use super::response::{map_status, read_body, reclassify_error_when, redact_value};
use crate::connector::result::{CallError, CallErrorCode, CallOk, redact_message};

/// An HTTP call ready to send.
pub(super) struct HttpCall {
    pub(super) account: String,
    pub(super) method: String,
    /// The function's `read_only` flag: a 2xx whose body did not read whole
    /// fails as `incomplete_response` for a read-only function, and stays a
    /// success carrying the incompleteness for any other function.
    pub(super) read_only: bool,
    /// URL rendered BEFORE auth injection (function query included, auth query
    /// NOT). This is the URL recorded in the event log.
    pub(super) pre_auth_url: String,
    pub(super) rendered_body: Option<Value>,
    /// Rendered `body_form` pairs, DECODED (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.1); percent-encoded
    /// into the wire body by `send_raw`. Mutually exclusive with
    /// `rendered_body` by validation.
    pub(super) rendered_form: Option<Vec<(String, String)>>,
    /// Rendered per-function headers (spec 4.4). Values use `account.*`/`args.*`
    /// only; `secret.*` is forbidden here by `validate_templates`.
    pub(super) headers: BTreeMap<String, String>,
    /// The effective `response_pick` projection (spec 4.5); empty when the
    /// function declares none or `--full` bypasses it.
    pub(super) response_pick: Vec<String>,
    pub(super) auth: Option<AuthSpec>,
    /// The connector-level false-success predicate (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.2), applied by
    /// `send_raw` after a 2xx maps Ok and after body redaction, BEFORE the
    /// `response_pick` projection.
    pub(super) error_when: Option<ErrorWhen>,
    pub(super) secrets: BTreeMap<String, Secret>,
    pub(super) account_fields: BTreeMap<String, String>,
    pub(super) timeout_sec: u64,
    /// The resolved secrets, each with its label, for interim redaction.
    pub(super) redactions: Vec<Redaction>,
}

/// The rendered user and password of a Basic auth spec: what the
/// `Authorization` header encodes, and what [`Redaction::basic`] scrubs.
pub(super) fn basic_credentials(
    username_template: &str,
    password_template: &str,
    ctx: &RenderCtx,
) -> Result<(String, Secret), CallError> {
    let render = |t: &str| {
        render_raw(t, ctx)
            .map_err(|e| CallError::new(CallErrorCode::Config, format!("auth render failed: {e}")))
    };
    Ok((
        render(username_template)?,
        Secret::new(render(password_template)?),
    ))
}

/// The redaction of the Basic credential `auth` renders, if it is Basic
/// auth and renders at all (a render failure fails the call anyway).
pub(super) fn basic_redaction(
    auth: Option<&AuthSpec>,
    account: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, Secret>,
) -> Option<Redaction> {
    let Some(AuthSpec::Basic {
        username_template,
        password_template,
    }) = auth
    else {
        return None;
    };
    let ctx = RenderCtx {
        account,
        args: &Value::Null,
        secrets,
    };
    let (user, pass) = basic_credentials(username_template, password_template, &ctx).ok()?;
    Some(Redaction::basic(&user, &pass, "basic"))
}

impl HttpCall {
    /// Sends the request and returns the mapped result, redacting any error
    /// MESSAGE through the same interim literal scrub as the body. Kept as
    /// defense in depth regardless of what a given transport error's
    /// `Display` happens to include (ureq 2's did put the request URL in it,
    /// which for `query`-kind auth carries the resolved secret): every error
    /// produced after secrets are resolved is scrubbed before it can be
    /// printed or logged (spec 6.2).
    pub(super) fn send(&self) -> (Result<CallOk, CallError>, Option<u16>) {
        let (result, status) = self.send_raw();
        (result.map_err(|e| self.redact_error(e)), status)
    }

    /// Sends the request with redirects disabled and the per-function timeout,
    /// injecting auth after the pre-auth URL is fixed, then maps the response
    /// (or transport error) into the result taxonomy. Returns the mapped
    /// result and the observed HTTP status when one exists.
    ///
    /// ureq 3 notes: `http_status_as_error(false)` keeps every status (2xx
    /// through 5xx) coming back as `Ok(response)` exactly like ureq 2's
    /// `Error::Status(_, resp)` did, so the taxonomy below is still driven
    /// entirely by `map_status` reading the real response, never by ureq's own
    /// status-to-error translation. `allow_non_standard_methods(true)` keeps
    /// an unusual/lowercased connector `method` string sendable, matching
    /// ureq 2's untyped `&str` method (ureq 3's methods are typed
    /// `http::Method`, which rejects nothing byte-valid but does gate
    /// non-standard tokens behind this flag).
    fn send_raw(&self) -> (Result<CallOk, CallError>, Option<u16>) {
        let method = match ureq::http::Method::from_bytes(self.method.as_bytes()) {
            Ok(m) => m,
            Err(e) => {
                return (
                    Err(CallError::new(
                        CallErrorCode::Config,
                        format!("invalid HTTP method `{}`: {e}", self.method),
                    )),
                    None,
                );
            }
        };
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(self.timeout_sec)))
            .allow_non_standard_methods(true)
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);

        // Build the request URL: the pre-auth URL plus a `query`-kind auth
        // param, if any. Only this final URL is sent; the pre-auth URL is what
        // the event log records.
        let ctx = RenderCtx {
            account: &self.account_fields,
            args: &Value::Null,
            secrets: &self.secrets,
        };
        let request_url = match &self.auth {
            Some(AuthSpec::Path { value_template }) => match render_raw(value_template, &ctx) {
                Ok(seg) => self
                    .pre_auth_url
                    .replacen("{{auth}}", &encode_path_segment(&seg), 1),
                Err(e) => {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Config,
                            format!("auth render failed: {e}"),
                        )),
                        None,
                    );
                }
            },
            _ => match self.auth_query(&ctx) {
                Ok(Some((param, value))) => {
                    assemble_url(&self.pre_auth_url, &format!("{param}={value}"))
                }
                Ok(None) => self.pre_auth_url.clone(),
                Err(e) => return (Err(e), None),
            },
        };

        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(&request_url);
        // Headers were fully assembled by `render_http` (function headers plus
        // the default User-Agent unless overridden, spec 4.4), so send them
        // verbatim.
        for (name, value) in &self.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        // Header / Basic auth injection.
        match self.auth_header(&ctx) {
            Ok(Some((name, value))) => builder = builder.header(name, value),
            Ok(None) => {}
            Err(e) => return (Err(e), None),
        }

        // The outgoing body payload: a JSON body serializes with the JSON
        // content type, a form body (spec 2026-07-22-official-connectors-wave-3,
        // section 4.1) percent-encodes its pairs with the urlencoded content
        // type. The two are mutually exclusive by validation.
        let payload = match &self.rendered_body {
            Some(body) => match serde_json::to_vec(body) {
                Ok(bytes) => Some((bytes, "application/json")),
                Err(e) => {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Config,
                            format!("body serialize failed: {e}"),
                        )),
                        None,
                    );
                }
            },
            None => self.rendered_form.as_ref().map(|pairs| {
                (
                    encode_form_body(pairs).into_bytes(),
                    "application/x-www-form-urlencoded",
                )
            }),
        };
        let response = match payload {
            Some((bytes, content_type)) => {
                // Mirrors ureq 2's `send_json`: only add the default content
                // type when the function's own headers did not already set one.
                let has_content_type = self
                    .headers
                    .keys()
                    .any(|k| k.eq_ignore_ascii_case("content-type"));
                if !has_content_type {
                    builder = builder.header("content-type", content_type);
                }
                match builder.body(bytes) {
                    Ok(request) => agent.run(request),
                    Err(e) => {
                        return (
                            Err(CallError::new(
                                CallErrorCode::Config,
                                format!("request could not be built: {e}"),
                            )),
                            None,
                        );
                    }
                }
            }
            None => match builder.body(()) {
                Ok(request) => agent.run(request),
                Err(e) => {
                    return (
                        Err(CallError::new(
                            CallErrorCode::Config,
                            format!("request could not be built: {e}"),
                        )),
                        None,
                    );
                }
            },
        };

        let response = match response {
            Ok(resp) => resp,
            Err(e) => {
                // ureq 3 gives timeouts their own variant (no more sniffing
                // "timed out" out of a generic transport error's Display).
                let code = if matches!(e, ureq::Error::Timeout(_)) {
                    CallErrorCode::Timeout
                } else {
                    CallErrorCode::Network
                };
                return (Err(CallError::new(code, e.to_string())), None);
            }
        };

        let status = response.status().as_u16();
        // Parse `Retry-After` before the response is consumed by the reader;
        // only used to populate `retry_after_sec` on a 429 (spec section 8).
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        // The `Link` header must be read before the response is consumed by the
        // body reader; surfaced verbatim in the result (spec 4.4).
        let link = response
            .headers()
            .get("Link")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let (body, incomplete) = read_body(response);
        // 12. Interim literal secret redaction (see the TODO below).
        let body = self.redact(body);
        let mut mapped = map_status(status, body, incomplete, retry_after, self.read_only);
        // error_when reclassification (spec
        // 2026-07-22-official-connectors-wave-3, section 4.2): a 2xx body
        // matching the connector's predicate maps to a `service` error
        // instead of a false success. Runs on the already-redacted body, so
        // an extracted message is scrubbed like any other body text, and
        // BEFORE the `response_pick` projection below (a reclassified
        // response is never projected; the error envelope carries the
        // extracted message instead). Non-2xx statuses arrive here as Err
        // and keep the status-table mapping untouched. Ordering with the
        // incompleteness rule: a read-only cut body already failed in
        // `map_status`, so only a non-read-only call reaches this match; when
        // it does, the reclassification wins and the error carries no
        // completeness keys (a reclassified response is an error).
        if let Ok(CallOk::Http { body, .. }) = &mapped
            && let Some(rule) = &self.error_when
            && let Some(err) = reclassify_error_when(rule, body, status)
        {
            mapped = Err(err);
        }
        if let Ok(CallOk::Http {
            link: link_slot,
            body: body_slot,
            picked,
            ..
        }) = &mut mapped
        {
            *link_slot = link;
            if !self.response_pick.is_empty() {
                *body_slot = project(body_slot, &self.response_pick);
                *picked = true;
            }
        }
        (mapped, Some(status))
    }

    /// The `query`-kind auth param, already percent-encoded, if auth is a
    /// `Query` variant.
    fn auth_query(&self, ctx: &RenderCtx) -> Result<Option<(String, String)>, CallError> {
        match &self.auth {
            Some(AuthSpec::Query {
                param,
                value_template,
            }) => {
                let value = render_encoded(value_template, ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                Ok(Some((encode_component(param), value)))
            }
            _ => Ok(None),
        }
    }

    /// The auth header name/value, if auth is a `Header` or `Basic` variant.
    fn auth_header(&self, ctx: &RenderCtx) -> Result<Option<(String, String)>, CallError> {
        match &self.auth {
            Some(AuthSpec::Header {
                header,
                value_template,
            }) => {
                let value = render_raw(value_template, ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                Ok(Some((header.clone(), value)))
            }
            Some(AuthSpec::Basic {
                username_template,
                password_template,
            }) => {
                let (user, pass) = basic_credentials(username_template, password_template, ctx)?;
                let token = base64_encode(format!("{user}:{}", pass.expose_secret()).as_bytes());
                Ok(Some((
                    "Authorization".to_string(),
                    format!("Basic {token}"),
                )))
            }
            _ => Ok(None),
        }
    }

    // TODO(redaction-layer): interim literal redaction, replaced by the dedicated LLM-output redaction story.
    /// Replaces every literal occurrence of each resolved secret value in the
    /// body's string leaves with `[redacted:<ENV_NAME>]`. Only exact literal
    /// matches are caught (spec 6.2, accepted interim limit).
    fn redact(&self, body: Value) -> Value {
        if self.redactions.is_empty() {
            return body;
        }
        redact_value(body, &self.redactions)
    }

    /// Scrubs an error message with the same interim literal redaction as the
    /// body. A transport error's URL (and thus a `query`-kind auth secret) can
    /// otherwise leak into `error.message`.
    fn redact_error(&self, mut err: CallError) -> CallError {
        err.message = redact_message(err.message, &self.redactions);
        err
    }
}
