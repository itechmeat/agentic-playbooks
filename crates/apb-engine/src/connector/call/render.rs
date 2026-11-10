//! Rendering an HTTP function's wire shape (method, pre-auth URL, query,
//! body, form body, headers) and the URL hardening (spec 6.1). A child of
//! `call` (ADP): `mod.rs`, `http.rs` and the offline contract runner import
//! it, it never imports them back (its dependencies are `encode` (this
//! directory), the send stamp (outside `call`) and apb-core).

use std::collections::BTreeMap;

use apb_core::connector::def::FunctionSpec;
use apb_core::connector::template::{
    RenderCtx, render_body, render_encoded, render_raw, resolve_optional_arg,
    single_args_placeholder,
};
use apb_core::secret::Secret;
use serde_json::Value;

use crate::connector::call::url_encode::{assemble_url, encode_args_for_url, encode_component};
use crate::connector::intent::stamp::{self, Stamp};
use crate::connector::result::{CallError, CallErrorCode};

use crate::connector::call::wire::USER_AGENT;

pub struct RenderedRequest {
    pub method: String,
    pub pre_auth_url: String,
    pub rendered_body: Option<Value>,
    /// The rendered `body_form` pairs (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.1), DECODED and in
    /// manifest (BTreeMap) order; percent-encoding happens at send time so the
    /// offline runner and the dry-run terminal compare readable values.
    /// `None` when the function declares no `body_form` (mutually exclusive
    /// with `rendered_body` by validation).
    pub rendered_form: Option<Vec<(String, String)>>,
    /// The headers that would be sent: the rendered per-function `headers` with
    /// the default `User-Agent` folded in unless a function header already names
    /// it (case-insensitively). Auth headers are NOT included here (auth is
    /// injected on the wire, after this render).
    pub headers: BTreeMap<String, String>,
    /// A `mime` function's composed message: the body path and the
    /// formatted bytes the live path base64url-encodes into the body.
    pub mime_bytes: Option<(String, Vec<u8>)>,
}

pub(crate) fn render_http(
    function: &FunctionSpec,
    account_fields: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, Secret>,
    stamp: Option<&Stamp>,
) -> Result<RenderedRequest, CallError> {
    let ctx = RenderCtx {
        account: account_fields,
        args,
        secrets,
    };
    let method = function.method.clone().unwrap_or_else(|| "GET".to_string());
    // The URL base renders with raw substitution EXCEPT that `{{args.*}}` values
    // are pre-encoded: an account field used as a URL prefix (`base_url`) is
    // trusted config that must keep its `://` (spec 6.1) and so cannot go through
    // whole-value percent-encoding, while argument values in a path segment must
    // still be encoded so they cannot inject traversal or extra query structure.
    let url_args = encode_args_for_url(args);
    let url_ctx = RenderCtx {
        account: account_fields,
        args: &url_args,
        secrets,
    };
    let base = render_raw(function.url.as_deref().unwrap_or(""), &url_ctx)
        .map_err(|e| CallError::new(CallErrorCode::Config, format!("URL render failed: {e}")))?;
    let query = render_query(function, &ctx)?;
    let pre_auth_url = assemble_url(&base, &query);
    validate_url(&pre_auth_url)?;
    // Text modes prepare the named args before they reach the body, so no
    // body template can place the raw value; a write also has forged send
    // markers neutralized and carries its own stamp.
    let body_args = stamp::prepare_body_args(function, args, stamp);
    let body_ctx = RenderCtx {
        args: &body_args,
        ..ctx
    };
    let mut rendered_body = match &function.body {
        Some(b) => Some(render_body(b, &body_ctx).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("body render failed: {e}"))
        })?),
        None => None,
    };
    let rendered_form = if function.body_form.is_empty() {
        None
    } else {
        Some(render_form(function, &body_ctx)?)
    };
    let mut headers = BTreeMap::new();
    for (name, template) in &function.headers {
        let value = render_raw(template, &ctx).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("header `{name}` render failed: {e}"),
            )
        })?;
        headers.insert(name.clone(), value);
    }
    // Default User-Agent (spec 4.4) unless a function header already names it.
    let has_ua = headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent"));
    if !has_ua {
        headers.insert("User-Agent".to_string(), USER_AGENT.to_string());
    }
    stamp::apply_key(function, stamp, &mut rendered_body, &mut headers)
        .map_err(|m| CallError::new(CallErrorCode::Config, m))?;

    // D4: a `mime` function composes its message and puts the READABLE
    // preview at the body path, in the dry-run and the contract render
    // alike. The live path replaces the preview with the base64url of the
    // formatted bytes before the call is built.
    let mut mime_bytes: Option<(String, Vec<u8>)> = None;
    if let Some(m) = &function.mime {
        let composed = crate::connector::compose::compose(
            "message",
            &m.message,
            &ctx,
            &crate::connector::compose::ComposeOpts {
                keep_bcc: true,
                message_id_domain: None,
            },
        )?;
        let Some(body) = &mut rendered_body else {
            return Err(CallError::new(
                CallErrorCode::Config,
                "mime needs a rendered object body",
            ));
        };
        if insert_at_path(body, &m.at, composed.preview.clone()).is_err() {
            return Err(CallError::new(
                CallErrorCode::Config,
                format!("mime path `{}` does not name an object on the way", m.at),
            ));
        }
        mime_bytes = Some((m.at.clone(), composed.formatted));
    }

    Ok(RenderedRequest {
        method,
        pre_auth_url,
        rendered_body,
        rendered_form,
        headers,
        mime_bytes,
    })
}

