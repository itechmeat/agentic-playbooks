//! The in-process token source of an `oauth2_refresh` auth block: it mints a
//! bearer access token from a refresh token at call time, caches the minted
//! token in process memory only, refreshes before expiry and once after a
//! 401, and maps every token-endpoint failure into the call taxonomy
//! without ever carrying the endpoint's HTTP status.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use apb_core::connector::def::{AuthSpec, sole_secret};
use apb_core::connector::template::{RenderCtx, render_raw};
use apb_core::secret::{Redaction, Secret};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::connector::result::{CallError, CallErrorCode, redact_message};

/// How long before expiry a cached token counts as fresh: the leeway
/// covers clock drift and requests already in flight.
const EXPIRY_LEEWAY: Duration = Duration::from_secs(60);

/// The `expires_in` used when the token answer carries none.
const DEFAULT_EXPIRES_IN: u64 = 3600;

/// One cached access token, in process memory only. Nothing about it is
/// written anywhere.
struct Cached {
    access: Secret,
    expires_at: Instant,
    /// The token answer's own `scope`, as the endpoint said it. Accounts
    /// sharing this entry apply their own declared set over it on read, so
    /// one account's declaration never stands in for another's.
    token_scope: Option<BTreeSet<String>>,
}

static TOKENS: OnceLock<Mutex<HashMap<[u8; 32], Cached>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<[u8; 32], Cached>> {
    TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The cache key: SHA-256 of token_url, a zero byte, client_id, a zero
/// byte, the refresh token's bytes. Two accounts with different refresh
/// tokens never share an entry.
fn cache_key(token_url: &str, client_id: &str, refresh: &Secret) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(token_url.as_bytes());
    input.push(0);
    input.extend_from_slice(client_id.as_bytes());
    input.push(0);
    input.extend_from_slice(refresh.expose_secret().as_bytes());
    Sha256::digest(&input).into()
}

/// A token-endpoint failure. It never carries the endpoint's HTTP status: a
/// token status is not a service status and must never reach
/// `CallError.http_status`, `EventMeta.http_status` or the send ledger.
#[derive(Debug)]
pub(crate) struct TokenError {
    pub code: CallErrorCode,
    pub message: String,
}

impl TokenError {
    fn new(code: CallErrorCode, message: impl Into<String>) -> Self {
        TokenError {
            code,
            message: message.into(),
        }
    }

    /// The message scrubbed against the call's redactions (the refresh
    /// token among them), so an endpoint error text carrying request data
    /// cannot leak a credential.
    fn redacted(self, redactions: &[Redaction]) -> Self {
        TokenError {
            code: self.code,
            message: redact_message(self.message, redactions),
        }
    }
}

impl From<TokenError> for CallError {
    fn from(e: TokenError) -> Self {
        let mut err = CallError::new(e.code, e.message);
        err.http_status = None;
        err
    }
}

/// A successful mint.
pub(crate) struct Minted {
    pub access: Secret,
    /// The answer's `expires_in`, for the cache.
    pub expires_in: Option<u64>,
    /// The granted scopes: the account's declared set when it says one,
    /// else the token answer's `scope`. `None` when neither says anything,
    /// which means no scope check.
    pub granted: Option<BTreeSet<String>>,
    /// The endpoint returned another refresh token. apb does not store it.
    pub rotated: bool,
    /// The rotated refresh token, only so the call can redact it.
    pub new_refresh: Option<Secret>,
}

/// One rendered `oauth2_refresh` binding for one call.
#[derive(Debug)]
pub(crate) struct TokenSource {
    /// The connector and account names, for the failure messages.
    connector: String,
    account: String,
    token_url: String,
    client_id: String,
    client_secret: Option<Secret>,
    refresh_token: Secret,
    granted_from_account: Option<BTreeSet<String>>,
}

