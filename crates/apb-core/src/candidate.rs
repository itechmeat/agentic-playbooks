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

pub use crate::candidate_pointer::{CANDIDATE_FILE, clear_candidate_if, read_candidate};
const LOCK_FILE: &str = ".candidate.lock";

/// `scope` of a forward patch in its provenance and in the MCP tool.
pub const SCOPE_NEXT_RUNS: &str = "next_runs";
/// `scope` of an in-run patch (the default).
pub const SCOPE_CURRENT_RUN: &str = "current_run";

/// Trial outcomes recorded in [`TrialRecord::outcome`].
pub const OUTCOME_PROMOTED: &str = "promoted";
pub const OUTCOME_REJECTED: &str = "rejected";
pub const OUTCOME_SUPERSEDED: &str = "superseded";

/// `supervisor.policy.trial_candidates`: whether a run started without an
/// explicit version runs the candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrialPolicy {
    /// Never: the candidate runs only when asked for by version.
    Off,
    /// Every start without an explicit version runs it (the default).
    NextRun,
    /// That share of starts (0..=1) runs it, by a random draw per start.
    Share(f64),
}

/// Reads `supervisor.policy.trial_candidates`: `off`, `next_run` (the
/// default, also for anything unreadable) or `{ share: <0..1> }`.
pub fn trial_policy(playbook: &Playbook) -> TrialPolicy {
    let Some(value) = playbook
        .supervisor
        .as_ref()
        .and_then(|s| s.policy.as_ref())
        .and_then(|p| p.get("trial_candidates"))
    else {
        return TrialPolicy::NextRun;
    };
    if let Some(s) = value.as_str() {
        return match s {
            "off" => TrialPolicy::Off,
            _ => TrialPolicy::NextRun,
        };
    }
    match value.get("share").and_then(serde_yaml_ng::Value::as_f64) {
        Some(p) if p.is_finite() => TrialPolicy::Share(p.clamp(0.0, 1.0)),
        _ => TrialPolicy::NextRun,
    }
}

impl TrialPolicy {
    /// Whether a start with the uniform draw `draw` (in `[0, 1)`) runs the
    /// candidate.
    pub fn admits(self, draw: f64) -> bool {
        match self {
            TrialPolicy::Off => false,
            TrialPolicy::NextRun => true,
            TrialPolicy::Share(p) => draw < p,
        }
    }
}

/// A uniform draw in `[0, 1)` for a [`TrialPolicy::Share`] start, from a
/// random v4 UUID (no extra dependency, no shared state).
pub fn random_draw() -> f64 {
    let bits = uuid::Uuid::new_v4().as_u128() >> 75; // 53 random bits
    bits as f64 / (1u64 << 53) as f64
}

/// The candidate a start without an explicit version runs: the pointer's
/// version when it differs from `current`, still holds a definition, and the
/// current version's `trial_candidates` policy admits the start for `draw`.
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

fn playbook_dir(root: &Path, id: &str) -> PathBuf {
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
/// in the new version's `base_version`.
pub fn create_forward_patch(
    root: &Path,
    id: &str,
    base_version: &str,
    yaml: &str,
    patch: &ForwardPatch,
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
        },
    )?;
    let dir = playbook_dir(root, id);
    let _lock = lock(&dir)?;
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

    fn with_policy(policy: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nsupervisor:\n  policy:\n    trial_candidates: {policy}\nnodes: []\nedges: []\n"
        ))
        .unwrap()
    }

    #[test]
    fn the_policy_reads_off_next_run_and_a_share() {
        assert_eq!(trial_policy(&with_policy("off")), TrialPolicy::Off);
        assert_eq!(trial_policy(&with_policy("next_run")), TrialPolicy::NextRun);
        assert_eq!(
            trial_policy(&with_policy("{ share: 0.25 }")),
            TrialPolicy::Share(0.25)
        );
        assert_eq!(
            trial_policy(&with_policy("{ share: 7 }")),
            TrialPolicy::Share(1.0)
        );
    }

    #[test]
    fn a_share_admits_draws_below_it() {
        assert!(TrialPolicy::Share(0.3).admits(0.29));
        assert!(!TrialPolicy::Share(0.3).admits(0.3));
        assert!(!TrialPolicy::Off.admits(0.0));
        let d = random_draw();
        assert!((0.0..1.0).contains(&d));
    }
}
