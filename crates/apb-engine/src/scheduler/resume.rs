//! Resume planning (Task 3: resume rework).
//!
//! `plan_resume` folds a run's journal and decides where and how a resume
//! should proceed WITHOUT executing anything or mutating state. The engine's
//! `resume_inner` uses it to journal a `run_resumed` event and hand `drive`
//! the start node plus a `StartMode`; the MCP `run_resume` tool uses it to
//! compute the ack shape before the driver runs (Task 7 will call it for the
//! ack before spawning the detached driver).

use std::path::Path;

use apb_core::registry::is_safe_segment;

use crate::error::EngineError;
use crate::event::read_all;
use crate::state::{NodeStatus, RunState, RunStatus};

/// How `drive` treats the start node of a resume.
///
/// - `Rerun`: execute the start node (restart interrupted work, or an explicit
///   `--from-node` re-run).
/// - `After`: the start node is already finished; do NOT re-execute it. Seed
///   the frontier by evaluating its outgoing edges against the folded status
///   and outputs, then continue from the first ready successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Rerun,
    After,
}

/// Why a resume starts where it does. Drives both audit reasoning and the MCP
/// ack's `reason` field (snake_case via `as_str`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeReason {
    /// Exactly one node was left started-but-unfinished: restart it.
    InterruptedRestart,
    /// No interrupted work; the last finished node's edges are evaluated to
    /// seed the frontier without re-executing it.
    AdvancePastFinished,
    /// Two or more interrupted branches (a parallel fork cut short): restart
    /// from the last finished node (today's behavior).
    ParallelFallback,
    /// The caller named an explicit `--from-node`.
    ExplicitFromNode,
}

impl ResumeReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResumeReason::InterruptedRestart => "interrupted_restart",
            ResumeReason::AdvancePastFinished => "advance_past_finished",
            ResumeReason::ParallelFallback => "parallel_fallback",
            ResumeReason::ExplicitFromNode => "explicit_from_node",
        }
    }
}

/// The resolved plan for a resume: which node to start at, whether to re-run it
/// or advance past it, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeDecision {
    pub start_node: String,
    pub mode: StartMode,
    pub reason: ResumeReason,
}

/// Decides where a resume should proceed from the run's journal alone, without
/// executing anything or mutating state.
///
/// Semantics (spec / plan, Global Constraints):
/// - An explicit `from_node` always wins: restart exactly that node
///   (`ExplicitFromNode`, `Rerun`).
/// - Otherwise, an argument-free resume of an already-succeeded run is refused
///   with an error pointing at `--from-node` (there is nothing to resume).
/// - Exactly one interrupted node (its last lifecycle state is
///   `node_started`/`attempt_started` with no `node_finished`) -> restart it
///   (`InterruptedRestart`, `Rerun`).
/// - Two or more interrupted nodes -> `ParallelFallback`, `Rerun` from
///   `last_node`.
/// - None interrupted -> `AdvancePastFinished`, `After` from `last_node`.
pub fn plan_resume(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
) -> Result<ResumeDecision, EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let state = RunState::fold(&read_all(&run_dir)?);

    // An explicit target always wins: restart exactly that node.
    if let Some(n) = from_node {
        return Ok(ResumeDecision {
            start_node: n.to_string(),
            mode: StartMode::Rerun,
            reason: ResumeReason::ExplicitFromNode,
        });
    }

    // An argument-free resume of a run that already succeeded is a no-op: no
    // interrupted work to restart and no unfinished frontier to advance into.
    // Refuse it and point at the only meaningful option.
    if state.run_status == RunStatus::Succeeded {
        return Err(EngineError::Invalid(
            "run already succeeded; nothing to resume - pass --from-node to re-run from a specific node"
                .into(),
        ));
    }

    // Interrupted = started but never finished: a node left `Running` (its
    // journal ends at `node_started`) or `Interrupted` (an open
    // `attempt_started` with no `attempt_finished`). Both are nodes whose work
    // the crash cut short.
    let interrupted: Vec<String> = state
        .nodes
        .iter()
        .filter(|(_, st)| matches!(st, NodeStatus::Running | NodeStatus::Interrupted))
        .map(|(n, _)| n.clone())
        .collect();

    match interrupted.as_slice() {
        [only] => Ok(ResumeDecision {
            start_node: only.clone(),
            mode: StartMode::Rerun,
            reason: ResumeReason::InterruptedRestart,
        }),
        [] => {
            let start_node = last_node_or_err(&state)?;
            Ok(ResumeDecision {
                start_node,
                mode: StartMode::After,
                reason: ResumeReason::AdvancePastFinished,
            })
        }
        _ => {
            let start_node = last_node_or_err(&state)?;
            Ok(ResumeDecision {
                start_node,
                mode: StartMode::Rerun,
                reason: ResumeReason::ParallelFallback,
            })
        }
    }
}

fn last_node_or_err(state: &RunState) -> Result<String, EngineError> {
    state
        .last_node
        .clone()
        .ok_or_else(|| EngineError::Invalid("nothing to resume from".into()))
}
