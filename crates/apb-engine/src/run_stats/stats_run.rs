//! One run as `apb stats` and the run estimate read it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::decision::report::RunJournal;
use crate::event::{Event, EventPayload};

/// One run as the statistics read it.
#[derive(Debug, Clone)]
pub struct StatsRun {
    pub run_id: String,
    pub playbook: String,
    pub version: String,
    pub events: Vec<Event>,
    /// The run's playbook snapshot, for `expected_duration`.
    pub snapshot: Option<apb_core::schema::Playbook>,
    /// The run was a candidate trial (issue #192, from its manifest).
    pub candidate_trial: bool,
    /// Each node's profile primary model, from the run manifest (issue
    /// #193).
    pub expected_models: BTreeMap<String, String>,
    /// Who executed the run's agent steps: `cli` or `host` (from the
    /// manifest, `cli` without one); `None` for a mode this apb does not
    /// know (a newer apb's), so it is never pooled with a known one.
    pub execution: Option<String>,
    /// Each node's profile and bundle digests, from the manifest.
    pub node_profiles: BTreeMap<String, crate::manifest::NodeDigests>,
    /// The run's grades that count ([`crate::grade::trusted_grades_of`]),
    /// read only by [`StatsRun::with_grades`]: the readers that need no
    /// quality (estimates, retros) skip the grade files and their MACs.
    pub grades: Vec<apb_core::quality::grade::Grade>,
}

impl StatsRun {
    /// Reads one run directory; `None` when its journal does not read.
    pub fn load(run_dir: &Path) -> Option<Self> {
        let j = RunJournal::load(run_dir)?;
        let version = j
            .events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { version, .. } => Some(version.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let manifest = crate::manifest::read(run_dir).ok().flatten();
        let m = manifest.as_ref();
        Some(StatsRun {
            run_id: j.run_id,
            playbook: j.playbook,
            version,
            events: j.events,
            snapshot: crate::legacy_snapshot::load_run_playbook(run_dir),
            candidate_trial: m.is_some_and(|m| m.candidate_trial),
            expected_models: m
                .map(crate::attempt_models::primary_models_of)
                .unwrap_or_default(),
            execution: match m {
                None => Some(
                    apb_core::execution::ExecutionMode::default()
                        .as_str()
                        .to_string(),
                ),
                Some(m) => m.execution_mode().ok().map(|x| x.as_str().to_string()),
            },
            node_profiles: m
                .map(crate::manifest::RunExecutionManifest::node_digests)
                .unwrap_or_default(),
            grades: Vec::new(),
        })
    }

    /// The run with its grades that count read from `run_dir`.
    pub fn with_grades(mut self, run_dir: &Path) -> Self {
        self.grades = crate::grade::trusted_grades_of(run_dir);
        self
    }

    pub(crate) fn started_ms(&self) -> Option<u128> {
        self.events.iter().find_map(|e| match &e.payload {
            EventPayload::RunStarted { .. } => Some(e.ts),
            _ => None,
        })
    }
}
