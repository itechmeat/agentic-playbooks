//! The run's baseline check, as the journal records it.
//!
//! The playbook's `baseline_check` names facts read twice: once before the
//! first node that can act (`apb_core::effects::node_acts`) and once at the
//! finish node, each journaled as one `baseline_observed` per check; the
//! finish then journals one `baseline_compared` per observation. The before
//! snapshot lives only in the journal, so a pause, a resume or a crash
//! reuses it and never takes it again; an acting node that started with no
//! before snapshot makes it `missed`, and every comparison of that check
//! unknown. An after snapshot stays valid only while no acting node starts
//! after it: a resume that re-runs acting nodes takes a new one.
//!
//! [`BaselineState::fold`] is the one reading of that record, for the drive
//! (`scheduler/baseline.rs`), the run surfaces, the needs-a-look count and
//! `apb stats`: which comparisons are current is decided there and nowhere
//! else. `read` reads a source and `project` runs the project's own checks.

mod ctx;
pub(crate) mod project;
pub(crate) mod read;

use std::collections::BTreeMap;

use apb_core::baseline::Observation;
use apb_core::project_checks::ProjectCommand;
use apb_core::schema::Playbook;

use crate::event::{Event, EventPayload};

/// `baseline_observed.phase` values.
pub mod phase {
    pub const BEFORE: &str = "before";
    pub const AFTER: &str = "after";
}

/// `baseline_observed.status` values.
pub mod status {
    /// The source was read; each observation carries a value or its own
    /// unknown reason.
    pub const OBSERVED: &str = "observed";
    /// The source could not be read at all (`reason` says why).
    pub const UNKNOWN: &str = "unknown";
    /// An acting node started before any before snapshot was taken.
    pub const MISSED: &str = "missed";
}

/// One journaled snapshot of one check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub status: String,
    pub reason: Option<String>,
    pub observations: BTreeMap<String, Observation>,
    pub input_digest: String,
    pub inputs: Vec<String>,
    pub commands: Vec<ProjectCommand>,
    pub workdir: Option<String>,
    pub inherited_from: Option<String>,
}

impl Snapshot {
    /// The check, phase and snapshot a `baseline_observed` event carries.
    pub fn from_event(payload: &EventPayload) -> Option<(&str, &str, Snapshot)> {
        let EventPayload::BaselineObserved {
            check,
            phase,
            status,
            reason,
            observations,
            input_digest,
            inputs,
            commands,
            workdir,
            inherited_from,
        } = payload
        else {
            return None;
        };
        let snap = Snapshot {
            status: status.clone(),
            reason: reason.clone(),
            observations: observations.clone(),
            input_digest: input_digest.clone(),
            inputs: inputs.clone(),
            commands: commands.clone(),
            workdir: workdir.clone(),
            inherited_from: inherited_from.clone(),
        };
        Some((check, phase, snap))
    }

    /// The stand-in for a before snapshot that was never journaled.
    pub(crate) fn not_taken() -> Self {
        Snapshot {
            status: status::UNKNOWN.into(),
            reason: Some("no before snapshot was taken".into()),
            ..Snapshot::default()
        }
    }

    /// The before snapshot of a check whose before was missed.
    pub(crate) fn missed() -> Self {
        Snapshot {
            status: status::MISSED.into(),
            reason: Some("a node that can act started before the before snapshot was taken".into()),
            ..Snapshot::default()
        }
    }

    /// An observation of this snapshot: its value, or unknown with the
    /// reason the whole snapshot or the one observation is unknown.
    pub fn observation(&self, id: &str) -> Observation {
        if let Some(o) = self.observations.get(id) {
            return o.clone();
        }
        Observation::unknown(match (self.status.as_str(), &self.reason) {
            (_, Some(r)) => r.clone(),
            (status::MISSED, None) => "the before snapshot was missed".to_string(),
            _ => format!("`{id}` was not observed"),
        })
    }

