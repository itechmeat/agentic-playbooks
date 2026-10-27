//! Approval of a version by its exact trust digest.
//!
//! A version an import wrote with scripts carries code nobody on this
//! machine wrote. It runs only after an approval that names the digest of
//! that exact content: the approver says what they looked at, and content
//! that changed afterwards (a script edited after the import or after the
//! approval) digests differently and needs a new approval. Acknowledging an
//! untrusted run does not count as this approval. A version saved on top of
//! such a version before its approval keeps the import record, so the rule
//! follows the scripts through every save and patch.
//!
//! The rule records consent; it does not stop an agent. Over MCP the
//! refusal names the digest, so an agent can approve by echoing it back:
//! the tool asks it to show the scripts to the person first, and the
//! approval is recorded as the agent's. On the CLI and the dashboard a
//! person names the digest.

use std::path::Path;

use super::{ScriptEntry, read_provenance_in};
use crate::content::{TreeLimits, read_tree_files};
use crate::registry::{Registry, RegistryError};
use crate::trust::{OriginKind, TrustStore};

/// Why a version could not be approved.
#[derive(Debug, thiserror::Error)]
pub enum ApproveError {
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The version was imported with scripts: the approval must name the
    /// digest (`digest`) of the content it covers.
    #[error("this version was imported with scripts: approve it by naming its digest {digest}")]
    DigestRequired {
        digest: String,
        scripts: Vec<ScriptEntry>,
    },
    /// The named digest is not the version's current content.
    #[error("digest mismatch: the version's content digests to {actual}, not {expected}")]
    DigestMismatch { expected: String, actual: String },
    #[error("could not record the approval: {0}")]
    Trust(#[from] std::io::Error),
}

/// What [`approve_version`] approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved {
    pub version: String,
    pub digest: String,
}

/// The scripts of `<playbook_dir>/<version>` as they are now, when that
/// version holds scripts an import brought (directly, or carried by a save
/// on top of an unapproved import) and still waits for an approval of
/// `live_digest` (its trust digest now). `None` when the version has no
/// such scripts, or when `live_digest` is approved.
pub fn pending_import(
    playbook_dir: &Path,
    version: &str,
    live_digest: &str,
    trust: &TrustStore,
) -> Option<Vec<ScriptEntry>> {
    let recorded = read_provenance_in(playbook_dir, version)?.imported?.scripts;
    (!recorded.is_empty() && !trust.is_approved(live_digest))
        .then(|| scripts_to_review(&playbook_dir.join(version), recorded))
}

/// What an approval of `version_dir` is shown: its scripts as they are now,
/// or the fingerprints the import recorded when the tree cannot be read
/// (never an empty list for a version known to hold scripts).
fn scripts_to_review(version_dir: &Path, recorded: Vec<ScriptEntry>) -> Vec<ScriptEntry> {
    live_scripts(version_dir).unwrap_or(recorded)
}

/// The scripts under `version_dir/scripts` by fingerprint, as they are now:
/// what an approval covers. `None` when the tree cannot be read.
fn live_scripts(version_dir: &Path) -> Option<Vec<ScriptEntry>> {
    let dir = version_dir.join("scripts");
    if !dir.is_dir() {
        return Some(Vec::new());
    }
    let (files, _) = read_tree_files(&dir, &TreeLimits::default()).ok()?;
    Some(files.iter().map(ScriptEntry::of).collect())
}

/// Whether `<playbook_dir>/<version>` was imported with scripts and its
/// current content (`live_digest`) is not approved.
pub fn import_pending(
    playbook_dir: &Path,
    version: &str,
    live_digest: &str,
    trust: &TrustStore,
) -> bool {
    pending_import(playbook_dir, version, live_digest, trust).is_some()
}

/// Approves `version` (the current one when `None`) of playbook `id` under
/// the definition parent `def_parent` (`<project>/.apb` or the global
/// store's parent) as `origin`.
///
/// A version imported with scripts needs `expected_digest`
/// ([`ApproveError::DigestRequired`] otherwise, listing the digest and the
/// scripts). A named digest must equal the version's trust digest now
/// ([`ApproveError::DigestMismatch`] otherwise), for any version.
pub fn approve_version(
    def_parent: &Path,
    id: &str,
    version: Option<&str>,
    expected_digest: Option<&str>,
    origin: OriginKind,
) -> Result<Approved, ApproveError> {
    let reg = Registry::open_dir(def_parent)?;
    let loaded = reg.load(id, version)?;
    let digest = loaded.trust_digest()?;
    let playbook_dir = def_parent.join("playbooks").join(id);
    match expected_digest {
        Some(expected) if expected != digest => {
            return Err(ApproveError::DigestMismatch {
                expected: expected.to_string(),
                actual: digest,
            });
        }
        Some(_) => {}
        None => {
            let recorded = read_provenance_in(&playbook_dir, &loaded.version)
                .and_then(|p| p.imported)
                .map(|i| i.scripts)
                .unwrap_or_default();
            if !recorded.is_empty() {
                let scripts = scripts_to_review(&playbook_dir.join(&loaded.version), recorded);
                return Err(ApproveError::DigestRequired { digest, scripts });
            }
        }
    }
    TrustStore::load().approve(&digest, id, origin)?;
    Ok(Approved {
        version: loaded.version,
        digest,
    })
}
