# Server Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an operator run the apb dashboard on a networked machine behind a reverse proxy, protected by up to two issued API keys, with a browser login that exchanges a key for an HttpOnly session cookie, while the loopback-only local experience stays byte-for-byte unchanged.

**Architecture:** `apb-core` gains a `server_auth` module (key file, issue, verify, revoke, constant-time compare, CSPRNG tokens) and a `server:` section on `GlobalConfig`. `apb-server` gains an `auth` module (an axum middleware over the whole router, an in-memory session store, and an auth-failure rate limiter) plus three `/api/auth/*` endpoints, and its `run_server` takes a bind address with a hard non-loopback-without-keys interlock. The frontend gains one auth store, a fetch layer that marks the CSRF header and reacts to 401, and a login screen `App.svelte` renders in place of the router when the server says auth is required and the browser is not authenticated.

**Tech Stack:** Rust workspace (edition 2024, workspace-inherited deps), axum 0.8 (`middleware::from_fn_with_state`, `ConnectInfo`), `sha2`, `subtle`, `getrandom`, `base64`, `serde_yaml_ng`; svelte 5 (runes), shadcn-svelte components, vitest.

**Spec:** docs/superpowers/specs/2026-08-16-server-mode-design.md

## Global Constraints

- The spec is settled. Where the spec decides something, implement it; do not redesign it.
- No em-dashes (U+2014) and no exclamation marks anywhere in docs, code comments, or user-facing strings. No CJK anywhere.
- Machine-facing fields and identifiers are English; the JSON error codes are exactly `auth`, `csrf`, `rate_limited`.
- New direct dependencies: `sha2` (already a workspace dep), `subtle`, `getrandom`, `base64`. Verify the latest stable version of each against crates.io at implementation time and pin the major/minor in `[workspace.dependencies]`; the code in this plan uses `getrandom::fill`, `base64::engine::general_purpose::URL_SAFE_NO_PAD`, and `subtle::ConstantTimeEq`, all stable APIs. If a fetched version renamed any of them, adapt the call and note it in the commit message.
- Every state file is written through `apb_core::fsutil::atomic_write_private` (temp plus fsync plus rename, mode 0600 on unix). `server-auth.yaml` is never written any other way.
- Secrets are never logged: the plaintext API key is printed exactly once by `apb server key issue` and never written to a log, an error message, or a response body. Session tokens exist in memory only as SHA-256 hashes plus the one Set-Cookie header that mints them.
- No std `MutexGuard` may be held across an `.await` in `apb-server` (clippy `await_holding_lock` is denied). Every lock in the auth path is taken and dropped inside a plain block before any await.
- Work on a feature branch (for example `feat/server-mode`); never commit to local `main`.
- Every commit uses `git commit --signoff` (the DCO bot blocks unsigned commits) and ends the message with the trailer `Co-Authored-By: Claude <model> <noreply@anthropic.com>` where `<model>` is the implementer's own model name.
- Per task, before the commit step: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings` must both be clean.
- For tasks touching `web/`: `bun run test` and `bun run check` must both be clean in `web/`.
- Do not push, publish, tag, or open a PR. Everything stays local until the owner approves.

## Design decisions (settled, do not reopen)

- **`GET /api/health` is left alone.** The spec describes it as returning `{ok:true}`; the handler at `crates/apb-server/src/routes/meta.rs:5-7` actually returns `{"status":"ok"}` and nothing else. The spec's instruction was to verify it leaks no other state and trim it if it does, and it does not, so the shape stays as it is. Nothing in `web/` or the docs reads it, so changing it would only churn.
- **The key set is live, not a startup snapshot.** `AuthState` holds the loaded records plus the key file's `(mtime, length)`. It re-stats the file at most once per 60 seconds on the ordinary request path, so a busy server pays one `stat` per minute and every other request is filesystem-free, and it re-stats immediately (throttle bypassed) whenever a presented key fails to verify, which is exactly the moment a stale set gives the wrong answer. Issuing the first key, rotating, or revoking a compromised key therefore takes effect within a minute at worst and immediately after any failed request, with no dashboard restart. A file that vanished or became malformed at runtime keeps the current key set and logs one line: losing the file must neither open the server nor lock the operator out. `apb server key issue` and `apb server key revoke` say this, and `docs/DEPLOYMENT.md` repeats it.
- **A malformed key file is a startup error, never "no keys".** `server_auth::load_from` validates each record's `sha256` field and rejects unknown fields, and `run_server` propagates that error. A typo can never quietly disable authentication.
- **`created_at` is UTC ISO-8601 produced by `apb_core::dismiss::iso_utc(apb_core::clock::now_ms_u64())`.** `apb_core::clock` is the single wall-clock source the spec names; `iso_utc` is the repository's existing, dependency-free formatter for exactly this shape. No date crate is added.
- **The rate limiter and the session store live only in `apb-server`.** `apb-core::server_auth` owns the file format, the hashing, the CSPRNG, and the constant-time compare, so `apb-server` gains no new crate dependency at all and the workspace's dependency direction (core, then engine, then mcp, with cli and server on top) is unchanged.
- **Token-in-query-param is not implemented for the WebSocket**, per the spec: the browser sends the cookie on the upgrade request automatically, and non-browser clients send the bearer header.
- **Session lookup is a HashMap, and that is a documented exception to the constant-time rule.** API keys are verified with `ct_eq_str` in a linear scan over the stored records; session tokens are looked up by their SHA-256 hex in a `HashMap`, which is an ordinary hash-and-compare and not constant time. This is deliberate: the looked-up value is already a one-way hash of a 256-bit CSPRNG token, so any timing signal is about the hash and yields no usable preimage. The exception is stated in the `auth.rs` module doc so a later reader does not "fix" it into a linear scan.
- **Credential evaluation is a union, not a strict sequence.** The spec numbers bearer as step 3 and cookie as step 4, which reads as sequential. `evaluate` tries the bearer header first and, independently, falls through to the cookie whenever the bearer is absent or does not verify; 401 comes only when both fail. That is strictly more robust than a literal sequential reading (a stale bearer header alongside a live session would otherwise reject a legitimate browser) and matches the spec's intent that either credential is sufficient.
- **A rate-limited client stays rate-limited for the window even with a valid credential.** The block is checked before the credential is read, so once an IP is over budget every non-exempt request from it answers 429 until the window rolls over, valid key or not. This is intentional: the limiter exists to make online guessing expensive, and letting a correct guess immediately escape the block would defeat that. Exempt paths (health, login, status, hooks, static) remain reachable, and the window is one minute, so the operator cost is bounded. A test in Task 4 pins this behavior.

---

### Task 1: apb-core `server_auth` module

**Files:**
- Modify: `Cargo.toml` (add `base64`, `getrandom`, `subtle` to `[workspace.dependencies]`, appended after the existing `globset = "0.4"` line; note that `globset` sits mid-list, with the connector-related entries below it, so "after globset" is a position, not the end of the table)
- Modify: `crates/apb-core/Cargo.toml` (add the three `.workspace = true` lines to `[dependencies]`)
- Modify: `crates/apb-core/src/lib.rs` (add `pub mod server_auth;` between `pub mod scope;` and `pub mod skills;`)
- Create: `crates/apb-core/src/server_auth.rs`
- Test: create `crates/apb-core/tests/suite/server_auth_test.rs`, register it in `crates/apb-core/tests/main.rs`

**Interfaces:**
- Consumes: `apb_core::fsutil::atomic_write_private(&Path, &[u8]) -> io::Result<()>`, `apb_core::fsutil::lock_dir(&Path, &str) -> io::Result<DirLock>`, `apb_core::content::hex_lower(&[u8]) -> String`, `apb_core::clock::now_ms_u64() -> u64`, `apb_core::dismiss::iso_utc(u64) -> String`, `apb_core::config::config_dir() -> Option<PathBuf>`.
- Produces: `pub struct KeyRecord { id, sha256, created_at }`, `pub struct AuthFile { keys: Vec<KeyRecord> }`, `pub enum AuthError`, and the functions `auth_file_path`, `load`, `load_from`, `issue`, `issue_into`, `revoke`, `revoke_in`, `verify`, `hash_hex`, `random_token`, `ct_eq_str`, plus the constants `KEY_PREFIX`, `MAX_KEYS`, `AUTH_FILE`. Tasks 3 to 6 depend on these exact names.

- [ ] **Step 1: add the dependencies**

In the root `Cargo.toml`, append to `[workspace.dependencies]` (after the `globset = "0.4"` line):

```toml
# Server-mode API keys and session tokens (spec 2026-08-16-server-mode-design):
# CSPRNG bytes, unpadded base64url encoding, and a constant-time comparison for
# everything derived from a secret. No password-hash crate: these are 256-bit
# random tokens, not user-chosen passwords.
base64 = "0.22"
getrandom = "0.3"
subtle = "2.6"
```

In `crates/apb-core/Cargo.toml`, add to `[dependencies]` after `globset.workspace = true`:

```toml
base64.workspace = true
getrandom.workspace = true
subtle.workspace = true
```

- [ ] **Step 2: write the failing tests**

Create `crates/apb-core/tests/suite/server_auth_test.rs`:

```rust
//! `apb_core::server_auth`: the server-mode API key file. Every test drives
//! the path-taking API (`issue_into`, `load_from`, `revoke_in`) against a
//! tempdir, so none of them touches process env and none needs the shared
//! env lock.

use apb_core::server_auth::{self, KEY_PREFIX, MAX_KEYS};

fn key_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("server-auth.yaml")
}

#[test]
fn issue_then_verify_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_path(&dir);
    let (plain, record) = server_auth::issue_into(&path).unwrap();
    assert!(plain.starts_with(KEY_PREFIX), "key carries the prefix: {plain}");
    assert_eq!(
        plain.len(),
        KEY_PREFIX.len() + 43,
        "32 CSPRNG bytes in unpadded base64url are 43 chars: {plain}"
    );
    assert_eq!(record.sha256.len(), 64, "the stored hash is bare hex");
    assert_eq!(record.id, record.sha256[..8], "the id is the hash prefix");
    assert!(record.created_at.ends_with('Z'), "created_at is UTC ISO-8601");

    let file = server_auth::load_from(&path).unwrap();
    assert_eq!(file.keys.len(), 1);
    assert_eq!(
        server_auth::verify(&file.keys, &plain).as_deref(),
        Some(record.id.as_str())
    );
    assert_eq!(server_auth::verify(&file.keys, "apb_wrong"), None);
    assert_eq!(
        server_auth::verify(&file.keys, &record.sha256),
        None,
        "the stored hash is not itself a usable credential"
    );

    // The plaintext key is never persisted.
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains(&plain), "the key must not be stored in plain text");
}

#[test]
fn two_keys_are_allowed_and_a_third_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_path(&dir);
    let (first, _) = server_auth::issue_into(&path).unwrap();
    let (second, _) = server_auth::issue_into(&path).unwrap();
    assert_ne!(first, second, "each issue mints fresh randomness");

    let file = server_auth::load_from(&path).unwrap();
    assert_eq!(file.keys.len(), MAX_KEYS);
    assert!(server_auth::verify(&file.keys, &first).is_some());
    assert!(server_auth::verify(&file.keys, &second).is_some());

    let err = server_auth::issue_into(&path).unwrap_err().to_string();
    assert!(err.contains("revoke"), "the refusal must name the remedy: {err}");
    assert!(!err.contains('!'), "no exclamation marks: {err}");
    assert_eq!(
        server_auth::load_from(&path).unwrap().keys.len(),
        MAX_KEYS,
        "a refused issue leaves the file untouched"
    );
}

#[test]
fn revoke_removes_one_key_and_rejects_an_unknown_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_path(&dir);
    let (first, first_rec) = server_auth::issue_into(&path).unwrap();
    let (second, _) = server_auth::issue_into(&path).unwrap();

    let removed = server_auth::revoke_in(&path, &first_rec.id).unwrap();
    assert_eq!(removed.id, first_rec.id);
    let file = server_auth::load_from(&path).unwrap();
    assert_eq!(file.keys.len(), 1);
    assert_eq!(server_auth::verify(&file.keys, &first), None);
    assert!(server_auth::verify(&file.keys, &second).is_some());

    let err = server_auth::revoke_in(&path, "deadbeef").unwrap_err().to_string();
    assert!(err.contains("deadbeef"), "the error names the id: {err}");
}

#[test]
fn the_key_file_is_private_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_path(&dir);
    server_auth::issue_into(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "owner-only, got {mode:o}");
    }
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no temp files left behind");
}

#[test]
fn a_malformed_file_is_an_error_not_an_empty_key_set() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_path(&dir);

    std::fs::write(&path, "keys: not-a-list\n").unwrap();
    assert!(server_auth::load_from(&path).is_err(), "wrong shape must fail");

    std::fs::write(&path, "keys:\n  - id: abc\n    sha256: zz\n    created_at: x\n").unwrap();
    let err = server_auth::load_from(&path).unwrap_err().to_string();
    assert!(err.contains("sha256"), "a bad hash field is named: {err}");

    std::fs::write(&path, "keys:\n  - id: abc\n    sha256: aa\n    created_at: x\n    extra: 1\n").unwrap();
    assert!(server_auth::load_from(&path).is_err(), "unknown fields are rejected");
}

#[test]
fn an_absent_file_is_an_empty_key_set() {
    let dir = tempfile::tempdir().unwrap();
    let file = server_auth::load_from(&key_path(&dir)).unwrap();
    assert!(file.keys.is_empty(), "no file means auth is simply off");
}

#[test]
fn ct_eq_str_matches_plain_equality() {
    assert!(server_auth::ct_eq_str("abc", "abc"));
    assert!(!server_auth::ct_eq_str("abc", "abd"));
    assert!(!server_auth::ct_eq_str("abc", "abcd"));
    assert!(server_auth::ct_eq_str("", ""));
}
```

Register the module in `crates/apb-core/tests/main.rs`, keeping the list alphabetical (insert between `schema_test` and `validate_duration_test`):

```rust
#[path = "suite/server_auth_test.rs"]
mod server_auth_test;
```

- [ ] **Step 3: run the tests and watch them fail**

```sh
cargo test -p apb-core --test main server_auth
```

Expected: a compile error, `unresolved import `apb_core::server_auth``.

- [ ] **Step 4: implement the module**

Create `crates/apb-core/src/server_auth.rs`:

```rust
//! Server-mode API keys (spec 2026-08-16-server-mode-design).
//!
//! At most two keys exist at a time, which is the rotation window and not a
//! key-management system: issue the second, move clients over, revoke the
//! first. A key is `apb_` plus 32 CSPRNG bytes in unpadded base64url; only its
//! SHA-256 is stored, in `<config_dir>/server-auth.yaml` written 0600 through
//! the shared atomic-write helper. The plaintext value is returned exactly
//! once, by `issue`, for a single print, and is never persisted or logged.
//!
//! Everything derived from a secret is compared with `subtle::ConstantTimeEq`;
//! this module owns that comparison for the whole workspace (the run-hook
//! endpoint in apb-server uses `ct_eq_str` too), so no call site re-decides
//! whether a plain `==` on a secret is acceptable.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::content::hex_lower;
use crate::fsutil::atomic_write_private;

/// Every issued key starts with this, so an operator can recognize one on
/// sight and a secret scanner can pattern-match it.
pub const KEY_PREFIX: &str = "apb_";

/// The rotation window: two live keys, never three.
pub const MAX_KEYS: usize = 2;

/// File name under the global config dir.
pub const AUTH_FILE: &str = "server-auth.yaml";

/// Lock file serializing read-modify-write over the key file, so two
/// concurrent `apb server key` invocations cannot lose an entry.
const AUTH_LOCK: &str = "server-auth.lock";

/// One issued key, as stored. `sha256` is bare lowercase hex (64 chars), not
/// the `sha256:<hex>` form used for content digests: this file is compared
/// against a freshly computed hash, never against a content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRecord {
    /// First 8 hex chars of `sha256`. Short, stable, and safe to print.
    pub id: String,
    pub sha256: String,
    /// UTC ISO-8601, from the single wall-clock source.
    pub created_at: String,
}

