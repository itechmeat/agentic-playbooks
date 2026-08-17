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
//! an explicit cap, verify the signature over those exact bytes, decide
//! whether it is a redelivery, charge the accept cap only if it is not, then
//! append and answer 200 with an empty body. No run starts, no agent is
//! spawned, and the body is never logged.
//!
//! A valid signature is accepted whatever the per-IP failure limiter thinks:
//! the limiter guards the rejection path only, because the documented
//! topology attributes every delivery to one proxy address and a stranger's
//! bad signatures must not be able to cost a provider its events.

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
/// How long a resolved webhook secret is cached per account. Caching it for a
/// few seconds is a deliberate, bounded exception to the codebase's "never
/// cache a secret" rule: without it a flooding client that sends well-formed
/// but bogus signatures would trigger a fresh secret resolution per request,
/// and a `{{cmd:...}}` secret shells out under `spawn_blocking` for up to
/// `CMD_SECRET_TIMEOUT` (10s), so a handful of concurrent bad-signature POSTs
/// could pin the blocking pool. The window is short so a rotated secret takes
/// effect within seconds, and the value is still never logged or returned.
pub const SECRET_CACHE_TTL_MS: u128 = 10_000;

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

/// One cached webhook secret and when it was resolved. Its `Debug` redacts the
/// value so an accidental debug-print of [`IngestState`] cannot leak a secret.
#[derive(Clone)]
struct CachedSecret {
    secret: Arc<str>,
    resolved_at: u128,
}

impl std::fmt::Debug for CachedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedSecret")
            .field("secret", &"<redacted>")
            .field("resolved_at", &self.resolved_at)
            .finish()
    }
}

