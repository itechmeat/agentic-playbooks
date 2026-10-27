//! What one node execution is handed beyond the node itself
//! ([`ExecOptions`]) and what it returns ([`AttemptOutcome`]), plus what a
//! whole drive returns ([`RunResult`]).

use crate::event::EventPayload;
use crate::state::{NodeStatus, RunStatus};

#[derive(Debug)]
pub struct RunResult {
    pub run_id: String,
    pub outcome: RunStatus,
}

/// The outcome of one node execution (`execute_node`). A normal execution
/// `Finished` with a status, output, and the events drive must write in its
/// return batch. An interactive `agent_task` that asked a question via the
/// stdout marker instead of finishing returns `Suspended` (spec 2026-07-20):
/// the drive loop parks it on the question and re-invokes once an answer
/// arrives.
pub(crate) enum AttemptOutcome {
    Finished {
        status: NodeStatus,
        output: String,
        events: Vec<EventPayload>,
    },
    Suspended {
        question: String,
        options: Vec<String>,
    },
    /// The engine pauses the run: the node hit a provider quota it cannot
    /// wait out (issue #200, design 6; the attempt is journaled
    /// `interrupted` already), or the budget gate refused a list batch. The
    /// node gets no `node_finished`, and the drive pauses the run with
    /// `pause`, so the resume runs the node again.
    EnginePaused {
        pause: crate::pause::PauseRequest,
        events: Vec<EventPayload>,
    },
    /// A list node stopped between items for a pause (a drain pause, or a
    /// `--now` pause whose items are journaled interrupted already): no
    /// `node_finished`, and the control scan applies the pause; the resume
    /// continues the visit.
    Halted { events: Vec<EventPayload> },
}

/// A `resume`-transport re-invocation of an interactive node (spec 2026-07-20,
/// Task 7). Carries the session id captured from the attempt that asked, plus
/// the user's answer to hand the agent as the follow-up prompt. When present,
/// `execute_node` re-enters the primary executor's own session via its resume
/// form instead of re-invoking from scratch with a transcript. Chosen by the
/// drive loop, which downgrades to a plain (`resume: None`) re-invocation when
/// no session was captured or the agent has no resume form.
pub(crate) struct ResumeContext {
    pub session: String,
    pub answer: String,
}

/// A `live`-transport execution of an interactive node (spec 2026-07-20, Task
/// 11). Present only when the drive loop resolved the node's `interaction` to
/// `Live` on claude/claude-code AND could resolve the current exe; a downgrade
/// hands `None`. When present, `execute_node` injects the `apb __ask-server`
/// sidecar into the claude argv, appends the live prompt paragraph instead of
/// the marker contract, and drives the channel observation on this (the drive)
/// thread via the adapter's per-poll `on_tick`.
pub(crate) struct LiveContext {
    /// Current apb executable, resolved by the drive layer (a resolution
    /// failure downgrades before we get here, so this is always present).
    pub exe: std::path::PathBuf,
    /// Per-server tool timeout in ms handed to the sidecar injection.
    pub timeout_ms: u64,
}

/// How one execution runs beyond its node: a one-shot prompt override, an
/// answer round's session, the live sidecar, provenance tracking, the list
/// item it runs, and the drive's stop flags (a list node reads them between
/// items).
#[derive(Default)]
pub(crate) struct ExecOptions<'a> {
    pub override_prompt: Option<String>,
    pub resume: Option<ResumeContext>,
    pub live: Option<LiveContext>,
    /// False for a member of a concurrent batch that shares the tree with
    /// a sibling: `HEAD` moves by both, so no commit could be attributed.
    pub track_provenance: bool,
    pub item: Option<&'a ItemExec>,
    pub stop: Option<&'a crate::stop::StopFlags>,
    /// The run's concurrency cap, for a list node's item pool.
    pub max_parallel: usize,
    /// The drive's control cursor: a list node admits no item while a
    /// Pause or an Abort after it waits to be applied.
    pub control_after: Option<u64>,
    /// The run's budget gate: a list node weighs it before each batch.
    pub budget: Option<&'a super::budget_gate::DriveGate>,
}

/// What every node execution runs against: the run, its state, the flag
/// that kills it, and the journal its attempts write to (a list item's
/// journal stamps the item).
#[derive(Clone, Copy)]
pub(crate) struct NodeCtx<'a, 'j> {
    pub playbook: &'a apb_core::schema::Playbook,
    pub run_dir: &'a std::path::Path,
    pub workdir: &'a std::path::Path,
    pub run_id: &'a str,
    pub state: &'a crate::state::RunState,
    pub cfg: &'a crate::run_config::RunConfig,
    pub cancel: &'a std::sync::atomic::AtomicBool,
    pub env_scrub: &'a [String],
    pub journal: &'a super::journal::Journal<'j>,
    pub decisions: Option<&'a crate::decision::DecisionRunner>,
}

/// What one item's execution is handed beyond its node.
pub(crate) struct ItemExec {
    pub id: String,
    /// The node template, rendered once for every item of the visit.
    pub template: String,
    /// The `## item` section the prompt ends with.
    pub section: String,
    /// The declared item outputs (the status file contract).
    pub outputs: Vec<String>,
    /// The list declares a check: the completion check is skipped.
    pub checked: bool,
    /// The item's JSON on disk, for a script step.
    pub file: std::path::PathBuf,
}

impl ItemExec {
    /// `APB_ITEM_ID` and `APB_ITEM_FILE` for a script step.
    pub(crate) fn script_env(&self) -> Result<Vec<(&'static str, String)>, String> {
        crate::adapter::item_env(&self.id, &self.file)
    }
}
