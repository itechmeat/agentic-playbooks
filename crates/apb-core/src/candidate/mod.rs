//! Candidate versions (issue #192): a forward patch a supervisor made for the
//! next runs waits in the `candidate` pointer next to `current` until trial
//! runs prove it. One candidate at a time: a newer forward patch replaces it
//! (its provenance keeps the lineage through `base_version`), a successful
//! trial promotes it per `promote_supervisor_patches`, a failed one rejects
//! it. The pointer is written atomically like `current`, and every
//! read-modify-write of it runs under the playbook's candidate lock.

use std::io;
use std::path::{Path, PathBuf};

use crate::fsutil::{DirLock, atomic_write, lock_dir};
use crate::schema::Playbook;
use crate::versioning::{
    TrialRecord, VersionProvenance, VersioningError, create_patch_version_with, read_provenance,
    write_provenance,
};

pub mod policy;

pub use policy::{PromotePolicy, TrialPolicy, promote_policy, trial_policy};

use crate::candidate_pointer::CANDIDATE_LOCK_FILE as LOCK_FILE;
pub use crate::candidate_pointer::{
    CANDIDATE_FILE, OUTCOME_SUPERSEDED, clear_candidate_if, read_candidate,
};

/// `scope` of a forward patch in its provenance and in the MCP tool.
pub const SCOPE_NEXT_RUNS: &str = "next_runs";
/// `scope` of an in-run patch (the default).
pub const SCOPE_CURRENT_RUN: &str = "current_run";

/// Trial outcomes recorded in [`TrialRecord::outcome`].
pub const OUTCOME_PROMOTED: &str = "promoted";
pub const OUTCOME_REJECTED: &str = "rejected";

/// A uniform draw in `[0, 1)` for a [`TrialPolicy::Share`] start, from a
/// random v4 UUID (no extra dependency, no shared state).
pub fn random_draw() -> f64 {
    let bits = uuid::Uuid::new_v4().as_u128() >> 75; // 53 random bits
    bits as f64 / (1u64 << 53) as f64
}

/// How many forward-patch generations [`lineage_reaches`] follows.
const MAX_LINEAGE: usize = 64;

/// Whether the forward patch `version` descends from `current_version`: its
/// provenance's `base_version` is `current_version`, or is itself a forward
/// patch (`scope: next_runs`) that descends from it. A candidate whose
/// lineage no longer reaches `current` was made from a line a person (or an
/// in-run promotion) has since moved past; running or promoting it would
/// undo that newer version.
pub fn lineage_reaches(playbook_dir: &Path, version: &str, current_version: &str) -> bool {
    let mut at = version.to_string();
    for _ in 0..MAX_LINEAGE {
        let Some(p) = crate::versioning::read_provenance_in(playbook_dir, &at) else {
            return false;
        };
        if at != version && p.scope.as_deref() != Some(SCOPE_NEXT_RUNS) {
            return false;
        }
        match p.base_version {
            Some(b) if b == current_version => return true,
            Some(b) => at = b,
            None => return false,
        }
    }
    false
}

/// The candidate a start without an explicit version runs: the pointer's
/// version when it differs from `current`, still holds a definition,
/// descends from `current` ([`lineage_reaches`]), and the current version's
/// `trial_candidates` policy admits the start for `draw`.
/// The current version's policy decides: it is the person-approved line, and
/// a forward patch cannot change the `supervisor` block anyway.
pub fn choose_for_start(
    playbook_dir: &Path,
    current: &Playbook,
    current_version: &str,
    draw: f64,
) -> Option<String> {
    let candidate = read_candidate(playbook_dir)?;
    if candidate == current_version
        || !playbook_dir
            .join(&candidate)
            .join("playbook.yaml")
            .is_file()
        || !lineage_reaches(playbook_dir, &candidate, current_version)
    {
        return None;
    }
    trial_policy(current).admits(draw).then_some(candidate)
}

/// The lock every read-modify-write of the pointer and a candidate's trial
/// record takes.
pub fn lock(playbook_dir: &Path) -> io::Result<DirLock> {
    lock_dir(playbook_dir, LOCK_FILE)
}

/// The directory of playbook `id` under the project `root`.
pub fn playbook_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".apb/playbooks").join(id)
}

/// What a supervisor's forward patch carries besides its YAML.
#[derive(Debug, Clone, Default)]
pub struct ForwardPatch {
    pub run_id: String,
    pub classification: String,
    pub rationale: Option<String>,
    pub evidence: Vec<String>,
}

/// What [`create_forward_patch`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedCandidate {
    pub version: String,
    /// The candidate it replaced, now marked `superseded`.
    pub replaced: Option<String>,
}

/// Creates the forward patch as a new patch version of `base_version` (the
/// same validation and immutability as an in-run patch, provenance with
/// `scope: next_runs`) and makes it the candidate. A candidate it replaces
/// gets the `superseded` outcome naming the new version; the lineage stays
/// in the new version's `base_version`. The caller holds the playbook's
/// candidate [`lock`] (`_held`), so its own checks (the base is still the
/// line on trial, the run's patch budget) and this write are one step.
pub fn create_forward_patch(
    root: &Path,
    id: &str,
    base_version: &str,
    yaml: &str,
    patch: &ForwardPatch,
    _held: &DirLock,
) -> Result<CreatedCandidate, VersioningError> {
    let version = create_patch_version_with(
        root,
        id,
        base_version,
        yaml,
        VersionProvenance {
            created_by: "supervisor".to_string(),
            run_id: Some(patch.run_id.clone()),
            classification: Some(patch.classification.clone()),
            scope: Some(SCOPE_NEXT_RUNS.to_string()),
            base_version: Some(base_version.to_string()),
            rationale: patch.rationale.clone(),
            evidence: patch.evidence.clone(),
            trial: None,
            imported: None,
        },
    )?;
    let dir = playbook_dir(root, id);
    let replaced = read_candidate(&dir).filter(|c| *c != version);
    atomic_write(&dir.join(CANDIDATE_FILE), version.as_bytes())?;
    if let Some(old) = &replaced {
        update_trial(root, id, old, |t| {
            t.outcome = Some(OUTCOME_SUPERSEDED.to_string());
            t.run_id = Some(patch.run_id.clone());
            t.reason = Some(format!("superseded by {version}"));
        })?;
    }
    Ok(CreatedCandidate { version, replaced })
}

/// Applies `f` to the trial record of `version` (created when absent) and
/// writes the sidecar back. Callers hold [`lock`].
pub fn update_trial(
    root: &Path,
    id: &str,
    version: &str,
    f: impl FnOnce(&mut TrialRecord),
) -> Result<TrialRecord, VersioningError> {
    let mut provenance = read_provenance(root, id, version)?.unwrap_or_default();
    let mut trial = provenance.trial.take().unwrap_or_default();
    f(&mut trial);
    provenance.trial = Some(trial.clone());
    write_provenance(root, id, version, &provenance)?;
    Ok(trial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draw_is_in_the_unit_interval() {
        let d = random_draw();
        assert!((0.0..1.0).contains(&d));
    }
}
