//! The inbound webhook listener (spec 2026-08-16-webhook-ingest-design).
//!
//! A second, separate `TcpListener` with its own `Router` carrying exactly
//! three routes: `GET`/`POST /hooks/{connector}/{account}` and
//! `GET /healthz`. Nothing else, ever. The dashboard router can create and
//! run playbooks, so an ingest surface sharing it would turn one
//! misconfigured tunnel into remote code execution. Keeping them structurally
//! apart, and testing that they are, is the single most important property
//! of this feature.
//!
//! The dashboard's auth middleware deliberately does not apply here:
//! providers cannot send a bearer key, so the signature is the
//! authentication. That is why there is no unsigned mode, no opt-out flag,
//! and no path that stores anything before verifying.
//!
//! Request order for a delivery: validate both path segments, resolve the
//! connector and account (unknown pair is a flat 404), read the raw body with
//! an explicit cap, verify the signature over those exact bytes, then append
//! and answer 200 with an empty body. No run starts, no agent is spawned, and
//! the body is never logged.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use apb_core::config::GlobalConfig;
use apb_core::connector::config::{Account, AccountsFile, global_config_path};
use apb_core::connector::def::{ChallengeDialect, ConnectorDoc};
use apb_core::connector::inbox::{Appended, Inbox};
use apb_core::connector::secrets;
use apb_core::connector::store;
use apb_core::connector::template::{Namespace, RenderCtx, placeholders, render_raw};
use apb_core::connector::webhook::{self, Challenge};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;

/// Hard cap on a delivery body. Provider payloads are a few kilobytes; this
/// is generous and still far below axum's implicit default.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Accepted appends per account per minute. Beyond it, deliveries are dropped
/// with a 200 and a counter rather than a 500, so a provider stops retrying
/// instead of queueing days of redelivery against a busy account.
pub const ACCEPT_RATE_PER_MIN: u32 = 600;
/// Length of the accept window, anchored at the first accept in it rather
/// than a calendar boundary, for the same reason as `FAILURE_WINDOW_MS`: a
/// burst straddling a minute edge must not get two budgets (a calendar-minute
/// window let a retry storm land ~1200 appends in 200ms by crossing the
/// boundary). Same 60s length, so "per minute" in `ACCEPT_RATE_PER_MIN`'s
/// name stays accurate.
pub const ACCEPT_WINDOW_MS: u128 = 60_000;
/// Rejected deliveries from one client address inside one window before that
/// address is refused outright. Same value and same rolling-window shape as
/// the dashboard's `auth::MAX_FAILURES_PER_WINDOW`, keyed and logged
/// differently.
pub const MAX_FAILURES_PER_WINDOW: u32 = 10;
/// Length of the failure window, anchored at the first failure rather than at
/// a calendar boundary, so a burst straddling a minute edge cannot get two
/// budgets.
pub const FAILURE_WINDOW_MS: u128 = 60_000;
/// Bound on the rolling-window maps, mirroring `auth::MAX_RATE_LIMIT_ENTRIES`:
/// an attacker rotating source addresses (failures) or hammering many
/// accounts (accepts) must not be able to grow either one without limit.
pub const MAX_RATE_LIMIT_ENTRIES: usize = 4096;

/// Prunes one rolling `(window_start_ms, count)` map: drops every entry whose
/// window has expired, and clears the map outright when it has grown past
/// `MAX_RATE_LIMIT_ENTRIES`. Clearing rather than evicting is deliberate,
/// since the alternative is an LRU whose eviction order an attacker chooses.
/// Shared by both `Windows` maps so the accept window gets exactly the same
/// shape as the failure window, not a lookalike with its own bugs.
fn prune_window<K: Eq + std::hash::Hash>(
    map: &mut HashMap<K, (u128, u32)>,
    now_ms: u128,
    window_ms: u128,
) {
    map.retain(|_, (start, _)| now_ms.saturating_sub(*start) < window_ms);
    if map.len() > MAX_RATE_LIMIT_ENTRIES {
        map.clear();
    }
}