/// The whole key file. An absent file parses as an empty set, which is what
/// "auth disabled" means.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthFile {
    pub keys: Vec<KeyRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no config directory: set HOME or APB_CONFIG_DIR")]
    NoConfigDir,
    #[error("invalid key file `{0}`: {1}")]
    Invalid(String, String),
    #[error("key file `{0}`: {1}")]
    Io(String, String),
    #[error("at most 2 server keys may exist at once; revoke one first with `apb server key revoke <id>`")]
    TooManyKeys,
    #[error("no server key with id `{0}`")]
    UnknownKey(String),
    #[error("could not read random bytes from the operating system: {0}")]
    Random(String),
}

/// `<config_dir>/server-auth.yaml`.
pub fn auth_file_path() -> Result<PathBuf, AuthError> {
    crate::config::config_dir()
        .map(|d| d.join(AUTH_FILE))
        .ok_or(AuthError::NoConfigDir)
}

/// SHA-256 of a secret as bare lowercase hex. The only hashing this feature
/// does, for both stored keys and in-memory session tokens.
pub fn hash_hex(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    hex_lower(&h.finalize())
}

/// 32 bytes from the OS CSPRNG in unpadded base64url. Used for the key body
/// and for session tokens; never for anything that must be human-typed.
pub fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| AuthError::Random(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Constant-time string comparison. Lengths are not secret (a length mismatch
/// returns immediately), the contents are: equal-length inputs are compared
/// with `subtle::ConstantTimeEq` so no byte position leaks through timing.
pub fn ct_eq_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// The id of the key `presented` is, or `None`. Every stored key is compared
/// even after a match so the loop's duration does not reveal which key was
/// presented.
pub fn verify(keys: &[KeyRecord], presented: &str) -> Option<String> {
    let digest = hash_hex(presented);
    let mut found: Option<String> = None;
    for k in keys {
        if ct_eq_str(&digest, &k.sha256) {
            found = Some(k.id.clone());
        }
    }
    found
}

/// Reads a key file. A missing file is an empty set; a malformed one is an
/// error, so a typo can never silently disable authentication.
pub fn load_from(path: &Path) -> Result<AuthFile, AuthError> {
    if !path.is_file() {
        return Ok(AuthFile::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AuthError::Io(path.display().to_string(), e.to_string()))?;
    let parsed: AuthFile = serde_yaml_ng::from_str(&raw)
        .map_err(|e| AuthError::Invalid(path.display().to_string(), e.to_string()))?;
    for k in &parsed.keys {
        if k.sha256.len() != 64 || !k.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AuthError::Invalid(
                path.display().to_string(),
                format!("key `{}` has a malformed sha256 field", k.id),
            ));
        }
    }
    Ok(parsed)
}

fn save_to(path: &Path, file: &AuthFile) -> Result<(), AuthError> {
    let yaml = serde_yaml_ng::to_string(file)
        .map_err(|e| AuthError::Invalid(path.display().to_string(), e.to_string()))?;
    atomic_write_private(path, yaml.as_bytes())
        .map_err(|e| AuthError::Io(path.display().to_string(), e.to_string()))
}

fn lock_for(path: &Path) -> Result<crate::fsutil::DirLock, AuthError> {
    let dir = path.parent().ok_or_else(|| {
        AuthError::Invalid(
            path.display().to_string(),
            "path has no parent directory".to_string(),
        )
    })?;
    crate::fsutil::lock_dir(dir, AUTH_LOCK)
        .map_err(|e| AuthError::Io(dir.display().to_string(), e.to_string()))
}

/// Mints a key into `path` and returns `(plaintext, record)`. The plaintext is
/// the caller's only chance to see it.
pub fn issue_into(path: &Path) -> Result<(String, KeyRecord), AuthError> {
    let _lock = lock_for(path)?;
    let mut file = load_from(path)?;
    if file.keys.len() >= MAX_KEYS {
        return Err(AuthError::TooManyKeys);
    }
    let plain = format!("{KEY_PREFIX}{}", random_token()?);
    let sha256 = hash_hex(&plain);
    let record = KeyRecord {
        id: sha256[..8].to_string(),
        sha256,
        created_at: crate::dismiss::iso_utc(crate::clock::now_ms_u64()),
    };
    file.keys.push(record.clone());
    save_to(path, &file)?;
    Ok((plain, record))
}

/// Removes the key with `id` from `path` and returns the removed record. The
/// id is a prefix of the stored hash, so it is secret-derived and gets the
/// same constant-time comparison as everything else in this module, even
/// though an id is printable and not itself a credential.
pub fn revoke_in(path: &Path, id: &str) -> Result<KeyRecord, AuthError> {
    let _lock = lock_for(path)?;
    let mut file = load_from(path)?;
    let Some(pos) = file.keys.iter().position(|k| ct_eq_str(&k.id, id)) else {
        return Err(AuthError::UnknownKey(id.to_string()));
    };
    let removed = file.keys.remove(pos);
    save_to(path, &file)?;
    Ok(removed)
}

/// `load_from` on the standard config-dir path.
pub fn load() -> Result<AuthFile, AuthError> {
    load_from(&auth_file_path()?)
}

/// `issue_into` on the standard config-dir path.
pub fn issue() -> Result<(String, KeyRecord), AuthError> {
    issue_into(&auth_file_path()?)
}

/// `revoke_in` on the standard config-dir path.
pub fn revoke(id: &str) -> Result<KeyRecord, AuthError> {
    revoke_in(&auth_file_path()?, id)
}
```

Add to `crates/apb-core/src/lib.rs`, between `pub mod scope;` and `pub mod skills;`:

```rust
pub mod server_auth;
```

- [ ] **Step 5: run the tests and watch them pass**

```sh
cargo test -p apb-core --test main server_auth
```

Expected: 7 passed, 0 failed.

- [ ] **Step 6: gates and commit**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p apb-core
```

Then:

```sh
git add Cargo.toml Cargo.lock crates/apb-core/Cargo.toml crates/apb-core/src/lib.rs crates/apb-core/src/server_auth.rs crates/apb-core/tests/suite/server_auth_test.rs crates/apb-core/tests/main.rs
git commit --signoff -m "$(cat <<'EOF'
feat(core): server_auth module for server-mode API keys

Issue, verify, and revoke up to two API keys stored as SHA-256 hashes in
<config_dir>/server-auth.yaml (0600, atomic). Keys are `apb_` plus 32 CSPRNG
bytes in unpadded base64url; the plaintext is returned once for printing and
never persisted. All secret-derived comparisons go through subtle's
constant-time equality, exposed as ct_eq_str for reuse.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `server:` config section and bind resolution

**Files:**
- Modify: `crates/apb-core/src/config.rs` (`GlobalConfig` struct at lines 10-45; add `ServerConfig` next to `SuggestionSettings` at the end of the file)
- Test: modify `crates/apb-core/tests/suite/config_test.rs` (append new tests after `project_suggestion_settings_read_the_project_config`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub struct ServerConfig { bind: Option<String>, public_base_url: Option<String>, trusted_proxies: Vec<String> }` with `resolve_bind(&self, flag: Option<&str>) -> Result<IpAddr, String>`, `trusted_proxy_set(&self) -> Result<BTreeSet<IpAddr>, String>`, `public_scheme_is_https(&self) -> bool`, the constant `DEFAULT_BIND`, and the field `pub server: ServerConfig` on `GlobalConfig`. Tasks 3 and 4 consume all of these.

- [ ] **Step 1: write the failing tests**

Append to `crates/apb-core/tests/suite/config_test.rs`:

```rust
#[test]
fn server_section_loads_and_defaults() {
    let _lock = crate::common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", dir.path());
    }

    // A config with no server section keeps every default.
    std::fs::write(dir.path().join("config.yaml"), "port: 7321\n").unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.server.bind, None);
    assert_eq!(cfg.server.public_base_url, None);
    assert!(cfg.server.trusted_proxies.is_empty());

    let yaml = "port: 7321\nserver:\n  bind: \"0.0.0.0\"\n  public_base_url: https://apb.example.com\n  trusted_proxies: [\"127.0.0.1\", \"10.0.0.7\"]\n";
    std::fs::write(dir.path().join("config.yaml"), yaml).unwrap();
    let cfg = GlobalConfig::load().unwrap();
    assert_eq!(cfg.server.bind.as_deref(), Some("0.0.0.0"));
    assert_eq!(
        cfg.server.public_base_url.as_deref(),
        Some("https://apb.example.com")
    );
    assert_eq!(cfg.server.trusted_proxies.len(), 2);
    assert!(cfg.server.public_scheme_is_https());

    // An unknown key inside the section is a hard error, like every other
    // section in this file.
    std::fs::write(
        dir.path().join("config.yaml"),
        "server:\n  bnid: 0.0.0.0\n",
    )
    .unwrap();
    let broken = GlobalConfig::load();

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
    assert!(broken.is_err(), "a typo in the server section must not be ignored");
}

#[test]
fn bind_precedence_is_flag_then_config_then_loopback() {
    use apb_core::config::ServerConfig;
    use std::net::{IpAddr, Ipv4Addr};

    let empty = ServerConfig::default();
    assert_eq!(
        empty.resolve_bind(None).unwrap(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "no flag and no config means loopback"
    );
    assert_eq!(
        empty.resolve_bind(Some("0.0.0.0")).unwrap(),
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    );

    let configured = ServerConfig {
        bind: Some("10.0.0.5".to_string()),
        ..Default::default()
    };
    assert_eq!(
        configured.resolve_bind(None).unwrap(),
        "10.0.0.5".parse::<IpAddr>().unwrap()
    );
    assert_eq!(
        configured.resolve_bind(Some("127.0.0.1")).unwrap(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "the flag wins over the config"
    );

    let bad = ServerConfig {
        bind: Some("not-an-ip".to_string()),
        ..Default::default()
    };
    let err = bad.resolve_bind(None).unwrap_err();
    assert!(err.contains("not-an-ip"), "the error names the value: {err}");
    assert!(!err.contains('!'), "no exclamation marks: {err}");
}

#[test]
fn trusted_proxies_parse_into_a_set() {
    use apb_core::config::ServerConfig;
    use std::net::IpAddr;

    let cfg = ServerConfig {
        trusted_proxies: vec!["127.0.0.1".to_string(), " 10.0.0.7 ".to_string()],
        ..Default::default()
    };
    let set = cfg.trusted_proxy_set().unwrap();
    assert!(set.contains(&"127.0.0.1".parse::<IpAddr>().unwrap()));
    assert!(set.contains(&"10.0.0.7".parse::<IpAddr>().unwrap()));

    let cidr = ServerConfig {
        trusted_proxies: vec!["10.0.0.0/8".to_string()],
        ..Default::default()
    };
    let err = cidr.trusted_proxy_set().unwrap_err();
    assert!(err.contains("10.0.0.0/8"), "CIDR is not supported in v1: {err}");
}
```

- [ ] **Step 2: run the tests and watch them fail**

```sh
cargo test -p apb-core --test main config
```

Expected: compile errors, `no field `server` on type `GlobalConfig`` and `no `ServerConfig` in `config``.

- [ ] **Step 3: implement**

In `crates/apb-core/src/config.rs`, extend the imports at the top of the file:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
```

Replace the `GlobalConfig` struct with this complete version (only the last field is new):

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalConfig {
    /// Default web server port (overridden by the CLI flag).
    pub port: Option<u16>,
    /// Agent descriptions: id -> command/transport.
    pub agents: BTreeMap<String, AgentDef>,
    /// Deprecated (schema 1). Executors were removed from the schema (Task 9),
    /// but an old config.yaml may still carry them. We accept and IGNORE them
    /// so loading doesn't break (otherwise deny_unknown_fields would block all
    /// runs until migration); `apb migrate` strips them from the file. Not
    /// serialized back out.
    #[serde(default, rename = "executors", skip_serializing)]
    pub legacy_executors: Option<serde_yaml_ng::Value>,
    /// Deprecated (schema 1), see `legacy_executors`.
    #[serde(default, rename = "default_executor", skip_serializing)]
    pub legacy_default_executor: Option<serde_yaml_ng::Value>,
    /// Runner registry for script nodes (8d): e.g. `ts: [bun, deno]`. The
    /// first one available on the machine is used.
    pub runners: BTreeMap<String, Vec<String>>,
    /// Auto-registration of workspaces in the project registry (spec 6.2).
    /// `None`/`true` enables it, `false` disables it. Also disabled by env
    /// `APB_NO_REGISTRY=1` and in CI.
    pub registry: Option<bool>,
    /// Days in the `unreachable` state before transitioning to `tombstoned`
    /// (spec 6.4). `None` -> 14.
    pub registry_unreachable_days: Option<u64>,
    /// Days to keep `tombstoned` before physical cleanup (spec 6.4).
    /// `None` -> 90.
    pub registry_purge_days: Option<u64>,
    /// Timing knobs for the suggestion-decision store (spec
    /// 2026-07-29-suggestion-decisions-design). Absent keys fall back to the
    /// named constants in `crate::dismiss`; a project `.apb/config.yaml` may
    /// override either key for its own project.
    pub suggestions: SuggestionSettings,
    /// Server-deployment knobs (spec 2026-08-16-server-mode-design). Absent
    /// section means the historical behavior: loopback bind, no proxy trust.
    pub server: ServerConfig,
}
```

Append at the end of `crates/apb-core/src/config.rs`:

```rust
/// The address the dashboard binds when no flag is given. Loopback, because a
/// server that anyone on the network can reach must be an explicit decision.
pub const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Optional `server:` section of the global config (spec
/// 2026-08-16-server-mode-design). Every key is optional: an operator who
/// never touches the section keeps today's loopback-only behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// IP address to bind, parsed at startup. `0.0.0.0` for a server
    /// deployment. An unparseable value is a startup error, never a silent
    /// fallback to loopback.
    pub bind: Option<String>,
    /// Public origin the dashboard is reached at, e.g.
    /// `https://apb.example.com`. Used to print absolute URLs and to decide
    /// whether the session cookie carries the `Secure` attribute.
    pub public_base_url: Option<String>,
    /// Exact peer IPs whose `X-Forwarded-For` and `X-Forwarded-Proto` headers
    /// are believed. No CIDR ranges in v1: a reverse proxy has one address.
    /// Forwarded headers are never used for an authentication decision, only
    /// for rate-limit keying and logging.
    pub trusted_proxies: Vec<String>,
}

impl ServerConfig {
    /// Bind precedence: `--bind` flag, then `server.bind`, then loopback.
    pub fn resolve_bind(&self, flag: Option<&str>) -> Result<IpAddr, String> {
        match flag.or(self.bind.as_deref()) {
            None => Ok(DEFAULT_BIND),
            Some(raw) => raw
                .trim()
                .parse::<IpAddr>()
                .map_err(|e| format!("invalid bind address `{raw}`: {e}")),
        }
    }

    /// `trusted_proxies` as parsed addresses. A CIDR range or any other
    /// unparseable entry is an error rather than a silently ignored line.
    pub fn trusted_proxy_set(&self) -> Result<BTreeSet<IpAddr>, String> {
        let mut out = BTreeSet::new();
        for raw in &self.trusted_proxies {
            let addr = raw.trim().parse::<IpAddr>().map_err(|e| {
                format!("invalid server.trusted_proxies entry `{raw}`: {e} (exact IP addresses only, no CIDR ranges)")
            })?;
            out.insert(addr);
        }
        Ok(out)
    }

    /// Whether the configured public origin is https, which is one of the two
    /// signals that make the session cookie `Secure`.
    pub fn public_scheme_is_https(&self) -> bool {
        self.public_base_url
            .as_deref()
            .map(|u| u.trim().starts_with("https://"))
            .unwrap_or(false)
    }
}
```

- [ ] **Step 4: run the tests and watch them pass**

```sh
cargo test -p apb-core --test main config
```

Expected: all `config_test` tests pass, including the three new ones.

- [ ] **Step 5: gates and commit**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p apb-core
```