impl TokenSource {
    /// Renders the auth block against the account fields and the resolved
    /// secrets. `Ok(None)` for every other auth kind. `Err` is a `config`
    /// CallError naming the field.
    pub(crate) fn render(
        auth: Option<&AuthSpec>,
        account: &BTreeMap<String, String>,
        secrets: &BTreeMap<String, Secret>,
        connector: &str,
        account_name: &str,
    ) -> Result<Option<Self>, CallError> {
        let Some(AuthSpec::Oauth2Refresh {
            token_url,
            client_id_template,
            client_secret_template,
            refresh_token_template,
            granted_scopes_template,
            ..
        }) = auth
        else {
            return Ok(None);
        };
        let ctx = RenderCtx {
            account,
            args: &Value::Null,
            secrets,
        };
        let render = |label: &str, t: &str| {
            render_raw(t, &ctx).map_err(|e| {
                CallError::new(
                    CallErrorCode::Config,
                    format!("oauth `{label}` render failed: {e}"),
                )
            })
        };
        let token_url = render("token_url", token_url)?;
        check_token_url(&token_url)?;
        let client_id = render("client_id_template", client_id_template)?;
        let client_secret = client_secret_template
            .as_deref()
            .and_then(sole_secret)
            .map(|field| {
                secrets.get(field).cloned().ok_or_else(|| {
                    CallError::new(
                        CallErrorCode::Config,
                        format!("oauth client secret field `{field}` is not resolved"),
                    )
                })
            })
            .transpose()?;
        let refresh_field = sole_secret(refresh_token_template).ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                "oauth `refresh_token_template` must be one `{{secret.*}}` placeholder",
            )
        })?;
        let refresh_token = secrets.get(refresh_field).cloned().ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("oauth refresh token field `{refresh_field}` is not resolved"),
            )
        })?;
        // Lenient: an absent account field means "not said", so a scope
        // template that is exactly one absent `{{account.x}}` renders to no
        // declared set at all.
        let granted_from_account = granted_scopes_template
            .as_deref()
            .and_then(|t| match sole_account_field(t) {
                Some(field) => account.get(&field).cloned(),
                None => Some(render("granted_scopes_template", t).ok()?).filter(|s| !s.is_empty()),
            })
            .map(|s| apb_core::connector::oauth::scope_set(&s));
        Ok(Some(TokenSource {
            connector: connector.to_string(),
            account: account_name.to_string(),
            token_url,
            client_id,
            client_secret,
            refresh_token,
            granted_from_account,
        }))
    }

    /// The granted set the account itself declares, when it says one: the
    /// check a call makes before anything is minted.
    pub(crate) fn declared_scopes(&self) -> Option<&BTreeSet<String>> {
        self.granted_from_account.as_ref()
    }

    /// A cached access token with more than 60 s left, else a refresh
    /// bounded by what is left of `deadline`.
    pub(crate) fn access_token(
        &self,
        deadline: Instant,
        redactions: &[Redaction],
    ) -> Result<Minted, TokenError> {
        let key = cache_key(&self.token_url, &self.client_id, &self.refresh_token);
        if let Ok(map) = cache().lock()
            && let Some(hit) = map.get(&key)
            && hit.expires_at > Instant::now() + EXPIRY_LEEWAY
        {
            return Ok(Minted {
                access: hit.access.clone(),
                expires_in: None,
                granted: self.granted(hit.token_scope.clone()),
                rotated: false,
                new_refresh: None,
            });
        }
        self.refresh_now(deadline, redactions)
    }

    /// Drops this binding's cached token and refreshes before `deadline`.
    pub(crate) fn refresh_now(
        &self,
        deadline: Instant,
        redactions: &[Redaction],
    ) -> Result<Minted, TokenError> {
        let (mut minted, token_scope) = self.refresh_request(deadline, redactions)?;
        let key = cache_key(&self.token_url, &self.client_id, &self.refresh_token);
        let expires_in = minted.expires_in.unwrap_or(DEFAULT_EXPIRES_IN);
        if let Ok(mut map) = cache().lock() {
            map.insert(
                key,
                Cached {
                    access: minted.access.clone(),
                    expires_at: Instant::now() + Duration::from_secs(expires_in),
                    token_scope: token_scope.clone(),
                },
            );
        }
        minted.granted = self.granted(token_scope);
        Ok(minted)
    }

    /// Drops this binding's cached token without refreshing. The IMAP path
    /// calls it after an auth rejection; the HTTP path refreshes instead.
    #[allow(dead_code)]
    pub(crate) fn forget(&self) {
        let key = cache_key(&self.token_url, &self.client_id, &self.refresh_token);
        if let Ok(mut map) = cache().lock() {
            map.remove(&key);
        }
    }

    /// The granted set an answer carries: the account's declared set when
    /// it says one, else the token's `scope`.
    fn granted(&self, token_scope: Option<BTreeSet<String>>) -> Option<BTreeSet<String>> {
        match &self.granted_from_account {
            Some(set) => Some(set.clone()),
            None => token_scope,
        }
    }

    /// The refresh request: POST of the form body, redirects off, the
    /// deadline as the only budget. A passed deadline fails `timeout`
    /// without a request. Returns the mint (its `granted` still unset) and
    /// the answer's own scope set.
    fn refresh_request(
        &self,
        deadline: Instant,
        redactions: &[Redaction],
    ) -> Result<(Minted, Option<BTreeSet<String>>), TokenError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TokenError::new(
                CallErrorCode::Timeout,
                "the call's deadline passed before the token request",
            ));
        }
        let config = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(remaining))
            .allow_non_standard_methods(true)
            .http_status_as_error(false)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut pairs = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            (
                "refresh_token".to_string(),
                self.refresh_token.expose_secret().to_string(),
            ),
            ("client_id".to_string(), self.client_id.clone()),
        ];
        if let Some(cs) = &self.client_secret {
            pairs.push(("client_secret".to_string(), cs.expose_secret().to_string()));
        }
        let request = ureq::http::Request::builder()
            .method("POST")
            .uri(&self.token_url)
            .header("Accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(encode_form_body(&pairs).into_bytes())
            .map_err(|e| {
                TokenError::new(
                    CallErrorCode::Config,
                    format!("token request build failed: {e}"),
                )
            })?;
        let response = match agent.run(request) {
            Ok(resp) => resp,
            Err(e) => {
                let code = if matches!(e, ureq::Error::Timeout(_)) {
                    CallErrorCode::Timeout
                } else {
                    CallErrorCode::Network
                };
                return Err(TokenError::new(code, e.to_string()).redacted(redactions));
            }
        };
        let status = response.status().as_u16();
        let (body, _incomplete, _bytes) = read_token_body(response);
        let fail = |code: CallErrorCode, message: String| {
            TokenError::new(code, message).redacted(redactions)
        };
        if !(200..300).contains(&status) {
            let code_name = apb_core::connector::oauth::token_error_code(&body)
                .unwrap_or_else(|| "unknown".to_string());
            return Err(match (status, code_name.as_str()) {
                (400 | 401, "invalid_grant") => fail(
                    CallErrorCode::Auth,
                    format!(
                        "the OAuth refresh token of account `{a}` was refused (invalid_grant): it was revoked or has expired; run `apb connector oauth {c} --account {a}` and store the new refresh token",
                        a = self.account,
                        c = self.connector,
                    ),
                ),
                (400 | 401, "invalid_client") => fail(
                    CallErrorCode::Auth,
                    format!(
                        "the OAuth client credentials of account `{a}` were refused (invalid_client): check `client_id` and `client_secret`; run `apb connector oauth {c} --account {a}` again after fixing them",
                        a = self.account,
                        c = self.connector,
                    ),
                ),
                (400 | 401, other) => fail(
                    CallErrorCode::Auth,
                    format!(
                        "the token endpoint refused the refresh of account `{a}` ({other}); run `apb connector oauth {c} --account {a}` and store the new refresh token",
                        a = self.account,
                        c = self.connector,
                    ),
                ),
                (_, _) => fail(
                    CallErrorCode::Service,
                    "the token endpoint failed; the token status is not a service status"
                        .to_string(),
                ),
            });
        }
        let grant = apb_core::connector::oauth::parse_token_response(&body).map_err(|e| {
            fail(
                CallErrorCode::Service,
                format!("the token endpoint answered without a usable token: {e}"),
            )
        })?;
        let rotated = grant
            .refresh_token
            .as_ref()
            .map(|new| !new.ct_eq(self.refresh_token.expose_secret()))
            .unwrap_or(false);
        let token_scope = grant
            .scope
            .as_deref()
            .map(apb_core::connector::oauth::scope_set);
        Ok((
            Minted {
                access: grant.access_token,
                expires_in: grant.expires_in,
                granted: None,
                rotated,
                new_refresh: if rotated { grant.refresh_token } else { None },
            },
            token_scope,
        ))
    }
}