/// The counters, all rolled by comparison rather than by a timer. Both maps
/// use the same rolling `(window_start_ms, count)` shape, anchored at the
/// first event in the window rather than a calendar boundary: `accepts` is
/// keyed per account, `failures` per client address.
#[derive(Debug, Default)]
struct Windows {
    accepts: HashMap<String, (u128, u32)>,
    failures: HashMap<IpAddr, (u128, u32)>,
    dropped: HashMap<String, u64>,
}

impl Windows {
    fn prune_failures(&mut self, now_ms: u128) {
        prune_window(&mut self.failures, now_ms, FAILURE_WINDOW_MS);
    }

    fn prune_accepts(&mut self, now_ms: u128) {
        prune_window(&mut self.accepts, now_ms, ACCEPT_WINDOW_MS);
    }
}

/// Everything the ingest listener keeps between requests: rate windows, drop
/// counters, and the proxy addresses whose forwarded headers are believed.
/// Connector manifests, accounts and secrets are read per request so an edit
/// takes effect immediately and no secret is ever cached.
#[derive(Debug, Clone, Default)]
pub struct IngestState {
    windows: Arc<Mutex<Windows>>,
    /// Exact peer addresses whose `X-Forwarded-For` is trusted, read from
    /// `server.trusted_proxies`. The ingest listener shares the dashboard's
    /// proxy configuration because it sits behind the same proxy, and it is
    /// resolved once at construction like every other startup decision.
    trusted_proxies: Arc<BTreeSet<IpAddr>>,
}

impl IngestState {
    /// Reads `server.trusted_proxies` from the global config.
    ///
    /// A malformed entry is a startup error, never a silently empty set. The
    /// landed dashboard does exactly this (`AuthState::new` propagates
    /// `trusted_proxy_set()`'s error out through `run_server`), and the reason
    /// applies with more force here: an ingest listener that quietly forgot
    /// its proxy attributes every delivery to the proxy's own address, which
    /// is the failure this whole derivation exists to prevent. An absent
    /// config file is not an error and simply means no proxy is trusted.
    pub fn new() -> Result<Self, String> {
        let cfg = GlobalConfig::load()?;
        Ok(Self::with_trusted_proxies(cfg.server.trusted_proxy_set()?))
    }

    /// The same state with an explicit proxy set, for tests and for a caller
    /// that already resolved the config.
    pub fn with_trusted_proxies(trusted: BTreeSet<IpAddr>) -> Self {
        IngestState {
            windows: Arc::default(),
            trusted_proxies: Arc::new(trusted),
        }
    }

    /// The address a request is attributed to, derived exactly the way the
    /// dashboard's `auth::client_ctx` derives it: the socket peer, or the
    /// rightmost `X-Forwarded-For` entry when that peer is a trusted proxy.
    /// Rightmost, because only the entry the trusted proxy itself appended is
    /// worth anything; everything to its left was supplied by the caller.
    ///
    /// This matters more here than on the dashboard. The documented topology
    /// puts a TLS-terminating proxy on the same host, so keying on the raw
    /// socket peer would give every delivery the proxy's loopback address:
    /// one sender with a stale secret would lock out every provider, and the
    /// fail2ban filter in DEPLOYMENT.md would ban the proxy itself.
    ///
    /// Signature verification never consults this. It runs over the untouched
    /// raw body bytes and the configured secret, never over a header a caller
    /// could shape.
    fn client_ip(&self, headers: &HeaderMap, peer: IpAddr) -> IpAddr {
        if !self.trusted_proxies.contains(&peer) {
            return peer;
        }
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
            .unwrap_or(peer)
    }

    /// How many deliveries were dropped for one account by the accept cap.
    pub fn dropped(&self, connector: &str, account: &str) -> u64 {
        let guard = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .dropped
            .get(&pair(connector, account))
            .copied()
            .unwrap_or(0)
    }

