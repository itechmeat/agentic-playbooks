//! Resume planning (Task 3: resume rework).
//!
//! `plan_resume` folds a run's journal and decides where and how a resume
//! should proceed WITHOUT executing anything or mutating state. The engine's
//! `resume_inner` uses it to journal a `run_resumed` event and hand `drive`
//! the start node plus a `StartMode`; the MCP `run_resume` tool uses it to
//! compute the ack shape before the driver runs (Task 7 will call it for the
//! ack before spawning the detached driver).

use std::path::Path;

use super::*;
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

/// The re-invocation transport chosen for an interactive node's answer round
/// (spec 2026-07-20, Task 7), from its resolved `interaction` ceiling, the
/// session captured from the asking attempt, and the agent's resume form.
#[cfg_attr(test, derive(Debug))]
pub(crate) enum ResumeChoice {
    /// Re-enter the agent's own session (the `resume` transport).
    Resume,
    /// Re-invoke from scratch with the Q&A transcript. `reason` is `Some` when
    /// this is a pre-flight DOWNGRADE from `resume` (the caller journals it);
    /// `None` when the ceiling was already `reprompt` (nothing to journal).
    Reprompt { reason: Option<String> },
}

/// The pre-flight resume-vs-reprompt decision (spec 2026-07-20, Task 7),
/// factored out as a pure function so every branch - including the defensive
/// "captured a session but the agent has no resume form" branch, which no
/// built-in agent can reach today (the agents that capture a session all have a
/// resume form) - is unit-testable. `Live` is downgraded to `Resume` by the
/// caller before this runs; a residual `Live` is treated as `Reprompt`
/// defensively. A missing session or missing resume form downgrades with a
/// reason naming the failure (transport is a ceiling, not a promise).
pub(crate) fn resume_decision(
    interaction: Interaction,
    agent_id: &str,
    node: &str,
    session: Option<&String>,
    resume_form: Option<&Vec<String>>,
) -> ResumeChoice {
    match interaction {
        Interaction::Reprompt | Interaction::Live => ResumeChoice::Reprompt { reason: None },
        Interaction::Resume => match (session, resume_form) {
            (Some(_), Some(_)) => ResumeChoice::Resume,
            (None, _) => ResumeChoice::Reprompt {
                reason: Some(format!(
                    "resume unavailable: no agent session id was captured for node `{node}`; using reprompt"
                )),
            },
            (Some(_), None) => ResumeChoice::Reprompt {
                reason: Some(format!(
                    "resume unavailable: agent `{agent_id}` has no resume form; using reprompt"
                )),
            },
        },
    }
}

/// Builds the reprompt re-invocation prompt for an interactive node's answer
/// round (spec 2026-07-20, Task 6/7): the original rendered prompt followed by
/// THIS visit's Q&A transcript. The window is scoped to the current visit by
/// skipping the prior visits' channel entries (counted from `events` up to this
/// visit's `NodeStarted`), so a looped re-entry does not replay earlier rounds.
/// The transcript is plain quoted text appended AFTER rendering, so no template
/// expansion runs over agent/user text (V13 namespaces do not apply). Shared by
/// the pre-flight reprompt path and the runtime downgrade after a failed resume.
pub(crate) fn build_reprompt_override(
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    node_prompt: &str,
    events: &[Event],
    node: &str,
) -> Result<String, EngineError> {
    let base = render_node_prompt(run_dir, run_id, state, cfg, node_prompt)?;
    let visit_start = current_visit_start_seq(events, node);
    let prior_q = questions_asked_before_seq(events, node, visit_start);
    let prior_a = questions_answered_before_seq(events, node, visit_start);
    let questions: Vec<_> = read_questions_after(run_dir, None)?
        .into_iter()
        .filter(|q| q.node == node)
        .skip(prior_q)
        .collect();
    let answers: Vec<_> = read_answers_after(run_dir, None)?
        .into_iter()
        .filter(|a| a.node == node)
        .skip(prior_a)
        .collect();
    let mut transcript = String::new();
    for (q, a) in questions.iter().zip(answers.iter()) {
        transcript.push_str(&format!("Q: {}\nA: {}\n\n", q.question, a.answer));
    }
    Ok(format!(
        "{base}\n\n## prior questions and answers\n{}",
        transcript.trim_end()
    ))
}

