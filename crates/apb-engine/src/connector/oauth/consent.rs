//! The loopback consent flow behind `apb connector oauth` (D1): a person's
//! terminal opens the provider's authorization URL, the provider calls back
//! on 127.0.0.1 with the code, apb exchanges it for a refresh token and
//! prints it once. Nothing is written to disk and nothing is journaled; the
//! only secret resolved here is the client secret the auth block names.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::connector::def::AuthSpec;
use apb_core::connector::template::RenderCtx;
use apb_core::connector::{oauth, sole_secret};
use apb_core::secret::{Secret, ct_eq_str};
use serde_json::Value;

use crate::connector::call::LiveTarget;
use crate::connector::call::auth::resolve_named_secrets;
use crate::connector::call::{CallError, CallErrorCode};
use crate::connector::result::redact_message;

/// Everything the flow needs, rendered against one trusted account.
pub struct ConsentPlan {
    pub connector: String,
    pub account: String,
    /// The provider's authorization endpoint, rendered.
    pub authorize_url: String,
    /// The token endpoint, rendered.
    pub token_url: String,
    /// The rendered client id (not a secret).
    pub client_id: String,
    pub client_secret: Option<Secret>,
    /// The scopes the flow asks for.
    pub scopes: Vec<String>,
    /// Extra authorization-URL parameters, rendered (an absent `account.*`
    /// drops the pair).
    pub authorize_params: BTreeMap<String, String>,
    /// The account field the printed refresh token belongs in, for the
    /// CLI's storage hint.
    pub refresh_field_hint: String,
}

/// Loads the installed connector and the account (single or default when
/// `None`), requires connector and account trust, renders the auth block,
/// and resolves only the secret fields `client_secret_template` names. The
/// refresh token field is never resolved: on a first consent its
/// `{{env.VAR}}` is usually still empty, and a `{{cmd:...}}` helper must
/// not run.
pub fn plan(
    root: &Path,
    connector: &str,
    account: Option<&str>,
    scopes: &[String],
) -> Result<ConsentPlan, CallError> {
    let loaded = apb_core::connector::store::load(connector).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}`: {e}"),
        )
    })?;
    let healthcheck = loaded.doc.healthcheck.clone().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}` declares no healthcheck"),
        )
    })?;
    let live = LiveTarget::load(root, connector, account, &healthcheck)?;
    live.require_trust()?;
    let target = live.into_target()?;

    let Some(AuthSpec::Oauth2Refresh {
        token_url,
        authorize_url: Some(authorize_url),
        client_id_template,
        client_secret_template,
        refresh_token_template,
        consent_scopes,
        authorize_params,
        ..
    }) = target.doc.auth.as_ref()
    else {
        return Err(CallError::new(
            CallErrorCode::Config,
            format!(
                "connector `{connector}` declares no `oauth2_refresh` auth block with an `authorize_url`; `apb connector oauth` cannot run"
            ),
        ));
    };
    let ctx = RenderCtx {
        account: &target.maccount.fields,
        args: &Value::Null,
        secrets: &BTreeMap::new(),
    };
    let render = |label: &str, template: &str| {
        apb_core::connector::template::render_raw(template, &ctx).map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("oauth `{label}` render failed: {e}"),
            )
        })
    };
    let token_url = render("token_url", token_url.as_str())?;
    // The same rule as the refresh path: the exchange carries the client
    // secret, the code and the verifier, so plain http only on the loopback.
    super::token::check_token_url(&token_url)?;
    let authorize_base = render("authorize_url", authorize_url.as_str())?;
    let client_id = render("client_id_template", client_id_template.as_str())?;
    // Extra authorization parameters: a value that is exactly one absent
    // `{{account.x}}` drops the pair.
    let mut rendered_params = BTreeMap::new();
    for (k, v) in authorize_params {
        if let Some(field) = v
            .strip_prefix("{{account.")
            .and_then(|r| r.strip_suffix("}}"))
            && !target.maccount.fields.contains_key(field)
        {
            continue;
        }
        rendered_params.insert(k.clone(), render(k.as_str(), v.as_str())?);
    }
    // The scopes: the caller's `--scope` values, else `consent_scopes`
    // rendered and split; none at all is an error naming `--scope`.
    let scopes = if !scopes.is_empty() {
        scopes.to_vec()
    } else {
        match consent_scopes {
            Some(template) => {
                let rendered = render("consent_scopes", template.as_str())?;
                rendered
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            }
            None => {
                return Err(CallError::new(
                    CallErrorCode::InvalidArgs,
                    "the connector declares no `consent_scopes`; pass at least one `--scope`",
                ));
            }
        }
    };
    // Only the client secret is resolved.
    let client_secret = match client_secret_template.as_deref().and_then(sole_secret) {
        Some(field) => {
            let mut resolved = resolve_named_secrets(root, &target.maccount, &[field])?;
            resolved.remove(field)
        }
        None => None,
    };
    let refresh_field_hint = sole_secret(refresh_token_template.as_str())
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                "the auth block's `refresh_token_template` must be one `{{secret.*}}` placeholder",
            )
        })?
        .to_string();
    Ok(ConsentPlan {
        connector: connector.to_string(),
        account: target.account.clone(),
        authorize_url: authorize_base,
        token_url,
        client_id,
        client_secret,
        scopes,
        authorize_params: rendered_params,
        refresh_field_hint,
    })
}

