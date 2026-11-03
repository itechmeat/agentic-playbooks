//! A billing confirmation covers one launch: the nonce a billing `ask`
//! refusal hands out is a fresh challenge, recorded in the workspace with
//! the launch it was offered for, and the first confirmation that echoes it
//! for that same launch consumes it. A second start with the same nonce
//! finds nothing to redeem and is asked again, so one confirmation never
//! authorizes repeated runs.
//!
//! The launch check only looks at a challenge ([`is_open`]); the grant it
//! passes carries a [`Claim`], and the challenge is consumed
//! ([`LaunchGrant::consume_challenge`](super::launch::LaunchGrant::consume_challenge))
//! only where the start is otherwise committed: right before the engine
//! writes the run's launch record, before a detached start hands its grant
//! over, or once for a whole `apb eval`. A check that later refuses for
//! another reason (consent, params, budget, an untrusted playbook) leaves
//! it open, so the person is not asked about billing again.
//!
//! The record holds the launch's deterministic nonce
//! ([`super::launch::nonce`]: the workspace, the version, the choice, the
//! permit, the policy state and the predicted set), so a challenge confirms
//! only the exact launch and predicted set it was offered for. It is
//! consumed by removing its file, which only one caller can do; a challenge
//! older than [`TTL_MS`] is refused and swept.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the open challenges of a workspace live.
const DIR: &str = ".apb/launch-challenges";

/// How long an offered challenge can be confirmed: a person answers a
/// question within the hour, or is asked again.
pub const TTL_MS: u64 = 60 * 60 * 1000;

#[derive(Serialize, Deserialize)]
struct Record {
    /// The deterministic nonce of the launch the challenge was offered for.
    launch: String,
    issued_ms: u64,
}

fn dir(root: &Path) -> PathBuf {
    root.join(DIR)
}

/// The file of `token`: named by its digest, so a token that is not a
/// plain name never reaches a path.
fn file(root: &Path, token: &str) -> PathBuf {
    let name = apb_core::content::sha256_hex(token.as_bytes());
    dir(root).join(&name[..32])
}

/// A fresh challenge for the launch whose nonce is `launch`, recorded under
/// `root`. When it cannot be recorded (a read-only process, a workspace
/// that went away) the token is still returned and simply never redeems;
/// without a random source nothing is recorded either, since a token anyone
/// could compute from the launch must never confirm it.
pub fn issue(root: &Path, launch: &str) -> String {
    let Ok(random) = apb_core::server_auth::random_token() else {
        return UNRECORDED.to_string();
    };
    let token = format!(
        "launch-{}",
        &apb_core::content::sha256_hex(format!("{random}\n{launch}").as_bytes())[..32]
    );
    sweep(root);
    let record = Record {
        launch: launch.to_string(),
        issued_ms: apb_core::clock::now_ms_u64(),
    };
    if let Ok(bytes) = serde_json::to_vec(&record) {
        let _ = apb_core::fsutil::atomic_write_private_under(
            &root.join(".apb"),
            &file(root, &token),
            &bytes,
        );
    }
    token
}

/// The token handed out when no challenge could be recorded: never open.
const UNRECORDED: &str = "launch-unrecorded";

/// A challenge a launch check accepted, to consume once the start is
/// committed: the workspace it is recorded in and the token. Only ever in
/// memory or in a private handoff file, never in the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub root: PathBuf,
    pub token: String,
}

/// The record of `token` when it was offered for the launch whose nonce is
/// `launch`.
fn record_for(path: &Path, launch: &str) -> Option<Record> {
    let record = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Record>(&b).ok())?;
    apb_core::secret::ct_eq_str(&record.launch, launch).then_some(record)
}

fn fresh(record: &Record) -> bool {
    apb_core::clock::now_ms_u64().saturating_sub(record.issued_ms) <= TTL_MS
}

/// Whether `token` is an open challenge for the launch whose nonce is
/// `launch`, without consuming it.
pub fn is_open(root: &Path, token: &str, launch: &str) -> bool {
    record_for(&file(root, token), launch).is_some_and(|r| fresh(&r))
}

/// Whether `token` is an open challenge for the launch whose nonce is
/// `launch`; when it is, it is consumed here and redeems nothing again.
pub fn redeem(root: &Path, token: &str, launch: &str) -> bool {
    let path = file(root, token);
    let Some(record) = record_for(&path, launch) else {
        return false;
    };
    // Only the caller whose removal succeeds holds the confirmation.
    if std::fs::remove_file(&path).is_err() {
        return false;
    }
    fresh(&record)
}

/// Removes the challenges past their time.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(dir(root)) else {
        return;
    };
    let now = apb_core::clock::now_ms_u64();
    for e in entries.flatten() {
        let stale = std::fs::read(e.path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Record>(&b).ok())
            .is_none_or(|r| now.saturating_sub(r.issued_ms) > TTL_MS);
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