```sh
git add crates/apb-core/src/config.rs crates/apb-core/tests/suite/config_test.rs
git commit --signoff -m "$(cat <<'EOF'
feat(core): server section on the global config

Adds an optional `server:` block carrying bind, public_base_url and
trusted_proxies, with bind precedence (flag, config, loopback), strict
parsing of proxy addresses (no CIDR in v1), and the https detection that
decides the Secure cookie attribute.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: `apb server key` CLI, `--bind`, and the startup interlock

**Files:**
- Create: `crates/apb-cli/src/server.rs`
- Modify: `crates/apb-cli/src/main.rs` (module list at lines 1-10, `use` block at lines 17-31, `Command` enum `Dashboard` variant at lines 193-200, dispatch at lines 354 and 401)
- Modify: `crates/apb-cli/src/util.rs` (add `resolve_bind` after `resolve_port`, line 27)
- Modify: `crates/apb-cli/src/serve.rs` (`dashboard` at lines 9-27, `dev_cmd`'s server spawn at line 130, tests module at line 193)
- Modify: `crates/apb-server/src/lib.rs` (`run_server` at lines 146-177)
- Test: create `crates/apb-server/tests/suite/bind_interlock_test.rs` and register it in `crates/apb-server/tests/main.rs`; create `crates/apb-cli/tests/suite/server_key_cli_test.rs` and register it in `crates/apb-cli/tests/main.rs`

**Interfaces:**
- Consumes: `apb_core::server_auth::{issue, load, revoke, KeyRecord}` (Task 1), `apb_core::config::{GlobalConfig, ServerConfig}` (Task 2), `crate::util::{print_json, print_table}`.
- Produces: `apb_server::check_bind_allowed(IpAddr, usize) -> Result<(), String>`, `apb_server::run_server(bind: IpAddr, port: u16)`, `apb_cli::util::resolve_bind(Option<&str>) -> Result<IpAddr, String>`, `apb_cli::serve::dashboard(bind: IpAddr, port: u16, no_open: bool)`, and the CLI surface `apb server key issue|list|revoke`. Task 4 replaces the body of `run_server` again to attach `AuthState`.

- [ ] **Step 1: write the failing interlock test**

Create `crates/apb-server/tests/suite/bind_interlock_test.rs`:

```rust
//! The startup interlock: binding anywhere but loopback requires at least one
//! issued API key. Pure precondition, so it is checked without opening a
//! socket.

use apb_server::check_bind_allowed;
use std::net::{IpAddr, Ipv4Addr};

#[test]
fn loopback_needs_no_keys() {
    assert!(check_bind_allowed(IpAddr::V4(Ipv4Addr::LOCALHOST), 0).is_ok());
    assert!(check_bind_allowed("::1".parse().unwrap(), 0).is_ok());
}

#[test]
fn non_loopback_without_keys_is_refused() {
    let err = check_bind_allowed(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).unwrap_err();
    assert!(err.contains("0.0.0.0"), "the error names the address: {err}");
    assert!(
        err.contains("apb server key issue"),
        "the error names the remedy: {err}"
    );
    assert!(!err.contains('!'), "no exclamation marks: {err}");
    assert!(!err.contains('\u{2014}'), "no em-dashes: {err}");

    let err = check_bind_allowed("10.0.0.5".parse().unwrap(), 0).unwrap_err();
    assert!(err.contains("10.0.0.5"), "{err}");
}

#[test]
fn non_loopback_with_a_key_is_allowed() {
    assert!(check_bind_allowed(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1).is_ok());
    assert!(check_bind_allowed("10.0.0.5".parse().unwrap(), 2).is_ok());
}
```

Register in `crates/apb-server/tests/main.rs`, keeping the list alphabetical (insert after the `api_test` block):

```rust
#[path = "suite/bind_interlock_test.rs"]
mod bind_interlock_test;
```

- [ ] **Step 2: write the failing CLI test**

Create `crates/apb-cli/tests/suite/server_key_cli_test.rs`:

```rust
//! `apb server key issue|list|revoke` against a temp global config dir,
//! driving the real binary the way the other CLI suites do. The config dir is
//! passed per spawn with `Command::env`, never by mutating this process's env
//! (other suites in this binary spawn concurrently).

use std::path::Path;
use std::process::Command;

fn apb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_apb")
}