/// A flow waiting for the person's browser to come back.
pub struct Pending {
    /// The URL the person opens.
    pub url: String,
    listener: TcpListener,
    state: String,
    verifier: String,
    redirect_uri: String,
}

/// Binds 127.0.0.1:0, makes the verifier, challenge and state, and returns
/// the authorization URL.
pub fn start(plan: &ConsentPlan) -> std::io::Result<Pending> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/");
    let verifier = oauth::new_verifier();
    let state = oauth::new_state();
    let challenge = oauth::pkce_challenge(&verifier);
    let url = oauth::authorize_url(
        &plan.authorize_url,
        &oauth::AuthorizeParams {
            client_id: &plan.client_id,
            redirect_uri: &redirect_uri,
            scopes: &plan.scopes.join(" "),
            state: &state,
            challenge: &challenge,
            extras: &plan.authorize_params,
        },
    );
    listener.set_nonblocking(true)?;
    Ok(Pending {
        url,
        listener,
        state,
        verifier,
        redirect_uri,
    })
}

/// What a finished consent carries: the refresh token, printed once, and
/// the scopes the provider says it granted.
#[derive(Debug)]
pub struct ConsentResult {
    pub refresh_token: Secret,
    pub granted: Option<String>,
}

/// Waits for the one callback, then exchanges the code. Requests to any
/// other path get 404 and the wait goes on. One deadline bounds accepting,
/// reading (at most 8 KiB of request line and headers per connection) and
/// the code exchange; a client that never completes its request cannot
/// hold the flow past it.
pub fn finish(
    plan: &ConsentPlan,
    mut pending: Pending,
    timeout: Duration,
) -> Result<ConsentResult, CallError> {
    let deadline = Instant::now() + timeout;
    let code = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CallError::new(
                CallErrorCode::Timeout,
                "the consent flow ran past its timeout without a callback",
            ));
        }
        let stream = match pending.listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(e) => {
                return Err(CallError::new(
                    CallErrorCode::Network,
                    format!("the consent listener failed: {e}"),
                ));
            }
        };
        pending_listener_nonblocking(&mut pending);
        let mut stream = stream;
        let _ = stream.set_nonblocking(false);
        let target = match read_request_target(&stream, deadline) {
            Some(target) => target,
            // An unfinished or oversized request: close it and go on
            // waiting.
            None => continue,
        };
        // The request line is `GET <target> HTTP/1.1`; the target is the
        // second token.
        let path = target.split(' ').nth(1).unwrap_or("/").to_string();
        let cb = oauth::parse_callback(&path);
        if path.starts_with("/?") || cb.code.is_some() || cb.state.is_some() || cb.error.is_some() {
            // A callback-shaped target (or the root): decide the flow.
            if cb.duplicate {
                return Err(CallError::new(
                    CallErrorCode::InvalidArgs,
                    "the callback names code, state or error more than once; the flow ends here",
                ));
            }
            if let Some(err) = cb.error {
                return Err(CallError::new(
                    CallErrorCode::Permission,
                    format!("the consent was refused by the provider: {err}"),
                ));
            }
            let Some(code) = cb.code else {
                return Err(CallError::new(
                    CallErrorCode::InvalidArgs,
                    "the callback carries no code; the flow ends here",
                ));
            };
            let Some(state) = cb.state else {
                return Err(CallError::new(
                    CallErrorCode::Permission,
                    "the callback carries no state; the flow ends here",
                ));
            };
            if !ct_eq_str(&state, &pending.state) {
                return Err(CallError::new(
                    CallErrorCode::Permission,
                    "the callback's state does not match the one the flow sent; no code was exchanged",
                ));
            }
            answer_ok(&mut stream);
            break code;
        }
        // Any other path (a browser's favicon, a scanner): 404, keep
        // waiting.
        answer_404(&mut stream);
    };
    // The exchange, bounded by the same deadline.
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(CallError::new(
            CallErrorCode::Timeout,
            "the consent flow ran past its timeout before the exchange",
        ));
    }
    let grant = exchange(
        plan,
        &code,
        &pending.redirect_uri,
        &pending.verifier,
        remaining,
    )?;
    let Some(refresh_token) = grant.refresh_token else {
        return Err(CallError::new(
            CallErrorCode::Auth,
            "the provider returned no refresh token; revoke the app's access in your account settings and run again (the request already asks for prompt=consent when the connector sets it)",
        ));
    };
    Ok(ConsentResult {
        refresh_token,
        granted: grant.scope,
    })
}

