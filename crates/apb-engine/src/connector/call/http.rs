//! An HTTP call ready to send, and the sending itself: auth injected after
//! the pre-auth URL is fixed, redirects off, the per-function timeout, the
//! body read under its cap, the status mapped into the result taxonomy, the
//! connector's false-success predicate, the `response_pick` projection, and
//! the interim literal secret redaction of everything that comes back.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use apb_core::connector::def::{AuthSpec, ErrorWhen, FunctionSpec};
use apb_core::connector::template::{RenderCtx, render_encoded, render_raw};
use apb_core::secret::{Redaction, Secret};
use serde_json::Value;

use crate::connector::call::encode::{
    base64_encode, encode_form_body, encode_path_segment, project,
};
use crate::connector::call::paginate::{WalkStats, Walked, walk as walk_pages};
use crate::connector::call::response::{map_status, read_body, redact_value};
use crate::connector::call::shape::{RawResponse, ShapeRules, check_content_type, shape};
use crate::connector::call::url_encode::{assemble_url, encode_component};
use crate::connector::call::wire::BODY_CAP;
use crate::connector::oauth::{ROTATION_WARNING, TokenSource};
use crate::connector::result::{
    BodyIncomplete, CallError, CallErrorCode, CallOk, Dispatched, redact_message,
};

use crate::connector::call::render::{render_http, validate_url};

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
    /// The minted access token of an `oauth2_refresh` call, in process
    /// memory only.
    pub(super) access: Option<Secret>,
    /// The token source a 401 refreshes through. `None` for every other
    /// auth kind.
    pub(super) token_source: Option<TokenSource>,
    /// The one deadline of the whole call: minting, the service request,
    /// the refresh and retry after a 401, and every page share it.
    pub(super) deadline: Instant,
    /// Warnings minting already produced (a rotated refresh token).
    pub(super) warnings: Vec<String>,
    /// The declared paging plan; `None` for a single-page read.
    pub(super) paginate: Option<PagePlan>,
    /// The function's `decode_base64url` paths, decoded after
    /// `error_when` on a 2xx.
    pub(super) decode_base64url: Vec<String>,
    /// The function's `content_types` media-type guard (D5): checked on a
    /// 2xx before the body is read.
    pub(super) content_types: Vec<String>,
    /// The function's name, for the guard's error message.
    pub(super) function_name: String,
    /// The connector-level false-success predicate (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.2), applied by
    /// `send_raw` after a 2xx maps Ok and after body redaction, BEFORE the
    /// `response_pick` projection.
    pub(super) error_when: Option<ErrorWhen>,
    pub(super) secrets: BTreeMap<String, Secret>,
    pub(super) account_fields: BTreeMap<String, String>,
    /// The resolved secrets, each with its label, for interim redaction.
    /// Grown in place when a refresh mints further credentials, hence the
    /// mutex.
    pub(super) redactions: Mutex<Vec<Redaction>>,
}

impl HttpCall {
    /// The redaction list, tolerant of a poisoned lock (a redaction list is
    /// never structurally invalid).
    pub(super) fn redactions(&self) -> std::sync::MutexGuard<'_, Vec<Redaction>> {
        self.redactions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers one more credential for the interim redaction.
    pub(super) fn register_redaction(&self, value: Secret, label: &str) {
        let mut list = self.redactions();
        list.push(Redaction::new(value, label.to_string()));
    }
}

