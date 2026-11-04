//! Passkeys registered on this machine (WebAuthn credentials).
//!
//! One file holds every passkey a person registered with apb:
//! `<config_dir>/passkeys.yaml`, written 0600 through the shared
//! atomic-write helper, with a sibling `passkeys.lock` serializing
//! read-modify-write. Each record keeps the WebAuthn credential id, a free
//! text label, the public COSE key, the signature algorithm and the WebAuthn
//! signature counter. Nothing here is secret: a passkey's private key never
//! leaves the authenticator, so the file holds public material only, but it
//! is still written private because credential ids are usable for tracking.
//!
//! Decision paths call [`count`] or [`try_load`]. An unparsable file counts
//! as no passkeys, which means those decisions fall back to today's
//! behavior; a person notices through `apb passkey list`, which reports the
//! parse error instead of hiding it.

use std::path::{Path, PathBuf};

use crate::fsutil::atomic_write_private;
use serde::{Deserialize, Serialize};

/// File name under the global config dir.
pub const PASSKEY_FILE: &str = "passkeys.yaml";

/// Lock file serializing read-modify-write over the passkey file, so two
/// concurrent `apb passkey` invocations cannot lose an entry.
const PASSKEY_LOCK: &str = "passkeys.lock";

/// ES256 (ECDSA over P-256 with SHA-256), the COSE algorithm `-7`.
pub const ALG_ES256: i64 = -7;

/// EdDSA (Ed25519), the COSE algorithm `-8`.
pub const ALG_EDDSA: i64 = -8;

/// The only signature algorithms this file accepts.
pub const SUPPORTED_ALGS: [i64; 2] = [ALG_ES256, ALG_EDDSA];

/// True when `alg` is one of the supported COSE algorithms.
pub fn alg_supported(alg: i64) -> bool {
    SUPPORTED_ALGS.contains(&alg)
}

/// One registered passkey, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasskeyRecord {
    /// The WebAuthn credential id, base64url without padding.
    pub id: String,
    /// Free text, one line, non-empty, at most 100 chars.
    pub label: String,
    /// The raw COSE key bytes, standard base64. Public, not secret.
    pub public_key_cose: String,
    /// The COSE algorithm identifier: -7 (ES256) or -8 (EdDSA).
    pub alg: i64,
    /// The WebAuthn signature counter of the credential.
    pub sign_count: u32,
    /// UTC ISO-8601, from the single wall-clock source.
    pub created_at: String,
}

/// The whole passkey file. An absent file parses as an empty set, which is
/// what "no passkeys registered" means.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PasskeyFile {
    pub passkeys: Vec<PasskeyRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum PasskeyError {
    #[error("no config directory: set HOME or APB_CONFIG_DIR")]
    NoConfigDir,
    #[error("invalid passkey file `{0}`: {1}")]
    Invalid(String, String),
    #[error("passkey file `{0}`: {1}")]
    Io(String, String),
    #[error(
        "a passkey labeled `{0}` already exists; remove it first with `apb passkey remove {0}`"
    )]
    DuplicateLabel(String),
    #[error("this passkey credential is already registered")]
    DuplicateCredential,
    #[error("no passkey with label `{0}`; see `apb passkey list`")]
    UnknownLabel(String),
}

/// `<config_dir>/passkeys.yaml`.
pub fn passkey_file_path() -> Result<PathBuf, PasskeyError> {
    crate::config::config_dir()
        .map(|d| d.join(PASSKEY_FILE))
        .ok_or(PasskeyError::NoConfigDir)
}

