//! What lives beside a playbook's versions: the `current` pointer as read,
//! each version's provenance sidecar (`meta/<version>.yaml`) with its trial
//! record, and the end of a candidate's trial. Depends on nothing else of
//! the version store, so both the store and the move of `current` build on
//! it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::VersioningError;
use super::imported::ImportProvenance;
use crate::fsutil::atomic_write;
use crate::registry::is_safe_segment;

/// Who made a version, stored beside it in `meta/<version>.yaml`. Which
/// version is in use is not part of it: the `current` pointer is the one
/// authority for that (see [`VersionInfo::is_current`]). Sidecars written by
/// older builds carry a `promoted` flag that drifted from `current` (a
/// rollback or a save that did not move `current` left it set); it is
/// ignored on read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct VersionProvenance {
    pub created_by: String,
    pub run_id: Option<String>,
    pub classification: Option<String>,
    // --- forward patches and candidate trials (issue #192) ---
    /// `next_runs` for a forward patch (a candidate for the next runs);
    /// absent for a patch the run migrated onto (`current_run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub scope: Option<String>,
    /// The version the patch was made from: the lineage of a candidate that
    /// replaced an earlier one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub base_version: Option<String>,
    /// Why the supervisor made the forward patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub rationale: Option<String>,
    /// What the rationale rests on: journal seqs, node ids, durations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub evidence: Vec<String>,
    /// How the candidate fared in its trials; absent until a trial run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub trial: Option<TrialRecord>,
    // --- end forward patches ---
    /// Where an imported version came from (`created_by: import`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub imported: Option<ImportProvenance>,
}

/// The trial record of a candidate version (issue #192): how many trial runs
/// it passed, and its fate once decided. A historical fact like the rest of
/// the sidecar: whether the version is in use is still `current` alone.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct TrialRecord {
    /// Trial runs that succeeded with every goal criterion holding.
    #[serde(default)]
    pub successes: u32,
    /// `promoted`, `rejected` or `superseded`; absent while it is still on
    /// trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub outcome: Option<String>,
    /// The trial run that decided the outcome (for `superseded`: the run
    /// whose forward patch replaced it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub run_id: Option<String>,
    /// Why it was rejected, or the version that superseded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
    /// The latest quality decision under `promote_supervisor_patches:
    /// { when: quality }`; absent under every other policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub quality: Option<crate::quality::QualityRecord>,
}

/// The version `current` names in `playbook_dir`, when it names one that
/// is a safe path segment (a tampered pointer such as `../x` reads as none).
pub fn read_current_pointer(playbook_dir: &Path) -> Option<String> {
    fs::read_to_string(playbook_dir.join("current"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && is_safe_segment(s))
}

/// The trust digest of the committed version directory `version_dir`.
pub(crate) fn written_digest(version_dir: &Path) -> io::Result<String> {
    let yaml = fs::read_to_string(version_dir.join("playbook.yaml"))?;
    crate::scope::definition_digest(&yaml, version_dir).map_err(io::Error::other)
}

/// The provenance of `version` of the playbook in `playbook_dir`, `None`
/// when it has none or it does not read.
pub fn read_provenance_in(playbook_dir: &Path, version: &str) -> Option<VersionProvenance> {
    if !is_safe_segment(version) {
        return None;
    }
    let yaml =
        fs::read_to_string(playbook_dir.join("meta").join(format!("{version}.yaml"))).ok()?;
    yaml_serde::from_str(&yaml).ok()
}

/// Ends the trial of the candidate of the playbook in `playbook_dir`,
/// unless it is `keep`: removes the pointer and records the `superseded`
/// outcome with `reason` on its trial record. Returns the version it ended.
/// The caller holds the candidate lock (`_held`).
pub fn supersede_candidate_locked(
    playbook_dir: &Path,
    keep: Option<&str>,
    reason: &str,
    _held: &crate::fsutil::DirLock,
) -> Result<Option<String>, VersioningError> {
    let Some(candidate) = crate::candidate_pointer::read_candidate(playbook_dir) else {
        return Ok(None);
    };
    if keep == Some(candidate.as_str()) {
        return Ok(None);
    }
    // Read the record before the pointer goes: a sidecar that does not
    // read refuses with the candidate still on trial.
    let sidecar = sidecar_path(playbook_dir, &candidate)
        .is_file()
        .then(|| load_sidecar(playbook_dir, &candidate))
        .transpose()?;
    crate::candidate_pointer::clear_candidate_if(playbook_dir, &candidate)?;
    if let Some(mut provenance) = sidecar {
        let mut trial = provenance.trial.take().unwrap_or_default();
        trial.outcome = Some(crate::candidate_pointer::OUTCOME_SUPERSEDED.to_string());
        trial.reason = Some(reason.to_string());
        provenance.trial = Some(trial);
        write_sidecar(playbook_dir, &candidate, &provenance)?;
    }
    Ok(Some(candidate))
}

/// Whether the trial record of `version` can be updated: its sidecar is
/// absent or reads. A move checks this before its first write.
pub(crate) fn check_trial_record(
    playbook_dir: &Path,
    version: &str,
) -> Result<(), VersioningError> {
    if sidecar_path(playbook_dir, version).is_file() {
        load_sidecar(playbook_dir, version)?;
    }
    Ok(())
}

/// The sidecar of `version`, which must exist and read.
fn load_sidecar(playbook_dir: &Path, version: &str) -> Result<VersionProvenance, VersioningError> {
    let path = sidecar_path(playbook_dir, version);
    yaml_serde::from_str(&fs::read_to_string(&path)?)
        .map_err(|e| VersioningError::Schema(format!("meta/{version}.yaml does not read: {e}")))
}

fn write_sidecar(
    playbook_dir: &Path,
    version: &str,
    provenance: &VersionProvenance,
) -> Result<(), VersioningError> {
    let yaml =
        yaml_serde::to_string(provenance).map_err(|e| VersioningError::Schema(e.to_string()))?;
    atomic_write(&sidecar_path(playbook_dir, version), yaml.as_bytes())?;
    Ok(())
}

/// The provenance sidecar of `version` of the playbook in `playbook_dir`.
fn sidecar_path(playbook_dir: &Path, version: &str) -> PathBuf {
    playbook_dir.join("meta").join(format!("{version}.yaml"))
}

/// Applies `f` to the trial record of `version` of the playbook in
/// `playbook_dir` (created when absent) and writes the sidecar back. The
/// caller holds the candidate lock. A sidecar that does not read is an
/// error, never silently replaced.
pub fn update_trial_in(
    playbook_dir: &Path,
    version: &str,
    f: impl FnOnce(&mut TrialRecord),
) -> Result<TrialRecord, VersioningError> {
    if !is_safe_segment(version) || !playbook_dir.join(version).is_dir() {
        return Err(VersioningError::NotFound(version.to_string()));
    }
    let mut provenance = if sidecar_path(playbook_dir, version).is_file() {
        load_sidecar(playbook_dir, version)?
    } else {
        VersionProvenance::default()
    };
    let mut trial = provenance.trial.take().unwrap_or_default();
    f(&mut trial);
    provenance.trial = Some(trial.clone());
    write_sidecar(playbook_dir, version, &provenance)?;
    Ok(trial)
}