/// What a token-mode next page needs to re-render the request: the spec,
/// the function (its query template included), and the original args.
pub(crate) struct PagePlan {
    pub spec: apb_core::connector::paginate::Paginate,
    pub function: FunctionSpec,
    pub args: Value,
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
    pub(super) fn send(&self) -> Dispatched {
        let mut warnings = self.warnings.clone();
        // The access token every page sends; a refresh after a 401
        // replaces it for the rest of the walk.
        let mut access = self.access.clone();
        let rules = ShapeRules {
            read_only: self.read_only,
            error_when: self.error_when.as_ref(),
            decode_paths: &self.decode_base64url,
        };
        // The live fetch of one page. The 401 refresh and its single retry
        // happen here, so the walk stays pure over its closures.
        let mut fetch = |url: &str, allowance: usize| -> Result<RawResponse, CallError> {
            let mut raw = self.send_raw(url, access.as_ref(), allowance)?;
            if raw.status == 401
                && let Some(source) = &self.token_source
            {
                let minted = {
                    let list = self.redactions();
                    source.refresh_now(self.deadline, &list)
                };
                match minted {
                    Ok(minted) => {
                        if minted.rotated {
                            warnings.push(ROTATION_WARNING.to_string());
                        }
                        self.register_redaction(minted.access.clone(), "oauth:access_token");
                        if let Some(new) = &minted.new_refresh {
                            self.register_redaction(new.clone(), "oauth:refresh_token");
                        }
                        // The retry's answer is final: a second 401
                        // maps to auth as today, any other status is
                        // taken as it comes.
                        access = Some(minted.access.clone());
                        raw = self.send_raw(url, access.as_ref(), allowance)?;
                    }
                    Err(token_err) => {
                        // The service's 401 stands; only the message
                        // grows, so the send ledger reads a refused
                        // send (http_status stays 401).
                        let err = map_status(401, Value::Null, None, None, self.read_only)
                            .expect_err("a 401 maps to an error");
                        let mut err = err;
                        err.message = format!(
                            "{}; renewing the token failed: {}",
                            err.message, token_err.message
                        );
                        return Err(self.redact_error(err));
                    }
                }
            }
            Ok(raw)
        };

        let (result, stats) = match &self.paginate {
            None => {
                let mut stats = WalkStats {
                    pages: 1,
                    last_status: None,
                };
                match fetch(&self.pre_auth_url, BODY_CAP) {
                    Ok(raw) => {
                        stats.last_status = Some(raw.status);
                        match shape(raw, &rules) {
                            Ok(ok) => (Ok(Walked { ok, more: false }), stats),
                            Err(e) => (Err(e), stats),
                        }
                    }
                    Err(e) => (Err(e), stats),
                }
            }
            Some(plan) => {
                let mut next_url = |token: &str| -> Result<String, CallError> {
                    let mut args = plan.args.clone();
                    if let Some(arg) = &plan.spec.next.token_arg
                        && let Some(map) = args.as_object_mut()
                    {
                        map.insert(arg.clone(), Value::String(token.to_string()));
                    }
                    let rendered = render_http(
                        &plan.function,
                        &self.account_fields,
                        &args,
                        &self.secrets,
                        None,
                    )?;
                    Ok(rendered.pre_auth_url)
                };
                walk_pages(
                    &self.pre_auth_url,
                    &plan.spec,
                    &rules,
                    &mut fetch,
                    &mut next_url,
                )
            }
        };

        // After shaping: the whole envelope is scrubbed (body and link),
        // then the projection. Errors are scrubbed through the same
        // redactions.
        let result = match result {
            Ok(walked) => {
                let more = walked.more;
                let pages = stats.pages;
                let ok = match walked.ok {
                    CallOk::Http {
                        status,
                        body,
                        incomplete,
                        link,
                        picked,
                        pages: ok_pages,
                    } => {
                        let body = self.redact(body);
                        let link = link.map(|l| redact_message(l, &self.redactions()));
                        let (body, picked) = if self.response_pick.is_empty() {
                            (body, picked)
                        } else {
                            (project(&body, &self.response_pick), true)
                        };
                        let _ = ok_pages;
                        CallOk::Http {
                            status,
                            body,
                            incomplete,
                            link,
                            picked,
                            pages: Some((pages, more)),
                        }
                    }
                    other => other,
                };
                Ok(ok)
            }
            Err(err) => Err(self.redact_error(err)),
        };
        Dispatched {
            result,
            http_status: stats.last_status,
            pages: Some(stats.pages),
            warnings,
        }
    }