    /// Whether one more append is allowed for this account in the current
    /// rolling window. Counts the drop when it is not. Mirrors
    /// `note_failure`: prune first, anchor the window at the first accept,
    /// and restart it once it has expired. Anchoring at first-use rather than
    /// a calendar minute matters here specifically: a calendar-minute window
    /// let a burst straddling the boundary get two separate budgets back to
    /// back (~1200 appends in 200ms), which is exactly the retry-storm case
    /// this cap exists to bound.
    fn allow_accept(&self, connector: &str, account: &str) -> bool {
        let key = pair(connector, account);
        let now = apb_core::clock::now_ms();
        let mut guard = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        guard.prune_accepts(now);
        let entry = guard.accepts.entry(key.clone()).or_insert((now, 0));
        if now.saturating_sub(entry.0) >= ACCEPT_WINDOW_MS {
            *entry = (now, 0);
        }
        if entry.1 >= ACCEPT_RATE_PER_MIN {
            *guard.dropped.entry(key).or_insert(0) += 1;
            return false;
        }
        entry.1 += 1;
        true
    }

    /// Whether this client is still allowed to be wrong. Called before the
    /// expensive work so a flood of bad signatures costs almost nothing.
    /// Mirrors `auth::RateLimiter::is_blocked`: over budget only while the
    /// window that recorded the failures is still open.
    fn peer_allowed(&self, client: IpAddr) -> bool {
        let now = apb_core::clock::now_ms();
        let guard = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        match guard.failures.get(&client) {
            Some((start, count)) => {
                !(now.saturating_sub(*start) < FAILURE_WINDOW_MS
                    && *count > MAX_FAILURES_PER_WINDOW)
            }
            None => true,
        }
    }