fn run(cfg: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(apb_bin())
        .args(["server"])
        .args(args)
        .env("APB_CONFIG_DIR", cfg)
        .env_remove("CI")
        .env_remove("APB_NO_REGISTRY")
        .output()
        .expect("run apb server");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn issue_list_revoke_cycle() {
    let cfg = tempfile::tempdir().unwrap();

    // Nothing issued yet: list says so without failing.
    let (stdout, _, ok) = run(cfg.path(), &["key", "list"]);
    assert!(ok, "an empty list is not an error");
    assert!(
        stdout.contains("no server keys"),
        "empty list must explain the state: {stdout}"
    );

    // Issue prints the key exactly once, plus its id.
    let (stdout, _, ok) = run(cfg.path(), &["key", "issue"]);
    assert!(ok, "issue must succeed: {stdout}");
    let key = stdout
        .lines()
        .find(|l| l.starts_with("apb_"))
        .expect("the key is printed on its own line")
        .to_string();
    assert_eq!(
        stdout.matches(&key).count(),
        1,
        "the key is printed once and only once: {stdout}"
    );
    assert!(
        stdout.contains("shown once"),
        "issue warns that the key is not recoverable: {stdout}"
    );
    assert!(!stdout.contains('!'), "no exclamation marks: {stdout}");

    // The list shows an id and a timestamp, never the key.
    let (stdout, _, ok) = run(cfg.path(), &["key", "list"]);
    assert!(ok);
    assert!(!stdout.contains(&key), "list must not echo the key: {stdout}");
    let id = key_id(cfg.path());
    assert!(stdout.contains(&id), "list shows the id: {stdout}");

    // A second key is fine, a third is refused.
    assert!(run(cfg.path(), &["key", "issue"]).2, "a second key is allowed");
    let (_, stderr, ok) = run(cfg.path(), &["key", "issue"]);
    assert!(!ok, "a third key must fail");
    assert!(stderr.contains("revoke"), "the refusal names the remedy: {stderr}");

    // Revoke by id frees a slot again.
    assert!(run(cfg.path(), &["key", "revoke", &id]).2, "revoke succeeds");
    assert!(run(cfg.path(), &["key", "issue"]).2, "a slot is free again");

    let (_, stderr, ok) = run(cfg.path(), &["key", "revoke", "deadbeef"]);
    assert!(!ok, "an unknown id must fail");
    assert!(stderr.contains("deadbeef"), "{stderr}");
}

/// The id of the first key in the store, read straight from the file.
fn key_id(cfg: &Path) -> String {
    let raw = std::fs::read_to_string(cfg.join("server-auth.yaml")).unwrap();
    let file: apb_core::server_auth::AuthFile = serde_yaml_ng::from_str(&raw).unwrap();
    file.keys[0].id.clone()
}

#[test]
fn json_listing_carries_ids_and_timestamps_only() {
    let cfg = tempfile::tempdir().unwrap();
    run(cfg.path(), &["key", "issue"]);
    let (stdout, _, ok) = run(cfg.path(), &["key", "list", "--json"]);
    assert!(ok, "{stdout}");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    let keys = v["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1);
    assert!(keys[0]["id"].is_string());
    assert!(keys[0]["created_at"].is_string());
    assert!(
        keys[0].get("sha256").is_none(),
        "the stored hash is not part of the listing: {stdout}"
    );
}
```

`apb-cli`'s dev-dependencies already carry `serde_yaml_ng` and `tempfile`; `apb-core` is a normal dependency, so `apb_core::server_auth` resolves in tests.

Register in `crates/apb-cli/tests/main.rs`, alphabetically (between `run_doctor_cli_test` and `stdio_profile_e2e_test`):

```rust
#[path = "suite/server_key_cli_test.rs"]
mod server_key_cli_test;
```

- [ ] **Step 3: run both and watch them fail**

```sh
cargo test -p apb-server --test main bind_interlock
cargo test -p apb-cli --test main server_key
```

Expected: `cannot find function `check_bind_allowed` in crate `apb_server``, and the CLI test failing at `apb server` with clap's `unrecognized subcommand 'server'`.

- [ ] **Step 4: implement the interlock and the new `run_server` signature**

In `crates/apb-server/src/lib.rs`, replace the imports block and `run_server` with this complete version:

```rust
use axum::Router;
use axum::routing::{get, post, put};
use std::net::{IpAddr, SocketAddr};

pub use state::AppState;
```

```rust
/// Startup interlock (spec 2026-08-16-server-mode-design): binding anywhere
/// but the loopback interface requires at least one issued API key. This is a
/// hard error and not a warning, because the API can start runs and make
/// authenticated connector calls, which makes an open bind equivalent to
/// remote code execution.
pub fn check_bind_allowed(bind: IpAddr, key_count: usize) -> Result<(), String> {
    if bind.is_loopback() || key_count > 0 {
        return Ok(());
    }
    Err(format!(
        "refusing to bind {bind}: no API key is configured, so the dashboard would be reachable from the network without authentication. Issue one with `apb server key issue`, or keep the default 127.0.0.1 bind"
    ))
}

/// The host part of the dashboard URL for a bind address, with IPv6 bracketed.
fn display_host(bind: IpAddr) -> String {
    match bind {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// Runs the global, machine-wide dashboard: one server, no project binding.
/// Playbooks and runs are aggregated across every reachable project in the
/// registry; project-specific requests carry `?workspace=<id>`. A single
/// instance lock lives in the config dir so two global dashboards cannot race
/// on the same port.
pub async fn run_server(bind: IpAddr, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // The key file decides two things at once: whether the requested bind is
    // permitted at all, and (from Task 4 on) whether auth is enforced. A
    // malformed file is a startup error rather than a silent "no keys", so a
    // typo can never quietly open the server.
    let auth_file = apb_core::server_auth::load()?;
    check_bind_allowed(bind, auth_file.keys.len()).map_err(std::io::Error::other)?;

    let state = AppState::new_global();
    let cfg = apb_core::config::config_dir()
        .ok_or_else(|| std::io::Error::other("no config dir for the global server lock"))?;
    std::fs::create_dir_all(&cfg)?;
    // Bind the port BEFORE writing the lock file: the port bind is the real
    // mutual exclusion (a second server on the same port fails here), so if it
    // fails we must return without having written a lock that no cleanup path
    // would then remove.
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    let _lock = lock::write_global_lock(&cfg, port)?;
    // Real-time updates across all projects: a filesystem watcher broadcasts
    // change pings on the shared channel that the dashboard's WebSocket relays.
    // Best-effort: if it cannot start, the server still serves (the UI just
    // falls back to refetch-on-navigation).
    let _watcher = match watch::spawn_global_watcher(state.events.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            eprintln!("apb dashboard: real-time watcher unavailable: {e}");
            None
        }
    };
    let app = build_router(state);
    println!(
        "apb dashboard (global): http://{}:{port}",
        display_host(bind)
    );
    // ConnectInfo carries the socket peer address into every request, which the
    // auth layer needs for rate-limit keying and for deciding whether a
    // forwarded header came from a trusted proxy.
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;
    // Remove the lock both on normal shutdown and after catching a signal.
    lock::remove_global_lock(&cfg)?;
    result?;
    Ok(())
}
```

- [ ] **Step 5: implement the CLI**

Create `crates/apb-cli/src/server.rs`:

```rust
//! `apb server` subcommands (spec 2026-08-16-server-mode-design): the API keys
//! that authenticate a networked dashboard. A thin dispatch over
//! `apb_core::server_auth`, which owns the file format and the crypto.
//!
//! The plaintext key crosses this module exactly once, on the stdout of
//! `issue`. It is never written to a log line, never echoed by `list`, and
//! never included in an error message.

use std::process::ExitCode;

use apb_core::server_auth;
use clap::Subcommand;
use serde_json::json;

use crate::util::{print_json, print_table};

#[derive(Subcommand)]
pub(crate) enum ServerAction {
    /// Manage the API keys that authenticate the dashboard and its API
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum KeyAction {
    /// Issue a key and print it once. At most two keys exist at a time, which
    /// is the rotation window: issue the second, move clients over, revoke the
    /// first.
    Issue,
    /// Show key ids and creation times. Never the keys themselves.
    List {
        /// Machine-readable output for scripts
        #[arg(long)]
        json: bool,
    },
    /// Revoke a key by id (see `apb server key list`)
    Revoke { id: String },
}

pub(crate) fn server_cmd(action: ServerAction) -> ExitCode {
    match action {
        ServerAction::Key { action } => match action {
            KeyAction::Issue => issue_cmd(),
            KeyAction::List { json } => list_cmd(json),
            KeyAction::Revoke { id } => revoke_cmd(&id),
        },
    }
}

fn issue_cmd() -> ExitCode {
    match server_auth::issue() {
        Ok((key, record)) => {
            println!("{key}");
            println!();
            println!("key id: {}", record.id);
            println!("This key is shown once and is stored only as a hash. Save it now.");
            println!(
                "A running dashboard picks the change up within a minute, and immediately after any failed request."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("apb server key issue: {e}");
            ExitCode::from(2)
        }
    }
}

fn list_cmd(as_json: bool) -> ExitCode {
    let keys = match server_auth::load() {
        Ok(file) => file.keys,
        Err(e) => {
            eprintln!("apb server key list: {e}");
            return ExitCode::from(2);
        }
    };
    if as_json {
        let rows: Vec<serde_json::Value> = keys
            .iter()
            .map(|k| json!({ "id": k.id, "created_at": k.created_at }))
            .collect();
        print_json(&json!({ "keys": rows }));
        return ExitCode::SUCCESS;
    }
    if keys.is_empty() {
        println!(
            "no server keys; the dashboard runs unauthenticated and may only bind the loopback interface"
        );
        println!("issue one with `apb server key issue`");
        return ExitCode::SUCCESS;
    }
    let mut rows = vec![vec!["ID".to_string(), "CREATED".to_string()]];
    for k in &keys {
        rows.push(vec![k.id.clone(), k.created_at.clone()]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

fn revoke_cmd(id: &str) -> ExitCode {
    match server_auth::revoke(id) {
        Ok(record) => {
            println!("revoked key {} (created {})", record.id, record.created_at);
            println!(
                "A running dashboard stops accepting it within a minute, and immediately on its next use."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("apb server key revoke: {e}");
            ExitCode::from(2)
        }
    }
}
```

In `crates/apb-cli/src/main.rs`, add the module declaration in alphabetical order (between `mod selfupdate;` and `mod serve;` becomes `mod selfupdate; mod serve; mod server;`, so place it after `mod serve;`):

```rust
mod server;
```

Add to the `use` block, after the `use crate::serve::{...};` line:

```rust
use crate::server::{ServerAction, server_cmd};
```

and change the `util` import line to:

```rust
use crate::util::{resolve_bind, resolve_port};
```

Replace the `Dashboard` variant of `enum Command` with this complete version:

```rust
    /// Start the web dashboard (global, all projects)
    #[command(alias = "serve")]
    Dashboard {
        /// Port: the flag overrides the global config, default 7321.
        #[arg(long)]
        port: Option<u16>,
        /// IP address to bind: the flag overrides `server.bind` in the global
        /// config, default 127.0.0.1. Any non-loopback address requires at
        /// least one key from `apb server key issue`.
        #[arg(long)]
        bind: Option<String>,
        #[arg(long)]
        no_open: bool,
    },
```

Add a new variant right after `Dashboard`:

```rust
    /// Manage server mode: the API keys that authenticate a networked dashboard
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },
```

Replace the `Dashboard` dispatch arm and the `None` arm:

```rust
        Some(Command::Dashboard {
            port,
            bind,
            no_open,
        }) => match resolve_bind(bind.as_deref()) {
            Ok(addr) => dashboard(addr, resolve_port(port), no_open),
            Err(e) => {
                eprintln!("dashboard failed: {e}");
                ExitCode::from(2)
            }
        },
        Some(Command::Server { action }) => server_cmd(action),
```

```rust
        None => match resolve_bind(None) {
            Ok(addr) => dashboard(addr, resolve_port(None), false),
            Err(e) => {
                eprintln!("dashboard failed: {e}");
                ExitCode::from(2)
            }
        },
```

In `crates/apb-cli/src/util.rs`, add after `resolve_port`:

```rust
/// Bind address for the dashboard: the `--bind` flag, then `server.bind` from
/// the global config, then loopback. A malformed config or an unparseable
/// address is an error rather than a silent fallback, so an operator who
/// mistypes `0.0.0.0` never gets a loopback server they believe is public.
pub(crate) fn resolve_bind(flag: Option<&str>) -> Result<std::net::IpAddr, String> {
    let cfg = apb_core::config::GlobalConfig::load()?;
    cfg.server.resolve_bind(flag)
}
```

In `crates/apb-cli/src/serve.rs`, replace the `dashboard` function and add the URL helper:

```rust
/// Starts the single, global dashboard for the machine. There is no
/// project-scoped server: the dashboard aggregates every registered project,
/// so it does not bind to (or initialize) the current directory.
pub(crate) fn dashboard(bind: IpAddr, port: u16, no_open: bool) -> ExitCode {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if !no_open {
        let _ = open::that_detached(&browse_url(bind, port));
    }
    match rt.block_on(apb_server::run_server(bind, port)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if error_looks_like_addr_in_use(&e) {
                let holders = lookup_port_holders(port);
                eprintln!("{}", format_port_in_use_error(port, holders.as_deref()));
            } else {
                eprintln!("dashboard failed: {e}");
            }
            ExitCode::from(2)
        }
    }
}

/// The URL to open in a local browser for a given bind address. An
/// all-interfaces bind has no address of its own to visit, so the loopback
/// alias is used; any other bind is visited at its own address, IPv6
/// bracketed.
fn browse_url(bind: IpAddr, port: u16) -> String {
    let host = if bind.is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        bind
    };
    match host {
        IpAddr::V4(v4) => format!("http://{v4}:{port}"),
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
    }
}
```

Extend the imports at the top of `crates/apb-cli/src/serve.rs`. This is ADDITIVE: only the `std::net` line is new, and the existing `use apb_core::registry::init_project;` line MUST stay (`dev_cmd` calls `init_project` at serve.rs:120). The complete resulting import block is:

```rust
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::registry::init_project;
```

In `dev_cmd`, replace the background server line so it keeps the loopback bind explicitly:

```rust
        if let Err(e) = rt.block_on(apb_server::run_server(IpAddr::V4(Ipv4Addr::LOCALHOST), 7321)) {
```

Add to the existing `#[cfg(test)] mod tests` in `crates/apb-cli/src/serve.rs`:

```rust
    #[test]
    fn browse_url_maps_bind_to_a_visitable_address() {
        use super::browse_url;
        use std::net::{IpAddr, Ipv4Addr};
        assert_eq!(
            browse_url(IpAddr::V4(Ipv4Addr::LOCALHOST), 7321),
            "http://127.0.0.1:7321"
        );
        assert_eq!(
            browse_url(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 7321),
            "http://127.0.0.1:7321",
            "an all-interfaces bind is visited on loopback"
        );
        assert_eq!(
            browse_url("10.0.0.5".parse().unwrap(), 8080),
            "http://10.0.0.5:8080"
        );
        assert_eq!(
            browse_url("::1".parse().unwrap(), 7321),
            "http://[::1]:7321",
            "IPv6 hosts are bracketed"
        );
    }
```

The module's existing `use super::{error_looks_like_addr_in_use, format_port_in_use_error};`
line stays exactly as it is: the new test imports `browse_url` in its own body,
so no existing line in that module is touched.

- [ ] **Step 6: run the tests and watch them pass**

```sh
cargo test -p apb-server --test main bind_interlock
cargo test -p apb-cli --test main server_key
cargo test -p apb-cli
```

Expected: 3 interlock tests pass, 2 CLI tests pass, the `browse_url` unit test passes. `apb-cli` is a bin-only crate, so `--lib` is not a valid target filter there; the unqualified `cargo test -p apb-cli` covers both the unit tests inside `src/` and the integration binary.

- [ ] **Step 7: gates and commit**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```sh
git add crates/apb-cli/src/server.rs crates/apb-cli/src/main.rs crates/apb-cli/src/util.rs crates/apb-cli/src/serve.rs crates/apb-server/src/lib.rs crates/apb-server/tests/suite/bind_interlock_test.rs crates/apb-server/tests/main.rs crates/apb-cli/tests/suite/server_key_cli_test.rs crates/apb-cli/tests/main.rs
git commit --signoff -m "$(cat <<'EOF'
feat(cli): apb server key commands and a bindable dashboard

Adds `apb server key issue|list|revoke` over apb_core::server_auth, a
`--bind` flag on `apb dashboard` resolved against the new server.bind config
key, and the hard startup interlock that refuses a non-loopback bind when no
key exists. run_server now takes the bind address and serves with ConnectInfo
so the auth layer can see the socket peer.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: auth middleware, sessions, CSRF, and the rate limiter

**Files:**
- Create: `crates/apb-server/src/auth.rs`
- Modify: `crates/apb-server/src/state.rs` (`AppState` at lines 11-39)
- Modify: `crates/apb-server/src/lib.rs` (module list at lines 11-16, `build_router` at lines 23-139, `run_server` from Task 3)
- Test: create `crates/apb-server/tests/suite/auth_test.rs`, register it in `crates/apb-server/tests/main.rs`

**Interfaces:**
- Consumes: `apb_core::server_auth::{verify, hash_hex, random_token, KeyRecord}` (Task 1), `apb_core::config::ServerConfig` (Task 2), `apb_core::clock::now_ms`, axum 0.8 `middleware::from_fn_with_state`, `extract::{ConnectInfo, Request, State}`, `middleware::Next`.
- Produces: `apb_server::auth::{AuthState, SessionStore, RateLimiter, ClientCtx, Credential, auth_middleware, client_ctx, cookie_value, evaluate, is_exempt, log_auth_failure, SESSION_COOKIE, CSRF_HEADER, CSRF_VALUE, SESSION_TTL_MS, KEY_RELOAD_INTERVAL_MS}` (all `pub`), the `AuthState` methods `new(Option<PathBuf>, Vec<KeyRecord>, &ServerConfig)`, `disabled()`, `enabled()`, `key_count()`, `verify_key()`, `verify_key_with_reload()`, `maybe_reload()`, `sessions()`, `failures()`, the crate-internal response helpers `pub(crate) fn unauthorized() -> Response`, `pub(crate) fn rate_limited() -> Response` and `pub(crate) fn forbidden_csrf() -> Response`, and `AppState { auth: Arc<AuthState> }` with `AppState::new_global_with_auth` and `AppState::with_auth`. Task 5 consumes `AuthState`, `ClientCtx`, `Credential`, `evaluate`, `verify_key_with_reload`, `cookie_value`, `log_auth_failure`, `unauthorized`, `rate_limited`, `SESSION_COOKIE`.

- [ ] **Step 1: write the failing tests**

Create `crates/apb-server/tests/suite/auth_test.rs`:

```rust
//! The auth middleware: pass-through when no key exists, bearer and cookie
//! credentials, the CSRF second layer, exempt paths, and the failure rate
//! limiter. Requests go through the in-process router exactly like every other
//! suite here (`tower::ServiceExt::oneshot`), so no socket is opened; the
//! middleware falls back to a loopback client IP when `ConnectInfo` is absent,
//! which is precisely this situation.

use apb_core::config::ServerConfig;
use apb_core::server_auth;
use apb_server::auth::{AuthState, CSRF_HEADER, CSRF_VALUE, SESSION_COOKIE};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

/// A pinned-root state whose auth layer holds exactly one freshly issued key.
/// The key file lives in a tempdir that is dropped immediately and the auth
/// state is built with no watched path, so these tests never take the reload
/// branch; the dedicated reload test below keeps its own file alive.
fn authed(root: std::path::PathBuf) -> (String, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server-auth.yaml");
    let (key, _record) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(AuthState::new(None, file.keys, &ServerConfig::default()).unwrap());
    (key, AppState::new(root).with_auth(auth))
}

async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    dir
}

#[tokio::test]
async fn with_no_keys_every_route_stays_open() {
    let dir = seed();
    let state = AppState::new(dir.path().to_path_buf());
    let (status, _) = send(&state, Request::get("/api/runs").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "the local dashboard is unchanged");
}

#[tokio::test]
async fn a_valid_bearer_key_passes_and_an_invalid_one_is_401() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());

    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", "Bearer apb_not-a-real-key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth", "the 401 body shape is stable: {body}");

    let (status, body) = send(&state, Request::get("/api/runs").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth");
}

#[tokio::test]
async fn exempt_paths_are_reachable_without_a_credential() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());

    let (status, _) = send(&state, Request::get("/api/health").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "the health probe stays open");

    // The SPA shell must load so it can render the login screen.
    let res = build_router(state.clone())
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the static fallback is never gated"
    );

    // The run-hook endpoint carries its own path secret; an unknown one is a
    // 404 from the handler, never a 401 from the gate.
    let (status, _) = send(
        &state,
        Request::post("/api/hooks/run-1/deadbeef")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_session_cookie_authenticates_and_writes_need_the_csrf_header() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    let token = server_auth::random_token().unwrap();
    {
        state
            .auth
            .sessions()
            .insert(server_auth::hash_hex(&token), apb_core::clock::now_ms());
    }
    let cookie = format!("{SESSION_COOKIE}={token}");

    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a GET needs no marker header");

    let (status, body) = send(
        &state,
        Request::post("/api/connectors/demo/call")
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a cookie-authenticated write without the marker header is refused"
    );
    assert_eq!(body["error"], "csrf");

    let res = build_router(state.clone())
        .oneshot(
            Request::post("/api/connectors/demo/call")
                .header("cookie", &cookie)
                .header(CSRF_HEADER, CSRF_VALUE)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        StatusCode::FORBIDDEN,
        "with the marker header the request reaches the handler"
    );
}

#[tokio::test]
async fn a_bearer_write_needs_no_csrf_header() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());
    let res = build_router(state.clone())
        .oneshot(
            Request::post("/api/connectors/demo/call")
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::FORBIDDEN);
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn eleven_failures_in_a_window_earn_a_429() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    for i in 0..10 {
        let (status, _) = send(&state, Request::get("/api/runs").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {i} is a plain 401");
    }
    let (status, body) = send(&state, Request::get("/api/runs").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");

    let (status, _) = send(&state, Request::get("/api/health").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "exempt paths stay reachable");
}

#[tokio::test]
async fn a_tripped_limiter_blocks_even_a_valid_key_for_the_window() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());

    // Prove the key works before the limiter is tripped.
    let (status, _) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Burn the budget from the same client IP.
    for _ in 0..11 {
        send(&state, Request::get("/api/runs").body(Body::empty()).unwrap()).await;
    }

    // The block is evaluated before the credential is read, so a correct key
    // does not escape it. This is deliberate: a guesser who lands on the right
    // value must not be rewarded with instant access.
    let (status, body) = send(
        &state,
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");
}

#[tokio::test]
async fn the_websocket_upgrade_is_gated_like_every_other_api_route() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    let (status, body) = send(&state, Request::get("/api/ws").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "auth");
}

#[tokio::test]
async fn every_response_denies_framing() {
    let dir = seed();

    // With auth off, which is the local default.
    let open_state = AppState::new(dir.path().to_path_buf());
    let res = build_router(open_state)
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        res.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY"),
        "the panel must not be frameable even on a keyless dashboard"
    );

    // And on a rejection, where the response never reaches a handler.
    let (_key, state) = authed(dir.path().to_path_buf());
    let res = build_router(state)
        .oneshot(Request::get("/api/runs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
}

#[tokio::test]
async fn a_revoked_key_stops_working_and_a_new_one_starts_without_a_restart() {
    let dir = seed();
    // This test owns the key file for its whole duration, so the tempdir is
    // bound rather than dropped.
    let keydir = tempfile::tempdir().unwrap();
    let path = keydir.path().join("server-auth.yaml");
    let (key_a, record_a) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(
        AuthState::new(Some(path.clone()), file.keys, &ServerConfig::default()).unwrap(),
    );
    let state = AppState::new(dir.path().to_path_buf()).with_auth(auth);

    let bearer = |key: &str| {
        Request::get("/api/runs")
            .header("authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap()
    };

    let (status, _) = send(&state, bearer(&key_a)).await;
    assert_eq!(status, StatusCode::OK, "key A works to begin with");

    // Rotate the file underneath the running server, exactly as
    // `apb server key revoke` and `apb server key issue` would.
    server_auth::revoke_in(&path, &record_a.id).unwrap();
    let (key_b, _record_b) = server_auth::issue_into(&path).unwrap();
    // The stamp is (mtime, len); make sure the mtime moved even on a
    // coarse-grained filesystem clock.
    filetime_bump(&path);

    // The failing request forces the reload, which is what the spec asks for:
    // no restart, effect on the very next request.
    let (status, body) = send(&state, bearer(&key_a)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked key is rejected: {body}"
    );

    let (status, _) = send(&state, bearer(&key_b)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the newly issued key is accepted without a restart"
    );

    assert_eq!(state.auth.key_count(), 1, "the reloaded set holds only key B");
}

/// Makes sure the key file's mtime differs from whatever the auth state
/// recorded, on filesystems whose mtime granularity is a full second. Rewrites
/// the file's own bytes through the same atomic-private path the real writer
/// uses, so nothing about the content changes.
fn filetime_bump(path: &std::path::Path) {
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let raw = std::fs::read(path).unwrap();
    apb_core::fsutil::atomic_write_private(path, &raw).unwrap();
}
```

Register in `crates/apb-server/tests/main.rs`, alphabetically (right after the `api_test` block and before `bind_interlock_test`):

```rust
#[path = "suite/auth_test.rs"]
mod auth_test;
```

- [ ] **Step 2: run the tests and watch them fail**

```sh
cargo test -p apb-server --test main auth_test
```

Expected: `unresolved import `apb_server::auth``.

- [ ] **Step 3: implement the auth module**

Create `crates/apb-server/src/auth.rs`:

```rust
//! Server-mode authentication (spec 2026-08-16-server-mode-design).
//!
//! One axum middleware wraps the whole router. Its evaluation order is fixed:
//! exempt paths first, then the "no keys means no auth" pass-through that
//! keeps the local dashboard byte-for-byte as it was, then a bearer key, then
//! a session cookie, then 401. Cookie-authenticated writes additionally carry
//! a custom marker header, which a cross-site form cannot set.
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
use std::sync::{Mutex, MutexGuard};

use apb_core::server_auth::{self, KeyRecord};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};

use crate::state::AppState;

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

/// Live browser sessions, keyed by the SHA-256 of the session token. The raw
/// token exists only in the cookie; a memory dump of the server yields
/// nothing usable. Sessions are deliberately not persisted: a restart returns
/// the operator to the login screen, which is cheap and removes a state file.
#[derive(Default)]
pub struct SessionStore {
    entries: HashMap<String, u128>,
}

impl SessionStore {
    /// Registers a new session at `now_ms`, pruning expired entries and
    /// evicting the least recently used one when the store is full.
    pub fn insert(&mut self, token_hash: String, now_ms: u128) {
        self.prune(now_ms);
        if self.entries.len() >= MAX_SESSIONS {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, seen)| **seen)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(token_hash, now_ms);
    }

    /// Whether the hash names a live session, refreshing its sliding TTL. An
    /// expired entry is removed on the way out.
    pub fn touch(&mut self, token_hash: &str, now_ms: u128) -> bool {
        match self.entries.get(token_hash).copied() {
            Some(seen) if now_ms.saturating_sub(seen) < SESSION_TTL_MS => {
                self.entries.insert(token_hash.to_string(), now_ms);
                true
            }
            Some(_) => {
                self.entries.remove(token_hash);
                false
            }
            None => false,
        }
    }

    pub fn remove(&mut self, token_hash: &str) {
        self.entries.remove(token_hash);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn prune(&mut self, now_ms: u128) {
        self.entries
            .retain(|_, seen| now_ms.saturating_sub(*seen) < SESSION_TTL_MS);
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
                now_ms.saturating_sub(*start) < FAILURE_WINDOW_MS && *count > MAX_FAILURES_PER_WINDOW
            }
            None => false,
        }
    }

    fn prune(&mut self, now_ms: u128) {
        self.windows
            .retain(|_, (start, _)| now_ms.saturating_sub(*start) < FAILURE_WINDOW_MS);
        if self.windows.len() > MAX_RATE_LIMIT_ENTRIES {
            self.windows.clear();
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

/// The live key set plus what is needed to notice the file changing under it.
struct KeySet {
    keys: Vec<KeyRecord>,
    stamp: Option<FileStamp>,
    last_check_ms: u128,
}

/// Everything the auth layer needs, shared behind an `Arc` in [`AppState`].
///
/// The key set is live, not a startup snapshot: `server-auth.yaml` is
/// re-stat'ed and reloaded when its mtime or length changes. The check runs at
/// most once per [`KEY_RELOAD_INTERVAL_MS`] on the ordinary request path, so a
/// busy server pays one `stat` per minute and every other request is
/// filesystem-free, and it runs immediately (throttle bypassed) whenever a
/// presented key fails to verify. That is what makes issuing a first key or
/// revoking a compromised one take effect without restarting the dashboard: a
/// revoked key's next request forces the reload that rejects it, and a newly
/// issued key's first request forces the reload that accepts it.
pub struct AuthState {
    /// The key file to watch. `None` in tests that do not exercise reloading
    /// and in [`AuthState::disabled`]; reload checks are then no-ops.
    path: Option<std::path::PathBuf>,
    keys: Mutex<KeySet>,
    trusted_proxies: BTreeSet<IpAddr>,
    public_https: bool,
    sessions: Mutex<SessionStore>,
    failures: Mutex<RateLimiter>,
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
                last_check_ms: 0,
            }),
            trusted_proxies: BTreeSet::new(),
            public_https: false,
            sessions: Mutex::new(SessionStore::default()),
            failures: Mutex::new(RateLimiter::default()),
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
        Ok(Self {
            path,
            keys: Mutex::new(KeySet {
                keys,
                stamp,
                last_check_ms: apb_core::clock::now_ms(),
            }),
            trusted_proxies: cfg.trusted_proxy_set()?,
            public_https: cfg.public_scheme_is_https(),
            sessions: Mutex::new(SessionStore::default()),
            failures: Mutex::new(RateLimiter::default()),
        })
    }

    fn key_set(&self) -> MutexGuard<'_, KeySet> {
        self.keys.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-stat the key file and reload it when it changed. `force` bypasses
    /// the once-per-minute throttle and is used on an auth failure, which is
    /// exactly the moment a stale key set would produce the wrong answer.
    /// A file that disappeared or became unreadable leaves the current key set
    /// in place: losing the file must not silently disable authentication.
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

    /// Auth is enforced if and only if at least one key exists.
    pub fn enabled(&self) -> bool {
        !self.key_set().keys.is_empty()
    }

    pub fn key_count(&self) -> usize {
        self.key_set().keys.len()
    }

    /// The id of the key `presented` is, or `None`.
    pub fn verify_key(&self, presented: &str) -> Option<String> {
        let set = self.key_set();
        server_auth::verify(&set.keys, presented)
    }

    /// Verify, and on failure re-read the key file once before answering, so a
    /// key issued or revoked while the server was running takes effect on the
    /// very next request rather than on the next restart.
    pub fn verify_key_with_reload(&self, presented: &str, now_ms: u128) -> Option<String> {
        if let Some(id) = self.verify_key(presented) {
            return Some(id);
        }
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
        let live = {
            let mut sessions = auth.sessions();
            sessions.touch(&hash, now_ms)
        };
        if live {
            return Credential::Cookie;
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
pub async fn auth_middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let auth = state.auth.clone();
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

    if !auth.enabled() {
        return deny_framing(next.run(req).await);
    }

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    if is_exempt(&method, &path) {
        return deny_framing(next.run(req).await);
    }

    let blocked = {
        let failures = auth.failures();
        failures.is_blocked(ctx.ip, now)
    };
    if blocked {
        return deny_framing(rate_limited());
    }

    let credential = evaluate(&auth, req.headers(), now);
    let res = match credential {
        Credential::None => {
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
        store.insert("a".to_string(), 0);
        assert!(store.touch("a", 1_000));
        assert!(
            !store.touch("a", SESSION_TTL_MS + 2_000),
            "a session past its sliding TTL is dead"
        );
        assert!(store.is_empty(), "and is dropped on the way out");

        for i in 0..MAX_SESSIONS {
            store.insert(format!("s{i}"), 1_000 + i as u128);
        }
        assert_eq!(store.len(), MAX_SESSIONS);
        store.insert("newest".to_string(), 9_999_999);
        assert_eq!(store.len(), MAX_SESSIONS, "the cap holds");
        assert!(!store.touch("s0", 9_999_999), "the oldest was evicted");
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
}
```

- [ ] **Step 4: wire the state and the router**

In `crates/apb-server/src/state.rs`, replace the `AppState` struct and its impl block with this complete version:

```rust
#[derive(Clone)]
pub struct AppState {
    /// A pinned single project root. `None` is the production, global-only
    /// dashboard: there is no project-scoped server, and every project-specific
    /// request resolves its root from the `?workspace=<id>` param through the
    /// project registry. `Some` exists only for the pinned-root test harness
    /// (and keeps the older single-project handler tests unchanged): with a
    /// pinned root, a request that omits `workspace` falls back to it.
    pub root: Option<Arc<PathBuf>>,
    pub events: broadcast::Sender<String>,
    /// Server-mode authentication (spec 2026-08-16-server-mode-design).
    /// Disabled by default, which is exactly today's local behavior;
    /// `run_server` attaches a populated state when keys exist.
    pub auth: Arc<crate::auth::AuthState>,
}

impl AppState {
    /// Pinned to a single project root (test harness / backward-compat).
    pub fn new(root: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: Some(Arc::new(root)),
            events,
            auth: Arc::new(crate::auth::AuthState::disabled()),
        }
    }

    /// The global, machine-wide dashboard: no pinned root, projects resolved
    /// per request from the registry.
    pub fn new_global() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: None,
            events,
            auth: Arc::new(crate::auth::AuthState::disabled()),
        }
    }

    /// The global dashboard with server-mode auth attached.
    pub fn new_global_with_auth(auth: Arc<crate::auth::AuthState>) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            root: None,
            events,
            auth,
        }
    }

    /// Attaches an auth state to an existing one. The shape the tests use.
    pub fn with_auth(mut self, auth: Arc<crate::auth::AuthState>) -> Self {
        self.auth = auth;
        self
    }
}
```

In `crates/apb-server/src/lib.rs`, add the module to the list in alphabetical order, which puts it after `pub mod assets;` and before `pub mod lock;` (a-s-s sorts before a-u-t):

```rust
pub mod auth;
```

Replace the tail of `build_router` (from `.route("/api/ws", ...)` onward) with:

```rust
        .route("/api/ws", get(ws::ws_handler))
        .fallback(assets::static_handler)
        // The gate wraps everything, including the static fallback, so that
        // ClientCtx is present on every request. Exempt paths are decided
        // inside the middleware, not by leaving routes outside the layer.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        .with_state(state)
