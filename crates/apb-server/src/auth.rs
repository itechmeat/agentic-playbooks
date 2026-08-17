//! Server-mode authentication (spec 2026-08-16-server-mode-design).
//!
//! One axum middleware wraps the whole router. Its evaluation order is fixed:
//! the "no keys means no auth" pass-through that keeps the local dashboard
//! byte-for-byte as it was, then exempt paths, then a bearer key, then a
//! session cookie, then 401. Cookie-authenticated writes additionally carry a
//! custom marker header, which a cross-site form cannot set.
//!
//! The keyless pass-through has one exception: a server started bound to a
//! non-loopback address (`AuthState::require_keys`) must never fall back to
//! it just because the last key was revoked while it was running. The
//! startup interlock (`check_bind_allowed` in `crate::lib`) only checks the
//! precondition once, at boot; without a runtime re-check, letting the key
//! set empty out mid-run would silently reopen an RCE-equivalent panel to
//! the network. See [`AuthState::refuses_because_keyless`].
//!
//! Every lock in this module is a std mutex taken inside a plain block and
//! dropped before any await, because a guard held across an await stalls the
//! executor and is denied by clippy.
//!
//! One deliberate exception to the "no plain comparison on anything
//! secret-derived" rule: API keys are verified by a linear scan using
//! `server_auth::ct_eq_str`, but session tokens are looked up by their
//! SHA-256 hex in a `HashMap`, which is an ordinary hash-and-compare and not
//! constant time. That is intentional and safe here: the value being looked
//! up is already a one-way hash of a 256-bit CSPRNG token, so a timing signal
//! about the hash yields no usable preimage, and a map lookup is what keeps
//! the session path O(1) under load.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use apb_core::server_auth::{self, KeyRecord};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};

/// Name of the browser session cookie.
pub const SESSION_COOKIE: &str = "apb_session";
/// The CSRF second layer: a header a cross-site request cannot set.
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_VALUE: &str = "apb-dashboard";
/// Sliding session lifetime: seven days since the last request.
pub const SESSION_TTL_MS: u128 = 7 * 24 * 60 * 60 * 1000;
/// Hard cap on live sessions; the least recently used one is evicted.
pub const MAX_SESSIONS: usize = 1024;
/// Auth-failure budget: more than this many failures inside one window from
/// one client IP earns a 429 for the rest of the window.
pub const MAX_FAILURES_PER_WINDOW: u32 = 10;
pub const FAILURE_WINDOW_MS: u128 = 60_000;
/// Bound on the rate-limiter map, so an attacker rotating source addresses
/// cannot grow it without limit.
pub const MAX_RATE_LIMIT_ENTRIES: usize = 4096;

/// One live browser session: when it was last seen (for the sliding TTL) and
/// the id of the API key that minted it. Binding the session to its key is
/// what lets a revoked key take its sessions down with it: without the id, a
/// session outlives the key that authorized it for the full seven-day sliding
/// window, so revoking a compromised key would not actually log the attacker
/// out.
#[derive(Debug, Clone)]
struct SessionEntry {
    last_seen: u128,
    key_id: String,
}

/// Live browser sessions, keyed by the SHA-256 of the session token. The raw
/// token exists only in the cookie; a memory dump of the server yields
/// nothing usable. Sessions are deliberately not persisted: a restart returns
/// the operator to the login screen, which is cheap and removes a state file.
#[derive(Default)]
pub struct SessionStore {
    entries: HashMap<String, SessionEntry>,
}

impl SessionStore {
    /// Registers a new session at `now_ms`, bound to the id of the key that
    /// minted it, pruning expired entries and evicting the least recently used
    /// one when the store is full.
    pub fn insert(&mut self, token_hash: String, now_ms: u128, key_id: String) {
        self.prune(now_ms);
        if self.entries.len() >= MAX_SESSIONS {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_seen)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(
            token_hash,
            SessionEntry {
                last_seen: now_ms,
                key_id,
            },
        );
    }

    /// The id of the key that minted the session, when the hash names a live
    /// one, refreshing its sliding TTL. An expired entry is removed on the way
    /// out. The caller checks whether that key is still live and drops the
    /// session if it is not.
    pub fn touch(&mut self, token_hash: &str, now_ms: u128) -> Option<String> {
        match self.entries.get(token_hash).cloned() {
            Some(e) if now_ms.saturating_sub(e.last_seen) < SESSION_TTL_MS => {
                self.entries.insert(
                    token_hash.to_string(),
                    SessionEntry {
                        last_seen: now_ms,
                        key_id: e.key_id.clone(),
                    },
                );
                Some(e.key_id)
            }
            Some(_) => {
                self.entries.remove(token_hash);
                None
            }
            None => None,
        }
    }

    pub fn remove(&mut self, token_hash: &str) {
        self.entries.remove(token_hash);
    }