/// Reads a passkey file. A missing file is an empty set; a malformed one is
/// an error, so a typo can never silently hide a registered passkey from a
/// caller that asks for a strict read.
pub fn load_from(path: &Path) -> Result<PasskeyFile, PasskeyError> {
    if !path.is_file() {
        return Ok(PasskeyFile::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| PasskeyError::Io(path.display().to_string(), e.to_string()))?;
    parse_passkeys(&raw, path)
}

/// Parses passkey-file text that a caller already read. `path` only names the
/// file in error messages. Exposed so a caller that reads the text itself can
/// parse the bytes it already holds instead of reading the file a second
/// time.
pub fn parse_passkeys(raw: &str, path: &Path) -> Result<PasskeyFile, PasskeyError> {
    let parsed: PasskeyFile = yaml_serde::from_str(raw)
        .map_err(|e| PasskeyError::Invalid(path.display().to_string(), e.to_string()))?;
    for k in &parsed.passkeys {
        if !alg_supported(k.alg) {
            return Err(PasskeyError::Invalid(
                path.display().to_string(),
                format!(
                    "passkey `{}` uses COSE alg {}, only -7 and -8 are supported",
                    k.label, k.alg
                ),
            ));
        }
    }
    Ok(parsed)
}

fn save_to(path: &Path, file: &PasskeyFile) -> Result<(), PasskeyError> {
    let yaml = yaml_serde::to_string(file)
        .map_err(|e| PasskeyError::Invalid(path.display().to_string(), e.to_string()))?;
    atomic_write_private(path, yaml.as_bytes())
        .map_err(|e| PasskeyError::Io(path.display().to_string(), e.to_string()))
}

fn lock_for(path: &Path) -> Result<crate::fsutil::DirLock, PasskeyError> {
    let dir = path.parent().ok_or_else(|| {
        PasskeyError::Invalid(
            path.display().to_string(),
            "path has no parent directory".to_string(),
        )
    })?;
    crate::fsutil::lock_dir(dir, PASSKEY_LOCK)
        .map_err(|e| PasskeyError::Io(dir.display().to_string(), e.to_string()))
}

/// Registers a passkey into `path` under the lock. Refuses a duplicate label
/// or a duplicate credential id, a label that is empty, longer than 100
/// chars or carries a newline, and an algorithm other than -7 or -8.
pub fn add_into(
    path: &Path,
    label: &str,
    id: &str,
    public_key_cose: &str,
    alg: i64,
    sign_count: u32,
) -> Result<PasskeyRecord, PasskeyError> {
    let _lock = lock_for(path)?;
    let mut file = load_from(path)?;
    if !alg_supported(alg) {
        return Err(PasskeyError::Invalid(
            path.display().to_string(),
            format!("COSE alg {alg} is not supported, only -7 and -8 are"),
        ));
    }
    if file.passkeys.iter().any(|k| k.id == id) {
        return Err(PasskeyError::DuplicateCredential);
    }
    if file.passkeys.iter().any(|k| k.label == label) {
        return Err(PasskeyError::DuplicateLabel(label.to_string()));
    }
    validate_label(path, label)?;
    let record = PasskeyRecord {
        id: id.to_string(),
        label: label.to_string(),
        public_key_cose: public_key_cose.to_string(),
        alg,
        sign_count,
        created_at: crate::dismiss::iso_utc(crate::clock::now_ms_u64()),
    };
    file.passkeys.push(record.clone());
    save_to(path, &file)?;
    Ok(record)
}

/// Validates a label: non-empty, one line, at most 100 chars.
fn validate_label(path: &Path, label: &str) -> Result<(), PasskeyError> {
    if label.is_empty() {
        return Err(PasskeyError::Invalid(
            path.display().to_string(),
            "the label must not be empty".to_string(),
        ));
    }
    if label.len() > 100 {
        return Err(PasskeyError::Invalid(
            path.display().to_string(),
            "the label must be at most 100 characters".to_string(),
        ));
    }
    if label.contains('\n') || label.contains('\r') {
        return Err(PasskeyError::Invalid(
            path.display().to_string(),
            "the label must be a single line".to_string(),
        ));
    }
    Ok(())
}

/// Removes the passkey labeled `label` from `path` under the lock and returns
/// the removed record.
pub fn remove_by_label_into(path: &Path, label: &str) -> Result<PasskeyRecord, PasskeyError> {
    let _lock = lock_for(path)?;
    let mut file = load_from(path)?;
    let Some(pos) = file.passkeys.iter().position(|k| k.label == label) else {
        return Err(PasskeyError::UnknownLabel(label.to_string()));
    };
    let removed = file.passkeys.remove(pos);
    save_to(path, &file)?;
    Ok(removed)
}

/// Lists the passkeys in `path`. A missing file is an empty set.
pub fn list_from(path: &Path) -> Result<Vec<PasskeyRecord>, PasskeyError> {
    Ok(load_from(path)?.passkeys)
}

/// `try_load` on the standard config-dir path, treating a missing config dir
/// like a missing file.
pub fn try_load() -> Result<PasskeyFile, PasskeyError> {
    match passkey_file_path() {
        Ok(path) => load_from(&path),
        Err(PasskeyError::NoConfigDir) => Ok(PasskeyFile::default()),
        Err(e) => Err(e),
    }
}

/// File name under the global config dir holding the one-time enrollment
/// code `apb passkey enroll` prints. Written 0600, single use, five
/// minutes: the dashboard's enrollment page asks for it before it may run
/// a registration ceremony for the first passkey.
pub const ENROLL_CODE_FILE: &str = "passkey-enroll-code.yaml";
/// How long a one-time enrollment code stays valid.
pub const ENROLL_CODE_TTL_MS: u128 = 5 * 60 * 1000;

/// The one-time enrollment code file, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollCodeFile {
    pub code: String,
    /// Wall-clock ms at which the code stops being accepted.
    pub expires_at_ms: u64,
}