```

In `run_server`, replace the two lines that load the keys and build the state:

```rust
    let global_cfg = apb_core::config::GlobalConfig::load().map_err(std::io::Error::other)?;
    let auth_path = apb_core::server_auth::auth_file_path()?;
    let auth_file = apb_core::server_auth::load_from(&auth_path)?;
    check_bind_allowed(bind, auth_file.keys.len()).map_err(std::io::Error::other)?;
    // The path is handed to the auth state so it can notice the file changing:
    // issuing a first key or revoking a compromised one takes effect on a
    // running dashboard without a restart.
    let auth = std::sync::Arc::new(
        auth::AuthState::new(Some(auth_path), auth_file.keys, &global_cfg.server)
            .map_err(std::io::Error::other)?,
    );
    let auth_enabled = auth.enabled();
    let state = AppState::new_global_with_auth(auth);
```

and replace the startup print with:

```rust
    println!(
        "apb dashboard (global): http://{}:{port}",
        display_host(bind)
    );
    if auth_enabled {
        println!("authentication is on: sign in with a key from `apb server key issue`");
    }
    if let Some(url) = global_cfg.server.public_base_url.as_deref() {
        println!("public address: {url}");
    }
    // Behind a proxy with no trusted peer configured, every client arrives as
    // the proxy's own address, so all of them share one rate-limit key and a
    // single attacker can exhaust the failure budget for everyone.
    if global_cfg.server.public_base_url.is_some() && global_cfg.server.trusted_proxies.is_empty() {
        eprintln!(
            "apb dashboard: server.public_base_url is set but server.trusted_proxies is empty; every client will share the proxy's IP as one rate-limit key. Add the proxy's address to server.trusted_proxies (see docs/DEPLOYMENT.md)"
        );
    }
```

- [ ] **Step 5: run the tests and watch them pass**

```sh
cargo test -p apb-server --test main auth_test
cargo test -p apb-server --lib
```

Expected: 10 integration tests pass and 8 unit tests in `auth::tests` pass. The reload test sleeps just over a second on purpose (see `filetime_bump`), so it is the slowest of the set.

- [ ] **Step 6: gates and commit**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```sh
git add crates/apb-server/src/auth.rs crates/apb-server/src/state.rs crates/apb-server/src/lib.rs crates/apb-server/tests/suite/auth_test.rs crates/apb-server/tests/main.rs
git commit --signoff -m "$(cat <<'EOF'
feat(server): auth middleware with sessions, CSRF and rate limiting

One axum layer gates every /api route once a key exists: bearer keys for
machines, an in-memory sliding-TTL session store for the browser, a custom
marker header required on cookie-authenticated writes, and a per-IP
fixed-window failure limiter that answers 429. With no keys the layer is a
pass-through, so the local dashboard is unchanged.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: auth endpoints (login, logout, status)

**Files:**
- Create: `crates/apb-server/src/routes/auth.rs`
- Modify: `crates/apb-server/src/routes/mod.rs` (module list)
- Modify: `crates/apb-server/src/lib.rs` (`build_router`, add three routes after `/api/health`)
- Test: create `crates/apb-server/tests/suite/auth_endpoints_test.rs`, register it in `crates/apb-server/tests/main.rs`

**Interfaces:**
- Consumes: `crate::auth::{AuthState, ClientCtx, Credential, evaluate, cookie_value, log_auth_failure, rate_limited, unauthorized, SESSION_COOKIE}` and `AuthState::verify_key_with_reload` (Task 4), `apb_core::server_auth::{hash_hex, random_token}` (Task 1).
- Produces: `routes::auth::{login_handler, logout_handler, status_handler}` and the routes `POST /api/auth/login`, `POST /api/auth/logout`, `GET /api/auth/status`. Task 7 and Task 8 consume the wire shapes `{"auth_required":bool,"authenticated":bool}` and `{"key":"..."}`.

- [ ] **Step 1: write the failing tests**

Create `crates/apb-server/tests/suite/auth_endpoints_test.rs`:

```rust
//! `/api/auth/login`, `/api/auth/logout`, `/api/auth/status`. Same in-process
//! router harness as the rest of the suite; the AppState is cloned per request
//! so the shared session store survives across calls.

use apb_core::config::ServerConfig;
use apb_core::server_auth;
use apb_server::auth::{AuthState, CSRF_HEADER, CSRF_VALUE, SESSION_COOKIE};
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    dir
}

fn authed_with(root: std::path::PathBuf, cfg: ServerConfig) -> (String, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server-auth.yaml");
    let (key, _record) = server_auth::issue_into(&path).unwrap();
    let file = server_auth::load_from(&path).unwrap();
    let auth = Arc::new(AuthState::new(None, file.keys, &cfg).unwrap());
    (key, AppState::new(root).with_auth(auth))
}

async fn send(state: &AppState, req: Request<Body>) -> (StatusCode, Option<String>, serde_json::Value) {
    let res = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, cookie, json)
}

fn login_request(key: &str) -> Request<Body> {
    Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, CSRF_VALUE)
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "key": key })).unwrap(),
        ))
        .unwrap()
}

#[tokio::test]
async fn status_reports_auth_off_for_a_keyless_server() {
    let dir = seed();
    let state = AppState::new(dir.path().to_path_buf());
    let (status, _, body) = send(
        &state,
        Request::get("/api/auth/status").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_required"], false);
    assert_eq!(
        body["authenticated"], true,
        "with auth off every caller is authenticated by definition"
    );
}

#[tokio::test]
async fn status_reports_required_and_unauthenticated_before_login() {
    let dir = seed();
    let (_key, state) = authed_with(dir.path().to_path_buf(), ServerConfig::default());
    let (status, _, body) = send(
        &state,
        Request::get("/api/auth/status").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_required"], true);
    assert_eq!(body["authenticated"], false);
}

#[tokio::test]
async fn login_mints_a_usable_session_cookie() {
    let dir = seed();
    let (key, state) = authed_with(dir.path().to_path_buf(), ServerConfig::default());

    let (status, set_cookie, body) = send(&state, login_request(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["authenticated"], true);
    let set_cookie = set_cookie.expect("a session cookie is set");
    assert!(set_cookie.starts_with(&format!("{SESSION_COOKIE}=")));
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
    assert!(set_cookie.contains("Path=/"), "{set_cookie}");
    assert!(
        !set_cookie.contains("Secure"),
        "plain http must not set a Secure cookie the browser would drop: {set_cookie}"
    );
    assert!(
        !set_cookie.contains("Max-Age") && !set_cookie.contains("Expires"),
        "a browser session cookie carries no lifetime; the sliding expiry is server-side: {set_cookie}"
    );
    assert!(
        !set_cookie.contains(&key),
        "the API key never rides in the cookie: {set_cookie}"
    );

    let token = set_cookie
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let (status, _, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the cookie authenticates");

    let (status, _, body) = send(
        &state,
        Request::get("/api/auth/status")
            .header("cookie", &token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_required"], true);
    assert_eq!(body["authenticated"], true);

    // Logout drops the session and clears the cookie.
    let (status, cleared, _) = send(
        &state,
        Request::post("/api/auth/logout")
            .header("cookie", &token)
            .header(CSRF_HEADER, CSRF_VALUE)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cleared = cleared.expect("logout clears the cookie");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    let (status, _, _) = send(
        &state,
        Request::get("/api/runs")
            .header("cookie", &token)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked session no longer authenticates"
    );
}

#[tokio::test]
async fn a_wrong_key_is_401_and_the_eleventh_attempt_is_429() {
    let dir = seed();
    let (_key, state) = authed_with(dir.path().to_path_buf(), ServerConfig::default());
    for i in 0..10 {
        let (status, cookie, body) = send(&state, login_request("apb_wrong-key")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {i}: {body}");
        assert_eq!(body["error"], "auth");
        assert!(cookie.is_none(), "a failed login sets no cookie");
    }
    let (status, _, body) = send(&state, login_request("apb_wrong-key")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"], "rate_limited");
}

#[tokio::test]
async fn a_public_https_origin_makes_the_cookie_secure() {
    let dir = seed();
    let cfg = ServerConfig {
        bind: None,
        public_base_url: Some("https://apb.example.com".to_string()),
        trusted_proxies: Vec::new(),
    };
    let (key, state) = authed_with(dir.path().to_path_buf(), cfg);
    let (status, set_cookie, _) = send(&state, login_request(&key)).await;
    assert_eq!(status, StatusCode::OK);
    let set_cookie = set_cookie.expect("a session cookie is set");
    assert!(set_cookie.contains("Secure"), "{set_cookie}");
}

#[tokio::test]
async fn login_on_a_keyless_server_is_a_bad_request_not_a_session() {
    let dir = seed();
    let state = AppState::new(dir.path().to_path_buf());
    let (status, cookie, body) = send(&state, login_request("apb_anything")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "auth_disabled");
    assert!(cookie.is_none());
}
```