    /// Drops every session minted by a key that is no longer in `live_ids`.
    /// Called when the key set reloads, so a revoked key takes its sessions
    /// with it rather than leaving them valid for the sliding window.
    pub fn retain_live_keys(&mut self, live_ids: &BTreeSet<String>) {
        self.entries.retain(|_, e| live_ids.contains(&e.key_id));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn prune(&mut self, now_ms: u128) {
        self.entries
            .retain(|_, e| now_ms.saturating_sub(e.last_seen) < SESSION_TTL_MS);
    }
}

/// Fixed-window failure counter per client IP. Deliberately in memory and
/// deliberately tiny: this exists to blunt online guessing of a 256-bit key,
/// not to be a general traffic limiter.
#[derive(Default)]
pub struct RateLimiter {
    windows: HashMap<IpAddr, (u128, u32)>,
}

impl RateLimiter {
    /// Records one failure and reports whether the client is now over budget.
    pub fn record_failure(&mut self, ip: IpAddr, now_ms: u128) -> bool {
        self.prune(now_ms);
        let entry = self.windows.entry(ip).or_insert((now_ms, 0));
        if now_ms.saturating_sub(entry.0) >= FAILURE_WINDOW_MS {
            *entry = (now_ms, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 > MAX_FAILURES_PER_WINDOW
    }

    /// Whether the client is currently blocked, without recording anything.
    pub fn is_blocked(&self, ip: IpAddr, now_ms: u128) -> bool {
        match self.windows.get(&ip) {
            Some((start, count)) => {
                now_ms.saturating_sub(*start) < FAILURE_WINDOW_MS
                    && *count > MAX_FAILURES_PER_WINDOW
            }
            None => false,
        }
    }

    fn prune(&mut self, now_ms: u128) {
        self.windows
            .retain(|_, (start, _)| now_ms.saturating_sub(*start) < FAILURE_WINDOW_MS);
        // Evict the least-established entries instead of clearing the whole
        // map. Clearing let an address-rotating attacker overflow the map and
        // reset their own in-window block.
        //
        // The ordering is (already-blocked, count, age term). Blocked rows sort
        // last and are only evicted when nothing else is left. Below that,
        // lowest count goes first (the fresh single-hit rows a flood adds), with
        // the oldest start as the tie-break.
        //
        // The age term FLIPS inside the blocked class, and that is the whole
        // point. This limiter never calls `record_failure` once a row is
        // blocked: `auth_middleware` and the login handler both answer 429 on
        // `is_blocked` and return, so a blocked row is pinned at exactly
        // MAX_FAILURES_PER_WINDOW + 1 forever. Every blocked row therefore ties
        // on both leading terms, and an oldest-first tie-break would evict the
        // established block, which is always the oldest, letting an attacker
        // clear their own block by flooding rotated addresses to the same count.
        // Within the blocked class the FRESHEST block is evicted first instead,
        // so the established one survives a flood of newly blocked rows.
        while self.windows.len() > MAX_RATE_LIMIT_ENTRIES {
            let Some(victim) = self
                .windows
                .iter()
                .min_by_key(|(_, (start, count))| {
                    crate::ratelimit::eviction_key(*start, *count, MAX_FAILURES_PER_WINDOW)
                })
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.windows.remove(&victim);
        }
    }
}

/// How often the key file may be re-stat'ed on the ordinary request path.
pub const KEY_RELOAD_INTERVAL_MS: u128 = 60_000;

/// mtime in milliseconds plus byte length: enough to notice any edit the
/// atomic write produces (the temp-plus-rename always moves mtime), without
/// hashing the file on a request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    mtime_ms: u128,
    len: u64,
}

fn stamp_of(path: &std::path::Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Some(FileStamp {
        mtime_ms,
        len: meta.len(),
    })
}

/// SHA-256 of the key file's raw text, for the FORCED reload path only.
/// `(mtime, len)` alone is not enough there: `KeyRecord` serializes to a
/// fixed-width record (an 8-char id, a 64-char sha256 hex, a fixed-format
/// ISO-8601 timestamp), so a revoke immediately followed by an issue - the
/// exact rotation `apb server key` performs - produces a same-length file,
/// and on a filesystem with second-granularity mtimes that can land in the
/// same tick as the file it replaced. `FileStamp` would then see no change
/// at all and a revoked key would stay valid forever. The content hash
/// cannot miss that: any single differing byte changes it. Unlike the stamp,
/// this always costs an actual read, which is why it is reserved for the
/// forced path (see [`AuthState::verify_key_with_reload`]) and not run on
/// the throttled, once-a-minute background check.
fn content_hash_of(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|raw| server_auth::hash_hex(&raw))
}

/// The live key set plus what is needed to notice the file changing under it.
struct KeySet {
    keys: Vec<KeyRecord>,
    stamp: Option<FileStamp>,
    /// See [`content_hash_of`]. Compared only on a forced reload.
    content_hash: Option<String>,
    last_check_ms: u128,
}

/// Everything the auth layer needs, shared behind an `Arc` in [`AppState`].
///
/// The key set is live, not a startup snapshot: `server-auth.yaml` is
/// re-read and reloaded when it changes. On the ordinary request path (every
/// request, including exempt and cookie-authenticated ones) the check is
/// throttled to once per [`KEY_RELOAD_INTERVAL_MS`] and stat-only (mtime plus
/// length), so a busy server pays one cheap `stat` per minute there. A
/// presented bearer key forces a check on every attempt regardless of that
/// throttle (see [`AuthState::verify_key_with_reload`]), and that forced
/// check reads the file and compares a content hash rather than the stat
/// alone: a key that was just revoked is still sitting in the in-memory set
/// and would otherwise verify successfully against stale data, and a stat
/// alone is not reliable enough to catch it, because `KeyRecord` serializes
/// to a fixed-width record and a revoke immediately followed by an issue can
/// land inside the same mtime tick at the same file length (see
/// [`content_hash_of`]). So a bearer credential check now costs one small
/// file read, not just a stat - a few hundred bytes read per credential
/// check, not a hot-path cost; the parse-and-swap only runs when the hash
/// actually changed. A session-cookie check forces the same content-hash
/// reload for the same reason (see `evaluate`): a cookie-only workload would
/// otherwise never notice a same-tick revoke and could keep a revoked key's
/// sessions alive. That is what makes issuing a first key or revoking a
/// compromised one take effect on the very next request without a restart.
pub struct AuthState {
    /// The key file to watch. `None` in tests that do not exercise reloading
    /// and in [`AuthState::disabled`]; reload checks are then no-ops.
    path: Option<std::path::PathBuf>,
    keys: Mutex<KeySet>,
    trusted_proxies: BTreeSet<IpAddr>,
    public_https: bool,
    sessions: Mutex<SessionStore>,
    failures: Mutex<RateLimiter>,
    /// Set when the server was started bound to a non-loopback address (see
    /// [`AuthState::require_keys`]). `None` for the loopback default and for
    /// every test that does not exercise this path.
    required_bind: Option<IpAddr>,
    /// Whether the "auth is required but the key set is empty" state has
    /// already been logged, so the warning fires once per transition into
    /// that state rather than once per request. Reset the moment a key
    /// becomes available again.
    zero_keys_warned: AtomicBool,
}

impl AuthState {
    /// No keys and no file to watch: the historical local dashboard, and the
    /// default for the test harness.
    pub fn disabled() -> Self {
        Self {
            path: None,
            keys: Mutex::new(KeySet {
                keys: Vec::new(),
                stamp: None,
                content_hash: None,
                last_check_ms: 0,
            }),
            trusted_proxies: BTreeSet::new(),
            public_https: false,
            sessions: Mutex::new(SessionStore::default()),
            failures: Mutex::new(RateLimiter::default()),
            required_bind: None,
            zero_keys_warned: AtomicBool::new(false),
        }
    }

