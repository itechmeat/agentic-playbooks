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
    #[error(
        "at most 2 server keys may exist at once; revoke one first with `apb server key revoke <id>`"
    )]
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