    /// Sends one request of the call and reads its answer into a
    /// [`RawResponse`]: no mapping, no redaction. `url` is the pre-auth
    /// form (auth injected here), `allowance` caps the body read. Any
    /// transport error carries no status.
    fn send_raw(
        &self,
        url: &str,
        access: Option<&Secret>,
        allowance: usize,
    ) -> Result<RawResponse, CallError> {
        if self.deadline <= Instant::now() {
            return Err(CallError::new(
                CallErrorCode::Timeout,
                "the call's deadline passed before the request",
            ));
        }
        let method = ureq::http::Method::from_bytes(self.method.as_bytes()).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("invalid HTTP method `{}`: {e}", self.method),
            )
        })?;
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(
                self.deadline.saturating_duration_since(Instant::now()),
            ))
            .allow_non_standard_methods(true)
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);

        // Auth injection on the pre-auth URL, as before: a path segment
        // replaces `{{auth}}`, a query kind appends its pair.
        let ctx = RenderCtx {
            account: &self.account_fields,
            args: &Value::Null,
            secrets: &self.secrets,
        };
        let request_url = match &self.auth {
            Some(AuthSpec::Path { value_template }) => {
                let seg = render_raw(value_template, &ctx).map_err(|e| {
                    CallError::new(CallErrorCode::Config, format!("auth render failed: {e}"))
                })?;
                url.replacen("{{auth}}", &encode_path_segment(&seg), 1)
            }
            _ => match self.auth_query(&ctx)? {
                Some((param, value)) => assemble_url(url, &format!("{param}={value}")),
                None => url.to_string(),
            },
        };
        validate_url(&request_url)?;

        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(&request_url);
        for (name, value) in &self.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some((name, value)) = self.auth_header(&ctx, access)? {
            builder = builder.header(name, value);
        }

        // The outgoing body payload, as before.
        let payload = match &self.rendered_body {
            Some(body) => match serde_json::to_vec(body) {
                Ok(bytes) => Some((bytes, "application/json")),
                Err(e) => {
                    return Err(CallError::new(
                        CallErrorCode::Config,
                        format!("body serialize failed: {e}"),
                    ));
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
                        return Err(CallError::new(
                            CallErrorCode::Config,
                            format!("request could not be built: {e}"),
                        ));
                    }
                }
            }
            None => match builder.body(()) {
                Ok(request) => agent.run(request),
                Err(e) => {
                    return Err(CallError::new(
                        CallErrorCode::Config,
                        format!("request could not be built: {e}"),
                    ));
                }
            },
        };
        let response = match response {
            Ok(resp) => resp,
            Err(e) => {
                let code = if matches!(e, ureq::Error::Timeout(_)) {
                    CallErrorCode::Timeout
                } else {
                    CallErrorCode::Network
                };
                return Err(CallError::new(code, e.to_string()));
            }
        };
        let status = response.status().as_u16();
        // The media-type guard (D5) runs BEFORE the body is read: a
        // mismatched 2xx fails `service` no matter how big the body is.
        if (200..300).contains(&status) && !self.content_types.is_empty() {
            let content_type = response
                .headers()
                .get("Content-Type")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            check_content_type(
                &self.function_name,
                content_type.as_deref(),
                &self.content_types,
            )
            .map_err(|mut err| {
                // The service answered 2xx: a write it accepted went
                // through even though its answer is not read, so the send
                // ledger must not take it for a send that never left.
                err.http_status = Some(status);
                if !self.read_only {
                    err.incomplete = Some(BodyIncomplete::refused_content_type());
                }
                err
            })?;
        }
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        let link = response
            .headers()
            .get("Link")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let (body, incomplete, bytes) = read_body(response, allowance);
        Ok(RawResponse {
            status,
            content_type,
            link,
            retry_after,
            body,
            incomplete,
            bytes,
        })
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
    fn auth_header(
        &self,
        ctx: &RenderCtx,
        access: Option<&Secret>,
    ) -> Result<Option<(String, String)>, CallError> {
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
            Some(AuthSpec::Oauth2Refresh { .. }) => {
                let token = access.ok_or_else(|| {
                    CallError::new(
                        CallErrorCode::Config,
                        "the OAuth access token was not minted before the request",
                    )
                })?;
                Ok(Some((
                    "Authorization".to_string(),
                    format!("Bearer {}", token.expose_secret()),
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
        let list = self.redactions();
        if list.is_empty() {
            return body;
        }
        redact_value(body, &list)
    }

    /// Scrubs an error message with the same interim literal redaction as the
    /// body. A transport error's URL (and thus a `query`-kind auth secret) can
    /// otherwise leak into `error.message`.
    fn redact_error(&self, mut err: CallError) -> CallError {
        err.message = redact_message(err.message, &self.redactions());
        err
    }
}
