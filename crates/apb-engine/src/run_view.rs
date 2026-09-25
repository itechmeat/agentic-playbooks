//! What a run looks like to anyone reading it: the one model behind every
//! read-only run surface (`apb runs`, `apb wait`, MCP `run_status`,
//! `run_report`, `run_wait` and `runs_list`, the dashboard list and detail).
//!
//! Each of those used to read the journal and derive the status on its own,
//! and they drifted: one used the strict reader and failed on a line the
//! driver was still writing, one folded a child's journal with no liveness at
//! all, one ignored a dead driver. Every one of them now goes through
//! [`RunView::load`], so a run reads the same everywhere.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::EngineError;
use crate::event::{Event, EventPayload};
use crate::progress::ProgressSummary;
use crate::state::{RunState, RunStatus};

/// The journal as a read-only surface sees it: tolerant of a torn last line,
/// which is a normal transient state while the driver appends (see
/// [`crate::event::read_all_lossy_tail`]). The drive itself, and anything
/// that writes to the journal after reading it, stays on the strict
/// [`crate::event::read_all`].
pub fn read_events(run_dir: &Path) -> Result<Vec<Event>, EngineError> {
    crate::event::read_all_lossy_tail(run_dir)
}

/// One observation of a run, with the process-table overlay applied once.
#[derive(Debug)]
pub struct RunView {
    pub events: Vec<Event>,
    /// The pure fold of `events`: outputs, failure reason, reviews.
    pub state: RunState,
    pub progress: Option<ProgressSummary>,
    /// `None` when nothing claims to drive the run, otherwise whether that
    /// claim holds (see [`crate::liveness::driver_alive`]).
    pub driver_alive: Option<bool>,
    /// The status to report: the fold, corrected by liveness (see
    /// [`crate::liveness::reported_run_status`]).
    pub run_status: RunStatus,
}

/// A sub-playbook run started by a run, as its parent reports it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChildRun {
    pub node_id: String,
    pub run_id: String,
    /// The child's reported status, or `unknown` when its run directory is
    /// gone or unreadable.
    pub status: String,
}

impl RunView {
    /// Reads run `run_id` at `run_dir` once.
    pub fn load(run_dir: &Path, run_id: &str) -> Result<Self, EngineError> {
        let events = read_events(run_dir)?;
        let progress = crate::progress::from_run_dir(run_dir, &events);
        let driver_alive = crate::liveness::driver_alive(run_dir, run_id);
        let waiting = progress.as_ref().is_some_and(|p| p.waiting_on.is_some());
        let run_status = crate::liveness::reported_run_status(&events, waiting, driver_alive);
        Ok(Self {
            state: RunState::fold(&events),
            events,
            progress,
            driver_alive,
            run_status,
        })
    }

    /// Per-node status for live reporting (`lost` for a dead attempt,
    /// `running` for a live one the fold calls interrupted).
    pub fn nodes(&self) -> BTreeMap<String, String> {
        crate::liveness::reported_node_statuses(&self.events)
    }

    /// The failure reason, only for a run that actually ended `failed`.
    pub fn failure_reason(&self) -> Option<String> {
        (self.run_status == RunStatus::Failed)
            .then(|| {
                self.state
                    .failure_reason
                    .as_ref()
                    .map(crate::state::FailureReason::display)
            })
            .flatten()
    }

    /// The sub-playbook runs this run started, each reported through its own
    /// [`RunView`]: a child driven by a live parent reads `running`, not the
    /// bare fold's `interrupted`.
    pub fn children(&self, run_dir: &Path) -> Vec<ChildRun> {
        self.events
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::ChildRunStarted { node_id, run_id } => {
                    let status = run_dir
                        .parent()
                        .map(|runs| runs.join(run_id))
                        .filter(|d| d.is_dir())
                        .and_then(|d| RunView::load(&d, run_id).ok())
                        .map(|child| child.run_status.as_str().to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    Some(ChildRun {
                        node_id: node_id.clone(),
                        run_id: run_id.clone(),
                        status,
                    })
                }
                _ => None,
            })
            .collect()
    }
}