/// Resumes a run in a DETACHED driver process and returns that process's pid
/// immediately. The caller is expected to have already computed the resume
/// decision (`plan_resume`) for its acknowledgement: the child re-derives the
/// same decision from the same journal.
///
/// Equivalent to `resume_detached_with(root, run_id, from_node, false)`:
/// refuses on environment drift.
pub fn resume_detached(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
) -> Result<u32, EngineError> {
    resume_detached_with(root, run_id, from_node, false)
}

/// Like `resume_detached`, but with explicit permission to continue despite
/// environment drift (issue #45 finding 3). The drift preflight runs HERE,
/// before the detached driver is spawned: a drift the caller did not allow is
/// returned as an `Err` inline, instead of the old behaviour where the spawned
/// child failed its own drift check on null stdio and died silently, leaving
/// `run_resume` reporting `detached: true` for a run that never moved. When
/// drift is allowed, the override flag is forwarded to the child so it writes
/// the `EnvironmentDriftAccepted` events and skips its own refusal.
pub fn resume_detached_with(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
) -> Result<u32, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    // Synchronous preflight: surface a drift refusal HERE rather than letting
    // the detached child hit it with null stdio. The child re-runs the same
    // check (it is the authoritative one and writes the accepted events when
    // allowed); this parent-side pass exists only so the error reaches the
    // caller instead of vanishing with the child process.
    check_environment_drift(&run_dir, allow_environment_drift)?;
    let pid = crate::driver::spawn_detached_driver(
        root,
        run_id,
        from_node,
        true,
        allow_environment_drift,
    )
    .map_err(|e| EngineError::Invalid(format!("cannot start the detached run driver: {e}")))?;
    // As in `hand_to_detached_driver`: name the driver before returning, so a
    // stop issued the moment this call comes back sees a live driver instead of
    // finalizing a run the child is about to drive.
    crate::driver::publish_driver_pid(&run_dir, pid);
    Ok(pid)
}

/// Verifies agent binary fingerprints from the manifest against current ones (spec 3.6). On
/// mismatch - an `environment drift` error unless `allow` permits
/// continuing (then the fact is written as an event). Without manifest (executor-path) -
/// no-op.
pub(crate) fn check_environment_drift(
    run_dir: &Path,
    allow: bool,
) -> Result<Vec<EventPayload>, EngineError> {
    let mut drift_events = Vec::new();
    let Some(manifest) = crate::manifest::read(run_dir)? else {
        return Ok(drift_events);
    };
    for p in &manifest.profiles {
        for ri in &p.chain {
            // Fingerprint exactly the fixed binary (the one that will be
            // executed - execute_node builds the adapter from ri.canonical_executable),
            // not re-resolve against live config: otherwise config editing would give
            // false drift, and manifest-binary substitution could pass.
            let now_fp = crate::invocation::fingerprint_path(&ri.canonical_executable);
            if now_fp != ri.executable_fingerprint {
                if allow {
                    drift_events.push(EventPayload::EnvironmentDriftAccepted {
                        agent_id: ri.agent_id.clone(),
                        was: ri.executable_fingerprint.clone(),
                        now: now_fp.clone(),
                    });
                } else {
                    return Err(EngineError::Invalid(format!(
                        "environment drift: agent `{}` binary changed since run start (resume with allow-environment-drift to override)",
                        ri.agent_id
                    )));
                }
            }
        }
    }
    Ok(drift_events)
}

pub fn resume(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
) -> Result<RunResult, EngineError> {
    resume_with(root, run_id, from_node, false)
}

/// Like `resume`, but with explicit permission to continue despite environment drift.
pub fn resume_with(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
) -> Result<RunResult, EngineError> {
    resume_inner(root, run_id, from_node, allow_environment_drift, false)
}

