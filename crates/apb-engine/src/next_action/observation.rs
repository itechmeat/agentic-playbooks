//! One observation of a run and its live sub-playbook children: everything
//! the next actions are derived from, read once and at one instant.
//!
//! The journal, the channel files, liveness and the pause come from one
//! [`RunView`] per run (the read every status surface reports from); the
//! playbook snapshot and the send intents are read once beside it. Children
//! are followed through their `child_run_started` records while they have
//! not ended, recursively, up to [`MAX_CHILD_DEPTH`] levels.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use apb_core::schema::Playbook;

use crate::error::EngineError;
use crate::event::EventPayload;
use crate::run_outcome::intents::RunIntent;
use crate::run_view::RunView;
use crate::state::RunStatus;

/// How many levels of live children an observation follows. Deeper ones
/// are left out and the observation says so (`children_truncated`).
pub const MAX_CHILD_DEPTH: usize = 8;

/// One run at one instant.
#[derive(Debug)]
pub struct Observation {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub view: Arc<RunView>,
    /// The run's playbook snapshot, when it reads.
    pub playbook: Option<Playbook>,
    /// The one clock reading every deadline of this observation uses.
    pub now_ms: u64,
    /// The journal seq of the `run_paused` that holds a paused run.
    pub pause_seq: Option<u64>,
    /// The send intents nobody can tell went out, which a person resolves.
    pub open_intents: Vec<RunIntent>,
    /// The live sub-playbook runs this run started, oldest first.
    pub children: Vec<ChildObservation>,
    /// A live child sat deeper than [`MAX_CHILD_DEPTH`] and was left out.
    pub children_truncated: bool,
}

/// A live child run and the node of its parent that started it.
#[derive(Debug)]
pub struct ChildObservation {
    pub node_id: String,
    pub observation: Observation,
}

impl Observation {
    /// The executions of every node, derived from the journal this
    /// observation already read (never a second read).
    pub fn identity(&self) -> crate::identity::ExecutionIndex {
        crate::identity::index(&self.view.events)
    }

    /// The definition in force for every segment of the journal this
    /// observation already read, resolved against the run's stored
    /// definitions. Read-only.
    pub fn definitions(&self) -> crate::run_definitions::DefinitionsInForce {
        crate::run_definitions::DefinitionsInForce::load(&self.run_dir, &self.view.events)
    }

    /// Reads run `run_id` at `run_dir` and its live children once.
    pub fn load(run_dir: &Path, run_id: &str) -> Result<Self, EngineError> {
        let view = Arc::new(RunView::load(run_dir, run_id)?);
        Ok(Self::from_view(run_dir, run_id, view))
    }

    /// The observation around a view already read (a wait passes the view
    /// it decided on, so the actions never disagree with it).
    pub fn from_view(run_dir: &Path, run_id: &str, view: Arc<RunView>) -> Self {
        Self::build(run_dir, run_id, view, apb_core::clock::now_ms_u64(), 1)
    }

    fn build(run_dir: &Path, run_id: &str, view: Arc<RunView>, now_ms: u64, depth: usize) -> Self {
        let pause_seq = (view.run_status == RunStatus::Paused)
            .then(|| {
                view.events.iter().rev().find_map(|e| match e.payload {
                    EventPayload::RunPaused { .. } => Some(e.seq),
                    _ => None,
                })
            })
            .flatten();
        let open_intents = crate::run_outcome::intents::outcome_intents(&view.events, now_ms)
            .into_iter()
            .filter(RunIntent::is_open_unknown)
            .collect();
        let (children, children_truncated) = live_children(run_dir, &view, now_ms, depth);
        Self {
            run_id: run_id.to_string(),
            run_dir: run_dir.to_path_buf(),
            playbook: crate::legacy_snapshot::load_run_playbook(run_dir),
            now_ms,
            pause_seq,
            open_intents,
            children,
            children_truncated,
            view,
        }
    }

    pub fn status(&self) -> RunStatus {
        self.view.run_status
    }

    /// The run is stopped by itself: paused, interrupted with no live
    /// driver, or released while a deferred retry waits (the same rule the
    /// default wait applies).
    pub fn stopped(&self) -> bool {
        match self.status() {
            RunStatus::Paused => true,
            RunStatus::Interrupted => self.view.driver_alive != Some(true),
            RunStatus::Running => self.view.released().is_some(),
            _ => false,
        }
    }

    /// A live driver runs this run's clock.
    pub fn driver_alive(&self) -> bool {
        self.view.driver_alive == Some(true) && !self.stopped()
    }

    /// This run and every observed child, parent first, children depth
    /// first in start order.
    pub fn runs(&self) -> Vec<&Observation> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.observation.runs());
        }
        out
    }

    /// Whether any observed level left a child out.
    pub fn truncated(&self) -> bool {
        self.children_truncated || self.children.iter().any(|c| c.observation.truncated())
    }
}

/// The children of `view` that have not ended, each observed; past the
/// depth cap none is read and the second value says one was left out.
fn live_children(
    run_dir: &Path,
    view: &RunView,
    now_ms: u64,
    depth: usize,
) -> (Vec<ChildObservation>, bool) {
    let Some(runs) = run_dir.parent() else {
        return (Vec::new(), false);
    };
    let mut out = Vec::new();
    let mut truncated = false;
    let mut seen = std::collections::BTreeSet::new();
    for e in &view.events {
        let EventPayload::ChildRunStarted { node_id, run_id } = &e.payload else {
            continue;
        };
        if !apb_core::registry::is_safe_segment(run_id) || !seen.insert(run_id.clone()) {
            continue;
        }
        let dir = runs.join(run_id);
        let Ok(child) = RunView::load(&dir, run_id) else {
            continue;
        };
        if child.run_status.is_terminal() {
            continue;
        }
        if depth >= MAX_CHILD_DEPTH {
            truncated = true;
            continue;
        }
        out.push(ChildObservation {
            node_id: node_id.clone(),
            observation: Observation::build(&dir, run_id, Arc::new(child), now_ms, depth + 1),
        });
    }
    (out, truncated)
}