    /// Records one failure, mirroring `auth::RateLimiter::record_failure`:
    /// prune first, anchor the window at the first failure, and restart it
    /// once it has expired.
    fn note_failure(&self, client: IpAddr) {
        let now = apb_core::clock::now_ms();
        let mut guard = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        guard.prune_failures(now);
        let entry = guard.failures.entry(client).or_insert((now, 0));
        if now.saturating_sub(entry.0) >= FAILURE_WINDOW_MS {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
    }
}

fn pair(connector: &str, account: &str) -> String {
    format!("{connector}/{account}")
}

/// The whole ingest surface. Three routes and no fallback: an unknown path is
/// a 404 from axum itself, which is exactly the disclosure-free answer this
/// listener should give.
pub fn build_ingest_router(state: IngestState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/hooks/{connector}/{account}",
            get(get_hook_handler).post(post_hook_handler),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Binds and serves the ingest listener. Returns `io::Error` rather than a
/// boxed error so the future is `Send` and the dashboard can co-start it with
/// `tokio::spawn`.
pub async fn run_ingest_server(bind: IpAddr, port: u16) -> Result<(), std::io::Error> {
    // Config first, and loudly: a malformed proxy list must stop the listener
    // rather than start one that mis-attributes every delivery.
    let state = IngestState::new().map_err(std::io::Error::other)?;
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    let app = build_ingest_router(state);
    let host = match bind {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    println!("apb ingest: http://{host}:{port}/hooks/<connector>/<account>");
    // There is no key interlock here the way `check_bind_allowed` guards the
    // dashboard: the signature is the authentication, so there is nothing to
    // require. That is not a reason to be quiet about it. A non-loopback bind
    // means the hook endpoints face the network with no TLS of their own, and
    // an operator who did that by accident deserves to see it in the log.
    if !bind.is_loopback() {
        eprintln!(
            "apb ingest: binding {bind} puts the hook endpoints directly on the network with no TLS of their own. The supported topology is a TLS-terminating reverse proxy on this host reaching a loopback bind; see docs/DEPLOYMENT.md"
        );
    }
    if let Ok(cfg) = GlobalConfig::load()
        && cfg.ingest.public_base_url.is_none()
    {
        println!(
            "apb ingest: ingest.public_base_url is not set, so apb cannot print the callback URL to register with a provider"
        );
    }
    // ConnectInfo carries the socket peer into the handlers, which derive the
    // client address from it and, behind a configured trusted proxy, from the
    // rightmost X-Forwarded-For entry. That address is used only for
    // rate-limit keying and for the rejection log line; no authentication
    // decision is ever made from a header a caller controls.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}

/// Empty-bodied status, the only shape a refusal ever takes here.
fn flat(status: StatusCode) -> Response {
    (status, ()).into_response()
}

/// One line per rejected delivery, greppable by fail2ban. Names the client
/// address (the forwarded one behind a trusted proxy, so a ban lands on the
/// sender rather than on the proxy), the connector and the account, and
/// nothing else: no header, no body, no secret.
fn log_rejected(client: IpAddr, connector: &str, account: &str) {
    eprintln!("apb ingest_rejected ip={client} connector={connector} account={account}");
}

/// `GET /hooks/{connector}/{account}`: the verification handshake. Only
/// meaningful for a connector that declares a challenge dialect; anything
/// else is a 404, so a probe cannot enumerate which connectors exist by the
/// difference between 403 and 404.
async fn get_hook_handler(
    State(state): State<IngestState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath((connector, account)): AxPath<(String, String)>,
    Query(params): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let client = state.client_ip(&headers, peer.ip());
    if !state.peer_allowed(client) {
        return flat(StatusCode::UNAUTHORIZED);
    }
    let Some((doc, acct)) = resolve_target(&connector, &account) else {
        return flat(StatusCode::NOT_FOUND);
    };
    let hook = doc
        .webhook
        .as_ref()
        .expect("resolve_target checked the block");
    if hook.challenge != Some(ChallengeDialect::MetaHub) {
        return flat(StatusCode::NOT_FOUND);
    }
    let Some(template) = hook.verify_token.as_deref() else {
        return flat(StatusCode::NOT_FOUND);
    };
    let Some(token) = render_from_account(template, &doc, &acct) else {
        // The token could not be resolved (a missing env var, a failing
        // command). The operator sees this through `apb connector doctor`;
        // the caller sees a flat refusal.
        state.note_failure(client);
        log_rejected(client, &connector, &account);
        return flat(StatusCode::FORBIDDEN);
    };
    match webhook::meta_hub_challenge(&params, &token) {
        // The echoed value is attacker-supplied and reflected verbatim, so
        // it is served as plain text AND with sniffing disabled: without
        // nosniff a browser could still decide the bytes look like markup.
        Challenge::Echo(text) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            text,
        )
            .into_response(),
        Challenge::Reject => {
            state.note_failure(client);
            log_rejected(client, &connector, &account);
            flat(StatusCode::FORBIDDEN)
        }
    }
}

/// `POST /hooks/{connector}/{account}`: one delivery. Verify, append, answer
/// 200 with an empty body. Secret resolution and signature verification run
/// on the blocking pool (`spawn_blocking`), never inline on this async task;
/// nothing else on this path does any real work.
async fn post_hook_handler(
    State(state): State<IngestState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath((connector, account)): AxPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = state.client_ip(&headers, peer.ip());
    if !state.peer_allowed(client) {
        return flat(StatusCode::UNAUTHORIZED);
    }
    // The `DefaultBodyLimit` layer is what actually refuses an oversize
    // request, and it does so before this handler runs: reaching this line
    // means the body is already buffered and already within the cap. The
    // check is kept anyway as a cheap invariant on the one number that
    // matters, so a future change to the layer cannot silently widen it.
    if body.len() > MAX_BODY_BYTES {
        return flat(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let Some((doc, acct)) = resolve_target(&connector, &account) else {
        return flat(StatusCode::NOT_FOUND);
    };
    // Cloned out before `doc` moves into the blocking closure below: this is
    // the only piece of it still needed afterward (`dedupe_path`).
    let hook = doc
        .webhook
        .clone()
        .expect("resolve_target checked the block");

    let presented = headers
        .get(hook.signature.header.as_str())
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let secret_template = hook.signature.secret.clone();
    let prefix = hook.signature.prefix.clone();
    // Secret resolution can run a `{{cmd:...}}` reference, which shells out
    // and is polled with a blocking sleep for up to `CMD_SECRET_TIMEOUT`
    // (10s). That must never run on a tokio worker thread: this whole path is
    // reachable by an unauthenticated caller sending a bad-signature POST, so
    // without `spawn_blocking` a handful of concurrent requests could starve
    // every other request on the runtime, including `/healthz`.
    // `spawn_blocking` moves the closure (and everything it captures - the
    // secret value included) onto the blocking pool and back only carries out
    // the `bool`/`None` verdict, so the resolved secret never crosses back
    // into async code at all.
    let verify_body = body.clone();
    let verified = tokio::task::spawn_blocking(move || {
        let secret = render_from_account(&secret_template, &doc, &acct)?;
        // Over the exact bytes received: never a reparsed or reserialized
        // body, which would change them and break the MAC.
        Some(webhook::verify_signature_hex(
            &secret,
            &verify_body,
            &presented,
            &prefix,
        ))
    })
    .await;
    match verified {
        Ok(Some(true)) => {}
        Ok(Some(false) | None) => {
            state.note_failure(client);
            log_rejected(client, &connector, &account);
            return flat(StatusCode::UNAUTHORIZED);
        }
        Err(_) => {
            // The blocking task panicked (should not happen on this path);
            // fail closed rather than silently treat it as verified.
            return flat(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
        // Authenticated but unusable. A specific status is safe here: only a
        // holder of the shared secret can reach this line.
        return flat(StatusCode::BAD_REQUEST);
    };

    // Over the cap: drop with a 200 and a counter, so the provider stops
    // retrying rather than filling the disk twice over. `allow_accept` bumps
    // the in-process counter (kept for tests that read `state.dropped`
    // directly); the persisted counter below is the operator-visible source
    // of truth (`apb connector doctor`, the dashboard panel), so it must
    // survive a restart and be readable from any process, not just this one.
    // Best effort: a failure to persist must not turn an already-rejected
    // delivery into a 500 for a request that was never going to be stored.
    if !state.allow_accept(&connector, &account) {
        if let Ok(store) = Inbox::open(&connector, &account)
            && let Err(e) = store.note_dropped()
        {
            eprintln!("apb ingest_store_error connector={connector} account={account}: {e}");
        }
        return flat(StatusCode::OK);
    }

    let id = webhook::dedupe_id(&parsed, &body, hook.dedupe_path.as_deref());
    let Ok(store) = Inbox::open(&connector, &account) else {
        return flat(StatusCode::INTERNAL_SERVER_ERROR);
    };
    match store.append(&id, &parsed) {
        // A duplicate is a success from the provider's point of view: it
        // delivered, apb has it, and the retry must stop.
        Ok(Appended::Stored(_)) | Ok(Appended::Duplicate) => flat(StatusCode::OK),
        Err(e) => {
            // The error text names paths and io failures, never a body.
            eprintln!("apb ingest_store_error connector={connector} account={account}: {e}");
            flat(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// The connector manifest and the account a delivery names, or `None` when
/// either does not exist, the path segments are unusable, or the connector
/// declares no webhook block. One `None` for every one of those cases, so the
/// caller answers a single indistinguishable 404.
fn resolve_target(connector: &str, account: &str) -> Option<(ConnectorDoc, Account)> {
    for segment in [connector, account] {
        if !apb_core::registry::is_safe_segment(segment) {
            return None;
        }
        apb_core::profile::validate_profile_name(segment).ok()?;
    }
    let doc = load_connector_doc(connector)?;
    doc.webhook.as_ref()?;
    // Global accounts only: the hook path carries no workspace segment, so a
    // project-scoped account has no unambiguous project root at delivery
    // time, and picking one arbitrarily would silently change which secret
    // verifies a signature.
    let path = global_config_path(connector)?;
    let raw = std::fs::read_to_string(path).ok()?;
    let file: AccountsFile = serde_yaml_ng::from_str(&raw).ok()?;
    let acct = file.accounts.into_iter().find(|a| a.name == account)?;
    Some((doc, acct))
}

/// Reads and parses one connector's manifest directly, deliberately skipping
/// `store::load`'s whole-folder `content::tree_digest`. That digest exists so
/// the dashboard can tell whether an approved connector's content changed
/// since it was trusted (`content.rs`, connector approval) - a decision that
/// belongs to the dashboard's approval flow, never to ingest. The signature
/// on the request is the authentication here; a trust digest is not read or
/// checked on this path at all. Every delivery reaches this call, including
/// ones the accept cap is about to drop (`resolve_target` runs first), so
/// hashing the whole connector directory - `connector.yaml`, `PUBLIC.md`, and
/// any other files alongside them - per request would put real, unbounded-by-
/// account-config CPU work on a path an unauthenticated caller can trigger at
/// will. `connector` was already validated as a plain `[a-z0-9-]` slug by
/// `resolve_target`, so joining it into a path here carries no traversal
/// risk from the name alone - but skipping `store::load` means this function
/// must restore its symlink-containment check itself (see below), since
/// nothing else on this path re-adds it.
fn load_connector_doc(name: &str) -> Option<ConnectorDoc> {
    let base = store::connectors_dir()?;
    let cand = base.join(name);
    let canonical_path = std::fs::canonicalize(&cand).ok()?;
    // Symlink-containment check, mirroring `store::load`'s defense-in-depth:
    // even with `name` already validated as a plain `[a-z0-9-]` slug, a
    // symlink planted inside the connectors root (by an installed connector,
    // or by anything else with write access to `connectors_dir()`) could
    // resolve outside it. Every delivery reaches this call, including ones
    // an unauthenticated caller can trigger at will, so refusing containment
    // is not optional here.
    let canonical_root = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
    if !canonical_path.starts_with(&canonical_root) {
        return None;
    }
    let yaml = std::fs::read_to_string(canonical_path.join("connector.yaml")).ok()?;
    ConnectorDoc::from_yaml(&yaml, name).ok()
}

/// Renders one webhook template against the account, resolving only the
/// fields that template actually references. Resolving lazily matters: a
/// `{{cmd:...}}` reference runs a command, and a delivery must not run one
/// for a field it does not need.
fn render_from_account(template: &str, doc: &ConnectorDoc, account: &Account) -> Option<String> {
    let secret_names = doc.secret_fields();
    let mut account_fields: BTreeMap<String, String> = BTreeMap::new();
    let mut resolved: BTreeMap<String, String> = BTreeMap::new();
    for (ns, name) in placeholders(template).ok()? {
        let raw = account.fields.get(&name)?;
        match ns {
            Namespace::Secret if secret_names.contains(&name) => {
                resolved.insert(name, resolve_reference(raw)?);
            }
            Namespace::Account => {
                account_fields.insert(name, raw.clone());
            }
            // Neither args nor the auth marker can appear here: the connector
            // validator rejects both in the webhook block.
            _ => return None,
        }
    }
    let args = serde_json::Value::Null;
    let ctx = RenderCtx {
        account: &account_fields,
        args: &args,
        secrets: &resolved,
    };
    render_raw(template, &ctx).ok()
}

/// Resolves one `secret: true` account field value to its concrete secret.
///
/// The chain is deliberately narrow: the process environment, then the global
/// `<config_dir>/secrets.env`. `resolve_var` also consults a project
/// `.apb/secrets.env` under the root it is given, and the config dir has no
/// such file, so that step is a no-op by construction. The resolved value is
/// used inside one request and dropped; it is never cached, logged, or
/// returned.
fn resolve_reference(raw: &str) -> Option<String> {
    let root = apb_core::config::config_dir()?;
    if let Some(var) = secrets::parse_env_ref(raw) {
        return secrets::resolve_var(&root, &var);
    }
    if let Some(cmd) = secrets::parse_cmd_ref(raw) {
        return secrets::resolve_cmd(&cmd, secrets::CMD_SECRET_TIMEOUT).ok();
    }
    None
}
