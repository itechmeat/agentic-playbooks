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
use crate::pause::PauseBy;
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
    /// A `--now` pause (issue #200) interrupted several nodes at once: every
    /// one of them runs again. The first is the start node and the drive's
    /// frontier reconstruction re-admits the others; no finished node runs
    /// again.
    PausedParallelRestart,
}

impl ResumeReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResumeReason::InterruptedRestart => "interrupted_restart",
            ResumeReason::AdvancePastFinished => "advance_past_finished",
            ResumeReason::ParallelFallback => "parallel_fallback",
            ResumeReason::ExplicitFromNode => "explicit_from_node",
            ResumeReason::PausedParallelRestart => "paused_parallel_restart",
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
    let events = read_all(&run_dir)?;
    let state = RunState::fold(&events);

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
    // A revived node's `Running`/`Interrupted` predates its revival: the
    // re-run reaches it again, so it is not interrupted work of its own.
    let interrupted: Vec<String> = state
        .nodes
        .iter()
        .filter(|(n, st)| {
            matches!(st, NodeStatus::Running | NodeStatus::Interrupted)
                && !state.revived.contains(*n)
        })
        .map(|(n, _)| n.clone())
        .collect();
    // A re-run an operator or a supervisor asked for that never began (issue
    // #203): its revival is journaled, its node has not started again. It is
    // owed work; advancing past it instead would read its branch as dead and
    // run its joins without it. It never counts as an interrupted node: two
    // owed re-runs are not a cut-off parallel batch, and the parallel
    // fallback would re-run an unrelated last node. With nothing really
    // interrupted the first owed one is re-run, and the frontier rebuild puts
    // every other one back (`restore_frontier`).
    // The first owed one in journal order; reported as an interrupted
    // restart, the reason a re-run that never began is closest to (the
    // reason is a public string, so no new value is added for it).
    if interrupted.is_empty()
        && let Some(owed) = state
            .revivals
            .iter()
            .find(|r| r.nodes.contains(&r.from_node))
            .map(|r| &r.from_node)
    {
        return Ok(ResumeDecision {
            start_node: owed.clone(),
            mode: StartMode::Rerun,
            reason: ResumeReason::InterruptedRestart,
        });
    }

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
        // A `--now` pause interrupted these nodes itself, so every one of them
        // is a branch head of a finished node: rerun them all, nothing else.
        [first, ..] if state.run_status == RunStatus::Paused && paused_now(&events) => {
            Ok(ResumeDecision {
                start_node: first.clone(),
                mode: StartMode::Rerun,
                reason: ResumeReason::PausedParallelRestart,
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

/// Whether the run's current pause began as a `--now` pause (issue #200),
/// the one kind that leaves several nodes interrupted on purpose. A later
/// `run_paused` of the same pause (a new reason or `until`) does not change
/// what the pause did to the nodes.
fn paused_now(events: &[crate::event::Event]) -> bool {
    events
        .iter()
        .rev()
        .take_while(|e| !matches!(e.payload, EventPayload::RunResumed { .. }))
        .any(|e| {
            matches!(
                e.payload,
                EventPayload::RunPaused {
                    mode: Some(crate::pause::PauseMode::Now),
                    ..
                }
            )
        })
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
    playbook: &Playbook,
    events: &[Event],
    node: &str,
) -> Result<String, EngineError> {
    let node_prompt = match playbook.node(node).map(|n| &n.kind) {
        Some(NodeKind::AgentTask { prompt, .. }) => prompt.as_str(),
        _ => "",
    };
    let base = render_node_prompt(
        run_dir,
        run_id,
        state,
        cfg,
        node_prompt,
        &playbook.context_budget(node),
    )?;
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
    resume_detached_by(
        root,
        run_id,
        from_node,
        allow_environment_drift,
        PauseBy::Operator,
    )
}

/// [`resume_detached_with`] naming who resumes the run (issue #200): `by` is
/// journaled on the detached driver's `run_resumed`. The operator surfaces
/// pass `Operator`, the supervisor tools `Supervisor`, the waker and
/// `apb resume --all` `Engine`.
pub fn resume_detached_by(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
    by: PauseBy,
) -> Result<u32, EngineError> {
    resume_detached_req(
        root,
        run_id,
        &ResumeRequest {
            from_node,
            allow_environment_drift,
            by,
            budget: None,
        },
    )
}

/// What a resume asks for: where it restarts, whether environment drift is
/// accepted, who resumes, and the budget change it makes (see
/// [`crate::budget::admit_resume`]).
#[derive(Debug, Clone, Copy)]
pub struct ResumeRequest<'a> {
    pub from_node: Option<&'a str>,
    pub allow_environment_drift: bool,
    pub by: PauseBy,
    pub budget: Option<&'a crate::budget::BudgetResume>,
}

impl ResumeRequest<'_> {
    /// An operator's plain resume.
    pub fn operator() -> Self {
        ResumeRequest {
            from_node: None,
            allow_environment_drift: false,
            by: PauseBy::Operator,
            budget: None,
        }
    }
}

/// Resumes a run in a detached driver process (see [`resume_detached_with`])
/// as `req` asks, budget change included; returns the driver's pid.
pub fn resume_detached_req(
    root: &Path,
    run_id: &str,
    req: &ResumeRequest<'_>,
) -> Result<u32, EngineError> {
    resume_detached_inner(root, run_id, req, None)
}

/// The engine's resume of a paused run (the waker, `apb resume --all`): a
/// detached driver that journals its `run_resumed` only while the run is
/// still paused by the `run_paused` at journal sequence `pause_seq`, checked
/// under the run's event lock (see [`resume_pause`]).
pub(crate) fn resume_detached_pause(
    root: &Path,
    run_id: &str,
    pause_seq: u64,
) -> Result<u32, EngineError> {
    let req = ResumeRequest {
        by: PauseBy::Engine,
        ..ResumeRequest::operator()
    };
    resume_detached_inner(root, run_id, &req, Some(pause_seq))
}

fn resume_detached_inner(
    root: &Path,
    run_id: &str,
    req: &ResumeRequest<'_>,
    pause_seq: Option<u64>,
) -> Result<u32, EngineError> {
    let ResumeRequest {
        from_node,
        allow_environment_drift,
        by,
        budget,
    } = *req;
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
    require_resume_consent(root, run_id)?;
    crate::budget::check_resume(root, run_id, budget, by)?;
    // The same for a journal holding events of a type this binary does not
    // know and cannot skip safely (see `event::read_all`): refuse here, where
    // the caller sees the version mismatch.
    read_all(&run_dir)?;
    // And for a busy lock another run holds: the driver would die on it right
    // after this call reported the resume started (issue #200).
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml"))?;
    let playbook = crate::legacy_snapshot::parse_snapshot_playbook(&yaml)?;
    if let Some(tree) = resume_lock_tree(&playbook, &run_dir)? {
        crate::workdir::check_free_for(root, tree.as_deref(), run_id)?;
    }
    let budget_json = budget
        .filter(|b| !b.is_empty())
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| EngineError::Invalid(format!("cannot pass the budget change on: {e}")))?;
    let pid = crate::driver::spawn_detached_driver(
        root,
        run_id,
        &crate::driver::DriverArgs {
            from_node,
            resume: true,
            allow_environment_drift,
            by: Some(by),
            pause_seq,
            budget_json: budget_json.as_deref(),
        },
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
    // Host execution mode (0.23.0): a mode this binary does not know refuses
    // the resume with a version message, and a host-mode run executes no
    // profile binary, so a changed one is not drift.
    if manifest.is_host_mode()? {
        return Ok(drift_events);
    }
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

/// Tells the operator, once per resume, that the journal holds events a newer
/// apb wrote which this binary skips. Only called once the engine accepted
/// them as settled (see `event::read_all`).
fn warn_skipped_unknown_events(run_dir: &Path, run_id: &str) {
    let Ok(journal) = crate::event::read_journal(run_dir) else {
        return;
    };
    if journal.unknown.is_empty() {
        return;
    }
    let mut kinds: Vec<&str> = journal.unknown.iter().map(|u| u.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    eprintln!(
        "warning: run `{run_id}`: skipping {} event(s) of a type this apb {} does not know ({}); a newer apb wrote them before the run's last checkpoint, so the run continues without them",
        journal.unknown.len(),
        env!("CARGO_PKG_VERSION"),
        kinds.join(", ")
    );
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
    resume_by(
        root,
        run_id,
        from_node,
        allow_environment_drift,
        PauseBy::Operator,
    )
}

/// [`resume_with`] naming who resumes the run (issue #200), journaled as
/// `run_resumed.by`.
pub fn resume_by(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
    allow_environment_drift: bool,
    by: PauseBy,
) -> Result<RunResult, EngineError> {
    resume_req(
        root,
        run_id,
        &ResumeRequest {
            from_node,
            allow_environment_drift,
            by,
            budget: None,
        },
    )
}

/// Resumes a run in this process as `req` asks, budget change included.
pub fn resume_req(
    root: &Path,
    run_id: &str,
    req: &ResumeRequest<'_>,
) -> Result<RunResult, EngineError> {
    require_resume_consent(root, run_id)?;
    crate::budget::check_resume(root, run_id, req.budget, req.by)?;
    resume_inner_checked(root, run_id, req, false, None)
}

/// The engine's resume of the pause at journal sequence `pause_seq` (issue
/// #200): what the detached driver of [`resume_detached_pause`] runs.
/// `run_resumed` is journaled only while that pause still holds the run,
/// checked under the run's event lock, the lock `request_pause` and
/// `stop_run` journal under: a pause, resume or stop that came after the
/// waker's own check makes this a `Conflict` that journals nothing.
pub fn resume_pause(root: &Path, run_id: &str, pause_seq: u64) -> Result<RunResult, EngineError> {
    // A superseded pause ends here, before any check that could journal a
    // refusal (a consent, a busy workdir, a resume plan after a stop).
    pause_still_holds(root, run_id, pause_seq)?;
    require_resume_consent(root, run_id)?;
    crate::budget::check_resume(root, run_id, None, PauseBy::Engine)?;
    let req = ResumeRequest {
        by: PauseBy::Engine,
        ..ResumeRequest::operator()
    };
    resume_inner_checked(root, run_id, &req, false, Some(pause_seq))
}

/// The error [`resume_pause`] returns when the pause it was for no longer
/// holds the run.
pub const PAUSE_SUPERSEDED: &str = "the pause this resume was for no longer holds the run";

/// The error a resume returns when another pause, resume or stop changed the
/// run between its plan and its `run_resumed`: nothing was journaled.
pub const RESUME_SUPERSEDED: &str = "another pause, resume or stop changed the run while this resume was starting; nothing was resumed";

/// `Conflict(PAUSE_SUPERSEDED)` unless the run is paused by the `run_paused`
/// at journal sequence `seq`. A run that does not exist passes, so the
/// caller's own not-found error reports it.
fn pause_still_holds(root: &Path, run_id: &str, seq: u64) -> Result<(), EngineError> {
    if !is_safe_segment(run_id) {
        return Ok(());
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if run_dir.is_dir() && !crate::pause::holds(&read_all(&run_dir)?, seq) {
        return Err(EngineError::Conflict(PAUSE_SUPERSEDED.into()));
    }
    Ok(())
}

/// The journal sequence of the latest event that changed the run's lifecycle
/// (start, pause, resume, stop, finish): what a resume planned against, and
/// what a budget change confirmed for one pause is bound to.
pub fn lifecycle_seq(events: &[crate::event::Event]) -> Option<u64> {
    events
        .iter()
        .rev()
        .find(|e| {
            matches!(
                e.payload,
                EventPayload::RunStarted { .. }
                    | EventPayload::RunPaused { .. }
                    | EventPayload::RunResumed { .. }
                    | EventPayload::RunAborted { .. }
                    | EventPayload::RunFinished { .. }
            )
        })
        .map(|e| e.seq)
}

/// Journals `run_resumed` under the run's event lock (the lock
/// `request_pause`, `stop_run` and the wake-up probe journal under), only
/// while the run's lifecycle is still the one the resume planned against
/// (`planned`) and, for an engine resume of one pause, that pause still
/// holds the run. Two resumes racing each other, or a resume racing a pause
/// or a stop, journal one `run_resumed` at most. The budget change the
/// resume carries (`budget`, see `crate::budget::resume`) is journaled
/// right before it, under the same lock and checks, so a refused resume
/// journals neither; returns whether it journaled one.
///
/// On any failure (a refusal, a busy or unreadable journal, a failed write)
/// the busy lock the resume took goes back to how the journal leaves it:
/// parked under the run again when a `--keep-lock` pause holds it, released
/// otherwise ([`give_lock_back`]).
fn journal_resumed(
    log: &mut EventLog,
    run_dir: &Path,
    plan: &ResumeJournal<'_>,
    guard: &mut Option<crate::workdir::WorkdirGuard>,
    payload: EventPayload,
) -> Result<bool, EngineError> {
    let res = try_journal_resumed(log, run_dir, plan, payload);
    if res.is_err() {
        give_lock_back(run_dir, plan.run_id, guard);
    }
    res
}

/// What [`journal_resumed`] checks the journal against and adds to it.
struct ResumeJournal<'a> {
    run_id: &'a str,
    /// The lifecycle the resume planned against.
    planned: Option<u64>,
    /// The engine resume of the pause at this journal sequence.
    pause_seq: Option<u64>,
    by: PauseBy,
    budget: Option<&'a crate::budget::BudgetResume>,
}

fn try_journal_resumed(
    log: &mut EventLog,
    run_dir: &Path,
    plan: &ResumeJournal<'_>,
    payload: EventPayload,
) -> Result<bool, EngineError> {
    let _event_lock = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK)?;
    let events = read_all(run_dir)?;
    let refusal = match plan.pause_seq {
        Some(seq) if !crate::pause::holds(&events, seq) => Some(PAUSE_SUPERSEDED),
        _ if lifecycle_seq(&events) != plan.planned => Some(RESUME_SUPERSEDED),
        _ => None,
    };
    if let Some(why) = refusal {
        return Err(EngineError::Conflict(why.into()));
    }
    let budget = crate::budget::resume::resume_events(&events, plan.budget, plan.by)?;
    let changed = !budget.is_empty();
    for e in budget {
        log.append(e)?;
    }
    log.append(payload)?;
    Ok(changed)
}

/// The busy lock of a resume that journaled no `run_resumed`: parked under
/// the run again while a `--keep-lock` pause holds it, else released by the
/// guard. A journal that cannot be read falls back to what the lock was
/// when the resume took it (the run's own parked lock is parked again).
/// Decided and parked under the run's event lock, so a stop that lands in
/// between cannot leave a parked lock behind a stopped run.
fn give_lock_back(run_dir: &Path, run_id: &str, guard: &mut Option<crate::workdir::WorkdirGuard>) {
    // Best effort, like `stop`: a lock we could not take must not keep the
    // resume from giving the workdir back.
    let _event_lock = apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK).ok();
    let keep = match read_all(run_dir) {
        Ok(events) => {
            RunState::fold(&events).run_status == RunStatus::Paused
                && matches!(
                    crate::pause::waker::latest_pause(&events),
                    Some((_, true, _))
                )
        }
        Err(_) => guard
            .as_ref()
            .is_some_and(crate::workdir::WorkdirGuard::took_parked),
    };
    if keep
        && let Some(g) = guard.take()
        && let Err(e) = g.park(run_id)
    {
        eprintln!(
            "apb: warning: run `{run_id}` is paused with --keep-lock, but its lock could not be kept: {e}"
        );
    }
}

/// The engine's own irreversible-consent check on resume (0.24.0): a run
/// whose snapshot needs a consent that its manifest does not record is
/// refused here, whatever the caller, so no resume path skips the check the launch
/// surfaces ask for (`apb resume`, MCP `run_resume` record the consent
/// first). A sub-playbook reattached by its parent's resume goes through
/// `resume_inner` directly and is covered by the parent's check. A run that
/// does not exist falls through to the caller's own not-found error.
fn require_resume_consent(root: &Path, run_id: &str) -> Result<(), EngineError> {
    match crate::gate::engine_resume_consent_need(root, run_id) {
        Ok(None) => Ok(()),
        Ok(Some(need)) => Err(EngineError::Invalid(format!(
            "{}: {}; resume it with `apb resume` or MCP `run_resume` to be asked (consent_nonce: {})",
            crate::consent::REFUSAL_POLICY,
            crate::consent::refusal_detail(&need.playbook_id, &need.sources),
            need.nonce()
        ))),
        Err(v) if v.get("policy").and_then(|p| p.as_str()) == Some("not_found") => Ok(()),
        Err(v) => Err(EngineError::Invalid(format!(
            "cannot check the irreversible consent of run `{run_id}`: {v}"
        ))),
    }
}

/// The tree whose busy lock a resume of this run takes, or `None` when the
/// run takes no lock. Mirrors prepare's predicate
/// (`NodeKind::takes_workdir_lock`): a resumed parent with a sub-playbook
/// node (or a finish-with-prompt agent node) still takes the shared workdir
/// lock. The lock covers the run's working tree once one is journaled (issue
/// #67 item 8), so resuming a run over its own worktree does not wait on a
/// run in the execution root; the inner `None` is the execution root.
fn resume_lock_tree(
    playbook: &Playbook,
    run_dir: &Path,
) -> Result<Option<Option<PathBuf>>, EngineError> {
    if !playbook.nodes.iter().any(|n| n.kind.takes_workdir_lock()) {
        return Ok(None);
    }
    Ok(Some(
        RunState::fold(&read_all(run_dir)?)
            .worktree
            .map(PathBuf::from),
    ))
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
    by: PauseBy,
) -> Result<RunResult, EngineError> {
    let req = ResumeRequest {
        from_node,
        allow_environment_drift,
        by,
        budget: None,
    };
    resume_inner_checked(root, run_id, &req, allow_shared_workdir, None)
}

/// [`resume_inner`], journaling `run_resumed` only while the run is still
/// paused by the `run_paused` at journal sequence `pause_seq` when one is
/// given (see [`resume_pause`]).
fn resume_inner_checked(
    root: &Path,
    run_id: &str,
    req: &ResumeRequest<'_>,
    allow_shared_workdir: bool,
    pause_seq: Option<u64>,
) -> Result<RunResult, EngineError> {
    let ResumeRequest {
        from_node,
        allow_environment_drift,
        by,
        budget,
    } = *req;
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    // An engine resume of a pause that no longer holds the run stops before
    // it journals or locks anything.
    if let Some(seq) = pause_seq {
        pause_still_holds(root, run_id, seq)?;
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
    // Opening the log refuses a journal with an unknown event (one a newer
    // apb wrote) that no checkpoint settles; one that is settled is skipped,
    // and said so once.
    let mut log = EventLog::open(&run_dir)?;
    warn_skipped_unknown_events(&run_dir, run_id);
    for ev in drift_events {
        log.append(ev)?;
    }
    // The lifecycle this resume plans against; `run_resumed` is journaled
    // only while it is unchanged (`journal_resumed`).
    let planned = lifecycle_seq(&read_all(&run_dir)?);

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
        // A node a re-run revived has no result of this pass to advance from
        // (issue #203). Refused here, before anything is journaled; the
        // drive keeps the same check as a defensive backstop.
        if !state.current_status(&decision.start_node).is_finished() {
            return Err(EngineError::Invalid(format!(
                "node `{}` {} - pass --from-node to re-run it",
                decision.start_node,
                super::revive::why_not_finished(&state, &decision.start_node)
            )));
        }
        // Same active-set reconstruction as the `After` seed in `drive`: the
        // outstanding branch heads come from the journal, so a sibling branch
        // that never started still blocks a join instead of being written off.
        let outstanding = parallel::pending_heads(&playbook, &state);
        let active = active_set(&decision.start_node, &[], &outstanding);
        let has_direct_successor =
            !seed_successors(&playbook, &decision.start_node, &state, &active).is_empty();
        // `decision.start_node` is just the LAST node the journal happened to
        // finish. On a resume of a batch a Pause deferred mid-flight (Task 9),
        // its own edges can be empty (a join still waiting on siblings that
        // never started at all) while the run is nowhere near done: those
        // siblings are separate pending heads, reachable from an EARLIER
        // finished node (the fan-out point), not from `start_node`. Treat any
        // such still-runnable head as proof this is not a dead end; `drive`'s
        // own `restore_frontier` is what actually re-admits them.
        //
        // Gated on `RunStatus::Paused`, not just "no interrupted node": an
        // ordinary crash (a driver that died with no `RunPaused` on disk)
        // leaves the exact same journal shape - a finished node next to a
        // sibling with zero events - but there the missing sibling is
        // ambiguous (never started, or started and lost to a torn write?), so
        // `--from-node` stays required. A clean Pause is unambiguous: the
        // drive that wrote it faithfully finished writing everything up to
        // that point, so a never-started sibling is provably never started.
        let has_other_pending_work = state.run_status == RunStatus::Paused
            && outstanding.iter().any(|n| {
                n != &decision.start_node
                    && (!parallel::is_join(&playbook, n)
                        || !matches!(
                            parallel::join_readiness(&playbook, n, &state, &active),
                            JoinReadiness::NotReady
                        ))
            });
        if !has_direct_successor && !has_other_pending_work {
            return Err(EngineError::Invalid(format!(
                "node `{}` already finished with no pending successor to resume into - pass --from-node to re-run from a specific node",
                decision.start_node
            )));
        }
    }
    // The busy lock is taken BEFORE the `run_resumed` marker: a lock another
    // run holds refuses the resume and leaves the run as it was (paused),
    // instead of journaling a resume that dies on the spot. An engine resume
    // checks its pause once more first, so a pause that replaced it in the
    // meantime keeps the lock it parked.
    if let Some(seq) = pause_seq {
        pause_still_holds(root, run_id, seq)?;
    }
    let mut guard = match resume_lock_tree(&playbook, &run_dir)? {
        Some(tree) => {
            crate::workdir::acquire_tree_for(root, tree.as_deref(), allow_shared_workdir, run_id)?
        }
        None => None,
    };
    // Journal a proper `run_resumed` marker (folds to running), replacing the
    // old `RunPaused { reason: "resume from X" }` write that used to leave the
    // folded status stuck on paused for the rest of the run.
    let budget_changed = journal_resumed(
        &mut log,
        &run_dir,
        &ResumeJournal {
            run_id,
            planned,
            pause_seq,
            by,
            budget,
        },
        &mut guard,
        EventPayload::RunResumed {
            from_node: decision.start_node.clone(),
            by: Some(by),
            explicit: decision.reason == ResumeReason::ExplicitFromNode,
        },
    )?;
    // A budget set on a run that started without one needs the run's
    // estimate before the drive's gate reads it; written after the journal
    // lock is released.
    if budget_changed {
        crate::budget::resume::ensure_estimate(root, &run_dir);
    }
    // The persisted mode, as a resume drives it (`RunMode::on_resume`): an
    // agent-supervised run keeps its supervisor agent (heartbeat monitoring
    // and parking both continue), a session-supervised one resumes
    // autonomous, since the session that supervised it is gone.
    let mode = cfg.mode.on_resume();
    drive(
        playbook,
        &run_dir,
        root,
        &mut log,
        &cfg,
        decision.start_node,
        decision.mode,
        run_id.to_string(),
        mode,
        &mut guard,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pause::PauseRequest;
    use crate::workdir::{LockOwner, lock_owner};

    /// A stop journaled while a failed resume is about to re-park the lock
    /// must win: the resume decides only once the stop has finished.
    #[test]
    fn a_stop_landing_before_the_lock_goes_back_leaves_no_parked_lock() {
        let root = tempfile::tempdir().unwrap();
        let run_dir = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
        })
        .unwrap();
        log.append(
            PauseRequest {
                by: Some(PauseBy::Operator),
                keep_lock: true,
                ..Default::default()
            }
            .to_event(),
        )
        .unwrap();
        let guard = crate::workdir::acquire(root.path(), false).unwrap();
        // The stop holds the event lock while it journals the abort.
        let stop_lock = apb_core::fsutil::lock_dir(&run_dir, crate::stop::EVENT_LOCK).unwrap();
        let dir = run_dir.clone();
        let resume = std::thread::spawn(move || {
            let mut guard = guard;
            give_lock_back(&dir, "r1", &mut guard);
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        log.append(EventPayload::RunAborted {
            reason: "stopped".into(),
            by: None,
        })
        .unwrap();
        drop(stop_lock);
        resume.join().unwrap();
        let owner = lock_owner(&root.path().join(".apb/workdir.lock"));
        assert!(
            !matches!(owner, Some(LockOwner::PausedRun(_))),
            "a stopped run must not keep a parked lock, got {owner:?}"
        );
    }
}