/// Mints a fresh one-time enrollment code into `path` (the config dir),
/// replacing any code still there, and returns it. Eight characters from
/// an unambiguous alphabet, so a person can read it off a terminal.
pub fn write_enroll_code(dir: &Path) -> Result<String, PasskeyError> {
    let alphabet = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|e| PasskeyError::Io(dir.display().to_string(), e.to_string()))?;
    let code: String = bytes
        .iter()
        .map(|b| alphabet[(*b as usize) % alphabet.len()] as char)
        .collect();
    let file = EnrollCodeFile {
        code: code.clone(),
        expires_at_ms: crate::clock::now_ms_u64().saturating_add(ENROLL_CODE_TTL_MS as u64),
    };
    let path = dir.join(ENROLL_CODE_FILE);
    let _lock = crate::fsutil::lock_dir(dir, "passkeys.lock")
        .map_err(|e| PasskeyError::Io(dir.display().to_string(), e.to_string()))?;
    let yaml = yaml_serde::to_string(&file)
        .map_err(|e| PasskeyError::Invalid(path.display().to_string(), e.to_string()))?;
    crate::fsutil::atomic_write_private(&path, yaml.as_bytes())
        .map_err(|e| PasskeyError::Io(path.display().to_string(), e.to_string()))?;
    Ok(code)
}

/// Whether `code` is the live, unexpired one-time enrollment code in
/// `dir`, consuming it when it is (and clearing an expired one either
/// way). Single use: a second call with the same code reads `false`.
pub fn consume_enroll_code(dir: &Path, code: &str) -> Result<bool, PasskeyError> {
    let path = dir.join(ENROLL_CODE_FILE);
    let _lock = crate::fsutil::lock_dir(dir, "passkeys.lock")
        .map_err(|e| PasskeyError::Io(dir.display().to_string(), e.to_string()))?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(PasskeyError::Io(path.display().to_string(), e.to_string())),
    };
    let file: EnrollCodeFile = yaml_serde::from_str(&raw)
        .map_err(|e| PasskeyError::Invalid(path.display().to_string(), e.to_string()))?;
    let expired = crate::clock::now_ms() >= u128::from(file.expires_at_ms);
    let ok = !expired
        && file
            .code
            .as_bytes()
            .iter()
            .zip(code.as_bytes())
            .all(|(a, b)| a == b)
        && file.code.len() == code.len();
    // Single use: consumed or expired, the code is gone. A wrong guess
    // leaves the live code where it is.
    if ok || expired {
        let _ = std::fs::remove_file(&path);
    }
    Ok(ok)
}

/// Stores the authenticator's new sign count for credential `id` after a
/// verified assertion, under the store's lock: a cloned authenticator
/// replaying an older counter is refused the next time.
pub fn bump_sign_count(path: &Path, id: &str, new_count: u32) -> Result<(), PasskeyError> {
    let dir = path.parent().ok_or_else(|| {
        PasskeyError::Io(path.display().to_string(), "path has no parent".to_string())
    })?;
    let _lock = crate::fsutil::lock_dir(dir, PASSKEY_LOCK)
        .map_err(|e| PasskeyError::Io(dir.display().to_string(), e.to_string()))?;
    let mut file = load_from(path)?;
    let mut found = false;
    for p in &mut file.passkeys {
        if p.id == id {
            p.sign_count = new_count;
            found = true;
        }
    }
    if !found {
        return Err(PasskeyError::UnknownLabel(id.to_string()));
    }
    save_to(path, &file)
}