    /// Auth from the key file at `path` (already loaded into `keys` by the
    /// caller, which is how the bind interlock sees the count before the
    /// server starts) plus the `server:` config section.
    pub fn new(
        path: Option<std::path::PathBuf>,
        keys: Vec<KeyRecord>,
        cfg: &apb_core::config::ServerConfig,
    ) -> Result<Self, String> {
        let stamp = path.as_deref().and_then(stamp_of);
        let content_hash = path.as_deref().and_then(content_hash_of);
        Ok(Self {
            path,
            keys: Mutex::new(KeySet {
                keys,
                stamp,
                content_hash,
                last_check_ms: apb_core::clock::now_ms(),
            }),
            trusted_proxies: cfg.trusted_proxy_set()?,
            public_https: cfg.public_scheme_is_https(),
            sessions: Mutex::new(SessionStore::default()),
            failures: Mutex::new(RateLimiter::default()),
            required_bind: None,
            zero_keys_warned: AtomicBool::new(false),
        })
    }

    /// Marks the server as started bound to a non-loopback address: an empty
    /// key set must fail closed (every request refused, see
    /// [`AuthState::refuses_because_keyless`]) rather than fall back to the
    /// keyless local-dashboard pass-through. `check_bind_allowed` enforces the
    /// same precondition once, at startup; this is what keeps it enforced for
    /// the life of the process, since the key set can empty out at runtime
    /// (the last key revoked) without a restart. Loopback binds and the test
    /// harness never call this, so the historical default is unchanged there.
    pub fn require_keys(mut self, bind: IpAddr) -> Self {
        self.required_bind = Some(bind);
        self
    }

    fn key_set(&self) -> MutexGuard<'_, KeySet> {
        self.keys.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reloads the key file when it changed. `force` bypasses the
    /// once-per-minute throttle and switches from a stat-only comparison to
    /// a content-hash comparison (see [`content_hash_of`]); it is used on
    /// every bearer-credential check, which is exactly the case a stale key
    /// set produces the wrong answer for. A file that disappeared or became
    /// unreadable leaves the current key set in place: losing the file must
    /// not silently disable authentication.
    pub fn maybe_reload(&self, now_ms: u128, force: bool) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        {
            let set = self.key_set();
            if !force && now_ms.saturating_sub(set.last_check_ms) < KEY_RELOAD_INTERVAL_MS {
                return;
            }
        }
        if force {
            self.reload_by_content(path, now_ms);
        } else {
            self.reload_by_stamp(path, now_ms);
        }
        // A reload may have dropped a key; take its sessions down with it, so
        // revoking a compromised key does not leave week-long sessions behind.
        self.evict_dead_key_sessions();
    }

    /// Drops every live session whose minting key is no longer in the key set.
    /// Acquires the two locks in sequence, never nested, so it cannot deadlock
    /// against the request path (which also takes sessions then keys in turn).
    fn evict_dead_key_sessions(&self) {
        let live: BTreeSet<String> = {
            let set = self.key_set();
            set.keys.iter().map(|k| k.id.clone()).collect()
        };
        self.sessions().retain_live_keys(&live);
    }

    /// Whether `id` names a key currently in the live set. A browser session
    /// is only as valid as the key that minted it: the request path checks
    /// this on every cookie validation so a revoked key's sessions stop
    /// authenticating as soon as the key set reflects the revocation.
    pub fn key_id_is_live(&self, id: &str) -> bool {
        self.key_set().keys.iter().any(|k| k.id == id)
    }

    /// The ids of every key currently in the live set, in load order.
    pub fn live_key_ids(&self) -> Vec<String> {
        self.key_set().keys.iter().map(|k| k.id.clone()).collect()
    }

    /// The throttled, once-a-minute path: a bare `stat`, no file read. Good
    /// enough for the background check, whose only job is to notice an edit
    /// eventually; it is not what makes revocation take effect immediately
    /// (that is [`AuthState::reload_by_content`]).
    fn reload_by_stamp(&self, path: &std::path::Path, now_ms: u128) {
        let stamp = stamp_of(path);
        let mut set = self.key_set();
        set.last_check_ms = now_ms;
        if stamp == set.stamp {
            return;
        }
        match apb_core::server_auth::load_from(path) {
            Ok(file) => {
                set.keys = file.keys;
                set.stamp = stamp;
            }
            Err(e) => {
                // A malformed file at runtime keeps the previous key set: the
                // startup path already rejects a broken file, and an editing
                // slip must not open the server or lock the operator out.
                eprintln!("apb dashboard: keeping the current keys, {e}");
                set.stamp = stamp;
            }
        }
    }