    /// The journal event for this snapshot.
    pub(crate) fn event(&self, check: &str, phase: &str) -> EventPayload {
        EventPayload::BaselineObserved {
            check: check.to_string(),
            phase: phase.to_string(),
            status: self.status.clone(),
            reason: self.reason.clone(),
            observations: self.observations.clone(),
            input_digest: self.input_digest.clone(),
            inputs: self.inputs.clone(),
            commands: self.commands.clone(),
            workdir: self.workdir.clone(),
            inherited_from: self.inherited_from.clone(),
        }
    }
}

/// One journaled comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonRecord {
    pub observation: String,
    pub rule: String,
    pub before: Option<Observation>,
    pub after: Option<Observation>,
    pub verdict: String,
    pub regressed: Option<bool>,
    pub detail: Option<String>,
    pub enforced: bool,
}

/// What the journal says about one check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckState {
    /// The before snapshot, journaled once.
    pub before: Option<Snapshot>,
    /// The latest after snapshot no acting node has started since.
    pub after: Option<Snapshot>,
    /// The comparisons journaled for that after snapshot.
    pub comparisons: Vec<ComparisonRecord>,
}

/// The baseline record of one run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BaselineState {
    pub checks: BTreeMap<String, CheckState>,
    /// A node that can act has started in this run.
    pub acting_started: bool,
}

impl BaselineState {
    /// Folds `events` against `playbook` (the run's snapshot of it): a node
    /// the playbook does not know any more counts as acting.
    pub fn fold(playbook: &Playbook, events: &[Event]) -> Self {
        Self::fold_with(Some(playbook), events)
    }

    /// [`BaselineState::fold`] for a run whose playbook snapshot may not
    /// read: without it every node counts as acting, so a comparison is
    /// current only when no node started after it.
    pub fn fold_with(playbook: Option<&Playbook>, events: &[Event]) -> Self {
        let acts = |node: &str| {
            playbook
                .and_then(|p| p.node(node))
                .is_none_or(apb_core::effects::node_acts)
        };
        let mut s = BaselineState::default();
        for e in events {
            match &e.payload {
                EventPayload::NodeStarted { node, .. } if acts(node) => {
                    s.acting_started = true;
                    for c in s.checks.values_mut() {
                        c.after = None;
                        c.comparisons.clear();
                    }
                }
                p @ EventPayload::BaselineObserved { .. } => {
                    let Some((check, phase, snap)) = Snapshot::from_event(p) else {
                        continue;
                    };
                    let c = s.checks.entry(check.to_string()).or_default();
                    match phase {
                        phase::BEFORE if c.before.is_none() => c.before = Some(snap),
                        phase::AFTER => {
                            c.after = Some(snap);
                            c.comparisons.clear();
                        }
                        _ => {}
                    }
                }
                EventPayload::BaselineCompared {
                    check,
                    observation,
                    rule,
                    before,
                    after,
                    verdict,
                    regressed,
                    detail,
                    enforced,
                } => {
                    let c = s.checks.entry(check.clone()).or_default();
                    if c.after.is_some() {
                        c.comparisons.push(ComparisonRecord {
                            observation: observation.clone(),
                            rule: rule.clone(),
                            before: before.clone(),
                            after: after.clone(),
                            verdict: verdict.clone(),
                            regressed: *regressed,
                            detail: detail.clone(),
                            enforced: *enforced,
                        });
                    }
                }
                _ => {}
            }
        }
        s
    }

    /// The state of check `id`.
    pub fn check(&self, id: &str) -> Option<&CheckState> {
        self.checks.get(id)
    }

    /// Every current comparison: `(check, comparison)`, the latest one of
    /// each observation.
    pub fn current(&self) -> impl Iterator<Item = (&str, &ComparisonRecord)> {
        self.checks.iter().flat_map(|(id, c)| {
            let mut latest: BTreeMap<&str, &ComparisonRecord> = BTreeMap::new();
            for r in &c.comparisons {
                latest.insert(r.observation.as_str(), r);
            }
            latest.into_values().map(move |r| (id.as_str(), r))
        })
    }

    /// The current comparisons whose after value is worse than the before.
    pub fn regressions(&self) -> usize {
        self.current()
            .filter(|(_, r)| r.regressed == Some(true))
            .count()
    }
}
