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
    /// The refresh token the mint of this entry rotated, kept sealed only
    /// so every use of the entry (a cache hit included) registers it for
    /// redaction and carries the rotation warning: the stored refresh
    /// token is still the stale one for as long as the entry lives. Never
    /// serialized, logged or sent.
    rotated_refresh: Option<Secret>,
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
    /// The endpoint refused the refresh token itself (`invalid_grant`): the
    /// cache entry of that token is dropped, rotation included.
    grant_refused: bool,
}

impl TokenError {
    fn new(code: CallErrorCode, message: impl Into<String>) -> Self {
        TokenError {
            code,
            message: message.into(),
            grant_refused: false,
        }
    }

    /// The message scrubbed against the call's redactions (the refresh
    /// token among them), so an endpoint error text carrying request data
    /// cannot leak a credential.
    fn redacted(self, redactions: &[Redaction]) -> Self {
        TokenError {
            code: self.code,
            message: redact_message(self.message, redactions),
            grant_refused: self.grant_refused,
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

/// Whether a token request may go through a proxy the calling process
/// names (`HTTPS_PROXY` and the like, which ureq's default agent reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenProxy {
    /// ureq's default: the caller's proxy variables are honoured (the HTTP
    /// and IMAP connectors, as before).
    Caller,
    /// No proxy: an MCP connector's refresh token never goes to a proxy
    /// the calling process names.
    None,
}

/// The fresh access token of a mint, adopted by the call: the rotation
/// warning when the endpoint rotated the refresh token, the access token
/// and any rotated refresh token registered for redaction (`register`)
/// before anything is sent with them. The one code path the HTTP call's
/// first mint and its 401 refresh, and the MCP connect, share.
pub(crate) fn adopt_minted(
    minted: &Minted,
    register: &mut dyn FnMut(Secret, &'static str),
    warnings: &mut Vec<String>,
) -> Secret {
    // Once per call: a 401 refresh inherits the first mint's rotation, and
    // the listing surfaces join this vector as it is.
    if minted.rotated && !warnings.iter().any(|w| w == super::ROTATION_WARNING) {
        warnings.push(super::ROTATION_WARNING.to_string());
    }
    register(minted.access.clone(), "oauth:access_token");
    if let Some(new) = &minted.new_refresh {
        register(new.clone(), "oauth:refresh_token");
    }
    minted.access.clone()
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
    /// Whether the token request honours the caller's proxy.
    proxy: TokenProxy,
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
        proxy: TokenProxy,
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
            proxy,
        }))
    }

    /// The rendered token URL the request goes to.
    pub(crate) fn token_url(&self) -> &str {
        &self.token_url
    }

    /// The agent config of the token request: redirects off, the time left
    /// as the only budget, and no proxy for `TokenProxy::None`.
    fn agent_config(&self, remaining: Duration) -> ureq::config::Config {
        let builder = ureq::Agent::config_builder()
            .max_redirects(0)
            .timeout_global(Some(remaining))
            .allow_non_standard_methods(true)
            .http_status_as_error(false);
        match self.proxy {
            TokenProxy::Caller => builder.build(),
            TokenProxy::None => builder.proxy(None).build(),
        }
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
            // The same `Minted` a fresh mint gave: a rotated refresh token
            // is registered and warned about on every use of the entry.
            return Ok(Minted {
                access: hit.access.clone(),
                expires_in: None,
                granted: self.granted(hit.token_scope.clone()),
                rotated: hit.rotated_refresh.is_some(),
                new_refresh: hit.rotated_refresh.clone(),
            });
        }
        self.refresh_now(deadline, redactions)
    }

    /// Drops this binding's cached token and refreshes before `deadline`.
    /// A refresh the endpoint refuses `invalid_grant` drops the entry.
    pub(crate) fn refresh_now(
        &self,
        deadline: Instant,
        redactions: &[Redaction],
    ) -> Result<Minted, TokenError> {
        let key = cache_key(&self.token_url, &self.client_id, &self.refresh_token);
        let (mut minted, token_scope) = match self.refresh_request(deadline, redactions) {
            Ok(done) => done,
            Err(e) => {
                if e.grant_refused
                    && let Ok(mut map) = cache().lock()
                {
                    map.remove(&key);
                }
                return Err(e);
            }
        };
        let expires_in = minted.expires_in.unwrap_or(DEFAULT_EXPIRES_IN);
        if let Ok(mut map) = cache().lock() {
            // A refresh that did not rotate keeps the rotation an earlier
            // mint of this entry made: the stored refresh token is still
            // the stale one, so it stays registered and warned about.
            if minted.new_refresh.is_none()
                && let Some(earlier) = map.get(&key).and_then(|c| c.rotated_refresh.clone())
            {
                minted.rotated = true;
                minted.new_refresh = Some(earlier);
            }
            store(
                &mut map,
                key,
                Cached {
                    access: minted.access.clone(),
                    expires_at: Instant::now() + Duration::from_secs(expires_in),
                    token_scope: token_scope.clone(),
                    rotated_refresh: minted.new_refresh.clone(),
                },
            );
        }
        minted.granted = self.granted(token_scope);
        Ok(minted)
    }

    /// Ends this binding's cached access token without refreshing, so the
    /// next call mints a fresh one. The entry stays, expired, until the
    /// next insert: a refresh of the same token that does not rotate still
    /// carries an earlier rotation. The IMAP path calls it after an auth
    /// rejection; the HTTP path refreshes instead.
    #[allow(dead_code)]
    pub(crate) fn forget(&self) {
        let key = cache_key(&self.token_url, &self.client_id, &self.refresh_token);
        if let Ok(mut map) = cache().lock()
            && let Some(entry) = map.get_mut(&key)
        {
            entry.expires_at = Instant::now();
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
        let agent = ureq::Agent::new_with_config(self.agent_config(remaining));
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
        let refused = |code: CallErrorCode, message: String| TokenError {
            grant_refused: true,
            ..fail(code, message)
        };
        if !(200..300).contains(&status) {
            let code_name = apb_core::connector::oauth::token_error_code(&body)
                .unwrap_or_else(|| "unknown".to_string());
            return Err(match (status, code_name.as_str()) {
                (400 | 401, "invalid_grant") => refused(
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

/// Inserts `entry` under `key` and drops every other entry whose access
/// token has expired, so a long-lived process (the dashboard) keeps no
/// token, rotated refresh tokens included, past its use.
fn store(map: &mut HashMap<[u8; 32], Cached>, key: [u8; 32], entry: Cached) {
    let now = Instant::now();
    map.retain(|k, c| *k == key || c.expires_at > now);
    map.insert(key, entry);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn source(proxy: TokenProxy) -> TokenSource {
        let auth = AuthSpec::Oauth2Refresh {
            token_url: "https://auth.example/token".into(),
            authorize_url: None,
            client_id_template: "client-1".into(),
            client_secret_template: None,
            refresh_token_template: "{{secret.refresh}}".into(),
            granted_scopes_template: None,
            consent_scopes: None,
            authorize_params: BTreeMap::new(),
        };
        let secrets = BTreeMap::from([("refresh".to_string(), Secret::new("r-1"))]);
        TokenSource::render(Some(&auth), &BTreeMap::new(), &secrets, "c", "a", proxy)
            .unwrap()
            .unwrap()
    }

    /// The marker of the child process the proxy test runs its body in.
    const CHILD: &str = "APB_TOKEN_PROXY_CHILD";

    /// With `HTTPS_PROXY` and `HTTP_PROXY` set (in a child process of this
    /// test binary only), the agent config of a `TokenProxy::None` source
    /// has no proxy, and a `TokenProxy::Caller` source keeps the caller's.
    /// The child appends one line to a marker file after its assertions,
    /// so a child that matched no test (a renamed or moved test) fails the
    /// parent instead of passing it.
    #[test]
    fn a_no_proxy_source_sets_no_proxy() {
        if let Some(marker) = std::env::var_os(CHILD) {
            let none = source(TokenProxy::None).agent_config(Duration::from_secs(5));
            assert!(none.proxy().is_none(), "no proxy");
            let caller = source(TokenProxy::Caller).agent_config(Duration::from_secs(5));
            assert!(caller.proxy().is_some(), "the caller's proxy is read");
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(marker)
                .unwrap();
            writeln!(file, "ran").unwrap();
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "connector::oauth::token::tests::a_no_proxy_source_sets_no_proxy",
            ])
            .env(CHILD, &marker)
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .status()
            .unwrap();
        assert!(status.success(), "the child's body passed");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap_or_default(),
            "ran\n",
            "the child ran exactly this test, once"
        );
    }

    #[test]
    fn adopt_minted_registers_both_tokens_and_warns_on_rotation() {
        let minted = Minted {
            access: Secret::new("acc-1"),
            expires_in: None,
            granted: None,
            rotated: true,
            new_refresh: Some(Secret::new("ref-2")),
        };
        let mut seen: Vec<(String, &'static str)> = Vec::new();
        let mut warnings = Vec::new();
        let access = adopt_minted(
            &minted,
            &mut |s: Secret, label| seen.push((s.expose_secret().to_string(), label)),
            &mut warnings,
        );
        assert_eq!(access.expose_secret(), "acc-1");
        assert_eq!(
            seen,
            vec![
                ("acc-1".to_string(), "oauth:access_token"),
                ("ref-2".to_string(), "oauth:refresh_token"),
            ]
        );
        assert_eq!(warnings, vec![super::super::ROTATION_WARNING.to_string()]);
        let plain = Minted {
            rotated: false,
            new_refresh: None,
            ..minted
        };
        let mut seen = 0;
        let mut warnings = Vec::new();
        adopt_minted(&plain, &mut |_, _| seen += 1, &mut warnings);
        assert_eq!(seen, 1);
        assert!(warnings.is_empty());
    }

    /// A 401 refresh inherits the first mint's rotation: adopting both into
    /// one call's warnings says it once.
    #[test]
    fn adopting_two_rotated_mints_warns_once() {
        let rotated = || Minted {
            access: Secret::new("acc-1"),
            expires_in: None,
            granted: None,
            rotated: true,
            new_refresh: Some(Secret::new("ref-2")),
        };
        let mut warnings = Vec::new();
        adopt_minted(&rotated(), &mut |_, _| {}, &mut warnings);
        adopt_minted(&rotated(), &mut |_, _| {}, &mut warnings);
        assert_eq!(warnings, vec![super::super::ROTATION_WARNING.to_string()]);
    }

    /// A loopback token endpoint answering `answers` in order, one per
    /// connection, each `(status, json body)`. Returns its token URL.
    fn token_endpoint(answers: Vec<(u16, Value)>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for (status, body) in answers {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut sink = vec![0u8; length];
                std::io::Read::read_exact(&mut reader, &mut sink).unwrap();
                let body = body.to_string();
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (url, handle)
    }

    fn source_at(url: &str, refresh: &str) -> TokenSource {
        let auth = AuthSpec::Oauth2Refresh {
            token_url: url.into(),
            authorize_url: None,
            client_id_template: "client-1".into(),
            client_secret_template: None,
            refresh_token_template: "{{secret.refresh}}".into(),
            granted_scopes_template: None,
            consent_scopes: None,
            authorize_params: BTreeMap::new(),
        };
        let secrets = BTreeMap::from([("refresh".to_string(), Secret::new(refresh))]);
        TokenSource::render(
            Some(&auth),
            &BTreeMap::new(),
            &secrets,
            "c",
            "a",
            TokenProxy::None,
        )
        .unwrap()
        .unwrap()
    }

    /// The tests that use the process-wide cache run one at a time: an
    /// insert of one drops the expired entry another just forgot.
    static CACHE_TESTS: Mutex<()> = Mutex::new(());

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    fn cached(src: &TokenSource) -> bool {
        let key = cache_key(&src.token_url, &src.client_id, &src.refresh_token);
        cache().lock().unwrap().contains_key(&key)
    }

    /// After an IMAP auth rejection the access token is forgotten, but a
    /// later refresh of the same stale refresh token that does not rotate
    /// still carries the earlier rotation for redaction and the warning.
    #[test]
    fn forget_keeps_the_carried_rotation() {
        let _serial = CACHE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (url, server) = token_endpoint(vec![
            (
                200,
                serde_json::json!({ "access_token": "acc-f1", "refresh_token": "ref-f2", "expires_in": 3600 }),
            ),
            (
                200,
                serde_json::json!({ "access_token": "acc-f3", "expires_in": 3600 }),
            ),
        ]);
        let src = source_at(&url, "ref-f1");
        let first = src.access_token(deadline(), &[]).unwrap();
        assert!(first.rotated);
        src.forget();
        let again = src.access_token(deadline(), &[]).unwrap();
        assert_eq!(again.access.expose_secret(), "acc-f3", "a fresh mint");
        assert!(again.rotated, "the rotation is carried");
        assert_eq!(again.new_refresh.unwrap().expose_secret(), "ref-f2");
        server.join().unwrap();
    }

    /// A refresh the endpoint refuses `invalid_grant` drops the entry, and
    /// with it the sealed rotated refresh token.
    #[test]
    fn an_invalid_grant_refresh_drops_the_entry() {
        let _serial = CACHE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (url, server) = token_endpoint(vec![
            (
                200,
                serde_json::json!({ "access_token": "acc-g1", "refresh_token": "ref-g2", "expires_in": 3600 }),
            ),
            (400, serde_json::json!({ "error": "invalid_grant" })),
        ]);
        let src = source_at(&url, "ref-g1");
        src.access_token(deadline(), &[]).unwrap();
        assert!(cached(&src));
        let err = src.refresh_now(deadline(), &[]).err().unwrap();
        assert_eq!(err.code, CallErrorCode::Auth);
        assert!(!cached(&src), "the refused token's entry is gone");
        server.join().unwrap();
    }

    /// Every insert drops the other entries whose access token expired.
    #[test]
    fn an_insert_drops_expired_entries() {
        let entry = |expires_at| Cached {
            access: Secret::new("a"),
            expires_at,
            token_scope: None,
            rotated_refresh: Some(Secret::new("r")),
        };
        let now = Instant::now();
        let mut map = HashMap::new();
        map.insert([1u8; 32], entry(now));
        map.insert([2u8; 32], entry(now + Duration::from_secs(600)));
        map.insert([3u8; 32], entry(now));
        store(&mut map, [3u8; 32], entry(now + Duration::from_secs(600)));
        let mut keys: Vec<_> = map.keys().map(|k| k[0]).collect();
        keys.sort();
        assert_eq!(keys, vec![2, 3]);
    }
}