    /// The forced path: reads the file and compares its content hash, not
    /// just its stat. A stat alone can miss a same-length, same-tick
    /// revoke-then-issue (see [`content_hash_of`]), and that is exactly the
    /// case this path exists to catch.
    /// All of the filesystem work (the read, the hash, the stat and the parse)
    /// happens OUTSIDE the keys mutex; the lock is taken only to compare the
    /// hash and swap the result in. Holding it across a synchronous read put
    /// blocking IO on a tokio worker while every other request path contended
    /// on that same mutex, and this path now runs for cookie-authenticated
    /// requests and for unauthenticated login attempts too, so the contention
    /// window was widening rather than shrinking.
    fn reload_by_content(&self, path: &std::path::Path, now_ms: u128) {
        let Some(raw) = std::fs::read_to_string(path).ok() else {
            // Unreadable or missing: keep the current key set and leave the
            // stamp/hash untouched, so a transient failure here does not mask
            // a real change once the file becomes readable again. The throttle
            // is still stamped, so this cannot spin.
            self.key_set().last_check_ms = now_ms;
            return;
        };
        let hash = server_auth::hash_hex(&raw);
        {
            let mut set = self.key_set();
            set.last_check_ms = now_ms;
            if set.content_hash.as_deref() == Some(hash.as_str()) {
                return;
            }
        }
        // Parse the bytes already in hand rather than re-reading the file: a
        // second read could see different content than the hash was taken over.
        let parsed = apb_core::server_auth::parse_keys(&raw, path);
        let stamp = stamp_of(path);
        let mut set = self.key_set();
        match parsed {
            Ok(file) => {
                set.keys = file.keys;
                set.stamp = stamp;
                set.content_hash = Some(hash);
            }
            Err(e) => {
                // A malformed file keeps the previous key set, exactly as the
                // stamp path does: an editing slip must not open the server or
                // lock the operator out.
                eprintln!("apb dashboard: keeping the current keys, {e}");
                set.stamp = stamp;
                set.content_hash = Some(hash);
            }
        }
    }

    /// Auth is enforced if and only if at least one key exists.
    pub fn enabled(&self) -> bool {
        !self.key_set().keys.is_empty()
    }

    /// True exactly when the server requires at least one key (started bound
    /// to a non-loopback address, [`AuthState::require_keys`]) but the
    /// currently loaded key set is empty: `check_bind_allowed` only checks
    /// this precondition once, at startup, so this is the runtime re-check
    /// that stops a revoke-to-zero from silently reopening the keyless
    /// pass-through to the network. Logs the transition into this state
    /// exactly once (`apb auth_disabled_refused bind=<addr>`, on stderr), not
    /// once per request; the warning re-arms the moment a key is available
    /// again.
    pub fn refuses_because_keyless(&self) -> bool {
        let Some(bind) = self.required_bind else {
            return false;
        };
        if self.enabled() {
            self.zero_keys_warned.store(false, Ordering::Relaxed);
            return false;
        }
        if !self.zero_keys_warned.swap(true, Ordering::Relaxed) {
            eprintln!("apb auth_disabled_refused bind={bind}");
        }
        true
    }

    pub fn key_count(&self) -> usize {
        self.key_set().keys.len()
    }

    /// The id of the key `presented` is, or `None`.
    pub fn verify_key(&self, presented: &str) -> Option<String> {
        let set = self.key_set();
        server_auth::verify(&set.keys, presented)
    }

    /// Force-checks the key file's content, then verifies. A key that was
    /// just revoked can still be sitting in the in-memory set (it has not
    /// failed verification yet, so nothing would otherwise trigger a
    /// reload), so a "verify first, force-reload only on failure" order can
    /// never reject a stale-but-still-cached key: the very case revocation
    /// needs to hit. The forced check reads the file and compares a content
    /// hash rather than just its `stat`, because a same-length
    /// revoke-then-issue can land in the same mtime tick (see
    /// [`content_hash_of`]) and a stat-only comparison would miss it. A
    /// bearer credential is comparatively rare next to every other request
    /// (session cookies never take this path), so forcing this read here on
    /// every bearer attempt is a few hundred bytes read per credential
    /// check, not a per-request cost; the parse-and-replace only runs when
    /// the content actually changed. That is what makes issuing a first key
    /// or revoking a compromised one take effect on the very next request
    /// rather than on the next restart, or up to a minute later.
    pub fn verify_key_with_reload(&self, presented: &str, now_ms: u128) -> Option<String> {
        self.maybe_reload(now_ms, true);
        self.verify_key(presented)
    }

    /// Poison-tolerant: the guarded data is a cache of live sessions, not an
    /// invariant a panicking thread could have corrupted in a way that matters.
    pub fn sessions(&self) -> MutexGuard<'_, SessionStore> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn failures(&self) -> MutexGuard<'_, RateLimiter> {
        self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Per-request facts the auth layer derives once and hands to the endpoints
/// through request extensions: the client IP used for rate limiting and
/// logging, and whether the browser reached apb over https. Forwarded headers
/// are honored only when the socket peer is a configured trusted proxy, and
/// never for an authentication decision.
#[derive(Debug, Clone, Copy)]
pub struct ClientCtx {
    pub ip: IpAddr,
    pub https: bool,
}

/// Derives [`ClientCtx`]. `peer` is `None` when the router is driven in
/// process without a socket (the integration-test harness), which is treated
/// as a loopback client.
///
/// The RIGHTMOST `X-Forwarded-For` entry is used, not the leftmost. A reverse
/// proxy appends its own view of the peer to whatever header the client sent,
/// and Caddy's `reverse_proxy` does exactly that by default, so the leftmost
/// entries are attacker-supplied and spoofable while the last one is the only
/// entry the trusted proxy wrote itself. Taking the leftmost entry would let
/// any caller pick its own rate-limit key and forge the IP in the
/// `auth_failed` log line.
pub fn client_ctx(auth: &AuthState, headers: &HeaderMap, peer: Option<IpAddr>) -> ClientCtx {
    let peer = peer.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let trusted = auth.trusted_proxies.contains(&peer);
    let ip = if trusted {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
            .unwrap_or(peer)
    } else {
        peer
    };
    let forwarded_https = trusted
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().eq_ignore_ascii_case("https"))
            .unwrap_or(false);
    ClientCtx {
        ip,
        https: forwarded_https || auth.public_https,
    }
}

/// How a request proved its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    None,
    /// `Authorization: Bearer apb_...`, used by the CLI, scripts and CI. A
    /// browser cannot attach it cross-site, so it is exempt from the CSRF
    /// marker requirement.
    Bearer,
    /// The `apb_session` cookie minted by the login endpoint.
    Cookie,
}