pub(crate) fn insert_at_path(body: &mut Value, path: &str, value: Value) -> Result<(), CallError> {
    let mut cur = body;
    let segs: Vec<&str> = path.split('.').collect();
    for (i, seg) in segs.iter().enumerate() {
        let last = i == segs.len() - 1;
        let Some(map) = cur.as_object_mut() else {
            return Err(CallError::new(
                CallErrorCode::Config,
                format!("mime path `{path}` walks into a non-object"),
            ));
        };
        if last {
            map.insert((*seg).to_string(), value);
            return Ok(());
        }
        cur = map
            .entry((*seg).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    Ok(())
}

fn render_form(
    function: &FunctionSpec,
    ctx: &RenderCtx,
) -> Result<Vec<(String, String)>, CallError> {
    let mut pairs = Vec::new();
    for (key, template) in &function.body_form {
        if let Some(field) = single_args_placeholder(template)
            && resolve_optional_arg(ctx, field).is_none()
        {
            continue;
        }
        let value = render_raw(template, ctx).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("body_form render failed: {e}"),
            )
        })?;
        pairs.push((key.clone(), value));
    }
    Ok(pairs)
}

fn render_query(function: &FunctionSpec, ctx: &RenderCtx) -> Result<String, CallError> {
    let mut parts = Vec::new();
    for (key, template) in &function.query {
        // spec 5.1: a query value that is exactly `{{args.field}}` drops the
        // whole pair when the arg is absent, instead of erroring. An
        // explicit null arg is treated as absent and dropped too, so a
        // literal "null" never renders into the query string. A present,
        // non-null arg (scalar or not) still goes through `render_encoded`
        // below, which already renders scalars typed (string verbatim,
        // number/bool via their JSON text) and turns a non-scalar into a
        // Config error.
        if let Some(field) = single_args_placeholder(template)
            && resolve_optional_arg(ctx, field).is_none()
        {
            continue;
        }
        let value = render_encoded(template, ctx).map_err(|e| {
            CallError::new(CallErrorCode::Config, format!("query render failed: {e}"))
        })?;
        parts.push(format!("{}={value}", encode_component(key)));
    }
    Ok(parts.join("&"))
}

pub(crate) fn validate_url(url: &str) -> Result<(), CallError> {
    let lower = url.to_ascii_lowercase();
    let after = if let Some(rest) = lower.strip_prefix("https://") {
        &url[url.len() - rest.len()..]
    } else if let Some(rest) = lower.strip_prefix("http://") {
        &url[url.len() - rest.len()..]
    } else {
        return Err(CallError::new(
            CallErrorCode::Config,
            format!("rendered URL is not an absolute http(s) URL: `{url}`"),
        ));
    };
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    if after[..authority_end].contains('@') {
        return Err(CallError::new(
            CallErrorCode::Config,
            "rendered URL must not contain userinfo (user:pass@host)".to_string(),
        ));
    }
    Ok(())
}