/// The one `{{account.x}}` placeholder of `template` when the whole
/// template is exactly that placeholder.
fn sole_account_field(template: &str) -> Option<String> {
    let inner = template.strip_prefix("{{account.")?.strip_suffix("}}")?;
    Some(inner.to_string())
}

/// Reads a token endpoint answer (1 MiB cap): a JSON content type parses,
/// anything else is lossy text. Local to this leaf module (ADP): the call
/// path's `response::read_body` pulls the whole call tree in, and a token
/// answer needs only this much (the exchange contract is one JSON object).
fn read_token_body(mut response: ureq::http::Response<ureq::Body>) -> (Value, Option<()>, usize) {
    const BODY_CAP: usize = 1024 * 1024;
    let is_json = response.body().mime_type() == Some("application/json");
    let mut buf = Vec::new();
    let read = response
        .body_mut()
        .as_reader()
        .take(BODY_CAP as u64 + 1)
        .read_to_end(&mut buf);
    let bytes = buf.len();
    if read.is_err() || bytes > BODY_CAP {
        buf.truncate(BODY_CAP);
        let body = if is_json {
            Value::Null
        } else {
            Value::String(String::from_utf8_lossy(&buf).into_owned())
        };
        return (body, Some(()), bytes);
    }
    let body = if is_json {
        serde_json::from_slice(&buf)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&buf).into_owned()))
    } else {
        Value::String(String::from_utf8_lossy(&buf).into_owned())
    };
    (body, None, bytes)
}