/// Evaluates the credentials on a request as a union: a valid bearer key
/// passes, otherwise a live session cookie passes, and only when neither is
/// present and valid is the request unauthenticated. A stale bearer header
/// therefore never blocks a browser that holds a good session.
pub fn evaluate(auth: &AuthState, headers: &HeaderMap, now_ms: u128) -> Credential {
    if let Some(raw) = bearer_token(headers)
        && auth.verify_key_with_reload(&raw, now_ms).is_some()
    {
        return Credential::Bearer;
    }
    if let Some(token) = cookie_value(headers, SESSION_COOKIE) {
        let hash = server_auth::hash_hex(&token);
        let key_id = {
            let mut sessions = auth.sessions();
            sessions.touch(&hash, now_ms)
        };
        if let Some(key_id) = key_id {
            // Force one content-hash reload before trusting the session's key.
            // A cookie-only workload never triggers the bearer path's forced
            // reload, and the throttled stat-only background reload cannot
            // notice a same-length revoke+issue that lands inside one mtime tick
            // on a coarse-mtime filesystem (see `content_hash_of`). Without this
            // the in-memory key set could stay stale indefinitely and keep a
            // revoked key's browser sessions alive. This costs one small file
            // read per cookie request; the reload re-parses only when the
            // content hash actually changed, and it evicts dead-key sessions, so
            // a genuine revoke has already dropped this session before the
            // re-check.
            auth.maybe_reload(now_ms, true);
            if auth.key_id_is_live(&key_id) {
                return Credential::Cookie;
            }
            // The key that minted this session has been revoked: drop the
            // session so it cannot outlive the credential that authorized it.
            auth.sessions().remove(&hash);
        }
    }
    Credential::None
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let token = rest.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// One named value out of the raw `Cookie` header. No cookie crate: the header
/// is a semicolon-separated list of `name=value` pairs and apb only ever reads
/// its own session cookie out of it.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for raw in headers.get_all(header::COOKIE) {
        let Ok(text) = raw.to_str() else {
            continue;
        };
        for part in text.split(';') {
            if let Some((k, v)) = part.trim().split_once('=')
                && k.trim() == name
            {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Paths reachable without a credential: the health probe, the two endpoints
/// the login screen itself needs, the run-hook endpoint (authenticated by its
/// own path secret), and everything outside `/api/`, which is the embedded SPA
/// shell that has to render the login screen in the first place.
///
/// WARNING: the `!path.starts_with("/api/")` arm is a blanket exemption for
/// everything the static fallback serves. Any future route mounted under a
/// prefix other than `/api/` (a metrics endpoint, a second API version, a
/// download path) would silently bypass authentication entirely. Adding one is
/// a deliberate decision that has to be made here, in this predicate, and
/// covered by a test, not left to whoever adds the route.
pub fn is_exempt(method: &Method, path: &str) -> bool {
    if !path.starts_with("/api/") {
        return true;
    }
    match path {
        "/api/health" => method == Method::GET,
        "/api/auth/login" => method == Method::POST,
        "/api/auth/status" => method == Method::GET,
        _ => path.starts_with("/api/hooks/") && method == Method::POST,
    }
}

/// One stable, greppable line per auth failure, on stderr so a systemd unit
/// carries it into the journal for fail2ban. Successful auth is not logged per
/// request: that would be one line per asset fetch and would tell an operator
/// nothing.
pub fn log_auth_failure(ip: IpAddr, path: &str) {
    eprintln!("apb auth_failed ip={ip} path={path}");
}

pub(crate) fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "auth" })),
    )
        .into_response()
}

pub(crate) fn forbidden_csrf() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "error": "csrf" })),
    )
        .into_response()
}

pub(crate) fn rate_limited() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({ "error": "rate_limited" })),
    )
        .into_response()
}

fn is_safe_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

fn has_csrf_marker(headers: &HeaderMap) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == CSRF_VALUE)
        .unwrap_or(false)
}

/// Sets `X-Frame-Options: DENY` on a response. Applied to every response the
/// dashboard listener produces, whether or not auth is enabled: an
/// RCE-equivalent control panel must never be frameable, and that must not
/// depend on the operator having configured the header at the proxy.
fn deny_framing(mut res: Response) -> Response {
    res.headers_mut().insert(
        axum::http::header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("DENY"),
    );
    res
}