/// The token exchange of the authorization-code grant, bounded by
/// `remaining`. Failures map like the refresh request's do, through the
/// redactions built from the credentials this flow holds.
fn exchange(
    plan: &ConsentPlan,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
    remaining: Duration,
) -> Result<oauth::TokenGrant, CallError> {
    let redactions = {
        let mut list = Vec::new();
        if let Some(cs) = &plan.client_secret {
            list.push(apb_core::secret::Redaction::new(
                cs.clone(),
                "oauth:client_secret",
            ));
        }
        list
    };
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(remaining))
        .allow_non_standard_methods(true)
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut pairs = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("client_id".to_string(), plan.client_id.clone()),
    ];
    if let Some(cs) = &plan.client_secret {
        pairs.push(("client_secret".to_string(), cs.expose_secret().to_string()));
    }
    pairs.push(("code_verifier".to_string(), verifier.to_string()));
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(plan.token_url.as_str())
        .header("Accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(crate::connector::call::encode::encode_form_body(&pairs).into_bytes())
        .map_err(|e| {
            CallError::new(
                CallErrorCode::Config,
                format!("the token exchange could not be built: {e}"),
            )
        })?;
    let response = agent.run(request).map_err(|e| {
        let call_code = if matches!(e, ureq::Error::Timeout(_)) {
            CallErrorCode::Timeout
        } else {
            CallErrorCode::Network
        };
        CallError::new(call_code, e.to_string())
    })?;
    let status = response.status().as_u16();
    let (body, _incomplete, _bytes) =
        crate::connector::call::response::read_body(response, 1024 * 1024);
    if !(200..300).contains(&status) {
        let code_name = oauth::token_error_code(&body).unwrap_or_else(|| "unknown".to_string());
        let mut err = CallError::new(
            CallErrorCode::Auth,
            format!("the token endpoint refused the exchange ({code_name})"),
        );
        err.message = redact_message(err.message, &redactions);
        return Err(err);
    }
    oauth::parse_token_response(&body)
        .map_err(|e| {
            CallError::new(
                CallErrorCode::Service,
                format!("the token endpoint answered without a usable token: {e}"),
            )
        })
        .map_err(|mut e| {
            e.message = redact_message(e.message, &redactions);
            e
        })
}