/// Everything the ingest listener keeps between requests: rate windows, drop
/// counters, the proxy addresses whose forwarded headers are believed, and a
/// short-lived per-account secret cache. Connector manifests and accounts are
/// still read per request so an edit takes effect immediately; the resolved
/// secret is cached only for [`SECRET_CACHE_TTL_MS`] to keep a bad-signature
/// flood from re-resolving it (and re-shelling-out) on every request.
#[derive(Debug, Clone, Default)]
pub struct IngestState {
    windows: Arc<Mutex<Windows>>,
    /// Exact peer addresses whose `X-Forwarded-For` is trusted, read from
    /// `server.trusted_proxies`. The ingest listener shares the dashboard's
    /// proxy configuration because it sits behind the same proxy, and it is
    /// resolved once at construction like every other startup decision.
    trusted_proxies: Arc<BTreeSet<IpAddr>>,
    /// Resolved webhook secrets, keyed per `connector/account`, each with a
    /// short TTL. Only valid pairs reach resolution (an unknown pair is a 404
    /// first), so this is bounded by the number of configured accounts.
    secrets: Arc<Mutex<HashMap<String, CachedSecret>>>,
    /// Per-account singleflight gates, so concurrent cache misses for one
    /// account collapse to a single resolution rather than a thundering herd.
    secret_gates: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
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
        Self::from_config(&GlobalConfig::load()?)
    }

    /// The same state from a config the caller already loaded. The listener
    /// startup path uses this so the whole of `run_ingest_server` reads
    /// `config.yaml` exactly once: reading it twice made the proxy set and
    /// the rest of the startup decisions come from two different files
    /// whenever an edit landed between them.
    pub fn from_config(cfg: &GlobalConfig) -> Result<Self, String> {
        Ok(Self::with_trusted_proxies(cfg.server.trusted_proxy_set()?))
    }

    /// The same state with an explicit proxy set, for tests and for a caller
    /// that already resolved the config.
    pub fn with_trusted_proxies(trusted: BTreeSet<IpAddr>) -> Self {
        IngestState {
            windows: Arc::default(),
            trusted_proxies: Arc::new(trusted),
            secrets: Arc::default(),
            secret_gates: Arc::default(),
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

    /// Whether this client is still allowed to be wrong. Consulted only on
    /// the rejection path (a request carrying nothing that could verify), so
    /// a tripped limiter never refuses a validly signed delivery. Mirrors
    /// `auth::RateLimiter::is_blocked`: over budget only while the window that
    /// recorded the failures is still open.
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

    /// A still-fresh cached secret for `key`, or `None`.
    fn cached_secret(&self, key: &str, now: u128) -> Option<Arc<str>> {
        let guard = self.secrets.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(key).and_then(|c| {
            (now.saturating_sub(c.resolved_at) < SECRET_CACHE_TTL_MS).then(|| c.secret.clone())
        })
    }

    /// Stores a freshly resolved secret for `key`. The map is bounded by the
    /// number of configured accounts; it is cleared defensively if it ever
    /// grows past the shared cap, matching the rolling-window maps.
    fn store_secret(&self, key: String, secret: Arc<str>, now: u128) {
        let mut guard = self.secrets.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() > MAX_RATE_LIMIT_ENTRIES {
            guard.clear();
        }
        guard.insert(
            key,
            CachedSecret {
                secret,
                resolved_at: now,
            },
        );
    }

    /// The singleflight gate for one account, created on first use.
    fn secret_gate(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut guard = self.secret_gates.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() > MAX_RATE_LIMIT_ENTRIES {
            guard.clear();
        }
        guard.entry(key.to_string()).or_default().clone()
    }

    /// One webhook template resolved at most once per [`SECRET_CACHE_TTL_MS`],
    /// with concurrent misses for one cache key collapsed to a single
    /// resolution and the expensive resolution (which may shell out for a
    /// `{{cmd:...}}` reference) run on the blocking pool, never on a tokio
    /// worker. Returns `None` when the value is unresolvable or resolves to
    /// empty.
    ///
    /// `cache_key` carries the field role (`sig:` for a signature secret,
    /// `verify:` for a challenge token) as well as the connector/account pair,
    /// so the two never collide in the cache even when the same account field
    /// backs both.
    async fn resolve_cached(
        &self,
        cache_key: String,
        doc: ConnectorDoc,
        acct: Account,
        template: String,
    ) -> Option<Arc<str>> {
        if let Some(value) = self.cached_secret(&cache_key, apb_core::clock::now_ms()) {
            return Some(value);
        }
        // Singleflight: one resolver per cache key at a time. Whoever loses the
        // race waits here and then reads the value the winner cached.
        let gate = self.secret_gate(&cache_key);
        let _held = gate.lock().await;
        if let Some(value) = self.cached_secret(&cache_key, apb_core::clock::now_ms()) {
            return Some(value);
        }
        let resolved =
            tokio::task::spawn_blocking(move || render_from_account(&template, &doc, &acct))
                .await
                .ok()
                .flatten()?;
        if resolved.is_empty() {
            return None;
        }
        let value: Arc<str> = Arc::from(resolved.as_str());
        self.store_secret(cache_key, value.clone(), apb_core::clock::now_ms());
        Some(value)
    }

    /// The webhook signature secret for one account, resolved at most once per
    /// [`SECRET_CACHE_TTL_MS`]. Returns `None` when the secret is unresolvable
    /// or resolves to empty, which is a hard verification failure (an empty key
    /// would let anyone sign a delivery).
    async fn webhook_secret(
        &self,
        connector: &str,
        account: &str,
        doc: ConnectorDoc,
        acct: Account,
        template: String,
    ) -> Option<Arc<str>> {
        self.resolve_cached(
            format!("sig:{}", pair(connector, account)),
            doc,
            acct,
            template,
        )
        .await
    }

    /// The challenge verify token for one account, resolved through the same
    /// cached, singleflighted, blocking-pool machinery as the signature secret.
    /// The GET challenge handshake used to resolve this inline on the async
    /// worker, so an unauthenticated challenge flood against a `{{cmd:...}}`
    /// verify token could shell out per request and pin the shared runtime the
    /// dashboard also runs on. Returns `None` when the token is unresolvable or
    /// empty, which the caller treats as a verification failure.
    async fn challenge_token(
        &self,
        connector: &str,
        account: &str,
        doc: ConnectorDoc,
        acct: Account,
        template: String,
    ) -> Option<Arc<str>> {
        self.resolve_cached(
            format!("verify:{}", pair(connector, account)),
            doc,
            acct,
            template,
        )
        .await
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
    // Config first, once, and loudly: a malformed proxy list must stop the
    // listener rather than start one that mis-attributes every delivery, and
    // every startup decision below comes from this one read rather than from
    // whatever the file says a moment later.
    let cfg = GlobalConfig::load().map_err(std::io::Error::other)?;
    let state = IngestState::from_config(&cfg).map_err(std::io::Error::other)?;
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
    if cfg.ingest.public_base_url.is_none() {
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
    let Some(template) = hook.verify_token.clone() else {
        return flat(StatusCode::NOT_FOUND);
    };
    // Resolve the verify token through the same cached, singleflighted,
    // blocking-pool machinery as the signature secret. Resolving it inline on
    // this async task let an unauthenticated challenge flood against a
    // `{{cmd:...}}` token shell out per request and pin the shared runtime the
    // dashboard also runs on.
    let Some(token) = state
        .challenge_token(&connector, &account, doc, acct, template)
        .await
    else {
        // The token could not be resolved (a missing env var, a failing
        // command) or resolved to empty. The operator sees this through
        // `apb connector doctor`; the caller sees a flat refusal.
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

/// `POST /hooks/{connector}/{account}`: one delivery. Verify, dedupe, append,
/// answer 200 with an empty body. Secret resolution and signature
/// verification run on the blocking pool (`spawn_blocking`), never inline on
/// this async task; nothing else on this path does any real work.
///
/// Two orderings on this path are load-bearing and were both got wrong once:
///
///   * the failure limiter never refuses a validly signed delivery (spec
///     2026-08-16-webhook-ingest-design). It is consulted only where a
///     refusal was going to happen anyway, so a flood attributed to one
///     address (behind a same-host proxy with no `server.trusted_proxies`,
///     that is every address) cannot cost a provider its events;
///   * the accept cap is charged for a genuinely new delivery only, after
///     the dedupe decision. Charging it earlier let a retry storm on one
///     message spend the whole budget and silently drop new deliveries with
///     a 200 the provider reads as "accepted".
async fn post_hook_handler(
    State(state): State<IngestState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxPath((connector, account)): AxPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = state.client_ip(&headers, peer.ip());
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
    // The only place the failure limiter refuses anything on this path, and
    // it refuses only what could not have verified anyway: a client already
    // over its budget that presents no signature, or one that is not even
    // shaped like a digest this scheme could accept (wrong prefix, wrong
    // length, non-hex). Deciding that needs no secret, so this costs nothing.
    //
    // The tradeoff is deliberate: a flooding address that sends well-formed
    // hex still pays a full secret resolution and HMAC per request. Refusing
    // it earlier is the only thing that would make the flood cheap, and it is
    // exactly what would drop a validly signed delivery from an address a
    // shared proxy attributes the flood to. Losing a provider's events is the
    // worse failure, so the CPU is the price and the accept cap plus the body
    // limit bound the rest.
    if !state.peer_allowed(client) && !webhook::signature_is_well_formed(&presented, &prefix) {
        log_rejected(client, &connector, &account);
        return flat(StatusCode::UNAUTHORIZED);
    }
    // Resolve the secret once per account and cache it for a short window,
    // rather than resolving it on every request. Secret resolution can run a
    // `{{cmd:...}}` reference, which shells out and is polled with a blocking
    // sleep for up to `CMD_SECRET_TIMEOUT` (10s); this path is reachable by an
    // unauthenticated caller sending a bad-signature POST, so re-resolving it
    // per request would let a flood of well-formed-but-bogus signatures start
    // hundreds of blocking processes. `webhook_secret` runs that resolution on
    // the blocking pool, caches the result for `SECRET_CACHE_TTL_MS`, and
    // collapses concurrent misses for one account to a single resolution. A
    // validly signed delivery is still always accepted, because the secret it
    // is checked against is the same one, cached or freshly resolved. An
    // empty or unresolvable secret is a hard failure (`None`): `HMAC-SHA256`
    // accepts a zero-length key, so treating an empty secret as usable would
    // let anyone on the internet sign a delivery.
    let verify_body = body.clone();
    let verified = match state
        .webhook_secret(&connector, &account, doc, acct, secret_template)
        .await
    {
        // HMAC over the exact bytes received (never a reparsed or reserialized
        // body, which would change them and break the MAC). Cheap next to the
        // resolution above and bounded by `MAX_BODY_BYTES`, so it runs inline.
        Some(secret) => webhook::verify_signature_hex(&secret, &verify_body, &presented, &prefix),
        None => false,
    };
    if !verified {
        state.note_failure(client);
        log_rejected(client, &connector, &account);
        return flat(StatusCode::UNAUTHORIZED);
    }

    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
        // Authenticated but unusable. A specific status is safe here: only a
        // holder of the shared secret can reach this line.
        return flat(StatusCode::BAD_REQUEST);
    };

    let id = webhook::dedupe_id(&parsed, &body, hook.dedupe_path.as_deref());
    let Ok(store) = Inbox::open(&connector, &account) else {
        return flat(StatusCode::INTERNAL_SERVER_ERROR);
    };

    // Dedupe first, and before the accept cap. A redelivery of something
    // already stored appends nothing, so it must not spend the budget either:
    // a provider retry storm on one message would otherwise burn the whole
    // per-minute cap and make apb drop *new* deliveries with a 200, which the
    // provider reads as "accepted" and never retries. Those events would be
    // gone for good. A duplicate is a success from the provider's point of
    // view: it delivered, apb has it, and the retry must stop.
    //
    // `is_duplicate` reads no lock and `append` re-checks under one, so two
    // identical deliveries racing each other can still cost one budget slot
    // between them. That is a bounded, one-per-race cost rather than the
    // unbounded one this ordering removes.
    match store.is_duplicate(&id) {
        Ok(true) => return flat(StatusCode::OK),
        Ok(false) => {}
        Err(e) => {
            eprintln!("apb ingest_store_error connector={connector} account={account}: {e}");
            return flat(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Over the cap: drop with a 200 and a counter, so the provider stops
    // retrying rather than filling the disk twice over. `allow_accept` bumps
    // the in-process counter (kept for tests that read `state.dropped`
    // directly); the persisted counter below is the operator-visible source
    // of truth (`apb connector doctor`, the dashboard panel), so it must
    // survive a restart and be readable from any process, not just this one.
    // Best effort: a failure to persist must not turn an already-rejected
    // delivery into a 500 for a request that was never going to be stored.
    if !state.allow_accept(&connector, &account) {
        if let Err(e) = store.note_dropped() {
            eprintln!("apb ingest_store_error connector={connector} account={account}: {e}");
        }
        return flat(StatusCode::OK);
    }

    match store.append(&id, &parsed) {
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
/// never logged or returned to a caller; the ingest handler caches it per
/// account for [`SECRET_CACHE_TTL_MS`] only (see [`IngestState::webhook_secret`])
/// so a bad-signature flood cannot re-run this resolution on every request.
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