/// The gate. Runs for every request, including static assets, so that
/// [`ClientCtx`] is always available downstream and every response carries the
/// frame-protection header.
///
/// Extracts only the [`AuthState`] substate (via `FromRef<AppState>` in
/// `crate::state`), not the whole `AppState`: this module has no business
/// needing the rest of the app state, and importing the full type back here
/// would put `auth` and `state` in a dependency cycle.
pub async fn auth_middleware(
    State(auth): State<Arc<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let now = apb_core::clock::now_ms();
    let ctx = client_ctx(&auth, req.headers(), peer);
    req.extensions_mut().insert(ctx);

    // Throttled to once per minute, so a busy server pays one `stat` per
    // minute and nothing else. This is what lets the FIRST key issued on a
    // running dashboard turn authentication on: with no keys nothing ever
    // fails, so the forced reload on the failure path would never trigger.
    auth.maybe_reload(now, false);

    // Fail closed ahead of the ordinary keyless pass-through: a server that
    // was started bound to a non-loopback address must never let an
    // in-flight revocation of the last key reopen that pass-through. This
    // check runs before is_exempt on purpose - a server in this state
    // refuses everything, including the health probe and the static shell,
    // so the misconfiguration is impossible to miss rather than merely
    // gating the API surface.
    if auth.refuses_because_keyless() {
        return deny_framing(unauthorized());
    }

    if !auth.enabled() {
        return deny_framing(next.run(req).await);
    }

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if is_exempt(&method, &path) {
        return deny_framing(next.run(req).await);
    }

    // Verify first, then rate-limit. A valid bearer key or session cookie
    // always passes, whatever the limiter's state for this IP: the failure
    // limiter exists to blunt online guessing of a 256-bit key, not to lock
    // out legitimate users, and blocking before verifying would let eleven bad
    // requests from a shared NAT/proxy address 429 every real operator behind
    // it. The limiter is consulted and fed only when the credential is absent
    // or invalid, so a guesser (whose every attempt is invalid) is still
    // counted and eventually throttled.
    let credential = evaluate(&auth, req.headers(), now);
    let res = match credential {
        Credential::None => {
            let blocked = {
                let failures = auth.failures();
                failures.is_blocked(ctx.ip, now)
            };
            if blocked {
                rate_limited()
            } else {
                let over_budget = {
                    let mut failures = auth.failures();
                    failures.record_failure(ctx.ip, now)
                };
                log_auth_failure(ctx.ip, &path);
                if over_budget {
                    rate_limited()
                } else {
                    unauthorized()
                }
            }
        }
        Credential::Cookie if !is_safe_method(&method) && !has_csrf_marker(req.headers()) => {
            forbidden_csrf()
        }
        _ => next.run(req).await,
    };
    deny_framing(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn state_with_proxy(proxy: &str, public: Option<&str>) -> AuthState {
        let cfg = apb_core::config::ServerConfig {
            bind: None,
            public_base_url: public.map(|s| s.to_string()),
            trusted_proxies: vec![proxy.to_string()],
        };
        AuthState::new(None, Vec::new(), &cfg).unwrap()
    }

    #[test]
    fn forwarded_headers_are_ignored_from_an_untrusted_peer() {
        let auth = state_with_proxy("10.0.0.1", None);
        let h = headers(&[
            ("x-forwarded-for", "203.0.113.9"),
            ("x-forwarded-proto", "https"),
        ]);
        let ctx = client_ctx(&auth, &h, Some("198.51.100.4".parse().unwrap()));
        assert_eq!(ctx.ip, "198.51.100.4".parse::<IpAddr>().unwrap());
        assert!(!ctx.https, "an untrusted peer cannot claim https");
    }

    #[test]
    fn forwarded_headers_are_honored_from_a_trusted_peer() {
        let auth = state_with_proxy("10.0.0.1", None);
        // The client sent "6.6.6.6" itself and the trusted proxy appended the
        // address it actually saw. Only the appended, rightmost entry may be
        // believed.
        let h = headers(&[
            ("x-forwarded-for", "6.6.6.6, 1.2.3.4"),
            ("x-forwarded-proto", "https"),
        ]);
        let ctx = client_ctx(&auth, &h, Some("10.0.0.1".parse().unwrap()));
        assert_eq!(
            ctx.ip,
            "1.2.3.4".parse::<IpAddr>().unwrap(),
            "the rightmost entry is the one the trusted proxy wrote"
        );
        assert!(ctx.https);

        // A single-entry header behaves the same way: that one entry is the
        // proxy's own observation.
        let single = headers(&[("x-forwarded-for", "1.2.3.4")]);
        let ctx = client_ctx(&auth, &single, Some("10.0.0.1".parse().unwrap()));
        assert_eq!(ctx.ip, "1.2.3.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn a_public_https_base_url_alone_marks_the_scheme() {
        let auth = state_with_proxy("10.0.0.1", Some("https://apb.example.com"));
        let ctx = client_ctx(&auth, &HeaderMap::new(), Some("10.0.0.1".parse().unwrap()));
        assert!(ctx.https);
    }

    #[test]
    fn a_missing_peer_falls_back_to_loopback() {
        let auth = AuthState::disabled();
        let ctx = client_ctx(&auth, &HeaderMap::new(), None);
        assert_eq!(ctx.ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(!ctx.https);
    }

    #[test]
    fn cookie_values_are_read_out_of_a_multi_pair_header() {
        let h = headers(&[("cookie", "theme=dark; apb_session=abc123; other=1")]);
        assert_eq!(cookie_value(&h, SESSION_COOKIE).as_deref(), Some("abc123"));
        assert_eq!(cookie_value(&h, "absent"), None);
    }

    #[test]
    fn exempt_paths_are_exactly_the_documented_set() {
        assert!(is_exempt(&Method::GET, "/"));
        assert!(is_exempt(&Method::GET, "/assets/index.js"));
        assert!(is_exempt(&Method::GET, "/api/health"));
        assert!(is_exempt(&Method::POST, "/api/auth/login"));
        assert!(is_exempt(&Method::GET, "/api/auth/status"));
        assert!(is_exempt(&Method::POST, "/api/hooks/run-1/secret"));
        assert!(!is_exempt(&Method::POST, "/api/auth/logout"));
        assert!(!is_exempt(&Method::GET, "/api/runs"));
        assert!(!is_exempt(&Method::GET, "/api/ws"));
        assert!(!is_exempt(&Method::POST, "/api/health"));
    }

    #[test]
    fn sessions_expire_and_evict() {
        let mut store = SessionStore::default();
        store.insert("a".to_string(), 0, "k1".to_string());
        assert_eq!(store.touch("a", 1_000).as_deref(), Some("k1"));
        assert!(
            store.touch("a", SESSION_TTL_MS + 2_000).is_none(),
            "a session past its sliding TTL is dead"
        );
        assert!(store.is_empty(), "and is dropped on the way out");

        for i in 0..MAX_SESSIONS {
            store.insert(format!("s{i}"), 1_000 + i as u128, "k1".to_string());
        }
        assert_eq!(store.len(), MAX_SESSIONS);
        store.insert("newest".to_string(), 9_999_999, "k1".to_string());
        assert_eq!(store.len(), MAX_SESSIONS, "the cap holds");
        assert!(
            store.touch("s0", 9_999_999).is_none(),
            "the oldest was evicted"
        );
    }

    #[test]
    fn a_session_dies_with_the_key_that_minted_it() {
        let mut store = SessionStore::default();
        store.insert("sess".to_string(), 0, "key-a".to_string());
        // While key-a is live the session validates.
        let mut live = BTreeSet::new();
        live.insert("key-a".to_string());
        store.retain_live_keys(&live);
        assert_eq!(store.touch("sess", 1_000).as_deref(), Some("key-a"));
        // Revoke key-a: it leaves the live set, so the session is dropped.
        store.retain_live_keys(&BTreeSet::new());
        assert!(
            store.touch("sess", 2_000).is_none(),
            "a session must not outlive the key that minted it"
        );
    }

    /// Locks in that only a presented bearer credential forces a key reload.
    /// `evaluate` guards `verify_key_with_reload` behind `bearer_token(...)`
    /// being present, so a credential-less request and a cookie-only request
    /// must never reach it. Proven black-box: the key file is revoked on disk
    /// after `AuthState` is constructed, then both non-bearer shapes are
    /// evaluated; if either had forced a reload, the revoked key would
    /// already be gone from the in-memory set by the final check. A future
    /// refactor that widens the force-reload trigger to every request (not
    /// just a presented bearer key) would make this test fail.
    #[test]
    fn non_bearer_requests_never_force_a_key_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server-auth.yaml");
        let (key_a, record_a) = server_auth::issue_into(&path).unwrap();
        let file = server_auth::load_from(&path).unwrap();
        let auth = AuthState::new(
            Some(path.clone()),
            file.keys,
            &apb_core::config::ServerConfig::default(),
        )
        .unwrap();
        let now = apb_core::clock::now_ms();

        // Revoke key A on disk, underneath the running AuthState, without
        // going through anything that would itself force a reload.
        server_auth::revoke_in(&path, &record_a.id).unwrap();

        let no_credential = HeaderMap::new();
        assert_eq!(evaluate(&auth, &no_credential, now), Credential::None);

        let cookie_only = headers(&[("cookie", "apb_session=not-a-real-session")]);
        assert_eq!(evaluate(&auth, &cookie_only, now), Credential::None);

        assert_eq!(
            auth.verify_key(&key_a),
            Some(record_a.id),
            "neither call should have forced a reload that would drop the \
             revoked key from the in-memory set"
        );
    }

    /// Issues a key whose id is not an all-digit string, revoking and retrying
    /// until it gets one.
    ///
    /// A `KeyRecord` serializes to a fixed-width record, which is what makes a
    /// revoke-then-issue reproduce the same file length. The one exception is
    /// the id: it is the first 8 hex chars of the hash, and when those happen to
    /// be all digits the YAML writer quotes the value to preserve its string
    /// type, adding two bytes. Two keys that disagree on that make the file
    /// lengths differ for a reason that has nothing to do with what these tests
    /// are about, so the ids are pinned to the unquoted form instead of the
    /// same-length precondition being left to a coin flip.
    fn issue_unquoted_id(path: &std::path::Path) -> (String, apb_core::server_auth::KeyRecord) {
        loop {
            let (key, record) = server_auth::issue_into(path).unwrap();
            if !record.id.bytes().all(|b| b.is_ascii_digit()) {
                return (key, record);
            }
            server_auth::revoke_in(path, &record.id).unwrap();
        }
    }

    /// A same-length content change inside the same mtime tick is exactly
    /// what `(mtime, len)` alone cannot see: `KeyRecord` serializes to a
    /// fixed-width record, so a revoke immediately followed by an issue - the
    /// real rotation `apb server key` performs - leaves a single-key file the
    /// same byte length as before. The mtime is pinned back explicitly here
    /// (rather than relying on both writes racing inside the same real-world
    /// millisecond) to make the collision deterministic. The forced reload
    /// path must still catch it via the content hash.
    #[test]
    fn a_same_length_change_within_one_mtime_tick_is_still_caught_by_the_forced_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server-auth.yaml");
        let (key_a, record_a) = issue_unquoted_id(&path);
        let file = server_auth::load_from(&path).unwrap();
        let auth = AuthState::new(
            Some(path.clone()),
            file.keys,
            &apb_core::config::ServerConfig::default(),
        )
        .unwrap();

        let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let original_len = std::fs::metadata(&path).unwrap().len();

        server_auth::revoke_in(&path, &record_a.id).unwrap();
        let (key_b, _record_b) = issue_unquoted_id(&path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            original_len,
            "a single-key file stays the same length across a revoke+issue"
        );

        // Pin the mtime back to the original value: same length, same mtime,
        // different content. `FileStamp` alone would see no change at all.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(original_mtime).unwrap();

        let now = apb_core::clock::now_ms();
        assert_eq!(
            auth.verify_key_with_reload(&key_a, now),
            None,
            "the revoked key must not still verify just because the stamp is unchanged"
        );
        assert!(
            auth.verify_key_with_reload(&key_b, now).is_some(),
            "the newly issued key must verify once the content hash is checked"
        );
    }

    #[test]
    fn the_limiter_trips_after_the_budget_and_resets_next_window() {
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let mut limiter = RateLimiter::default();
        for _ in 0..MAX_FAILURES_PER_WINDOW {
            assert!(!limiter.record_failure(ip, 1_000));
        }
        assert!(limiter.record_failure(ip, 1_000), "the eleventh trips it");
        assert!(limiter.is_blocked(ip, 1_500));
        assert!(
            !limiter.is_blocked(ip, 1_000 + FAILURE_WINDOW_MS + 1),
            "the window is fixed, not sliding"
        );
        assert!(
            !limiter.is_blocked("198.51.100.4".parse().unwrap(), 1_500),
            "the block is per client IP"
        );
    }

    /// Overflowing the entry cap must not reset a still-in-window blocked IP.
    /// The old clear-all let an address-rotating attacker flush their own block
    /// by pushing the map past the cap; evicting the least-established rows
    /// keeps the blocked address blocked.
    #[test]
    fn overflowing_the_limiter_does_not_reset_a_blocked_ip() {
        let mut limiter = RateLimiter::default();
        let now = 1_000u128;
        let target: IpAddr = "203.0.113.7".parse().unwrap();
        for _ in 0..=MAX_FAILURES_PER_WINDOW {
            limiter.record_failure(target, now);
        }
        assert!(limiter.is_blocked(target, now), "the target starts blocked");

        // A flood of distinct fresh addresses (one failure each), the way an
        // IPv6-rotating attacker would push the map past the cap.
        let base: u128 = 0x2001_0db8_0000_0000_0000_0000_0000_0000;
        for i in 0..(MAX_RATE_LIMIT_ENTRIES as u128 + 50) {
            let ip = IpAddr::V6(std::net::Ipv6Addr::from(base + i));
            limiter.record_failure(ip, now);
        }

        assert!(
            limiter.is_blocked(target, now),
            "overflowing the cap must not reset a still-in-window blocked IP"
        );
    }

    /// The harder case: a flood that drives every rotated address to EXACTLY
    /// the failure budget, so count alone no longer separates them from a
    /// blocked row. Without the blocked-last term in the eviction ordering, the
    /// oldest-start tie-break would evict the blocked row first, since a
    /// blocked row is always the oldest.
    #[test]
    fn an_equal_count_flood_still_does_not_evict_a_blocked_row() {
        let mut limiter = RateLimiter::default();
        let target: IpAddr = "203.0.113.7".parse().unwrap();
        // The blocked row is recorded FIRST, so it holds the oldest window
        // start of every row in the map.
        let early = 1_000u128;
        for _ in 0..=MAX_FAILURES_PER_WINDOW {
            limiter.record_failure(target, early);
        }
        assert!(limiter.is_blocked(target, early));

        // Every flood row reaches exactly MAX_FAILURES_PER_WINDOW: at the
        // budget, but not over it, so none of them is blocked.
        let later = early + 1;
        let base: u128 = 0x2001_0db8_0000_0000_0000_0000_0000_0000;
        for i in 0..(MAX_RATE_LIMIT_ENTRIES as u128 + 50) {
            let ip = IpAddr::V6(std::net::Ipv6Addr::from(base + i));
            for _ in 0..MAX_FAILURES_PER_WINDOW {
                limiter.record_failure(ip, later);
            }
        }

        assert!(
            limiter.is_blocked(target, later),
            "an equal-count flood must not evict the blocked row it ties with"
        );
    }

    /// The case the previous test just missed, and the one that actually
    /// mattered. This limiter stops counting the moment a row crosses the
    /// budget: `auth_middleware` and the login handler both answer 429 on
    /// `is_blocked` and return without recording. Every blocked row is therefore
    /// pinned at exactly MAX_FAILURES_PER_WINDOW + 1, so a flood driven one step
    /// FURTHER than the previous test (to blocked, not merely to the budget)
    /// ties with the victim on both the blocked term and the count term. Only
    /// the flipped age term inside the blocked class separates them.
    #[test]
    fn a_flood_of_equally_blocked_rows_does_not_evict_the_established_block() {
        let mut limiter = RateLimiter::default();
        let target: IpAddr = "203.0.113.7".parse().unwrap();
        let early = 1_000u128;
        let later = early + 1;

        // Built directly rather than through `record_failure`, so every row is
        // already at the pinned blocked count when the eviction runs. Driving
        // them up one call at a time would let each in-progress row be evicted
        // as the lowest-count entry before it ever reached the threshold, which
        // is correct behavior but would exercise a different case than the tie
        // this test is about.
        //
        // The target is the oldest row in the map: the exact one an
        // oldest-first tie-break would have picked.
        limiter
            .windows
            .insert(target, (early, MAX_FAILURES_PER_WINDOW + 1));
        let base: u128 = 0x2001_0db8_0000_0000_0000_0000_0000_0000;
        for i in 0..(MAX_RATE_LIMIT_ENTRIES as u128 + 50) {
            limiter.windows.insert(
                IpAddr::V6(std::net::Ipv6Addr::from(base + i)),
                (later, MAX_FAILURES_PER_WINDOW + 1),
            );
        }

        limiter.prune(later);

        assert!(
            limiter.windows.len() <= MAX_RATE_LIMIT_ENTRIES,
            "back within the cap"
        );
        assert!(
            limiter.is_blocked(target, later),
            "a flood of equally blocked rows must shed its own fresh blocks, \
             never the established one"
        );
    }
}