Register in `crates/apb-server/tests/main.rs` (after `auth_test`):

```rust
#[path = "suite/auth_endpoints_test.rs"]
mod auth_endpoints_test;
```

- [ ] **Step 2: run the tests and watch them fail**

```sh
cargo test -p apb-server --test main auth_endpoints
```

Expected: every test fails with `404 Not Found` (the routes do not exist yet), except the keyless status test which fails the same way.

- [ ] **Step 3: implement the endpoints**

Create `crates/apb-server/src/routes/auth.rs`:

```rust
//! The three endpoints the browser login flow needs (spec
//! 2026-08-16-server-mode-design).
//!
//! `login` is the only place a raw API key crosses the HTTP boundary inbound.
//! It is exchanged for an opaque session token whose SHA-256 is what the
//! server keeps; the SPA never stores the key itself. `status` exists so the
//! SPA can decide whether to render the login screen without provoking a 401,
//! and it is exempt from the gate for exactly that reason.

use crate::auth::{
    ClientCtx, Credential, SESSION_COOKIE, cookie_value, evaluate, log_auth_failure, rate_limited,
};
use crate::state::AppState;

use apb_core::server_auth;
use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct LoginBody {
    key: String,
}

/// The session cookie exactly as the browser must store it. `Secure` is added
/// only when the browser actually reached apb over https, because a Secure
/// cookie sent over plain http is discarded and the loopback dashboard would
/// then never be able to log in.
///
/// Deliberately carries no `Max-Age` or `Expires`, so it is a browser session
/// cookie. The 7-day sliding expiry lives server-side in the session store,
/// which is the only place that can see activity: a cookie lifetime would
/// either outlive an evicted store entry (a cookie the server no longer
/// honors) or expire under a still-active session.
fn session_cookie(token: &str, https: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if https {
        cookie.push_str("; Secure");
    }
    cookie
}

fn cleared_cookie(https: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if https {
        cookie.push_str("; Secure");
    }
    cookie
}

fn with_cookie(mut res: Response, cookie: &str) -> Response {
    if let Ok(value) = header::HeaderValue::from_str(cookie) {
        res.headers_mut().insert(header::SET_COOKIE, value);
    }
    res
}

/// POST /api/auth/login: one API key in, one session cookie out.
pub(crate) async fn login_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<ClientCtx>,
    Json(body): Json<LoginBody>,
) -> Response {
    let auth = state.auth.clone();
    if !auth.enabled() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "auth_disabled" })),
        )
            .into_response();
    }
    let now = apb_core::clock::now_ms();
    let blocked = {
        let failures = auth.failures();
        failures.is_blocked(ctx.ip, now)
    };
    if blocked {
        return rate_limited();
    }
    // The reload-on-failure variant: an operator who has just issued the very
    // first key can sign in immediately, without restarting the dashboard.
    if auth.verify_key_with_reload(body.key.trim(), now).is_none() {
        let over_budget = {
            let mut failures = auth.failures();
            failures.record_failure(ctx.ip, now)
        };
        log_auth_failure(ctx.ip, "/api/auth/login");
        return if over_budget {
            rate_limited()
        } else {
            crate::auth::unauthorized()
        };
    }
    let token = match server_auth::random_token() {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "random", "message": e.to_string() })),
            )
                .into_response();
        }
    };
    {
        let mut sessions = auth.sessions();
        sessions.insert(server_auth::hash_hex(&token), now);
    }
    let res = Json(serde_json::json!({ "authenticated": true })).into_response();
    with_cookie(res, &session_cookie(&token, ctx.https))
}

/// POST /api/auth/logout: drops this session and clears the cookie. Goes
/// through the gate like any other write, so it carries the marker header.
pub(crate) async fn logout_handler(
    State(state): State<AppState>,
    Extension(ctx): Extension<ClientCtx>,
    headers: HeaderMap,
) -> Response {
    let auth = state.auth.clone();
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        let mut sessions = auth.sessions();
        sessions.remove(&server_auth::hash_hex(&token));
    }
    let res = Json(serde_json::json!({ "authenticated": false })).into_response();
    with_cookie(res, &cleared_cookie(ctx.https))
}

/// GET /api/auth/status: what the SPA needs to decide between the login screen
/// and the app, without probing a protected route and swallowing a 401.
pub(crate) async fn status_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let auth = state.auth.clone();
    let required = auth.enabled();
    let authenticated = if required {
        evaluate(&auth, &headers, apb_core::clock::now_ms()) != Credential::None
    } else {
        true
    };
    Json(serde_json::json!({
        "auth_required": required,
        "authenticated": authenticated,
    }))
    .into_response()
}
```

Add to `crates/apb-server/src/routes/mod.rs`, keeping the list alphabetical (before the existing first entry):

```rust
pub mod auth;
```

In `crates/apb-server/src/lib.rs`, add the three routes immediately after the `/api/health` route:

```rust
        .route("/api/health", get(routes::meta::health))
        .route("/api/auth/login", post(routes::auth::login_handler))
        .route("/api/auth/logout", post(routes::auth::logout_handler))
        .route("/api/auth/status", get(routes::auth::status_handler))
```

- [ ] **Step 4: run the tests and watch them pass**

```sh
cargo test -p apb-server --test main auth_endpoints
cargo test -p apb-server
```

Expected: 6 endpoint tests pass and the whole apb-server suite is green.

- [ ] **Step 5: gates and commit**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```sh
git add crates/apb-server/src/routes/auth.rs crates/apb-server/src/routes/mod.rs crates/apb-server/src/lib.rs crates/apb-server/tests/suite/auth_endpoints_test.rs crates/apb-server/tests/main.rs
git commit --signoff -m "$(cat <<'EOF'
feat(server): login, logout and status endpoints

POST /api/auth/login exchanges an API key for an HttpOnly SameSite=Lax
session cookie (Secure only when the request really arrived over https),
POST /api/auth/logout drops the session, and GET /api/auth/status tells the
SPA whether to show the login screen. Failed logins feed the same per-IP
limiter as the gate and emit the greppable auth_failed line.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: WebSocket coverage and the constant-time hook-secret compare

**Files:**
- Modify: `crates/apb-server/src/routes/runs.rs` (`post_hook_handler` at lines 225-254, specifically the secret match at line 247)
- Test: modify `crates/apb-server/tests/suite/auth_test.rs` (add the accepted-WS case), rely on the existing webhook cases in `crates/apb-server/tests/suite/runs_api_test.rs` (lines 104-190) for the hook regression

**Interfaces:**
- Consumes: `apb_core::server_auth::ct_eq_str` (Task 1), `apb_server::auth::SESSION_COOKIE` (Task 4).
- Produces: no new symbols. The hook handler's behavior is unchanged (a matching secret signals, anything else is a 404); only the comparison becomes constant time.

- [ ] **Step 1: write the failing test**

Append to `crates/apb-server/tests/suite/auth_test.rs`:

```rust
#[tokio::test]
async fn a_session_cookie_is_accepted_at_the_websocket_upgrade() {
    let dir = seed();
    let (_key, state) = authed(dir.path().to_path_buf());
    let token = server_auth::random_token().unwrap();
    {
        state
            .auth
            .sessions()
            .insert(server_auth::hash_hex(&token), apb_core::clock::now_ms());
    }
    // A plain GET cannot complete an upgrade, so the assertion is about the
    // gate: with a session cookie the request reaches the ws handler (which
    // answers with an upgrade error), without one it is stopped at 401.
    let res = build_router(state.clone())
        .oneshot(
            Request::get("/api/ws")
                .header("cookie", format!("{SESSION_COOKIE}={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_bearer_key_is_accepted_at_the_websocket_upgrade() {
    let dir = seed();
    let (key, state) = authed(dir.path().to_path_buf());
    let res = build_router(state.clone())
        .oneshot(
            Request::get("/api/ws")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: run the tests**

```sh
cargo test -p apb-server --test main auth_test
```

Expected: the two new tests pass already (the middleware from Task 4 covers `/api/ws`), confirming the WebSocket needs no separate gate. If either fails with 401, the exempt list is wrong and must be fixed before continuing.

- [ ] **Step 3: replace the hook-secret comparison**

In `crates/apb-server/src/routes/runs.rs`, replace the secret lookup inside `post_hook_handler` with this complete version:

```rust
    // The secret must match one of this run's hooks (otherwise 404 - a
    // foreign or incorrect secret must not accept the signal). Only the
    // comparison changes: a plain `==` leaks a live secret's bytes through
    // response timing, so each candidate is compared in constant time. The
    // first-match semantics of the previous `find` are preserved exactly,
    // including the break, so a run whose hooks somehow share a secret still
    // signals the same key it always did.
    let mut matched: Option<&String> = None;
    for (key, candidate) in hooks.iter() {
        if apb_core::server_auth::ct_eq_str(candidate, &secret) {
            matched = Some(key);
            break;
        }
    }
    let Some(key) = matched else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
```

- [ ] **Step 4: run the webhook regression**

```sh
cargo test -p apb-server --test main runs_api
cargo test -p apb-server --test main auth
```

Expected: the existing `hook endpoint` cases in `runs_api_test.rs` (correct secret signals the node, `deadbeef` is a 404) still pass, and all auth tests pass.

- [ ] **Step 5: gates and commit**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```sh
git add crates/apb-server/src/routes/runs.rs crates/apb-server/tests/suite/auth_test.rs
git commit --signoff -m "$(cat <<'EOF'
fix(server): constant-time compare for the run-hook secret

The webhook endpoint compared its path secret with a plain string equality
that short-circuits on the first differing byte. It now uses the same
constant-time helper the server-mode keys use, with the first-match lookup
semantics unchanged. Adds WebSocket cases proving the upgrade goes through
the auth gate with either credential.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: frontend auth foundation

**Files:**
- Create: `web/src/lib/auth.ts`
- Create: `web/src/lib/auth.test.ts`
- Modify: `web/src/lib/api/http.ts` (whole file; `getJson` at lines 5-9, `requestJson` at lines 28-36)
- Modify: `web/src/lib/ws.ts` (`subscribeChanges` at lines 11-41)
- Modify: `web/src/lib/api.test.ts` (15 `toHaveBeenCalledWith('/api...` assertions at lines 38, 45, 53, 60, 69, 109, 129, 148, 157, 175, 180, 190, 194, 215, 231)

**Interfaces:**
- Consumes: `GET /api/auth/status`, `POST /api/auth/login`, `POST /api/auth/logout` (Task 5).
- Produces: `web/src/lib/auth.ts` exporting `XRW_HEADER`, `XRW_VALUE`, `apiHeaders(extra?)`, `type AuthSnapshot`, the store `auth`, `refreshAuthStatus()`, `login(key)`, `logout()`, `markUnauthenticated()`. Task 8 consumes `auth`, `login`, `logout`, `refreshAuthStatus`.
- Direction of imports is one way: `auth.ts` never imports from `api/`, `api/http.ts` imports from `../auth`. That is what keeps the module graph acyclic.

- [ ] **Step 1: write the failing tests**

Create `web/src/lib/auth.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { get } from 'svelte/store'
import {
  apiHeaders,
  auth,
  login,
  logout,
  markUnauthenticated,
  refreshAuthStatus,
  XRW_HEADER,
  XRW_VALUE,
} from './auth'

const fetchMock = vi.fn<typeof fetch>()

beforeEach(() => {
  vi.stubGlobal('fetch', fetchMock)
  auth.set({ required: false, authenticated: true, checked: false })
})

afterEach(() => {
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

describe('apiHeaders', () => {
  it('always carries the marker header', () => {
    expect(apiHeaders()).toEqual({ [XRW_HEADER]: XRW_VALUE })
  })

  it('merges extra headers without losing the marker', () => {
    expect(apiHeaders({ 'content-type': 'application/json' })).toEqual({
      'content-type': 'application/json',
      [XRW_HEADER]: XRW_VALUE,
    })
  })
})

describe('refreshAuthStatus', () => {
  it('stores what the server reports', async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ auth_required: true, authenticated: false }),
    )
    const state = await refreshAuthStatus()
    expect(state).toEqual({ required: true, authenticated: false, checked: true })
    expect(get(auth)).toEqual(state)
    expect(fetchMock).toHaveBeenCalledWith('/api/auth/status', {
      headers: { [XRW_HEADER]: XRW_VALUE },
    })
  })

  it('treats an unreachable or unknown endpoint as auth off', async () => {
    fetchMock.mockResolvedValueOnce(new Response('nope', { status: 404 }))
    const state = await refreshAuthStatus()
    expect(state).toEqual({ required: false, authenticated: true, checked: true })
  })
})

describe('login', () => {
  it('posts the key and refreshes the status on success', async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse({ authenticated: true }))
      .mockResolvedValueOnce(jsonResponse({ auth_required: true, authenticated: true }))
    const result = await login('apb_secret')
    expect(result.ok).toBe(true)
    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/auth/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json', [XRW_HEADER]: XRW_VALUE },
      body: JSON.stringify({ key: 'apb_secret' }),
    })
    expect(get(auth)).toEqual({ required: true, authenticated: true, checked: true })
  })

  it('reports a rejected key without touching the store', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: 'auth' }, 401))
    const result = await login('apb_wrong')
    expect(result.ok).toBe(false)
    expect(result.message).toMatch(/not accepted/)
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('reports rate limiting distinctly', async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: 'rate_limited' }, 429))
    const result = await login('apb_wrong')
    expect(result.ok).toBe(false)
    expect(result.message).toMatch(/Too many attempts/)
  })
})

describe('logout', () => {
  it('posts logout and re-reads the status', async () => {
    fetchMock
      .mockResolvedValueOnce(jsonResponse({ authenticated: false }))
      .mockResolvedValueOnce(jsonResponse({ auth_required: true, authenticated: false }))
    await logout()
    expect(fetchMock).toHaveBeenNthCalledWith(1, '/api/auth/logout', {
      method: 'POST',
      headers: { [XRW_HEADER]: XRW_VALUE },
    })
    expect(get(auth)).toEqual({ required: true, authenticated: false, checked: true })
  })
})

describe('markUnauthenticated', () => {
  it('flips the store into the login state', () => {
    markUnauthenticated()
    expect(get(auth)).toEqual({ required: true, authenticated: false, checked: true })
  })
})
```

Append to `web/src/lib/api.test.ts` a test that the fetch layer reacts to a 401:

```ts
describe('401 handling', () => {
  it('flips the auth store and still throws an ApiError', async () => {
    const { auth } = await import('./auth')
    const { get } = await import('svelte/store')
    auth.set({ required: false, authenticated: true, checked: false })
    fetchMock.mockResolvedValueOnce(jsonResponse({ error: 'auth' }, 401))
    await expect(fetchPlaybook('demo')).rejects.toThrow(/HTTP 401/)
    expect(get(auth)).toEqual({ required: true, authenticated: false, checked: true })
  })
})
```

- [ ] **Step 2: run the tests and watch them fail**

```sh
cd web && bun run test
```

Expected: `auth.test.ts` fails to resolve `./auth`, and the 15 existing `toHaveBeenCalledWith` assertions in `api.test.ts` still pass (they break in step 4, after the header is added).

- [ ] **Step 3: implement the auth module**

Create `web/src/lib/auth.ts`:

```ts
// Server-mode authentication state for the dashboard (spec
// 2026-08-16-server-mode-design). The raw API key is never stored: the login
// screen posts it once and the server answers with an HttpOnly session cookie
// the browser attaches on its own, so this module holds only two booleans.
//
// This module deliberately uses the bare `fetch` and imports nothing from
// `./api`: the fetch layer imports the marker header and the 401 hook from
// here, and a cycle between the two would be a real ordering hazard.
import { writable } from 'svelte/store'

/** Marker header the server requires on cookie-authenticated writes. A
 * cross-site form cannot set it, which is the second CSRF layer behind
 * SameSite=Lax. Harmless when auth is off. */
export const XRW_HEADER = 'x-requested-with'
export const XRW_VALUE = 'apb-dashboard'

/** Headers every dashboard request carries, merged over the caller's own. */
export function apiHeaders(extra?: Record<string, string>): Record<string, string> {
  return { ...(extra ?? {}), [XRW_HEADER]: XRW_VALUE }
}

export interface AuthSnapshot {
  /** The server has at least one API key, so credentials are enforced. */
  required: boolean
  /** This browser currently holds a valid session (or auth is off). */
  authenticated: boolean
  /** A status read has completed at least once. */
  checked: boolean
}

/** Optimistic default: the local, keyless dashboard is the common case, and
 * starting in the authenticated state keeps it from flashing a login screen
 * while the first status read is in flight. A 401 corrects it immediately. */
export const auth = writable<AuthSnapshot>({
  required: false,
  authenticated: true,
  checked: false,
})

/** Reads GET /api/auth/status. An unreachable or older server (no such route)
 * is treated as auth off, so a dashboard built before server mode keeps
 * working against a newer frontend and vice versa. */
export async function refreshAuthStatus(): Promise<AuthSnapshot> {
  let next: AuthSnapshot = { required: false, authenticated: true, checked: true }
  try {
    const res = await fetch('/api/auth/status', { headers: apiHeaders() })
    if (res.ok) {
      const body = (await res.json()) as { auth_required?: boolean; authenticated?: boolean }
      next = {
        required: body.auth_required === true,
        authenticated: body.authenticated === true,
        checked: true,
      }
    }
  } catch {
    // Network failure: keep the permissive default rather than locking the
    // operator out of a dashboard that may simply be restarting.
  }
  auth.set(next)
  return next
}

/** Exchanges a pasted API key for a session cookie. The key is not kept
 * anywhere after this call returns. */
export async function login(key: string): Promise<{ ok: boolean; message?: string }> {
  let res: Response
  try {
    res = await fetch('/api/auth/login', {
      method: 'POST',
      headers: apiHeaders({ 'content-type': 'application/json' }),
      body: JSON.stringify({ key }),
    })
  } catch {
    return { ok: false, message: 'The server could not be reached.' }
  }
  if (res.ok) {
    await refreshAuthStatus()
    return { ok: true }
  }
  if (res.status === 429) {
    return { ok: false, message: 'Too many attempts. Wait a minute and try again.' }
  }
  return { ok: false, message: 'That key was not accepted.' }
}

/** Ends the session and returns the app to the login screen. */
export async function logout(): Promise<void> {
  try {
    await fetch('/api/auth/logout', { method: 'POST', headers: apiHeaders() })
  } catch {
    // A failed logout still drops the local view of the session below.
  }
  await refreshAuthStatus()
}

/** Called by the fetch layer on any 401: the session expired or never
 * existed, so the app shows the login screen instead of a generic error. */
export function markUnauthenticated(): void {
  auth.set({ required: true, authenticated: false, checked: true })
}
```

- [ ] **Step 4: implement the fetch layer and the WebSocket change**

Replace the top of `web/src/lib/api/http.ts` (the comment block, `getJson`, `ApiError`, and `requestJson`) with this complete version; `errorMessage`, `jsonHeaders`, `qs`, `pb` and `run` below it stay exactly as they are:

```ts
// The fetch layer every api module shares: JSON request/response handling,
// a single place where a failed response becomes a readable Error, and the
// two URL builders that keep ids escaped.
//
// Every request carries the CSRF marker header, and every 401 flips the shared
// auth store so the app can show the login screen instead of a toast nobody
// can act on. Both live in `../auth`, which imports nothing from here.

import { apiHeaders, markUnauthenticated } from '../auth'

export async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url, { headers: apiHeaders() })
  if (!res.ok) {
    if (res.status === 401) markUnauthenticated()
    throw new ApiError(`${url}: HTTP ${res.status}`, res.status)
  }
  return res.json() as Promise<T>
}

/// An error carrying the HTTP status, so callers can branch on it structurally
/// (e.g. a 409 conflict) instead of matching substrings in the message. `code`
/// is the machine-readable `error` field of the JSON body when the server sent
/// one, so a caller can map a documented code to its own copy.
export class ApiError extends Error {
  status: number
  code?: string
  detail?: string
  constructor(message: string, status: number, code?: string, detail?: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.detail = detail
  }
}

export async function requestJson<T>(url: string, init: RequestInit): Promise<T> {
  const headers = apiHeaders(init.headers as Record<string, string> | undefined)
  const res = await fetch(url, { ...init, headers })
  if (!res.ok) {
    if (res.status === 401) markUnauthenticated()
    const err = await errorMessage(res)
    throw new ApiError(err.message, res.status, err.code, err.detail)
  }
  if (res.status === 204) return undefined as T
  return res.json() as Promise<T>
}
```

Replace `web/src/lib/ws.ts` with this complete version:

```ts
import { refreshAuthStatus } from './auth'

// Subscribe to server change events over the dashboard WebSocket. Filesystem
// events are chatty and arrive steadily (~every few hundred ms) while a run
// streams, so a plain debounce is wrong twice over: too short and it does not
// coalesce; long enough to coalesce and it withholds every update until the
// run goes quiet (the view looks frozen again). Instead this throttles: the
// first frame fires immediately (real-time feel), then further frames fire at
// most once per `minIntervalMs`. A continuous stream becomes a steady ~1.6
// reloads/sec instead of one per frame; an isolated burst collapses to one.
// Teardown clears any pending trailing call and marks the subscription closed,
// so a late frame never fires after the view unmounts.
export function subscribeChanges(cb: () => void, minIntervalMs = 600): () => void {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const ws = new WebSocket(`${proto}://${location.host}/api/ws`)
  let last = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  let closed = false
  let opened = false
  const fire = () => {
    if (closed) return
    last = performance.now()
    cb()
  }
  ws.onopen = () => {
    opened = true
  }
  // A socket that closes without ever opening was refused at the upgrade, and
  // in server mode the usual reason is an absent or expired session. Re-read
  // the auth status rather than retrying blind against a gate that will keep
  // refusing.
  ws.onclose = () => {
    if (!opened && !closed) void refreshAuthStatus()
  }
  ws.onmessage = () => {
    if (closed) return
    const elapsed = performance.now() - last
    if (elapsed >= minIntervalMs) {
      clearTimeout(timer)
      timer = undefined
      fire()
    } else if (timer === undefined) {
      timer = setTimeout(() => {
        timer = undefined
        fire()
      }, minIntervalMs - elapsed)
    }
  }
  return () => {
    closed = true
    clearTimeout(timer)
    ws.close()
  }
}
```

- [ ] **Step 5: update the existing fetch-shape assertions**

Every request now carries the marker header, so `api.test.ts`'s exact-argument assertions need the header added. Add these two constants directly under the `jsonResponse` helper (after line 31):

```ts
const H = { 'x-requested-with': 'apb-dashboard' }
const JSON_H = { 'content-type': 'application/json', 'x-requested-with': 'apb-dashboard' }
```

Then apply exactly these replacements:

```ts
// line 38
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', { headers: H })
// line 45
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?version=1.0.0', { headers: H })
// line 53
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?version=1.0.0%2Bbuild', { headers: H })
// line 60
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo?workspace=ws-abc&version=1.0.0', { headers: H })
// lines 69-73 (the object literal keeps method and body, headers becomes JSON_H)
expect(fetchMock).toHaveBeenCalledWith('/api/runs/run-1/answer', {
  method: 'POST',
  headers: JSON_H,
  body: JSON.stringify({ node: 'ask', answer: 'left' }),
})
// lines 109-113
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks', {
  method: 'POST',
  headers: JSON_H,
  body: JSON.stringify({ id: 'demo', yaml: 'id: demo\n' }),
})
// lines 129-133
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', {
  method: 'PUT',
  headers: JSON_H,
  body: JSON.stringify({ yaml: 'id: demo\nnodes: []\n' }),
})
// line 148
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo', { method: 'DELETE', headers: H })
// lines 157-161
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo/layout?version=1.0.0', {
  method: 'PUT',
  headers: JSON_H,
  body: JSON.stringify({ layout }),
})
// line 175
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft', { headers: H })
// lines 180-184
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft', {
  method: 'PUT',
  headers: JSON_H,
  body: JSON.stringify({ instruction: 'hi' }),
})
// line 190
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft?workspace=ws-abc', { headers: H })
// lines 194-198
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/p/input-draft?workspace=ws-abc', {
  method: 'PUT',
  headers: JSON_H,
  body: JSON.stringify({ instruction: 'hi' }),
})
// lines 215-217
expect(fetchMock).toHaveBeenCalledWith('/api/playbooks/demo/diff?from=1.0.0&to=1.0.1', {
  headers: H,
})
// lines 231-241
expect(fetchMock).toHaveBeenCalledWith('/api/connectors/mock-tracker/call', {
  method: 'POST',
  headers: JSON_H,
  body: JSON.stringify({
    function: 'list_items',
    account: 'acct1',
    args: { q: 'hi' },
    dry_run: true,
    full: false,
  }),
})
```

The assertions at lines 86-89, 139 and 262-266 use `expect.any(Object)` or `expect.objectContaining`, so they need no change.

- [ ] **Step 6: run the tests and watch them pass**

```sh
cd web && bun run test && bun run check
```

Expected: `auth.test.ts` fully green, `api.test.ts` green including the new 401 case, `svelte-check` clean.

- [ ] **Step 7: commit**

```sh
git add web/src/lib/auth.ts web/src/lib/auth.test.ts web/src/lib/api/http.ts web/src/lib/ws.ts web/src/lib/api.test.ts
git commit --signoff -m "$(cat <<'EOF'
feat(web): auth store and an auth-aware fetch layer