/// A stream whose every read is bounded by one absolute deadline: a
/// client that keeps sending cannot stretch the wait, as a socket timeout
/// alone (which bounds only the silence between reads) would let it.
struct DeadlineReader<'a> {
    stream: &'a std::net::TcpStream,
    deadline: Instant,
}

impl std::io::Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(remaining))?;
        std::io::Read::read(&mut self.stream, buf)
    }
}

/// Reads one request head (request line plus headers) capped at 8 KiB and
/// bounded by `deadline`: no read asks for more than the bytes left under
/// the cap, and none waits past the deadline. `None` for an unfinished or
/// oversized request: the caller closes the connection and goes on waiting.
fn read_request_target(stream: &std::net::TcpStream, deadline: Instant) -> Option<String> {
    const HEAD_CAP: usize = 8 * 1024;
    let mut reader = std::io::Read::take(
        BufReader::new(DeadlineReader { stream, deadline }),
        HEAD_CAP as u64 + 1,
    );
    let mut first = String::new();
    if reader.read_line(&mut first).ok()? == 0 {
        return None;
    }
    let mut total = first.len();
    if total > HEAD_CAP {
        return None;
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(n) => {
                total += n;
                if total > HEAD_CAP {
                    return None;
                }
                if line == "\r\n" || line == "\n" {
                    return Some(first.trim_end().to_string());
                }
            }
            Err(_) => return None,
        }
    }
}