/// Shared implementation behind `resume`/`resume_with`. `allow_shared_workdir`
/// mirrors `RunOptions::allow_shared_workdir`: a sub-playbook child reattached
/// on resume runs on the parent's drive thread while the parent still holds the
/// PID-keyed workdir lock, so its own resume must skip a second acquire
/// (which would return WorkdirBusy). The public entry points pass `false`.
pub(crate) fn resume_inner(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
    allow_shared_workdir: bool,
) -> Result<RunResult, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    // Anti-drift: agent binary must not silently change between start and resume.
    let drift_events = check_environment_drift(&run_dir, allow_environment_drift)?;
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml"))?;
    // Legacy-shim (completion-plan Task 2): a run started before profiles
    // carries snapshot-executors. Choice of legacy-deserialization depends on
    // the snapshot CONTENT, not the presence of manifest - otherwise the second resume
    // (after the first already created ephemeral manifest) would go through
    // strict `Playbook::from_yaml` and fail on LegacyExecutors, making the run
    // non-resumable. When manifest is absent - create it, when present -
    // just read the existing. Do not weaken `Playbook::from_yaml`: live
    // definitions are still sent to migration.
    // A legacy snapshot needs its ephemeral manifest built once; the Playbook
    // itself is parsed by the shared read-only snapshot parser either way (it
    // tolerates the schema-1 executors resume once relied on inline).
    if crate::legacy_snapshot::has_legacy_executors(&yaml)
        && crate::manifest::read(&run_dir)?.is_none()
    {
        let m = crate::legacy_snapshot::build_ephemeral_manifest(&run_dir, &yaml)?;
        crate::manifest::write(&run_dir, &m)?;
    }
    let playbook = crate::legacy_snapshot::parse_snapshot_playbook(&yaml)?;
    let cfg = crate::run_config::read_run_config(&run_dir)?;
    let mut log = EventLog::open(&run_dir)?;
    for ev in drift_events {
        log.append(ev)?;
    }

    // Decide where and how to resume (Task 3): restart interrupted work, advance
    // past a finished node without re-executing it, fall back for a cut-short
    // parallel fork, or honor an explicit `--from-node`. A pointless resume (an
    // argument-free resume of a succeeded run) is refused here.
    let decision = plan_resume(root, run_id, from_node)?;
    // Refuse a pointless `After`-mode resume BEFORE journaling anything: if the
    // already-finished start node has no pending successor to advance into (for
    // example a no-arg resume of a failed terminal run whose last node has no
    // matching failure edge), return an error with NO journal side effect.
    // Writing `RunResumed` first and only discovering the empty frontier inside
    // `drive` would persist a marker after the terminal `RunFinished`, folding
    // the run to running forever and appending another marker on every retry.
    if decision.mode == StartMode::After {
        let state = RunState::fold(&read_all(&run_dir)?);
        if seed_successors(&playbook, &decision.start_node, &state).is_empty() {
            return Err(EngineError::Invalid(format!(
                "node `{}` already finished with no pending successor to resume into - pass --from-node to re-run from a specific node",
                decision.start_node
            )));
        }
    }
    // Journal a proper `run_resumed` marker (folds to running), replacing the
    // old `RunPaused { reason: "resume from X" }` write that used to leave the
    // folded status stuck on paused for the rest of the run.
    log.append(EventPayload::RunResumed {
        from_node: decision.start_node.clone(),
    })?;
    // Mirrors prepare's predicate (`NodeKind::takes_workdir_lock`): a resumed
    // parent with a sub-playbook node (or a finish-with-prompt agent node)
    // still takes the shared workdir lock.
    let is_write = playbook.nodes.iter().any(|n| n.kind.takes_workdir_lock());
    let _guard = if is_write {
        acquire(root, allow_shared_workdir)?
    } else {
        None
    };
    // 4a: resume is always autonomous; supervised resume - subject of phase 4b.
    // supervisor_expected is taken from persistent run cfg (not recreated) -
    // if the original run awaited external supervision, heartbeat-monitoring continues
    // to work even after resume.
    let supervisor_expected = cfg.supervisor_expected;
    drive(
        playbook,
        &run_dir,
        root,
        &mut log,
        &cfg,
        decision.start_node,
        decision.mode,
        run_id.to_string(),
        RunMode::Autonomous,
        supervisor_expected,
    )
}

/// Appends a `RunError` event for a detached driver that failed to start
/// (issue #45 finding 3), so the reason is visible through `run_status`
/// (`failure_reason`) and `apb doctor --run` instead of dying with the
/// driver's null stdio. `node` is `None` because a startup failure happens
/// before any node executes. Best-effort: a write failure here is logged to
/// stderr and swallowed, never masking the original error the caller holds.
pub fn record_run_error(
    root: &Path,
    run_id: &str,
    node: Option<&str>,
    reason: &str,
) -> Result<(), EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let mut log = EventLog::open(&run_dir)?;
    log.append(EventPayload::RunError {
        node: node.map(str::to_string),
        reason: reason.to_string(),
    })?;
    Ok(())
}