Adds web/src/lib/auth.ts holding the auth_required/authenticated snapshot,
the login and logout calls, and the marker header every request now carries.
The shared fetch layer flips the store on any 401, and a WebSocket refused at
the upgrade re-reads the auth status instead of retrying blind.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: login screen and app gating

**Files:**
- Create: `web/src/pages/Login.svelte`
- Create: `web/src/pages/Login.test.ts`
- Modify: `web/src/App.svelte` (script block at lines 1-72, template `{#if}` chain starting at line 79)
- Modify: `web/src/lib/components/Topbar.svelte` (script at lines 1-25, actions area at lines 58-60)

**Interfaces:**
- Consumes: `auth`, `login`, `logout`, `refreshAuthStatus` from `$lib/auth` (Task 7); shadcn-svelte `Button`, `Input`, `Card` from `$lib/components/ui/*` (already present in the repo).
- Produces: `Login.svelte` (no props) and the gating branch in `App.svelte`. No other page changes: the logout control lives inside `Topbar.svelte`, which every page already renders, so no call site gains a prop.

- [ ] **Step 1: write the failing test**

Create `web/src/pages/Login.test.ts`:

```ts
import { describe, it, expect } from 'vitest'
import { render } from 'svelte/server'
import Login from './Login.svelte'

describe('Login', () => {
  it('SSR-renders the key field and the sign-in action', () => {
    const { body } = render(Login, { props: {} })
    expect(body).toContain('Authorization key')
    expect(body).toContain('Sign in')
    expect(body).toContain('type="password"')
  })

  it('explains where the key comes from', () => {
    const { body } = render(Login, { props: {} })
    expect(body).toContain('apb server key issue')
  })
})
```

- [ ] **Step 2: run it and watch it fail**

```sh
cd web && bun run test Login
```

Expected: `Failed to resolve import "./Login.svelte"`.

- [ ] **Step 3: implement the login page**

Create `web/src/pages/Login.svelte`:

```svelte
<script lang="ts">
  // The whole app when the server says a credential is required and this
  // browser does not have one. One field, one button: the operator pastes the
  // key from `apb server key issue`, the server swaps it for an HttpOnly
  // cookie, and the key is never stored on this side.
  import { login } from '$lib/auth'
  import { Button } from '$lib/components/ui/button'
  import { Input } from '$lib/components/ui/input'
  import * as Card from '$lib/components/ui/card'
  import { Spinner } from '$lib/components/ui/spinner'
  import ShieldCheck from '@lucide/svelte/icons/shield-check'

  let key = $state('')
  let busy = $state(false)
  let error = $state('')

  async function submit(event: SubmitEvent) {
    event.preventDefault()
    if (busy || key.trim() === '') return
    busy = true
    error = ''
    const result = await login(key.trim())
    busy = false
    if (result.ok) {
      key = ''
      return
    }
    error = result.message ?? 'Sign in failed.'
  }
</script>

<main class="flex min-h-screen items-center justify-center bg-background p-4">
  <Card.Root class="w-full max-w-sm">
    <Card.Header>
      <div class="flex items-center gap-2">
        <ShieldCheck class="size-5 text-muted-foreground" />
        <Card.Title>Sign in to apb</Card.Title>
      </div>
      <Card.Description>
        This dashboard requires an authorization key. Create one on the server with
        <code class="rounded bg-muted px-1 py-0.5 text-xs">apb server key issue</code>.
      </Card.Description>
    </Card.Header>
    <form onsubmit={submit}>
      <Card.Content class="space-y-3">
        <label class="block text-sm font-medium" for="apb-key">Authorization key</label>
        <Input
          id="apb-key"
          type="password"
          autocomplete="off"
          spellcheck={false}
          placeholder="apb_..."
          bind:value={key}
          disabled={busy}
        />
        {#if error}
          <p class="text-sm text-destructive" role="alert">{error}</p>
        {/if}
      </Card.Content>
      <Card.Footer>
        <Button type="submit" class="w-full" disabled={busy || key.trim() === ''}>
          {#if busy}<Spinner class="size-4" />{/if}
          Sign in
        </Button>
      </Card.Footer>
    </form>
  </Card.Root>
</main>
```

If `$lib/components/ui/spinner` does not export `Spinner` under that name, check `web/src/lib/components/ui/spinner/index.ts` and use the exported name; the directory exists in the repo.

- [ ] **Step 4: gate the router**

In `web/src/App.svelte`, add these imports to the existing import block (after the `ChunkPending` import):

```ts
  import Login from './pages/Login.svelte'
  import { auth, refreshAuthStatus } from '$lib/auth'
```

Add this state and these effects right after the `hash` effect (after line 19):

```ts
  // Server mode: the app renders normally until the server says a credential
  // is required and this browser lacks one. Starting permissive keeps the
  // local, keyless dashboard from flashing a login screen on every load; a
  // 401 from any request flips the same store immediately.
  let authState = $state({ required: false, authenticated: true, checked: false })
  $effect(() => auth.subscribe((v) => (authState = v)))
  $effect(() => {
    void refreshAuthStatus()
  })
```

Change the first line of the template `{#if}` chain from `{#if route.page === 'new'}` to a new leading branch, leaving every existing branch untouched:

```svelte
{#if authState.required && !authState.authenticated}
  <Login />
{:else if route.page === 'new'}
```

- [ ] **Step 5: add the logout control**

In `web/src/lib/components/Topbar.svelte`, add to the script block (after the `Plug` icon import):

```ts
  import LogOut from '@lucide/svelte/icons/log-out'
  import { Button } from '$lib/components/ui/button'
  import { auth, logout } from '$lib/auth'

  // The control appears only in server mode: on a keyless local dashboard
  // there is no session to end and the item would be dead weight.
  let authState = $state({ required: false, authenticated: true, checked: false })
  $effect(() => auth.subscribe((v) => (authState = v)))
```

Replace the actions area at the end of the header with this complete version:

```svelte
  <div class="ml-auto flex items-center gap-2">
    {#if actions}{@render actions()}{/if}
    {#if authState.required && authState.authenticated}
      <Button variant="ghost" size="sm" onclick={() => logout()}>
        <LogOut class="size-4" />
        <span class="hidden sm:inline">Log out</span>
      </Button>
    {/if}
  </div>
```

- [ ] **Step 6: run the tests and watch them pass**

```sh
cd web && bun run test && bun run check && bun run build
```