/// The form body of a token request: percent-encoded `k=v` pairs joined
/// with `&`, the same component set (RFC 3986 unreserved plus `-._~`) the
/// query renderer uses, so a provider sees one encoding everywhere (D1's
/// refresh request). Local to this leaf module (ADP): `call/encode.rs`
/// pulls the whole HTTP call tree in.
fn encode_form_body(pairs: &[(String, String)]) -> String {
    use apb_core::url_component::encode;
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The absolute-http(s)-without-userinfo half of the URL hardening (spec
/// 6.1), local to this leaf module (ADP): the call path's `validate_url`
/// pulls the whole call tree in. The https-or-loopback rule (D1) stays in
/// [`check_token_url`].
fn validate_absolute_url(url: &str) -> Result<(), CallError> {
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

/// A token URL must be an absolute http(s) URL without userinfo, and https
/// unless it points at the loopback (the test servers).
pub(super) fn check_token_url(url: &str) -> Result<(), CallError> {
    validate_absolute_url(url)?;
    let lower = url.to_ascii_lowercase();
    let https = lower.starts_with("https://");
    let loopback = lower
        .strip_prefix("http://")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .map(|authority| {
            let host = authority
                .rsplit_once(':')
                .map(|(h, _)| h)
                .unwrap_or(authority);
            host == "127.0.0.1" || host == "localhost"
        })
        .unwrap_or(false);
    if https || loopback {
        Ok(())
    } else {
        Err(CallError::new(
            CallErrorCode::Config,
            "token_url must be https",
        ))
    }
}
