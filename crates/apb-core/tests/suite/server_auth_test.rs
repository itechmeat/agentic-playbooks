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
    assert!(
        plain.starts_with(KEY_PREFIX),
        "key carries the prefix: {plain}"
    );
    assert_eq!(
        plain.len(),
        KEY_PREFIX.len() + 43,
        "32 CSPRNG bytes in unpadded base64url are 43 chars: {plain}"
    );
    assert_eq!(record.sha256.len(), 64, "the stored hash is bare hex");
    assert_eq!(record.id, record.sha256[..8], "the id is the hash prefix");
    assert!(
        record.created_at.ends_with('Z'),
        "created_at is UTC ISO-8601"
    );

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
    assert!(
        !raw.contains(&plain),
        "the key must not be stored in plain text"
    );
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
    assert!(
        err.contains("revoke"),
        "the refusal must name the remedy: {err}"
    );
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

    let err = server_auth::revoke_in(&path, "deadbeef")
        .unwrap_err()
        .to_string();
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
    assert!(
        server_auth::load_from(&path).is_err(),
        "wrong shape must fail"
    );

    std::fs::write(
        &path,
        "keys:\n  - id: abc\n    sha256: zz\n    created_at: x\n",
    )
    .unwrap();
    let err = server_auth::load_from(&path).unwrap_err().to_string();
    assert!(err.contains("sha256"), "a bad hash field is named: {err}");

    std::fs::write(
        &path,
        "keys:\n  - id: abc\n    sha256: aa\n    created_at: x\n    extra: 1\n",
    )
    .unwrap();
    assert!(
        server_auth::load_from(&path).is_err(),
        "unknown fields are rejected"
    );
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