/// The fixed HTML page the browser sees after the callback.
fn answer_ok(stream: &mut std::net::TcpStream) {
    let body = "<html><body><p>apb got the code; you can close this window.</p></body></html>";
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// A 404 for any path that is not the callback.
fn answer_404(stream: &mut std::net::TcpStream) {
    let head = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.flush();
}

/// The listener stays non-blocking for the next accept.
fn pending_listener_nonblocking(pending: &mut Pending) {
    let _ = pending.listener.set_nonblocking(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    use serde_json::json;

    /// APB_CONFIG_DIR is process state: the env-dependent tests serialize
    /// on this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    use std::net::{SocketAddr, TcpStream};
    use std::sync::{Arc, Mutex, MutexGuard};

    const CONNECTOR: &str = "consent-conn";

    /// A tiny sequential HTTP stub for the token endpoint: one canned
    /// answer per connection, every request recorded.
    fn stub_server(answers: Vec<(u16, String)>) -> (String, u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_thread = seen.clone();
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                };
                let mut buf = String::new();
                let mut reader = BufReader::new(&stream);
                let mut content_length = 0usize;
                loop {
                    buf.clear();
                    match reader.read_line(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let lower = buf.to_ascii_lowercase();
                            if let Some(rest) = lower.strip_prefix("content-length:") {
                                content_length = rest.trim().parse().unwrap_or(0);
                            }
                            if buf == "\r\n" || buf == "\n" {
                                break;
                            }
                            seen_thread.lock().unwrap().push(buf.clone());
                        }
                    }
                }
                if content_length > 0 {
                    let mut body = vec![0u8; content_length];
                    if reader.read_exact(&mut body).is_ok() {
                        seen_thread
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&body).into_owned());
                    }
                }
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = (&stream).write_all(head.as_bytes());
                let _ = (&stream).write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), addr.port(), seen)
    }

    /// One loopback GET with `target`, returning the whole answer.
    fn get(addr: SocketAddr, target: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .unwrap();
        let mut out = String::new();
        let _ = BufReader::new(&mut s).read_to_string(&mut out);
        out
    }

    /// The consent connector installed under `cfg`, approved, with the
    /// account config written under `root`; the token endpoint points at
    /// `token_base`.
    fn setup(
        token_base: &str,
        refresh_field_value: &str,
    ) -> (
        MutexGuard<'static, ()>,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        let dir = cfg.path().join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dir).unwrap();
        let yaml = format!(
            r#"
name: {CONNECTOR}
version: 0.1.0
healthcheck: probe
auth:
  kind: oauth2_refresh
  token_url: "{{{{account.token_url}}}}"
  authorize_url: "https://accounts.example.com/o/oauth2/v2/auth"
  client_id_template: "{{{{account.client_id}}}}"
  refresh_token_template: "{{{{secret.refresh_token}}}}"
  consent_scopes: "scope.a scope.b"
account_fields:
  - {{ name: api_base, required: true }}
  - {{ name: token_url, required: true }}
  - {{ name: client_id, required: true }}
  - {{ name: refresh_token, required: true, secret: true }}
functions:
  - name: probe
    description: d
    read_only: true
    mock: {{ status: 200, body: {{ ok: true }} }}
"#
        );
        std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
        // A `{{cmd:...}}` account may not live in a project config; the
        // helper-backed one is written to the global connector config.
        let acct_path = if refresh_field_value.starts_with("{{cmd:") {
            cfg.path()
                .join("connector-config")
                .join(format!("{CONNECTOR}.yaml"))
        } else {
            root.path()
                .join(".apb/connector-config")
                .join(format!("{CONNECTOR}.yaml"))
        };
        std::fs::create_dir_all(acct_path.parent().unwrap()).unwrap();
        let account_yaml = format!(
            r#"
accounts:
  - name: main
    default: true
    api_base: http://api.example
    token_url: {token_base}/token
    client_id: client-id-1
    refresh_token: "{refresh_field_value}"
"#
        );
        std::fs::write(acct_path, account_yaml).unwrap();
        use apb_core::connector::{config, store};
        use apb_core::trust::{Kind, OriginKind, TrustStore};
        let loaded = store::load(CONNECTOR).unwrap();
        let mut trust = TrustStore::load();
        trust
            .approve_kind(
                &loaded.digest,
                CONNECTOR,
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        let accounts = config::load_merged(root.path(), CONNECTOR).unwrap();
        let acct = accounts.iter().find(|a| a.name == "main").unwrap();
        let digest = config::account_digest(acct);
        trust
            .approve_kind(
                &digest,
                &format!("{CONNECTOR}/main"),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        (guard, cfg, root)
    }

    fn plan_of(root: &Path) -> ConsentPlan {
        plan(root, CONNECTOR, None, &[]).unwrap()
    }

    #[test]
    fn finish_exchanges_the_code_and_returns_the_refresh_token() {
        let (token_base, _port, seen) = stub_server(vec![(
            200,
            json!({ "access_token": "at", "refresh_token": "rt-new" }).to_string(),
        )]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        // The state the URL carries is the one the callback must return.
        let state = state_of(&pending.url);
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let t = std::thread::spawn(move || get(addr, &format!("/?code=c1&state={state}")));
        let result = finish(&p, pending, Duration::from_secs(5)).unwrap();
        t.join().unwrap();
        assert_eq!(result.refresh_token.expose_secret(), "rt-new");
        let body = seen.lock().unwrap().join("");
        assert!(body.contains("grant_type=authorization_code"), "{body}");
        assert!(body.contains("code=c1"), "{body}");
        assert!(
            body.contains("redirect_uri=http%3A%2F%2F127.0.0.1"),
            "{body}"
        );
        assert!(body.contains("code_verifier="), "{body}");
        assert!(body.contains("client_id=client-id-1"), "{body}");
    }

    /// The `state` parameter of an authorization URL.
    fn state_of(url: &str) -> String {
        url.split('&')
            .find_map(|p| p.strip_prefix("state="))
            .unwrap_or_default()
            .to_string()
    }

    /// The port the flow listens on.
    fn pending_port(pending: &Pending) -> u16 {
        pending.listener.local_addr().unwrap().port()
    }

    #[test]
    fn a_wrong_state_fails_and_makes_no_exchange() {
        let (token_base, _port, seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let t = std::thread::spawn(move || {
            get(addr, "/?code=c1&state=wrong");
        });
        let err = finish(&p, pending, Duration::from_secs(5)).unwrap_err();
        t.join().unwrap();
        assert!(err.message.contains("state"), "{}", err.message);
        assert!(seen.lock().unwrap().is_empty(), "the exchange ran anyway");
    }

    #[test]
    fn a_favicon_request_gets_404_and_the_wait_goes_on() {
        let (token_base, _port, seen) = stub_server(vec![(
            200,
            json!({ "access_token": "at", "refresh_token": "rt" }).to_string(),
        )]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let state = state_of(&pending.url);
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let t = std::thread::spawn(move || {
            // The browser's favicon hit first: a 404, and the flow keeps
            // waiting.
            let answer = get(addr, "/favicon.ico");
            assert!(answer.starts_with("HTTP/1.1 404"), "{}", answer);
            get(addr, &format!("/?code=c1&state={state}"))
        });
        let result = finish(&p, pending, Duration::from_secs(5)).unwrap();
        t.join().unwrap();
        assert_eq!(result.refresh_token.expose_secret(), "rt");
        assert!(!seen.lock().unwrap().is_empty());
    }

    #[test]
    fn an_error_answer_ends_the_flow() {
        let (token_base, _port, seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let state = state_of(&pending.url);
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let t =
            std::thread::spawn(move || get(addr, &format!("/?error=access_denied&state={state}")));
        let err = finish(&p, pending, Duration::from_secs(5)).unwrap_err();
        t.join().unwrap();
        assert!(err.message.contains("access_denied"), "{}", err.message);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn no_refresh_token_in_the_answer_is_the_d1_error() {
        let (token_base, _port, _seen) =
            stub_server(vec![(200, json!({ "access_token": "at" }).to_string())]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let state = state_of(&pending.url);
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let t = std::thread::spawn(move || get(addr, &format!("/?code=c1&state={state}")));
        let err = finish(&p, pending, Duration::from_secs(5)).unwrap_err();
        t.join().unwrap();
        assert!(err.message.contains("no refresh token"), "{}", err.message);
    }

    #[test]
    fn the_timeout_ends_the_flow() {
        let (token_base, _port, _seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let started = Instant::now();
        let err = finish(&p, pending, Duration::from_millis(500)).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Timeout, "{}", err.message);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_half_request_does_not_hold_the_flow_past_the_timeout() {
        let (token_base, _port, _seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        // A client that writes half a request line and stays connected.
        let _t = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = s.write_all(b"GET /?code=c1&sta");
            let _ = s.flush();
            std::thread::sleep(Duration::from_secs(5));
        });
        let started = Instant::now();
        let err = finish(&p, pending, Duration::from_secs(2)).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(err.code, CallErrorCode::Timeout, "{}", err.message);
        assert!(
            elapsed < Duration::from_secs(4),
            "the half request held the flow for {elapsed:?}"
        );
    }

    #[test]
    fn a_request_line_that_never_ends_does_not_hold_the_flow_past_the_timeout() {
        let (token_base, _port, _seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        // A client that keeps sending bytes with no line end: every read
        // finds data, so only the cap and the deadline can stop it.
        let _t = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let chunk = [b'a'; 512];
            let until = Instant::now() + Duration::from_secs(8);
            while Instant::now() < until {
                if s.write_all(&chunk).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let err = finish(&p, pending, Duration::from_secs(2)).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(err.code, CallErrorCode::Timeout, "{}", err.message);
        assert!(
            elapsed < Duration::from_secs(4),
            "the endless request line held the flow for {elapsed:?}"
        );
    }

    #[test]
    fn a_slow_drip_under_the_cap_does_not_hold_the_flow_past_the_timeout() {
        let (token_base, _port, _seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        // One byte every 100 ms: about 80 bytes in 8 s, far under the
        // 8 KiB cap, and never silent long enough for a per-read timeout.
        // Only the absolute deadline can end this read.
        let _t = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let until = Instant::now() + Duration::from_secs(8);
            while Instant::now() < until {
                if s.write_all(b"a").and_then(|()| s.flush()).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        let err = finish(&p, pending, Duration::from_secs(2)).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(err.code, CallErrorCode::Timeout, "{}", err.message);
        assert!(
            elapsed < Duration::from_secs(4),
            "the slow drip held the flow for {elapsed:?}"
        );
    }

    #[test]
    fn oversized_headers_are_dropped_and_the_wait_goes_on() {
        let (token_base, _port, seen) = stub_server(vec![(
            200,
            json!({ "access_token": "at", "refresh_token": "rt" }).to_string(),
        )]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let state = state_of(&pending.url);
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let big = "X-Pad: ".to_string() + &"a".repeat(16 * 1024);
        let callback = format!("GET /?code=c1&state={state} HTTP/1.1\r\nHost: x\r\n\r\n");
        // The oversized client runs beside the flow: first it floods the
        // headers, then it sends the real callback.
        let t = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = s.write_all(format!("GET / HTTP/1.1\r\n{big}\r\n\r\n").as_bytes());
            let mut out = String::new();
            let _ = BufReader::new(&mut s).read_to_string(&mut out);
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = s.write_all(callback.as_bytes());
            let mut out = String::new();
            let _ = BufReader::new(&mut s).read_to_string(&mut out);
            out
        });
        let result = finish(&p, pending, Duration::from_secs(5)).unwrap();
        t.join().unwrap();
        assert_eq!(result.refresh_token.expose_secret(), "rt");
        assert!(!seen.lock().unwrap().is_empty());
    }

    #[test]
    fn two_code_parameters_end_the_flow() {
        let (token_base, _port, seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        let pending = start(&p).unwrap();
        let state = state_of(&pending.url);
        let addr: SocketAddr = format!("127.0.0.1:{}", pending_port(&pending))
            .parse()
            .unwrap();
        let t = std::thread::spawn(move || get(addr, &format!("/?code=c1&code=c2&state={state}")));
        let err = finish(&p, pending, Duration::from_secs(5)).unwrap_err();
        t.join().unwrap();
        assert!(err.message.contains("more than once"), "{}", err.message);
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The consent posts the client secret, the code and the verifier to
    /// the token endpoint: a plain-http one off the loopback is refused
    /// before the flow starts, as the refresh path refuses it.
    #[test]
    fn plan_refuses_a_plain_http_token_url_off_the_loopback() {
        let (_guard, _cfg, root) = setup("http://tokens.example", "{{env.APB_CONSENT_UNSET_VAR}}");
        let Err(err) = plan(root.path(), CONNECTOR, None, &[]) else {
            panic!("a plain-http token_url off the loopback was accepted");
        };
        assert_eq!(err.code, CallErrorCode::Config, "{}", err.message);
        assert!(err.message.contains("https"), "{}", err.message);
    }

    #[test]
    fn plan_succeeds_with_an_unset_refresh_token_env_var() {
        let (token_base, _port, _seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, "{{env.APB_CONSENT_UNSET_VAR}}");
        let p = plan_of(root.path());
        assert_eq!(p.refresh_field_hint, "refresh_token");
        assert_eq!(p.scopes, vec!["scope.a".to_string(), "scope.b".to_string()]);
    }

    #[test]
    fn plan_never_runs_a_refresh_token_helper() {
        let marker = tempfile::tempdir().unwrap();
        let marker_file = marker.path().join("marker");
        let helper = format!(
            "sh -c 'echo helper-ran > {}; echo helper-token'",
            marker_file.display()
        );
        let (token_base, _port, _seen) = stub_server(vec![]);
        let (_guard, _cfg, root) = setup(&token_base, &format!("{{{{cmd:{helper}}}}}"));
        let p = plan_of(root.path());
        assert_eq!(p.refresh_field_hint, "refresh_token");
        assert!(!marker_file.exists(), "the refresh token helper ran");
    }
}