/// The number of passkeys registered on this machine. Called on decision
/// paths, so it never fails: a missing config dir, a missing file and an
/// unparsable file all count as 0. A person notices an unparsable file
/// through `apb passkey list`.
pub fn count() -> usize {
    try_load().map(|f| f.passkeys.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("passkeys.yaml")
    }

    const ID_A: &str = "aWQ=";
    const COSE_A: &str = "pQE=";

    #[test]
    fn load_of_absent_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        let file = load_from(&path).unwrap();
        assert_eq!(file, PasskeyFile::default());
        assert!(list_from(&path).unwrap().is_empty());
    }

    #[test]
    fn add_then_list_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        let added = add_into(&path, "yubi", ID_A, COSE_A, ALG_ES256, 3).unwrap();
        assert_eq!(added.label, "yubi");
        assert_eq!(added.alg, ALG_ES256);
        assert_eq!(added.sign_count, 3);
        assert!(!added.created_at.is_empty());
        let listed = list_from(&path).unwrap();
        assert_eq!(listed, vec![added]);
    }

    #[test]
    fn save_creates_file_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        add_into(&path, "yubi", ID_A, COSE_A, ALG_EDDSA, 0).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn duplicate_label_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        add_into(&path, "yubi", ID_A, COSE_A, ALG_ES256, 0).unwrap();
        let err = add_into(&path, "yubi", "other", COSE_A, ALG_ES256, 0).unwrap_err();
        assert!(matches!(err, PasskeyError::DuplicateLabel(_)));
    }

    #[test]
    fn duplicate_credential_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        add_into(&path, "yubi", ID_A, COSE_A, ALG_ES256, 0).unwrap();
        let err = add_into(&path, "second key", ID_A, COSE_A, ALG_ES256, 0).unwrap_err();
        assert!(matches!(err, PasskeyError::DuplicateCredential));
    }

    #[test]
    fn alg_other_than_supported_refused_on_add_and_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        let err = add_into(&path, "yubi", ID_A, COSE_A, 1, 0).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
        let raw = "passkeys:\n- id: aWQ=\n  label: yubi\n  public_key_cose: pQE=\n  alg: 1\n  sign_count: 0\n  created_at: \"2026-01-01T00:00:00Z\"\n";
        let err = parse_passkeys(raw, &path).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
    }

    #[test]
    fn label_rules_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        let err = add_into(&path, "", ID_A, COSE_A, ALG_ES256, 0).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
        let err = add_into(&path, "two\nlines", ID_A, COSE_A, ALG_ES256, 0).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
        let long = "x".repeat(101);
        let err = add_into(&path, &long, ID_A, COSE_A, ALG_ES256, 0).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
        assert!(!path.exists());
    }

    #[test]
    fn remove_and_unknown_label() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        add_into(&path, "yubi", ID_A, COSE_A, ALG_ES256, 0).unwrap();
        let removed = remove_by_label_into(&path, "yubi").unwrap();
        assert_eq!(removed.id, ID_A);
        assert!(list_from(&path).unwrap().is_empty());
        let err = remove_by_label_into(&path, "yubi").unwrap_err();
        assert!(matches!(err, PasskeyError::UnknownLabel(_)));
    }

    #[test]
    fn invalid_yaml_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        std::fs::write(&path, "passkeys: [broken").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
    }

    #[test]
    fn unknown_field_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = tmp_path(&dir);
        std::fs::write(&path, "extra: 1\n").unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, PasskeyError::Invalid(_, _)));
    }

    #[test]
    fn count_and_try_load_over_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("apb");
        let prev = std::env::var_os("APB_CONFIG_DIR");
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", &cfg);
        }
        // A missing config dir and a missing file both read as empty.
        assert!(try_load().unwrap().passkeys.is_empty());
        assert_eq!(count(), 0);
        // One record parses and counts.
        add_into(
            &passkey_file_path().unwrap(),
            "yubi",
            ID_A,
            COSE_A,
            ALG_ES256,
            0,
        )
        .unwrap();
        assert_eq!(count(), 1);
        assert_eq!(try_load().unwrap().passkeys.len(), 1);
        // An unparsable file counts as zero; try_load reports the problem.
        std::fs::write(passkey_file_path().unwrap(), "passkeys: [broken").unwrap();
        assert_eq!(count(), 0);
        assert!(try_load().is_err());
        match prev {
            Some(v) => unsafe { std::env::set_var("APB_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("APB_CONFIG_DIR") },
        }
    }
}

#[cfg(test)]
mod enroll_code_tests {
    use super::*;

    #[test]
    fn an_enroll_code_is_single_use_and_wrong_codes_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let code = write_enroll_code(dir.path()).unwrap();
        assert_eq!(code.len(), 8);
        assert!(!consume_enroll_code(dir.path(), "wrongcod").unwrap());
        assert!(consume_enroll_code(dir.path(), &code).unwrap());
        assert!(
            !consume_enroll_code(dir.path(), &code).unwrap(),
            "single use"
        );
    }

    #[test]
    fn a_missing_code_file_reads_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!consume_enroll_code(dir.path(), "whatever").unwrap());
    }
}
