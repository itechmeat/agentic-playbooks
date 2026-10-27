//! The playbook line a candidate is on, and a trial run that ends now:
//! what both settlement paths (by successes, by quality) and a person's
//! decision act on.

use std::path::{Path, PathBuf};

use apb_core::candidate::{
    OUTCOME_PROMOTED, OUTCOME_REJECTED, clear_candidate_if, read_candidate, update_trial,
};
use apb_core::quality::QualityRecord;
use apb_core::registry::{LoadedPlaybook, Registry};
use apb_core::versioning::{TrialRecord, promote_version};

use crate::error::EngineError;
use crate::event::{Event, EventPayload};

/// The playbook line a candidate is on: where it lives, the candidate
/// version and the `current` it descends from.
pub(crate) struct Line {
    pub root: PathBuf,
    pub id: String,
    pub playbook_dir: PathBuf,
    pub candidate: String,
    pub current: String,
}

impl Line {
    /// The line of playbook `id` under `root` while it has a candidate.
    pub(super) fn open(root: &Path, id: &str) -> Option<Self> {
        let playbook_dir = apb_core::candidate::playbook_dir(root, id);
        let candidate = read_candidate(&playbook_dir)?;
        Some(Line {
            current: current_of(&playbook_dir),
            root: root.to_path_buf(),
            id: id.to_string(),
            playbook_dir,
            candidate,
        })
    }

    /// Whether the candidate and `current` pointers still name what this
    /// line read: a decision taken on the line is carried out only then.
    pub(super) fn unchanged(&self) -> bool {
        read_candidate(&self.playbook_dir).as_deref() == Some(self.candidate.as_str())
            && current_of(&self.playbook_dir) == self.current
    }

    /// The candidate's trial record as it stands, serialized: every trial
    /// end and every decision rewrites it, so a comparison taken before it
    /// changed is taken again.
    pub(super) fn trial_state(&self) -> Option<String> {
        let trial = apb_core::versioning::read_provenance(&self.root, &self.id, &self.candidate)
            .ok()
            .flatten()?
            .trial?;
        serde_json::to_string(&trial).ok()
    }

    /// The definition of `current`, which the promotion policy and the
    /// quality criteria come from.
    pub(super) fn current_definition(&self) -> Result<LoadedPlaybook, EngineError> {
        Ok(Registry::open(&self.root)?.load(&self.id, Some(&self.current))?)
    }

    /// Drops the candidate (it stays on disk as a version) and records why.
    pub(super) fn reject(
        &self,
        run_id: Option<&str>,
        reason: &str,
        quality: Option<QualityRecord>,
    ) -> Result<(), EngineError> {
        clear_candidate_if(&self.playbook_dir, &self.candidate)?;
        update_trial(&self.root, &self.id, &self.candidate, |t| {
            t.outcome = Some(OUTCOME_REJECTED.to_string());
            t.run_id = run_id.map(str::to_string).or(t.run_id.take());
            t.reason = Some(reason.to_string());
            if quality.is_some() {
                t.quality = quality;
            }
        })?;
        Ok(())
    }

    /// Makes the candidate `current` and records it.
    pub(super) fn promote(
        &self,
        run_id: Option<&str>,
        quality: Option<QualityRecord>,
    ) -> Result<TrialRecord, EngineError> {
        promote_version(&self.root, &self.id, &self.candidate)?;
        Ok(update_trial(&self.root, &self.id, &self.candidate, |t| {
            t.outcome = Some(OUTCOME_PROMOTED.to_string());
            t.run_id = run_id.map(str::to_string).or(t.run_id.take());
            if quality.is_some() {
                t.quality = quality;
            }
        })?)
    }
}

/// The version the `current` pointer names (empty when it names none).
pub(super) fn current_of(playbook_dir: &Path) -> String {
    std::fs::read_to_string(playbook_dir.join("current"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// A trial run that ends now, with the line it judges.
pub(crate) struct Trial {
    pub line: Line,
    pub run_id: String,
    pub events: Vec<Event>,
}

impl Trial {
    /// A run that migrated onto an in-run patch did not run the candidate
    /// as it is.
    pub(super) fn migrated(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RunMigrated { .. }))
    }

    /// The description of the first goal criterion that did not hold.
    pub(super) fn goal_failed(&self) -> Option<String> {
        self.events.iter().find_map(|e| match &e.payload {
            EventPayload::GoalChecked {
                status,
                description,
                ..
            } if status == "failed" => Some(description.clone()),
            _ => None,
        })
    }
}