Expected: both `Login.test.ts` cases pass, the existing `ProfileList.test.ts` SSR smoke test still passes (the Topbar change must not break server rendering), `svelte-check` is clean, and the production bundle builds.

- [ ] **Step 7: commit**

```sh
git add web/src/pages/Login.svelte web/src/pages/Login.test.ts web/src/App.svelte web/src/lib/components/Topbar.svelte
git commit --signoff -m "$(cat <<'EOF'
feat(web): login screen and app gating for server mode

App.svelte renders a single key-paste screen instead of the router when the
server reports auth_required and this browser has no session, and the topbar
grows a log-out control that appears only when auth is on.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: deployment docs, README and SECURITY updates

**Files:**
- Create: `docs/DEPLOYMENT.md`
- Modify: `README.md` (quick start at line 40, command table around line 118, `More:` link line, `## Security` section around lines 190-200)
- Modify: `SECURITY.md` (`## Safe use` section, lines 39-43)

**Interfaces:**
- Consumes: the CLI surface from Task 3 (`apb server key issue|list|revoke`, `apb dashboard --bind`), the config keys from Task 2 (`server.bind`, `server.public_base_url`, `server.trusted_proxies`), and the log line from Task 4 (`apb auth_failed ip=<ip> path=<path>`).
- Produces: no code. Every command, flag and config key named in the docs must already exist after Tasks 1 to 8.

- [ ] **Step 1: write the deployment runbook**

Create `docs/DEPLOYMENT.md`:

````markdown
# Deploying the apb dashboard on a server

By default `apb dashboard` binds `127.0.0.1` with no authentication, which is
safe only because nothing off the machine can reach it. The API can create
playbooks, start runs, and make authenticated connector calls, so an
unprotected dashboard on a public interface is equivalent to handing out remote
code execution. This document describes the one supported way to run it on a
server: an authenticated dashboard behind a reverse proxy that terminates TLS.

## The supported topology

```text
browser -> https://apb.example.com -> reverse proxy (TLS) -> 127.0.0.1:7321 -> apb dashboard
```

The proxy owns TLS, HSTS, and certificate renewal. apb keeps serving plain HTTP
on the loopback interface behind it and never terminates TLS itself.

## 1. Issue an authorization key AS THE SERVICE USER

The key file lives in the config directory of whoever runs the command:
`APB_CONFIG_DIR`, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. The
dashboard reads the config directory of whoever runs the dashboard. If you
issue a key from your own shell and the service runs as `apb`, the service
never sees that key and keeps running unauthenticated behind your proxy, and
the non-loopback interlock does not catch it because the bind is loopback.

Issue keys as the same user the service runs as:

```sh
sudo useradd --system --create-home --shell /usr/sbin/nologin apb   # once
sudo -u apb -H apb server key issue
```

`-H` matters: it sets `HOME` to the service user's home so the key lands in
`/home/apb/.config/apb/server-auth.yaml`. If the unit sets `XDG_CONFIG_HOME` or
`APB_CONFIG_DIR`, pass the same value here, and confirm with:

```sh
sudo -u apb -H apb server key list
```

The key itself is printed once, in the form `apb_` followed by 43 characters.
Only its SHA-256 is stored, with mode 0600, so a lost key cannot be recovered:
issue a new one and revoke the old one.

Authentication turns on the moment the first key exists and turns off again
when the last one is revoked. A running dashboard notices within a minute, and
immediately on the next request that fails to authenticate, so no restart is
needed for either.

```sh
sudo -u apb -H apb server key list             # ids and creation times, never the keys
sudo -u apb -H apb server key revoke <id>      # remove one
```

At most two keys exist at a time. That is the rotation window: issue the
second, move every client over, then revoke the first.

## 2. Bind

Keep the default `127.0.0.1` when the reverse proxy runs on the same host. That
is the recommended layout, and it means the dashboard is unreachable except
through the proxy.

Use `--bind 0.0.0.0` only when the proxy lives on another machine inside a
private network. Binding any non-loopback address with zero keys configured is
a startup error, not a warning.

```sh
apb dashboard --no-open                 # loopback, the default
apb dashboard --no-open --bind 0.0.0.0  # requires at least one key
```

The bind can also live in `<config_dir>/config.yaml`, where the flag overrides
it:

```yaml
port: 7321
server:
  bind: "127.0.0.1"
  public_base_url: "https://apb.example.com"
  trusted_proxies: ["127.0.0.1"]
```

`public_base_url` is the address the dashboard is reached at; when it is https,
the session cookie is issued with the `Secure` attribute. `trusted_proxies`
lists the exact peer addresses whose `X-Forwarded-For` and `X-Forwarded-Proto`
headers are believed. Those headers are used only for rate-limit keying,
logging, and the cookie `Secure` decision, never for an authentication
decision. Exact addresses only, no CIDR ranges.

Set `trusted_proxies` whenever `public_base_url` is set, and set it before
putting the dashboard behind the proxy. Without it every request arrives with
the proxy's own address, so all clients share a single rate-limit key and one
attacker can exhaust the failure budget for everyone. Startup prints a warning
naming this exact combination.

Only the RIGHTMOST `X-Forwarded-For` entry is believed. A proxy appends its own
view of the peer to whatever header the client sent, so the last entry is the
only one the proxy wrote itself; leftmost entries are client-supplied and
spoofable.

## 3. Reverse proxy

Caddy, which obtains and renews the certificate on its own:

```caddy
apb.example.com {
	reverse_proxy 127.0.0.1:7321
}
```

Caddy's `reverse_proxy` APPENDS the peer address to any `X-Forwarded-For`
header the client already sent, rather than replacing it. A caller can
therefore put anything it likes at the front of that list. apb reads the
RIGHTMOST entry precisely because that one is Caddy's own observation, so the
two-line config above is safe as written and needs no header scrubbing. Do not
"simplify" it by rewriting the header from the client's value.

nginx, with the certificate managed separately. Note `X-Forwarded-For
$remote_addr`, which sets a single-entry header from the socket peer and
discards whatever the client sent, so it is safe under either reading:

```nginx
server {
    listen 443 ssl http2;
    server_name apb.example.com;

    ssl_certificate     /etc/letsencrypt/live/apb.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/apb.example.com/privkey.pem;
    add_header Strict-Transport-Security "max-age=31536000" always;

    location / {
        proxy_pass http://127.0.0.1:7321;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;

        # The dashboard's live updates ride a WebSocket at /api/ws.
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 3600s;
    }
}

server {
    listen 80;
    server_name apb.example.com;
    return 301 https://$host$request_uri;
}
```

TLS and HSTS belong to the proxy. apb does not serve https, does not manage
certificates, and does not emit HSTS headers.

## 4. Run it as a service

`/etc/systemd/system/apb-dashboard.service`:

```ini
[Unit]
Description=apb dashboard
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=apb
Group=apb
ExecStart=/usr/local/bin/apb dashboard --no-open
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now apb-dashboard
journalctl -u apb-dashboard -f
```

Run apb as its own unprivileged user. That user owns the playbooks, the runs,
and every connector credential the runs use, so give it nothing else. It must
also be the user whose config directory holds the keys from step 1.

## 5. Verify that authentication is actually on

Do this before announcing the address to anyone. A key issued as the wrong user
leaves the dashboard unauthenticated behind the proxy, and nothing else in the
setup catches that.

```sh
curl -i https://apb.example.com/api/projects
```

Expected: `HTTP/2 401` with the body `{"error":"auth"}`. Any `200` here means
the running dashboard has no keys, so the key file belongs to a different user
than the service; redo step 1 with `sudo -u <service-user> -H`.

Then check the two other halves:

```sh
curl -i -H "Authorization: Bearer apb_..." https://apb.example.com/api/projects
```

Expected: `HTTP/2 200`. Finally open `https://apb.example.com` in a browser and
confirm the sign-in screen appears, that the key signs you in, and that the log
out control shows up in the top bar.

## 6. Signing in

Two credentials are accepted, and both work through any transparent proxy.

A browser: open the dashboard, paste the key once on the sign-in screen. The
server answers with an HttpOnly, SameSite=Lax session cookie valid for seven
days of activity; the key itself is never stored in the browser. Restarting the
server drops every session and returns to the sign-in screen.

A script or CI job:

```sh
curl -H "Authorization: Bearer apb_..." https://apb.example.com/api/runs
```

State-changing requests authenticated by the session cookie must also carry
`X-Requested-With: apb-dashboard`; the dashboard does this on its own. Bearer
requests do not need it.

## 7. Watching for brute force

Every failed authentication writes one line to stderr, which systemd puts in
the journal:

```text
apb auth_failed ip=203.0.113.9 path=/api/auth/login
```

More than 10 failures per minute from one address already earn HTTP 429 for the
rest of that minute. To ban repeat offenders at the firewall, add a fail2ban
filter at `/etc/fail2ban/filter.d/apb.conf`:

```ini
[Definition]
failregex = ^apb auth_failed ip=<HOST> path=\S+$
ignoreregex =
```

and a jail at `/etc/fail2ban/jail.d/apb.conf`:

```ini
[apb-dashboard]
enabled = true
backend = systemd
journalmatch = _SYSTEMD_UNIT=apb-dashboard.service
filter = apb
maxretry = 10
findtime = 600
bantime = 3600
```

## Notes and limits

- `POST /api/hooks/{run_id}/{secret}` stays reachable from the internet by
  design, without a key: it is how an external service signals a `wait:
  webhook` node, and it authenticates itself with the per-run path secret in
  its own URL. If nothing in your playbooks receives external webhooks, you can
  restrict it at the proxy, for example with a Caddy matcher that answers 404
  for `/api/hooks/*` or an nginx `location /api/hooks/ { deny all; }`. Do not
  restrict it if any run waits on a webhook.
- Every response carries `X-Frame-Options: DENY`, so the dashboard cannot be
  framed. That does not depend on your proxy configuration, and a proxy should
  not strip it.
- `apb dev` is a source-tree development command, not a deployment path. It
  serves the Vite dev server next to the API on the loopback interface. If keys
  exist, the developer signs in once through the Vite proxy like any other
  browser client.
- Sessions live in memory only. A restart signs everyone out.
- There are no user accounts, roles, or per-key scopes. A key is full access to
  the API, which is why there are at most two of them and why they belong only
  to operators.
- The MCP server (`apb mcp`) speaks stdio and never traverses HTTP, so nothing
  here applies to it.
````

- [ ] **Step 2: update the README**

Leave the quick start block at line 40 exactly as it is: the loopback dashboard
is still the right first command, and server mode is an opt-in on top of it.

Add one line to the command table, immediately after the line
`apb mcp             stdio MCP server for coding agents`:

```text
apb server key      issue / list / revoke the keys that protect a networked dashboard
```

Extend the `More:` link line to name the new document:

```markdown
More: [docs/INSTALL.md](docs/INSTALL.md), [docs/MCP.md](docs/MCP.md),
[docs/PROFILES.md](docs/PROFILES.md), [docs/CONNECTORS.md](docs/CONNECTORS.md),
[docs/HOST-INTEGRATION.md](docs/HOST-INTEGRATION.md),
[docs/DEPLOYMENT.md](docs/DEPLOYMENT.md).
```

Replace the whole `## Security` section with:

```markdown
## Security

> [!WARNING]
> Playbooks can execute local scripts and invoke coding agents. Treat third-party
> playbooks and imported bundles as executable code, and review them before running.

By default the dashboard binds `127.0.0.1` with no authentication, which is safe
only because nothing off the machine can reach it. To run it on a server, issue
an authorization key and put it behind a reverse proxy that terminates TLS:

```sh
apb server key issue     # printed once, stored only as a hash
apb dashboard --no-open  # keep the loopback bind, let the proxy reach it
```

With at least one key present, every `/api` route requires either
`Authorization: Bearer apb_...` or a session cookie obtained by signing in
through the dashboard. Binding a non-loopback address with no key configured is
refused at startup. The full runbook, including Caddy and nginx examples, a
systemd unit, and a fail2ban filter, is in
[docs/DEPLOYMENT.md](docs/DEPLOYMENT.md).

Do not expose `apb mcp` to untrusted users; it speaks stdio and has no
authentication of its own.

Please report suspected vulnerabilities privately as described in
[SECURITY.md](SECURITY.md).
```

- [ ] **Step 3: update SECURITY.md**

Replace the `## Safe use` section with:

```markdown
## Safe use

Treat third-party playbooks and imported bundles as executable code. Review them
before running.

The web dashboard binds `127.0.0.1` and runs unauthenticated by default. Before
exposing it to a network, issue an authorization key with `apb server key issue`
and place it behind a reverse proxy that terminates TLS; with a key present,
every `/api` route requires a bearer key or a session cookie, and binding a
non-loopback address without one is refused at startup. See
[docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for the supported topology.

The MCP interface speaks stdio and carries no authentication of its own. Do not
expose it to untrusted users or networks.
```

- [ ] **Step 4: check the conventions**

```sh
grep -n '—\|!' docs/DEPLOYMENT.md
```

Expected: no em-dashes; the only `!` matches are inside the GitHub alert marker `> [!WARNING]` in README.md, which is markup rather than prose (verify the grep over `docs/DEPLOYMENT.md` returns nothing at all).

Then confirm every command the docs name actually exists:

```sh
cargo run -p apb --bin apb -- server key list --help
cargo run -p apb --bin apb -- dashboard --help
```

Expected: the `key list` help shows `--json`; the dashboard help shows `--bind`, `--port`, `--no-open`.

- [ ] **Step 5: commit**

```sh
git add docs/DEPLOYMENT.md README.md SECURITY.md
git commit --signoff -m "$(cat <<'EOF'
docs: deployment runbook for the authenticated dashboard

Adds docs/DEPLOYMENT.md covering key issuance, bind guidance, Caddy and nginx
reverse proxy configs, a systemd unit, sign-in for browsers and scripts, and a
fail2ban filter matching the auth_failed log line. README and SECURITY.md now
describe the authenticated mode instead of a blanket do-not-expose warning.

Co-Authored-By: Claude <model> <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: final gates

**Files:**
- Modify: none expected. Any fix this task turns up is made in the file that owns it.

**Interfaces:**
- Consumes: everything from Tasks 1 to 9.
- Produces: a clean workspace under every gate the repository defines.

- [ ] **Step 1: Rust gates**

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo clippy --release
```

Expected: no diff from fmt, no clippy warnings in either profile, every test green.

- [ ] **Step 2: frontend gates**

```sh
cd web && bun run test && bun run check && bun run build
```

Expected: all vitest suites pass, `svelte-check` reports 0 errors and 0 warnings, and `web/dist` builds.

- [ ] **Step 3: code-ranker**

```sh
cargo metadata --format-version 1 >/dev/null
code-ranker check .
```

Expected: exit 0. For any violation, read `code-ranker docs base <ID>` first, fix it in the owning file, and re-run until clean. The two shapes most likely to be flagged here are `crates/apb-server/src/auth.rs` size (split the session store and the rate limiter into `auth/session.rs` and `auth/limit.rs` submodules if so, keeping every public name re-exported from `auth`) and the dependency direction (apb-core must not gain any dependency on apb-server; it does not in this plan).

- [ ] **Step 4: manual smoke on the loopback path**

```sh
cargo build
./target/debug/apb server key list
./target/debug/apb dashboard --no-open
```

Expected: with no keys, `key list` says so, the dashboard starts on `http://127.0.0.1:7321` with no authentication banner, and the UI loads without a login screen. Stop it, then:

```sh
./target/debug/apb server key issue
./target/debug/apb dashboard --no-open
```

Expected: the startup output names the authentication state, the browser shows the sign-in screen, the issued key signs in, and the log-out control appears in the topbar. Finally confirm the interlock:

```sh
./target/debug/apb server key revoke <id>
./target/debug/apb dashboard --no-open --bind 0.0.0.0
```

Expected: exit code 2 with the refusal naming `apb server key issue`.

- [ ] **Step 5: report**

Summarize for the owner: the tasks completed, the gate output, and the fact that nothing has been pushed. Do not push, tag, or open a PR.
