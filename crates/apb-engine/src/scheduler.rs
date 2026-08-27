use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use apb_core::config::{GlobalConfig, Interaction, SoulDelivery};
use apb_core::migration::validate_migration;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::{Registry, is_safe_segment};
use apb_core::schema::{FailurePolicy, Isolation, NodeKind, Outcome, Playbook, WaitFor};
use apb_core::store::ResolvedPlaybook;
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{promote_policy, promote_version, should_promote};
use serde::Serialize;

use crate::adapter::{AgentAdapter, AgentTask, ErrorClass, adapter_for};
use crate::context::{build_context, build_context_for_render, build_terminal_context, render};
use crate::control::{Control, read_control_after, read_control_cursor, write_control_cursor};
use crate::error::EngineError;
use crate::event::{Event, EventLog, EventPayload, ProfileProvenance, WakeTrigger, read_all};
use crate::inspect::{should_declare_lost, supervisor_silence_ms, write_supervisor_session};
use crate::manifest::{ManifestProfile, ManifestSkill, RunExecutionManifest};
use crate::parallel::{self, JoinReadiness};
use crate::question::{
    post_answer_if_unanswered, post_question, read_answers_after, read_questions_after,
};
use crate::review::read_reviews_after;
use crate::run_config::{
    CacheRunMode, RunConfig, copy_scripts, read_run_config, snapshot_playbook, write_run_config,
};
use crate::script::run_script;
use crate::signals::read_signals_after;
use crate::state::{NodeStatus, RunState, RunStatus};
use crate::workdir::{acquire, acquire_handover_within, acquire_queued};

/// Run mode: autonomous (as in phases 1-3, behavior unchanged) or supervised
/// (the engine stops on a wake event and waits for a command). Defined in
/// `run_config` because it is persisted with the run, re-exported here where
/// every caller expects to find it.
pub use crate::run_config::RunMode;

mod cache;
mod control_apply;
mod entry;
mod journal;
mod listing;
mod live;
mod node;
mod patch;
mod prepare;
mod rebind;
mod resume;
mod status_file;
mod supervisor;

pub(crate) use control_apply::{ControlScan, scan_control};
pub(crate) use entry::Prepared;
use entry::reap_dead_attempts;
pub use entry::{
    PreparedRun, RunOptions, drive_prepared, drive_run_from_dir, post_supervisor_command,
    prepare_supervised_background, run, run_background, run_background_resolved, run_cancel,
    run_resolved, start_detached, start_detached_resolved,
};
pub(crate) use journal::{
    Journal, attempt_started_count, current_visit_start_seq, last_attempt_session,
    last_question_asked_ts, last_wait_started_ts, node_finished_count,
    node_has_unanswered_channel_question, node_primary_invocation, node_started_count,
    question_answered_count, question_asked_count, questions_answered_before_seq,
    questions_asked_before_seq, review_decided_count, review_requested_count, wait_ended_count,
    wait_signalled_count, wait_started_count,
};
pub use listing::{RunSummary, list_runs};
pub(crate) use live::{observe_live_channels, tick_live_observation};
use node::*;
use patch::*;
use prepare::*;
use rebind::*;
pub(crate) use resume::{ResumeChoice, build_reprompt_override, resume_decision, resume_inner};
pub use resume::{ResumeDecision, ResumeReason, StartMode, plan_resume};
pub use resume::{record_run_error, resume, resume_detached, resume_detached_with, resume_with};
pub use supervisor::spawn_supervisor_agent;
use supervisor::*;

/// A one-line, machine-facing detail extracted from a failed finish-answer
/// attempt's last output, for the `RunError` anomaly reason. Empty output (a
/// bare spawn/timeout failure with no body) collapses to a fixed phrase so the
/// reason never renders a dangling `()`.
fn failure_detail(out: &str) -> String {
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return "no agent output".to_string();
    }
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    let capped: String = first_line.chars().take(200).collect();
    capped
}

/// Journals the hop `defaults.on_failure: <node>` just took, as the traversal it
/// is (spec 2026-08-05, Task 4).
///
/// The policy pushes its handler onto the frontier without consulting any
/// declared edge, so before this the journal recorded nothing: a driver that died
/// right after taking the route rebuilt a frontier
/// ([`parallel::pending_heads`]) that had forgotten the handler entirely. The
/// alternative was to re-derive the failure-policy predicate inside that pure
/// reconstruction, which would duplicate a rule that must not drift. Journaling
/// the route makes it visible to the ONE definition of "which way did this node
/// go" (`parallel::routed_targets`) for free, and `via_policy` keeps the record
/// honest about there being no such edge.
fn journal_policy_route(log: &mut EventLog, from: &str, to: &str) -> Result<(), EngineError> {
    log.append(EventPayload::EdgeTraversed {
        from: from.to_string(),
        to: to.to_string(),
        via_policy: true,
        uncounted: false,
    })?;
    Ok(())
}

/// Ends the run on a failure the playbook declared fatal (`defaults.on_failure:
/// stop`, spec 2026-07-26): the node reported a failure and no outgoing edge
/// takes it.
///
/// The reason is the node's own output rather than an engine phrase, which is
/// the difference between this and the `route` policy's error: the author asked
/// for this stop, so what the run report has to explain is what the node said,
/// not that an edge is missing.
///
/// Frontier heads that will now never run are journaled as cancelled, the way a
/// winning `any` join cancels its siblings; otherwise the run report leaves them
/// looking pending forever.
fn stop_on_unhandled_failure(
    log: &mut EventLog,
    node: &str,
    output: &str,
    frontier: &mut Vec<String>,
) -> Result<RunStatus, EngineError> {
    for pending in std::mem::take(frontier) {
        log.append(EventPayload::NodeStarted {
            node: pending.clone(),
            attempt: 1,
        })?;
        log.append(EventPayload::NodeFinished {
            node: pending,
            status: "cancelled".into(),
            attempt: 1,
            output: "cancelled".into(),
            artifacts: Vec::new(),
        })?;
    }
    log.append(EventPayload::RunError {
        node: Some(node.to_string()),
        reason: failure_detail(output),
    })?;
    log.append(EventPayload::RunFinished {
        outcome: "failed".into(),
    })?;
    Ok(RunStatus::Failed)
}

/// The one write-off shape shared by both admission-time stop checks in the
/// batch loop below (`run_cancel` and `cancel_now`): every member of `chunk`
/// gets a `NodeStarted` immediately followed by a `NodeFinished` with
/// `status: "cancelled"` and `output: "cancelled"`, and the same pair is
/// recorded into `batch_results` so the batch tail's own bookkeeping sees it.
/// `"cancelled"` output text matches what a member killed mid-flight writes,
/// so both paths render the same `{{nodes.<id>.output}}`, and the paired
/// start is what `cache::verify_connector_calls`, `journal::current_visit_start_seq`,
/// `progress::node_durations_seconds` and the interactive re-entry guard all
/// assume a finish is preceded by.
fn write_off_cancelled(
    log: &mut EventLog,
    batch_results: &mut Vec<(String, NodeStatus, String)>,
    chunk: &[String],
) -> Result<(), EngineError> {
    for n in chunk {
        log.append(EventPayload::NodeStarted {
            node: n.clone(),
            attempt: 1,
        })?;
        log.append(EventPayload::NodeFinished {
            node: n.clone(),
            status: NodeStatus::Cancelled.as_str().into(),
            attempt: 1,
            output: "cancelled".into(),
            artifacts: Vec::new(),
        })?;
        batch_results.push((n.clone(), NodeStatus::Cancelled, "cancelled".into()));
    }
    Ok(())
}

/// The minimal generated closing message a finish-with-prompt falls back to
/// when its answer-composition agent fails (issue #42 finding 5). The run's
/// substantive work is already done and its declared outcome stands; this is
/// only the cosmetic closing text, so a fixed, machine-facing note that points
/// at the journal is enough. English (machine-facing field), no em-dash or
/// exclamation per the repo convention.
fn fallback_finish_answer(node: &str) -> String {
    format!(
        "Run complete. The automated closing summary for finish node `{node}` \
         could not be composed; see the run events for the composition failure \
         and the full run context."
    )
}

/// Defense-in-depth backstop for sub-playbook nesting (spec C). A child that
/// would exceed this depth fails its parent node.
pub const MAX_SUBPLAYBOOK_DEPTH: usize = 5;

/// How many batch members the drive loop runs at once when neither the playbook
/// nor the run declares a cap (spec 2026-08-05 section 1.3). Wide enough that a
/// fan-out is worth batching, small enough that a fan-out of twenty nodes cannot
/// spawn twenty agent processes at once.
pub const DEFAULT_MAX_PARALLEL: usize = 4;

/// The concurrency cap in force for this run.
///
/// Precedence follows the `max_retries` chain (`node.rs`: the authored value
/// first, then a default, then a constant): `Playbook.defaults.max_parallel`,
/// then the run's own cap, then [`DEFAULT_MAX_PARALLEL`]. `RunOptions` and
/// `RunConfig` are ONE slot here - `prepare` copies the invocation value into the
/// persisted run config, which is what a detached driver reads back, so the cap
/// survives a re-drive.
///
/// A declared `0` reads as `1` (serialize) rather than as "admit nothing", which
/// would wedge the batch.
fn resolved_max_parallel(playbook: &Playbook, cfg: &RunConfig) -> usize {
    playbook
        .defaults
        .max_parallel
        .or(cfg.max_parallel)
        .map(|n| n.max(1))
        .unwrap_or(DEFAULT_MAX_PARALLEL)
}

/// Counter for generating unique supervisor tokens within a single engine
/// process (in addition to the timestamp in the token itself - in case of
/// several spawns within the same millisecond).
static SUPERVISOR_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Returns (playbook id, run's active version). The active version is
/// the latest `RunMigrated.to_version`; if there were no migrations - `RunStarted.version`.
/// Used by the `playbook_patch` tool to choose the base of the patch version.
pub fn run_playbook_ref(root: &Path, run_id: &str) -> Result<(String, String), EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let events = read_all(&run_dir)?;
    let mut id: Option<String> = None;
    let mut version: Option<String> = None;
    for event in &events {
        match &event.payload {
            EventPayload::RunStarted {
                playbook,
                version: v,
            } => {
                id = Some(playbook.clone());
                version = Some(v.clone());
            }
            EventPayload::RunMigrated { to_version, .. } => {
                version = Some(to_version.clone());
            }
            _ => {}
        }
    }
    match (id, version) {
        (Some(id), Some(version)) => Ok((id, version)),
        _ => Err(EngineError::NotFound(format!(
            "run `{run_id}` has no RunStarted event"
        ))),
    }
}

/// The origin (project vs global) a run was started under, from its provenance.
/// Used by the MCP rebind gate to resolve and trust-check a new profile the same
/// way run start resolved the run's node profiles (issue #45 finding 5).
pub fn run_profile_origin(root: &Path, run_id: &str) -> Result<PlaybookOrigin, EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    rebind::run_origin(&run_dir)
}

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
/// arrives. Consumed by Task 5 (timeout expiry in the park loop), Task 6 (the
/// marker scan that feeds it), and Task 7 (resume-vs-reprompt re-invocation).
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
}

/// The shared execution loop: advances the run from `start_node` to a finish node,
/// a pause, or exhausting the step limit. Used by both `run` and `resume`.
///
/// A thin wrapper over [`drive_inner`], which does the actual work and still
/// returns `Err` for every scheduler/prepare-refusal fault it hits (no
/// matching outgoing edge, an exceeded step budget, a stalled resume, a
/// sub-playbook that failed to resolve or prepare, an ordinary I/O fault
/// reading or writing the journal/control file). Before this wrapper, that
/// `Err` propagated straight to the caller: two callers (`run`, `run_resolved`)
/// have no fallback at all, so the run was simply left `running` forever with
/// no terminal event; the callers that DO have a fallback (`drive_prepared`,
/// `drive_run_from_dir`) appended a bare `run_finished(failed)` with no
/// explanation. Diagnosing either shape required reading engine source
/// (issue #42 finding 3). This wrapper appends the verbatim reason as a
/// `RunError` event and the terminal `run_finished(failed)` itself, then
/// returns `Ok` - so every caller, old and new alike, sees a normal terminal
/// result with the reason already in the journal and in `RunState::fold`'s
/// `failure_reason`.
#[allow(clippy::too_many_arguments)]
fn drive(
    playbook: Playbook,
    run_dir: &Path,
    root: &Path,
    log: &mut EventLog,
    cfg: &RunConfig,
    start_node: String,
    start_mode: StartMode,
    run_id: String,
    mode: RunMode,
    supervisor_expected: bool,
) -> Result<RunResult, EngineError> {
    let run_id_for_failure = run_id.clone();
    match drive_inner(
        playbook,
        run_dir,
        root,
        log,
        cfg,
        start_node,
        start_mode,
        run_id,
        mode,
        supervisor_expected,
    ) {
        Ok(r) => Ok(r),
        Err(e) => {
            // Best effort: the run directory may itself be in a bad state
            // (the same disk fault that failed the drive loop could also fail
            // this append) - never let a failure to explain shadow the fact
            // that the run failed. `node: None` here: `drive_inner`'s error
            // messages already name the node inline where one is known (`node
            // `x` has no outgoing edge...`), and there is no single local
            // variable that reliably holds "the node that was executing" at
            // every one of its `?` call sites.
            let _ = log.append(EventPayload::RunError {
                node: None,
                reason: e.to_string(),
            });
            let _ = log.append(EventPayload::RunFinished {
                outcome: "failed".into(),
            });
            Ok(RunResult {
                run_id: run_id_for_failure,
                outcome: RunStatus::Failed,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_inner(
    mut playbook: Playbook,
    run_dir: &Path,
    root: &Path,
    log: &mut EventLog,
    cfg: &RunConfig,
    start_node: String,
    start_mode: StartMode,
    run_id: String,
    mode: RunMode,
    supervisor_expected: bool,
) -> Result<RunResult, EngineError> {
    // Publish who is driving this run, for as long as the drive lasts
    // (Task 7 / issue #45 finding 10). Top-level runs own `driver.pid`;
    // sub-playbook children driven in-process by a parent write `driven_by`
    // instead so doctor/status never treat the parent process as a stale
    // dedicated driver of the child. The guard removes the claim on every
    // exit path.
    let _driver_claim = crate::driver::DriveClaim::claim(run_dir, cfg.parent_run.as_deref());
    let workdir = root.to_path_buf();
    // Adapter env scrubbing (spec 4.3): the union of every env var name
    // referenced by ANY installed connector config (both scopes), computed once
    // per run and removed from every spawned agent's environment - even runs
    // that use no connector, so a token for connector X can never leak into an
    // unrelated run's agent. `apb connector call`, spawned as a child of the
    // agent, resolves secrets from the dotenv files itself.
    let env_scrub = apb_core::connector::resolve::all_referenced_env_names(root);
    // The other active branch heads (besides `current`). A linear run keeps the
    // frontier empty - behavior identical to the old single `current`. A fork
    // (several unconditional outgoing edges) puts the extra targets here; when the
    // `current` branch runs into a not-yet-ready join or a dead end, we take the next one.
    let mut frontier: Vec<String> = Vec::new();
    // Reap before folding (spec 2026-08-05 section 2.4): an attempt whose driver
    // died is still journaled open, and nothing else will ever close it. Closing
    // it as `interrupted` HERE - before this drive reads its own starting state -
    // is what turns "the node reads as lost forever" into ordinary unfinished
    // work that the resume plan and the frontier reconstruction below pick up.
    // The second read happens only when something was actually reaped, so the
    // common case (a fresh run, or a resume with nothing dead) pays one read as
    // before. See `entry::reap_dead_attempts` for the boundaries.
    let mut entry_events = read_all(run_dir)?;
    if !reap_dead_attempts(&entry_events, log)?.is_empty() {
        entry_events = read_all(run_dir)?;
    }
    // The journal as this drive starts. A fresh run has barely one event; a drive
    // over an existing run dir (a resume) needs it twice: the `After` seed
    // evaluates the finished node's edges against it, and both modes rebuild from
    // it the branch heads the dead driver's in-memory frontier took with it.
    let entry_state = RunState::fold(&entry_events);
    let mut current = match start_mode {
        // Restart interrupted work, or an explicit `--from-node` re-run: the
        // start node is executed by the loop below.
        StartMode::Rerun => start_node,
        // Advance past an already-finished node without re-executing it: seed
        // the frontier by evaluating its outgoing edges against the folded
        // status and outputs (exactly the normal post-node advancement), then
        // start from the first ready successor.
        StartMode::After => {
            // The outstanding heads are handed in as extra active nodes so a
            // sibling branch that never started blocks a join here instead of
            // being written off as dead.
            let outstanding = parallel::pending_heads(&playbook, &entry_state);
            advance_frontier(
                &playbook,
                &start_node,
                &entry_state,
                &mut frontier,
                &outstanding,
                log,
            )?;
            // `start_node` is just the LAST node the journal happened to
            // finish, and its own edges can be empty while the run still has
            // real work elsewhere: a batch a Pause deferred (Task 9) leaves
            // siblings that never started at all, reachable only from an
            // EARLIER fan-out point, not from `start_node`. Pull every other
            // outstanding head in now, before deciding this is a dead end -
            // exactly what `restore_frontier` would do a few lines down
            // anyway, just early enough to save the empty-frontier case.
            if frontier.is_empty() {
                restore_frontier(&playbook, &entry_state, &start_node, &mut frontier);
            }
            if frontier.is_empty() {
                // A pointless resume: the start node already finished and has no
                // pending successor to advance into. `resume_inner` already
                // refuses this before journaling `RunResumed`; this is the
                // defensive backstop for any direct `After` drive.
                return Err(EngineError::Invalid(format!(
                    "node `{start_node}` already finished with no pending successor to resume into - pass --from-node to re-run from a specific node"
                )));
            }
            frontier.remove(0)
        }
    };
    // Put the lost branch heads back into the LIVE frontier, so this drive
    // actually executes them. Informing the seed above is not enough: every later
    // advance derives liveness from the frontier, so a head that is not in it
    // flips to dead at the next step and its joins fire without it.
    restore_frontier(&playbook, &entry_state, &current, &mut frontier);
    let max_steps = 10_000usize;
    // A counter of condition-node executions for the runtime max_loops check:
    // the validator (V11) only requires that a loop pass through such a node,
    // while enforcing the actual repeat-count limit is the engine's job.
    let mut cond_visits: BTreeMap<String, u32> = BTreeMap::new();
    // Heartbeat monitoring of the background agent (only when supervisor_expected):
    // we log SupervisorLost and respawn ONCE for the whole
    // drive loop - see the check at the start of each iteration below.
    let mut supervisor_lost_logged = false;
    // A single cursor over control.jsonl for the whole drive call: both the
    // top-of-loop scan at the start of each iteration and await_control (the
    // supervised wait) read from it, so the same entry can never be applied
    // twice. Details of the scheme (that top-of-loop advances the cursor only for
    // Pause/Abort/ContextAppend, while Retry/ContinueFrom is advanced only by
    // await_control) are in the comment before the scan below.
    //
    // Initialized from the persisted `runs/<id>/control.cursor` (Task 4
    // completion-plan defect 1), not hardcoded to `None`: without this, EVERY
    // drive invocation - including a resume, which is simply another `drive`
    // call - re-read control.jsonl from the very beginning and re-applied every
    // historical entry (duplicate `supervisor_action` events, a re-growing
    // context.md, and a stale Pause re-firing on the next drive). Every site
    // below that advances `control_cursor` also calls `write_control_cursor` in
    // the same step, so the persisted value never lags the in-memory one.
    let mut control_cursor: Option<u64> = read_control_cursor(run_dir)?;
    // The run-level cancel flag (Task 8). It is handed to every node this drive
    // executes, and a watcher thread sets it as soon as an Abort shows up in
    // control.jsonl - which is what lets a stop interrupt an agent that is
    // already running instead of waiting for it to finish. The watcher only
    // OBSERVES control.jsonl; the drive loop below still applies the Abort and
    // owns the cursor, so the abort takes effect exactly once. The guard is
    // dropped when this function returns, which stops and joins the thread.
    // `run_cancel` keeps its exact prior meaning: set on Abort only, read by
    // `control_apply` as "an Abort is pending".
    let run_cancel = Arc::new(AtomicBool::new(false));
    // `halt` is set on Abort OR Pause ("admit no new work"), unlike
    // `run_cancel`, which must stay Abort-only or a pause would be read as an
    // abort pending by `control_apply`. Read at the batch admission gate
    // below to stop chunk admission on a Pause without journaling anything
    // off (Task 9).
    let halt = Arc::new(AtomicBool::new(false));
    let watcher = crate::stop::StopWatcher::spawn(
        run_dir,
        control_cursor,
        Arc::clone(&run_cancel),
        Arc::clone(&halt),
    );
    // `fanout.fire()` (triggered by the watcher on Abort) is what lets a
    // run-level Abort reach a flag a batch member polls directly instead of
    // `run_cancel`: the batch registers its batch-local `cancel` with this
    // fanout, so an Abort now also latches `cancel` and kills members already
    // in flight, through their existing pre-attempt and post-adapter-error
    // cancel reads.
    let fanout = watcher.fanout();
    let _stop_watcher = watcher;
    // True once the loop has already taken the cancel short-circuit below, so
    // a pathological case (an unconsumable Retry queued ahead of the Abort
    // stops the top-of-loop scan before it reaches it) degrades to the old
    // behavior instead of spinning.
    let mut cancel_short_circuited = false;
    // Same one-shot idea, for the batch tail: separate from
    // `cancel_short_circuited` because the sequential arm and the batch arm
    // each own their own latch.
    let mut batch_stop_short_circuited = false;
    // One-shot prompt overrides set by the Retry{prompt_override} command:
    // node_id -> text that will replace the rendered prompt for EXACTLY the next
    // execution of that node, after which the entry is removed.
    let mut prompt_overrides: BTreeMap<String, String> = BTreeMap::new();
    let mut last_applied_patch: Option<AppliedPatch> = None;
    // `steps` counts only PRODUCTIVE iterations (node execution/branching): the
    // increment sits on those paths below. Waiting for a human_review decision spins
    // the loop (so the top-of-loop control.jsonl scan keeps catching Pause/Abort),
    // but does NOT count as a step - otherwise a long human wait would exhaust the
    // budget. The limit is checked here, at the start of the iteration.
    let mut steps = 0usize;

    loop {
        if steps >= max_steps {
            return Err(EngineError::Invalid(format!(
                "run exceeded {max_steps} steps without reaching a finish node"
            )));
        }
        match scan_control(
            root,
            run_dir,
            log,
            cfg,
            &mut playbook,
            &mut current,
            &mut control_cursor,
            &mut last_applied_patch,
            &run_cancel,
        )? {
            ControlScan::Terminal(outcome) => return Ok(RunResult { run_id, outcome }),
            ControlScan::Migrated => {
                frontier.clear();
                continue;
            }
            ControlScan::Proceed => {}
        }

        if supervisor_expected {
            monitor_supervisor_heartbeat(
                root,
                &run_id,
                &playbook,
                log,
                &mut supervisor_lost_logged,
            )?;
        }

        let state = RunState::fold(&read_all(run_dir)?);
        let node_kind = playbook
            .node(&current)
            .ok_or_else(|| EngineError::NotFound(current.clone()))?
            .kind
            .clone();

        if let NodeKind::Finish {
            outcome: o,
            prompt,
            profile: _,
        } = &node_kind
        {
            // A finish-with-prompt composes the run answer via an agent (spec
            // B); a finish without a prompt is instant with an empty output
            // (unchanged). NodeStarted + attempt events are written only for the
            // agent path.
            let answer_output = if let Some(p) = prompt {
                log.append(EventPayload::NodeStarted {
                    node: current.clone(),
                    attempt: 1,
                })?;
                // The journal borrows `log` so finish-answer can append its
                // attempt_started at spawn time; the block scopes that borrow so
                // `log` is free again for the return-batch and NodeFinished writes.
                let (st, out, evs) = {
                    let journal = Journal::new(&mut *log);
                    execute_finish_answer(
                        &playbook,
                        run_dir,
                        &workdir,
                        &current,
                        &run_id,
                        &state,
                        cfg,
                        p,
                        &run_cancel,
                        &env_scrub,
                        &journal,
                    )?
                };
                for ev in evs {
                    log.append(ev)?;
                }
                if st != NodeStatus::Succeeded {
                    // The CLOSING answer failed to compose. This must NOT flip
                    // the run's real outcome (issue #42 finding 5): reaching a
                    // terminal finish node means every substantive node already
                    // ran, so the run's result is the finish node's DECLARED
                    // `outcome`, not the success of a cosmetic answer step. The
                    // failed attempt(s) are already journaled by
                    // `execute_finish_answer`; add a RunError anomaly so the
                    // composition failure stays visible (harmless on a succeeded
                    // run - doctor/status read it only while `Failed`), then
                    // fall back to a minimal generated closing message and let
                    // the declared outcome stand.
                    log.append(EventPayload::RunError {
                        node: Some(current.clone()),
                        reason: format!(
                            "finish answer composition failed ({}); \
                             fell back to a generated closing message",
                            failure_detail(&out)
                        ),
                    })?;
                    fallback_finish_answer(&current)
                } else {
                    out
                }
            } else {
                String::new()
            };
            let outcome = match o {
                Outcome::Success => RunStatus::Succeeded,
                Outcome::Failure => RunStatus::Failed,
            };
            let s = match o {
                Outcome::Success => "succeeded",
                Outcome::Failure => "failed",
            };
            log.append(EventPayload::NodeFinished {
                node: current.clone(),
                status: "succeeded".into(),
                attempt: 1,
                output: answer_output,
                artifacts: Vec::new(),
            })?;
            if outcome == RunStatus::Succeeded
                && let Some(applied) = last_applied_patch.as_ref()
            {
                promote_applied_patch(root, run_dir, log, &playbook, applied)?;
            }
            log.append(EventPayload::RunFinished { outcome: s.into() })?;
            return Ok(RunResult { run_id, outcome });
        }

        // Context compaction before rendering the prompt: only if the upcoming
        // node (current or a frontier node) actually renders the run context
        // (`NodeKind::renders_context`: agent_task, prompt, finish-with-prompt,
        // or a playbook node with an instruction template - review R1-M3).
        // Triggered by drive (the sole writer) so that the ContextCompacted
        // event lands in the log before the context is read in execute_node.
        // Disabled if context_max_bytes is not set.
        let renders_context = node_kind.renders_context()
            || frontier
                .iter()
                .any(|n| playbook.node(n).is_some_and(|x| x.kind.renders_context()));
        if renders_context
            && let Some(ev) =
                maybe_compact_context(run_dir, &workdir, cfg, &read_all(run_dir)?, &env_scrub)?
        {
            log.append(ev)?;
        }

        // Concurrent fast path (every run mode, spec 2026-08-05 section 1.3): if,
        // together with current, the frontier has >= 2 ready batchable nodes
        // (`is_batchable`: agent_task/script, non-interactive, non-join), execute
        // them CONCURRENTLY on threads. drive remains the sole writer of events:
        // execute_node never touches the log, it returns events instead; drive
        // writes them as threads finish (order = finish order, spec 8.5).
        // human_review/wait/condition do not enter here (they are not slow and/or
        // they wait). An interactive node may park on a question and a join needs
        // its readiness verdict weighed, and the batch - running on worker threads
        // that cannot write events, park, or fail a barrier - can do neither; both
        // go through the sequential path below instead.
        //
        // `current` must be a batch member itself for the batch to form. A batch
        // of the FRONTIER heads alone would leave `current` neither executed nor
        // re-queued (the tail takes the next head), silently skipping it. So a
        // non-batchable `current` runs sequentially now and its siblings batch on
        // a later iteration.
        //
        // A supervised run batches exactly like an autonomous one; what stays
        // sequential is the SUPERVISION: every member runs to completion and the
        // failures then park one at a time, in batch order, at the batch tail
        // below (per-branch live parking is out of scope, spec 1.3).
        if is_batchable(&playbook, &current) {
            let mut batch: Vec<String> = vec![current.clone()];
            for n in &frontier {
                if is_batchable(&playbook, n) && !batch.contains(n) {
                    batch.push(n.clone());
                }
            }
            // Resolved per iteration, because a supervisor patch can replace the
            // playbook (and with it `defaults.max_parallel`) mid-run.
            let max_parallel = resolved_max_parallel(&playbook, cfg);
            // A cap of 1 means "no concurrency at all", so the batch is not
            // formed: `current` runs through the sequential path below and the
            // frontier heads are picked up on the following iterations. That is
            // both the honest reading of the cap and the arm with full per-node
            // parity (cache, artifacts, progress attribution).
            if batch.len() >= 2 && max_parallel >= 2 {
                steps += batch.len();
                // A shared cancel flag: once join:any is ready we set it, and
                // still-running branches kill their processes (7c-3).
                let cancel = Arc::new(AtomicBool::new(false));
                // Members poll ONE flag; the run-level stop reaches them through
                // it, so `execute_node`'s existing pre-attempt and
                // post-adapter-error cancel reads now cover the abort with
                // exactly the sequential semantics (Cancelled, output
                // "cancelled"). The guard unregisters at the end of the batch.
                let _fan = fanout.register(&cancel);
                // Node, terminal status and output per branch. The output is
                // kept (not just the status) because `defaults.on_failure:
                // stop` reports the failing node's own words as the run's
                // failure reason.
                let mut batch_results: Vec<(String, NodeStatus, String)> = Vec::new();
                // Every member's own pre-execution fingerprint, taken against
                // the tree as it stands before the FIRST chunk runs. Without
                // this a member's cache key silently depends on `max_parallel`,
                // because a later chunk fingerprints a tree earlier chunks have
                // already written into. The eligibility gate keeps this cheap:
                // a playbook without `cache: auto` computes nothing.
                let pre_fps: BTreeMap<String, String> = batch
                    .iter()
                    .filter_map(|n| {
                        cache::pre_fingerprint(&playbook, n, &workdir, cfg).map(|f| (n.clone(), f))
                    })
                    .collect();
                // Admission in deterministic batch order, at most `max_parallel`
                // members in flight at a time. `batch` is NEVER narrowed to the
                // chunk in hand: every join-liveness question below
                // (`feeds_ready_any`, `advance_frontier`) is asked against the
                // WHOLE logical batch, admitted plus still queued, because a
                // member waiting for a slot is still going to run and a join fed
                // by it must not be judged dead and fired early.
                for chunk in batch.chunks(max_parallel) {
                    // `cancel` is snapshotted BEFORE `run_cancel` is tested,
                    // even though `run_cancel` is checked first below: both
                    // flags are registered with the same fanout, and
                    // `fire()` (Task 8) stores `run_cancel` before this
                    // batch's `cancel` (registration order - `run_cancel` was
                    // registered at watcher construction, `cancel` only when
                    // this batch formed). Reading `run_cancel` first would let
                    // a `fire()` landing between the two reads slip through
                    // unnoticed: `run_cancel.load()` returns false (fire
                    // has not stored it yet from this reader's perspective),
                    // then `cancel.load()` returns true (fire's later store
                    // already landed) - taking the legacy join:any shape for
                    // a write-off that was really the abort.
                    //
                    // This ordering narrows that window rather than formally
                    // eliminating it (external review, finding 6): the model
                    // still permits the relaxed `cancel` load to observe
                    // `fire()`'s store while the SeqCst `run_cancel` load below
                    // returns stale, and on real hardware (x86, ARM) the
                    // anomaly is not observable. What makes the residual
                    // harmless rather than merely unlikely is the batch tail:
                    // it no longer depends on what any gate concluded, it reads
                    // both stop flags itself, so a write-off that took the
                    // wrong shape here still ends the run as stopped.
                    let cancel_now = cancel.load(Ordering::Relaxed);
                    // Admission-time re-check, against BOTH stop signals, kept
                    // as two separate `if` arms even though both now write a
                    // queued member off with the IDENTICAL paired-cancelled
                    // shape (`write_off_cancelled` below): `run_cancel` (a
                    // run-level Abort) and `cancel` (a batch-local join:any
                    // already won) are different REASONS a member never gets
                    // admitted, and `a_stop_during_a_batch_does_not_admit_the_queued_chunks`
                    // / `a_queued_branch_is_cancelled_when_an_any_join_is_already_won`
                    // each pin one reason on its own fixture - merging the two
                    // conditions into one `if` would blur which fixture
                    // exercises which trigger.
                    //
                    // `run_cancel` has to be read here: an `Abort` posted while
                    // an earlier chunk was running is not visible to this loop
                    // otherwise (the drive applies it only at the next
                    // top-of-loop scan), so every remaining chunk used to be
                    // admitted and SPAWNED in full after the operator said stop
                    // - real agent processes bought by a run that is already
                    // stopping. Killing the members already IN FLIGHT is
                    // handled separately (Task 9): `cancel` is registered with
                    // the run-level fanout above, so an Abort's `fanout.fire()`
                    // latches this very flag too and each running member's own
                    // poll (in `execute_node`) sees it and kills its process
                    // tree. This gate only has to write off the members not
                    // yet admitted.
                    if run_cancel.load(Ordering::SeqCst) {
                        write_off_cancelled(log, &mut batch_results, chunk)?;
                        continue;
                    }
                    // A batch-local join:any that an earlier chunk already
                    // satisfied cancels the batch. Members that were running
                    // when that happened are SIGKILLed through `cancel`;
                    // members not yet admitted are simply never started and
                    // are journaled with the same `cancelled` status their
                    // killed siblings get, so no batch member is left looking
                    // pending forever. Deciding this per queued node (only
                    // skip when its own any-join is ready) would be
                    // finer-grained than the kill itself, which stops every
                    // running member regardless of which join it feeds; one
                    // rule for both is the point. Same `write_off_cancelled`
                    // shape as the `run_cancel` arm above. The other two
                    // cancelled write-offs in the codebase - one in this file
                    // above (`stop_on_unhandled_failure`'s frontier cancel,
                    // hit on a declared-fatal failure rather than a stop) and
                    // one in `node.rs` (`advance_frontier`'s join:any sibling
                    // cancel, for nodes that never entered a batch) - use the
                    // same paired shape now too, for the same paired-start
                    // consumers.
                    if cancel_now {
                        write_off_cancelled(log, &mut batch_results, chunk)?;
                        continue;
                    }
                    // A pause: no NEW work, and NOTHING is written off. A paused
                    // run is resumable and `Cancelled` is terminal for
                    // `parallel::is_terminal`, so journaling a queued member
                    // here would make `pending_heads` skip it forever and the
                    // resume would silently drop that branch. The gate's
                    // POSITION is shared with the abort; its effect is not.
                    if halt.load(Ordering::SeqCst) {
                        break;
                    }
                    // Per-member cache probe, on the drive thread and in chunk
                    // order, through the same unit the sequential arm uses (spec
                    // 2026-08-05 section 1.4). A member that HITS is finished
                    // right here and never spawned, so it consumes no slot; the
                    // lookup, the artifact restore and the store all stay on one
                    // thread. A member that misses is journaled started, then
                    // spawned with its cache context kept here for `settle`.
                    //
                    // A hit does NOT get the in-batch any-join cancel treatment
                    // its executed siblings get: cancelling here would mean
                    // journaling members of this very chunk as cancelled after
                    // having started them. The batch-tail `advance_frontier`
                    // still resolves the join, so the in-batch kill remains what
                    // it always was - an optimization, not the semantics.
                    let mut spawn: Vec<&String> = Vec::new();
                    let mut ctxs: BTreeMap<String, cache::NodeCacheCtx> = BTreeMap::new();
                    for n in chunk {
                        // Started before the lookup, exactly as in the sequential
                        // arm: a hit is a node that ran, it just ran once before.
                        log.append(EventPayload::NodeStarted {
                            node: n.clone(),
                            attempt: 1,
                        })?;
                        let probe = cache::probe(
                            &playbook,
                            n,
                            run_dir,
                            &workdir,
                            &run_id,
                            &state,
                            cfg,
                            cache::ProbeContext {
                                pre_fingerprint: pre_fps.get(n.as_str()).map(String::as_str),
                                batch_member: true,
                            },
                        )?;
                        let (events, hit) = match probe {
                            cache::CacheProbe::Hit {
                                output,
                                artifacts,
                                events,
                            } => (events, Some((output, artifacts))),
                            cache::CacheProbe::Miss { ctx, events } => {
                                if let Some(c) = ctx {
                                    ctxs.insert(n.clone(), c);
                                }
                                spawn.push(n);
                                (events, None)
                            }
                        };
                        for ev in events {
                            log.append(ev)?;
                        }
                        if let Some((output, artifacts)) = hit {
                            log.append(EventPayload::NodeFinished {
                                node: n.clone(),
                                status: NodeStatus::Succeeded.as_str().into(),
                                attempt: 1,
                                output: output.clone(),
                                artifacts,
                            })?;
                            batch_results.push((n.clone(), NodeStatus::Succeeded, output));
                        }
                    }
                    if spawn.is_empty() {
                        continue;
                    }
                    // The chunk shares ONE journal (the drive's log behind a
                    // Mutex) so each worker thread appends its own
                    // attempt_started at spawn time and attempt_finished at
                    // return, while the collector on this thread appends the
                    // returned RetryStarted/FallbackTriggered and the per-node
                    // NodeFinished through the same journal. Scoped threads let
                    // the workers borrow `&Journal` without a 'static bound; the
                    // block scopes the borrow so `log` is free again for the next
                    // chunk's writes and the frontier writes below. Each append
                    // is one atomic line write, so the shared lock is
                    // uncontended in practice.
                    let journal = Journal::new(&mut *log);
                    let (tx, rx) = mpsc::channel();
                    std::thread::scope(|scope| -> Result<(), EngineError> {
                        for &n in &spawn {
                            let playbook_c = playbook.clone();
                            let rd = run_dir.to_path_buf();
                            let wd = workdir.clone();
                            let rid = run_id.clone();
                            let st = state.clone();
                            let cfg_c = cfg.clone();
                            let node = n.clone();
                            let op = prompt_overrides.remove(n.as_str());
                            let tx = tx.clone();
                            let cancel_c = Arc::clone(&cancel);
                            let scrub_c = env_scrub.clone();
                            let journal_ref = &journal;
                            scope.spawn(move || {
                                let res = execute_node(
                                    &playbook_c,
                                    &rd,
                                    &wd,
                                    &node,
                                    &rid,
                                    &st,
                                    &cfg_c,
                                    op,
                                    &cancel_c,
                                    &scrub_c,
                                    journal_ref,
                                    // Interactive nodes are excluded from the
                                    // concurrent batch, so neither a resume nor
                                    // the live sidecar ever originates here.
                                    None,
                                    None,
                                );
                                let _ = tx.send((node, res));
                            });
                        }
                        drop(tx);
                        // Collect completions in readiness order and write their events.
                        for (node, res) in rx {
                            let (status, output, evs) = match res? {
                                AttemptOutcome::Finished {
                                    status,
                                    output,
                                    events,
                                } => (status, output, events),
                                // Interactive nodes are excluded from the batch
                                // above, so a suspension here is impossible; be
                                // explicit rather than silently mishandle it.
                                AttemptOutcome::Suspended { .. } => {
                                    return Err(EngineError::Invalid(format!(
                                        "interactive node `{node}` must not run in a concurrent batch"
                                    )));
                                }
                            };
                            for ev in evs {
                                journal.append(ev)?;
                            }
                            // Declared-artifact capture and cache admission, the
                            // same unit the sequential arm uses. It runs on THIS
                            // (the drive) thread as each member returns, so the
                            // workspace fingerprint comparison and the store
                            // write stay single-threaded and the artifacts are in
                            // hand for the member's NodeFinished below.
                            let (artifacts, settle_events) = cache::settle(
                                ctxs.get(node.as_str()),
                                &playbook,
                                run_dir,
                                &workdir,
                                &run_id,
                                &node,
                                status,
                                &output,
                            );
                            for ev in settle_events {
                                journal.append(ev)?;
                            }
                            batch_results.push((node.clone(), status, output.clone()));
                            journal.append(EventPayload::NodeFinished {
                                node: node.clone(),
                                status: status.as_str().into(),
                                attempt: 1,
                                output,
                                artifacts,
                            })?;
                            // If this branch successfully fed a join:any - cancel the others.
                            if status == NodeStatus::Succeeded {
                                let state_peek = RunState::fold(&read_all(run_dir)?);
                                // Every batch member counts as active: the
                                // siblings may still be running, so nothing they
                                // can still reach may be written off as dead.
                                let active = active_set(&node, &frontier, &batch);
                                let feeds_ready_any =
                                    parallel::successors(&playbook, &node, &state_peek)
                                        .into_iter()
                                        .any(|s| {
                                            parallel::is_join(&playbook, &s)
                                                && parallel::join_mode(&playbook, &s)
                                                    == parallel::JoinMode::Any
                                                && matches!(
                                                    parallel::join_readiness(
                                                        &playbook,
                                                        &s,
                                                        &state_peek,
                                                        &active
                                                    ),
                                                    JoinReadiness::ReadySuccess
                                                )
                                        });
                                if feeds_ready_any {
                                    cancel.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                        Ok(())
                    })?;
                }
                rebuild_context_md(run_dir)?;
                // Attribute progress the members posted while the batch was in
                // flight, before the tail can return terminally. The batch can
                // exit without ever reaching another top-of-loop scan (the
                // unknown/interrupted pause, a supervised park verdict, or
                // `on_failure: stop`), so a named report would otherwise never
                // be journaled at all. `None` fallback: no single node is "the"
                // node here, so an unnamed report is left to the top-of-loop.
                control_cursor = drain_progress_after_execute(run_dir, log, control_cursor, None)?;
                // A stop landed while this batch was in flight. Go straight back
                // to the top-of-loop scan, which applies the Abort or Pause
                // exactly as it always has and returns terminal. Skipping the
                // tail also removes the supervised park and the on_failure
                // policy from a stopping run, and removes `advance_frontier`
                // over members that never ran. The one-shot latch is mandatory
                // for the same reason `cancel_short_circuited` exists: a
                // terminal command queued behind an unconsumable Retry leaves
                // `scan_control` returning Proceed, and an unlatched `continue`
                // would spin forever.
                //
                // The flags are read HERE, directly, exactly as the sequential
                // arm reads `run_cancel` at its own short-circuit - not through
                // the admission gate's locals. The gate runs only BEFORE each
                // chunk, so a stop landing while the FINAL (or only) chunk is in
                // flight is observed by no gate at all: every member is killed
                // and journaled cancelled, `advance_frontier` skips all of them,
                // the frontier goes empty, and the tail used to return
                // `EngineError::Invalid("parallel batch produced no runnable
                // successor...")` - journaling a deliberately stopped run as
                // FAILED with an engine message for a reason. Both flags are
                // write-once for the life of a drive and `halt` is stored before
                // `fanout.fire()` latches `run_cancel`, so this pair of loads is
                // monotone: whatever a gate saw, the tail still sees.
                let stop_now = run_cancel.load(Ordering::SeqCst) || halt.load(Ordering::SeqCst);
                if stop_now && !batch_stop_short_circuited {
                    batch_stop_short_circuited = true;
                    continue;
                }
                // unknown/interrupted in any branch - pause the run (as in the
                // sequential path).
                if batch_results
                    .iter()
                    .any(|(_, s, _)| matches!(s, NodeStatus::Unknown | NodeStatus::Interrupted))
                {
                    log.append(EventPayload::RunPaused {
                        reason: "parallel branch ended with unknown/interrupted status".into(),
                    })?;
                    return Ok(RunResult {
                        run_id,
                        outcome: RunStatus::Paused,
                    });
                }
                frontier.retain(|n| !batch.contains(n));

                // A supervised run never takes a failure decision itself: each
                // failed member raises its wake and waits, in batch order, AFTER
                // the whole batch completed - exactly as a sequential node parks
                // right after its own NodeFinished, and before any of the
                // autonomous failure policy below (which the sequential path also
                // skips by continuing out of the park).
                if mode == RunMode::Supervised {
                    let failed: Vec<(String, NodeStatus, String)> = batch
                        .iter()
                        .filter_map(|n| batch_results.iter().find(|(bn, _, _)| bn == n))
                        .filter(|(_, s, _)| matches!(s, NodeStatus::Failed | NodeStatus::TimedOut))
                        .cloned()
                        .collect();
                    match park_for_batch_failures(
                        root,
                        run_dir,
                        log,
                        cfg,
                        &mut playbook,
                        &mut control_cursor,
                        &mut prompt_overrides,
                        &mut frontier,
                        &mut last_applied_patch,
                        &failed,
                    )? {
                        BatchWake::NoFailures => {}
                        BatchWake::Terminal(outcome) => return Ok(RunResult { run_id, outcome }),
                        BatchWake::Resumed { next, also } => {
                            // The supervisor's directives lead. This iteration
                            // never advances the batch's own successors, so the
                            // heads the SUCCEEDED members would have produced are
                            // rebuilt from the journal - the same contract a
                            // resume uses, and the reason a park that clears the
                            // frontier does not lose the other branches.
                            current = next;
                            for t in also {
                                if !frontier.contains(&t) {
                                    frontier.push(t);
                                }
                            }
                            let after = RunState::fold(&read_all(run_dir)?);
                            restore_frontier(&playbook, &after, &current, &mut frontier);
                            continue;
                        }
                    }
                }

                let state_now = RunState::fold(&read_all(run_dir)?);

                // The declared failure policy (spec 2026-07-26), the batch
                // counterpart of the sequential check below. Scanned in batch
                // order, not completion order, so two failing branches always
                // name the same one.
                let unhandled: Vec<&(String, NodeStatus, String)> = batch
                    .iter()
                    .filter_map(|n| {
                        batch_results.iter().find(|(bn, s, _)| {
                            bn == n
                                && matches!(s, NodeStatus::Failed | NodeStatus::TimedOut)
                                && parallel::successors(&playbook, bn, &state_now).is_empty()
                        })
                    })
                    .collect();
                if playbook.defaults.on_failure == FailurePolicy::Stop
                    && let Some((node, _, out)) = unhandled.first()
                {
                    return Ok(RunResult {
                        run_id,
                        outcome: stop_on_unhandled_failure(log, node, out, &mut frontier)?,
                    });
                }
                for (node, _, _) in &unhandled {
                    if let Some(target) = playbook.defaults.on_failure.target_for(node)
                        && !frontier.iter().any(|n| n == target)
                    {
                        journal_policy_route(log, node, target)?;
                        frontier.push(target.to_string());
                    }
                }

                for node in &batch {
                    // A member that was killed or never admitted produced
                    // nothing, and `selected_edges` takes unconditional edges
                    // regardless of the source's status, so without this a
                    // cancelled member's `a -> a2` chain continues as if it had
                    // run. A member with no entry at all never reached the gate.
                    match batch_results.iter().find(|(n, _, _)| n == node) {
                        Some((_, NodeStatus::Cancelled, _)) | None => continue,
                        Some(_) => {}
                    }
                    advance_frontier(&playbook, node, &state_now, &mut frontier, &batch, log)?;
                }
                match frontier.is_empty() {
                    true => {
                        return Err(EngineError::Invalid(
                            "parallel batch produced no runnable successor and no finish".into(),
                        ));
                    }
                    false => current = frontier.remove(0),
                }
                continue;
            }
        }

        // A single chain: every branch feeds (status, output) into the shared tail
        // below (NodeFinished + failure handling + frontier advancement).
        // human_review/wait, when unresolved, spin the loop (no step is spent).
        //
        // human_review: pauses until a decision from the reviews.jsonl channel (written by
        // `apb review` / MCP / HTTP); only drive writes events. The Nth decision for
        // a node is consumed once there are already N ReviewDecided events for it.
        //
        // Declared artifacts to record on the shared `NodeFinished` below. Only
        // the node-cache branch sets it (captured on a store, replayed on a
        // hit); every other branch leaves it empty.
        let mut node_artifacts: Vec<apb_core::cache::ArtifactRef> = Vec::new();
        let (status, output) = if let NodeKind::HumanReview { options, prompt } = &node_kind {
            let events = read_all(run_dir)?;
            let decided = review_decided_count(&events, &current);
            let for_current: Vec<_> = read_reviews_after(run_dir, None)?
                .into_iter()
                .filter(|e| e.cmd.node == current)
                .collect();
            if let Some(entry) = for_current.into_iter().nth(decided) {
                steps += 1;
                log.append(EventPayload::NodeStarted {
                    node: current.clone(),
                    attempt: 1,
                })?;
                log.append(EventPayload::ReviewDecided {
                    node: current.clone(),
                    decision: entry.cmd.decision.clone(),
                    note: entry.cmd.note.clone(),
                })?;
                (NodeStatus::Succeeded, entry.cmd.decision.clone())
            } else {
                // No decision yet: declare the request once, then wait (poll).
                // The event carries an owner-facing pending instruction (issue
                // #42 finding 4) so a supervising agent, or any reader of the
                // log alone, can tell the owner an action is expected, what the
                // options are, and how to answer.
                if review_requested_count(&events, &current) <= decided {
                    let title = playbook.node(&current).and_then(|n| n.title.clone());
                    let instruction = crate::progress::review_instruction(
                        &current,
                        title.as_deref(),
                        options,
                        prompt.as_deref(),
                    );
                    log.append(EventPayload::ReviewRequested {
                        node: current.clone(),
                        options: options.clone(),
                        title,
                        instruction,
                        prompt: prompt.clone(),
                    })?;
                }
                std::thread::sleep(AWAIT_CONTROL_POLL);
                continue;
            }
        } else if let NodeKind::Wait {
            wait_for,
            timeout_seconds,
            ..
        } = &node_kind
        {
            let events = read_all(run_dir)?;
            // We declare the start of the wait once per visit (order/loop-resilient).
            if wait_started_count(&events, &current) <= wait_ended_count(&events, &current) {
                log.append(EventPayload::NodeStarted {
                    node: current.clone(),
                    attempt: 1,
                })?;
                let kind = match wait_for {
                    WaitFor::Timer { .. } => "timer",
                    WaitFor::Webhook { .. } => "webhook",
                };
                log.append(EventPayload::WaitStarted {
                    node: current.clone(),
                    kind: kind.into(),
                })?;
            }
            let start_ts = last_wait_started_ts(&read_all(run_dir)?, &current)
                .unwrap_or_else(apb_core::clock::now_ms);
            let elapsed = apb_core::clock::now_ms().saturating_sub(start_ts);
            let signalled = match wait_for {
                WaitFor::Timer { seconds } => elapsed >= u128::from(*seconds) * 1000,
                // We only resolve the wait via an UNCONSUMED signal: how many
                // signals total have arrived for the key versus how many this node
                // has already consumed (its WaitSignalled entries in the log). This way a
                // repeated entry into wait (a loop) is not resolved by a historical, already
                // consumed signal - a new one is required.
                WaitFor::Webhook { key } => {
                    let matching = read_signals_after(run_dir, None)?
                        .iter()
                        .filter(|e| &e.cmd.key == key)
                        .count();
                    let consumed = wait_signalled_count(&read_all(run_dir)?, &current);
                    matching > consumed
                }
            };
            if signalled {
                steps += 1;
                log.append(EventPayload::WaitSignalled {
                    node: current.clone(),
                })?;
                (NodeStatus::Succeeded, String::new())
            } else if elapsed >= u128::from(*timeout_seconds) * 1000 {
                steps += 1;
                log.append(EventPayload::WaitTimeout {
                    node: current.clone(),
                })?;
                (NodeStatus::TimedOut, "wait timeout".into())
            } else {
                std::thread::sleep(AWAIT_CONTROL_POLL);
                continue;
            }
        } else if parallel::is_join(&playbook, &current)
            && matches!(
                parallel::join_readiness(
                    &playbook,
                    &current,
                    &state,
                    &active_set(&current, &frontier, &[])
                ),
                JoinReadiness::ReadyFailure
            )
        {
            // A join node whose input branch has failed (spec 8.4): we do not
            // execute the node, we mark it a failure and route it into the usual failure
            // handling (autonomous branching / supervised wake).
            steps += 1;
            log.append(EventPayload::NodeStarted {
                node: current.clone(),
                attempt: 1,
            })?;
            (
                NodeStatus::Failed,
                "join: upstream branch failed".to_string(),
            )
        } else if let NodeKind::Playbook {
            playbook: child_ref,
            instruction: node_instr,
        } = &node_kind
        {
            // A sub-playbook node runs a full child run in-process on this drive
            // thread (spec C). It is NOT is_agent_or_script, so it never enters the
            // parallel fast path; run_playbook_node takes &mut EventLog and appends
            // ChildRunStarted itself (single writer). Intercepted here before
            // execute_node (whose Playbook arm is only a defensive error).
            steps += 1;
            log.append(EventPayload::NodeStarted {
                node: current.clone(),
                attempt: 1,
            })?;
            run_playbook_node(
                root,
                run_dir,
                log,
                &playbook,
                cfg,
                &run_id,
                &current,
                child_ref,
                node_instr.as_deref(),
            )?
        } else if is_interactive(&playbook, &current) {
            // Interactive agent_task (spec 2026-07-20): the agent may ask the
            // user a question mid-run and the node parks until it is answered.
            // Structured exactly like the human_review branch so it composes
            // with the whole control channel: every park spin does
            // `sleep(AWAIT_CONTROL_POLL); continue;` back to the top-of-loop
            // scan, which keeps applying Pause/Abort/ContextAppend/Patch/
            // Progress/Retry while the question is outstanding. The declare-once
            // and count-based-consumption guards are journal-count based, so
            // they hold across the bounce. Interactive nodes bypass the node
            // cache entirely (a hit would skip the question), so no
            // NodeCacheMiss is journaled for them.
            //
            // `timeout_failed`: set only by the `question_timeout_seconds`
            // expiry-without-`default_answer` case below (Task 5) - the one
            // path in this branch that must fail the node without ever
            // reaching `execute_node`. Every other path either falls through
            // to run an attempt or `continue`s the drive loop directly.
            let events = read_all(run_dir)?;
            let answered = question_answered_count(&events, &current);
            let mut timeout_failed: Option<String> = None;
            // Set by the answer-consumed arm below when the resolved transport
            // is `resume`: carries the captured session + the answer so the
            // fall-through `execute_node` re-enters the agent's own session
            // instead of re-invoking from scratch with a transcript.
            let mut resume_ctx: Option<ResumeContext> = None;

            // Live transport pre-flight (spec 2026-07-20, Task 11). The primary
            // executor's `(agent, interaction)` is snapshotted in the manifest;
            // a `Live` ceiling on claude/claude-code injects the `apb
            // __ask-server` sidecar into the single blocking attempt when the
            // current exe resolves. Otherwise a `Live` ceiling downgrades one
            // tier to `resume` (journaled once per visit at the NodeStarted
            // guard below), which `resume_decision` may further drop to
            // reprompt. Recomputed each drive iteration - cheap, and it never
            // moves mid-run because the manifest is immutable.
            let (prim_agent, prim_interaction) = node_primary_invocation(run_dir, &current)?
                .unwrap_or_else(|| (String::new(), Interaction::Reprompt));
            let live_exe: Option<std::path::PathBuf> = std::env::current_exe().ok();
            let live_claude = prim_agent == "claude" || prim_agent == "claude-code";
            let live_injectable =
                prim_interaction == Interaction::Live && live_claude && live_exe.is_some();
            // Downgrade reason when the ceiling is `Live` but injection is
            // unavailable (non-claude agent, or the current exe could not be
            // resolved). `None` when injectable, or the ceiling is not `Live`.
            let live_downgrade_detail: Option<String> = if prim_interaction == Interaction::Live
                && !live_injectable
            {
                Some(if !live_claude {
                    format!(
                        "live transport requires claude/claude-code; agent `{prim_agent}` for node `{current}` falls back to resume/reprompt"
                    )
                } else {
                    format!(
                        "live transport unavailable for node `{current}`: the current exe could not be resolved; falling back to resume/reprompt"
                    )
                })
            } else {
                None
            };
            // The transport actually used for an answer round: a downgraded
            // `Live` becomes `resume` (then resume_decision may drop to
            // reprompt); every other ceiling is used verbatim.
            let effective_interaction = if live_downgrade_detail.is_some() {
                Interaction::Resume
            } else {
                prim_interaction
            };
            // Per-server tool timeout for the sidecar injection: a BACKSTOP
            // strictly larger than `question_timeout_seconds` (the engine
            // enforces that budget on the drive thread), else the large default.
            let live_timeout_ms = match &node_kind {
                NodeKind::AgentTask {
                    question_timeout_seconds,
                    ..
                } => crate::adapter::live_tool_timeout_ms(*question_timeout_seconds),
                _ => crate::adapter::live_tool_timeout_ms(None),
            };
            // A pending (asked-but-unanswered) question means we are parked.
            if question_asked_count(&events, &current) > answered {
                let for_node: Vec<_> = read_answers_after(run_dir, None)?
                    .into_iter()
                    .filter(|a| a.node == current)
                    .collect();
                match for_node.into_iter().nth(answered) {
                    Some(entry) => {
                        // The next answer arrived: consume it (count-based), then
                        // pick the re-invocation transport by the node's resolved
                        // `interaction` ceiling (spec 2026-07-20, Task 7).
                        log.append(EventPayload::QuestionAnswered {
                            node: current.clone(),
                            answer: entry.answer.clone(),
                            answered_by: entry.answered_by.clone(),
                        })?;

                        // The re-invocation transport comes from the pre-flight
                        // `effective_interaction` computed above (the manifest
                        // ceiling, with a `Live` downgrade already folded in and
                        // journaled once per visit at the NodeStarted guard). A
                        // live-injectable node only reaches this answer-round arm
                        // when its agent ignored the tool and printed the marker
                        // instead (the sidecar happy path answers in-attempt and
                        // never parks); `resume_decision(Live)` maps that to
                        // reprompt, which is the marker mechanism itself.
                        //
                        // A resume needs both a captured agent session and a
                        // declarative resume form; either missing downgrades to
                        // reprompt, journaled with the reason (transport is a
                        // ceiling, not a promise). Pre-flight decision.
                        let session = last_attempt_session(&events, &current);
                        let resume_form = crate::invocation::resume_argv(&prim_agent);
                        match resume_decision(
                            effective_interaction,
                            &prim_agent,
                            &current,
                            session.as_ref(),
                            resume_form.as_ref(),
                        ) {
                            ResumeChoice::Resume => {
                                // Resume: hand execute_node the captured session
                                // and the answer as the follow-up prompt; no
                                // transcript, since the agent re-enters its own
                                // session.
                                resume_ctx = Some(ResumeContext {
                                    session: session.expect("resume implies a captured session"),
                                    answer: entry.answer.clone(),
                                });
                            }
                            ResumeChoice::Reprompt { reason } => {
                                if let Some(detail) = reason {
                                    log.append(EventPayload::SupervisorAction {
                                        action: "interaction_downgraded".into(),
                                        node: Some(current.clone()),
                                        detail,
                                    })?;
                                }
                                let node_prompt = match &node_kind {
                                    NodeKind::AgentTask { prompt, .. } => prompt.clone(),
                                    _ => String::new(),
                                };
                                let ov = build_reprompt_override(
                                    run_dir,
                                    &run_id,
                                    &state,
                                    cfg,
                                    &node_prompt,
                                    &events,
                                    &current,
                                )?;
                                prompt_overrides.insert(current.clone(), ov);
                            }
                        }
                        // Fall through to run the next attempt below.
                    }
                    None => {
                        // Still waiting: check whether `question_timeout_seconds`
                        // has elapsed for the pending question (spec
                        // 2026-07-20, Task 5). With a `default_answer`, post it
                        // through `post_answer_if_unanswered` (`answered_by:
                        // "timeout"` bypasses the `answer_by: human` policy,
                        // per spec Exact answer semantics), then bounce and
                        // let the next iteration's normal count-based
                        // consumption above pick it up. Without one, stop
                        // parking and fail the node outright: a node that
                        // declares a timeout with no default has nothing
                        // sensible to proceed with.
                        let (question_timeout_seconds, default_answer) = match &node_kind {
                            NodeKind::AgentTask {
                                question_timeout_seconds,
                                default_answer,
                                ..
                            } => (*question_timeout_seconds, default_answer.clone()),
                            _ => (None, None),
                        };
                        let expired = match (
                            question_timeout_seconds,
                            last_question_asked_ts(&events, &current),
                        ) {
                            (Some(secs), Some(asked_ts)) => {
                                apb_core::clock::now_ms().saturating_sub(asked_ts)
                                    >= u128::from(secs) * 1000
                            }
                            _ => false,
                        };
                        if !expired {
                            std::thread::sleep(AWAIT_CONTROL_POLL);
                            continue;
                        }
                        match default_answer {
                            Some(ans) => {
                                // `answered` is the node's answer count as
                                // observed by the read at the top of this
                                // branch - exactly the count that made
                                // `for_node.into_iter().nth(answered)` return
                                // `None` above. `post_answer_if_unanswered`
                                // re-checks it under a lock right before
                                // appending, closing the TOCTOU: a
                                // human/supervisor answer that lands in the
                                // gap between that read and this post makes
                                // the recheck fail, so this posts nothing and
                                // the genuine answer is what gets consumed.
                                post_answer_if_unanswered(
                                    run_dir, &current, &ans, "timeout", answered,
                                )?;
                                std::thread::sleep(AWAIT_CONTROL_POLL);
                                continue;
                            }
                            None => {
                                steps += 1;
                                let secs = question_timeout_seconds.unwrap_or(0);
                                timeout_failed = Some(format!(
                                    "interactive node `{current}` timed out after {secs}s waiting for an answer and has no default_answer"
                                ));
                            }
                        }
                    }
                }
            }
            // Live crash-recovery guard (final-fix-wave Finding 1, spec
            // 2026-07-20-interactive-nodes). Covers exactly this window: on the
            // live transport the sidecar wrote its question to questions.jsonl
            // (post_question) but drive crashed before its on_tick observation
            // journaled the matching QuestionAsked. On resume the journal shows
            // question_asked_count == 0, so the park arm above did not fire and
            // we are one step from spawning a FRESH live attempt - whose new
            // sidecar would compute its answer tail from the current (zero)
            // count and could swallow the orphaned question's answer as its
            // own, then journal the orphan afterwards: a mis-delivery. Instead,
            // adopt the orphaned channel entry: journal QuestionAsked from it,
            // raise the wake, and bounce to the top-of-loop scan so the next
            // iteration parks and the answer is delivered by re-invocation (the
            // reprompt transcript path), never a fresh live attempt racing the
            // orphan. Gated on `live_injectable`: only the live path posts to
            // the channel from inside the agent process ahead of the journal;
            // the marker/reprompt path journals QuestionAsked itself before it
            // ever posts. The `question_asked_count <= answered` check means we
            // never reached the park arm (so this is a fresh attempt, not a
            // consumed-answer re-invocation), and `node_has_unanswered_channel_question`
            // is what tells the orphan apart from the normal live happy path
            // (where the channel is still empty at this point).
            if timeout_failed.is_none()
                && live_injectable
                && resume_ctx.is_none()
                && question_asked_count(&events, &current) <= answered
                && node_has_unanswered_channel_question(run_dir, &current)?
            {
                let answered_in_channel = read_answers_after(run_dir, None)?
                    .into_iter()
                    .filter(|a| a.node == current)
                    .count();
                if let Some(orphan) = read_questions_after(run_dir, None)?
                    .into_iter()
                    .filter(|q| q.node == current)
                    .nth(answered_in_channel)
                {
                    log.append(EventPayload::QuestionAsked {
                        node: current.clone(),
                        question: orphan.question,
                        options: orphan.options,
                    })?;
                    crate::event::raise_wake(
                        run_dir,
                        log,
                        WakeTrigger::Anomaly,
                        &current,
                        "interactive question",
                    )?;
                    continue;
                }
            }
            if let Some(msg) = timeout_failed {
                (NodeStatus::Failed, msg)
            } else {
                // Run one agent attempt. NodeStarted is journaled once per node
                // visit (guarded by started-vs-finished counts, like the wait
                // branch): the first attempt of a visit emits it; a
                // re-invocation after an answer does not (the visit is still
                // open).
                if node_started_count(&events, &current) <= node_finished_count(&events, &current) {
                    steps += 1;
                    log.append(EventPayload::NodeStarted {
                        node: current.clone(),
                        attempt: 1,
                    })?;
                    // Journal the live->resume downgrade once per visit (spec
                    // 2026-07-20, Task 11): the ceiling was `Live` but injection
                    // is unavailable (non-claude agent or the exe did not
                    // resolve), so this visit runs the marker/resume path
                    // instead. Placed under the first-attempt guard so it fires
                    // exactly once, before the marker attempt runs.
                    if let Some(detail) = &live_downgrade_detail {
                        log.append(EventPayload::SupervisorAction {
                            action: "interaction_downgraded".into(),
                            node: Some(current.clone()),
                            detail: detail.clone(),
                        })?;
                    }
                }
                let override_prompt = prompt_overrides.remove(&current);
                // Whether THIS attempt is a resume re-invocation: a runtime
                // failure of a resume attempt downgrades to reprompt for the
                // SAME answer round (below), at most once (the reprompt attempt
                // that follows is not a resume attempt, so it never re-downgrades
                // - no transport ping-pong).
                let resume_attempt = resume_ctx.is_some();
                let outcome = {
                    let journal = Journal::new(&mut *log);
                    execute_node(
                        &playbook,
                        run_dir,
                        &workdir,
                        &current,
                        &run_id,
                        &state,
                        cfg,
                        override_prompt,
                        &run_cancel,
                        &env_scrub,
                        &journal,
                        // Resume context set by the answer-consumed arm above;
                        // `None` on the first attempt of a visit and on the
                        // reprompt path.
                        resume_ctx.take(),
                        // Live sidecar (spec 2026-07-20, Task 11): injected only
                        // when the ceiling is `Live` on claude and the exe
                        // resolved; a downgrade hands `None` and the marker path
                        // runs. Rebuilt per attempt so each re-invocation of an
                        // injectable node points the sidecar at the right exe.
                        if live_injectable {
                            Some(crate::scheduler::node::LiveContext {
                                exe: live_exe
                                    .clone()
                                    .expect("live_injectable implies a resolved exe"),
                                timeout_ms: live_timeout_ms,
                            })
                        } else {
                            None
                        },
                    )?
                };
                match outcome {
                    AttemptOutcome::Finished {
                        status,
                        output,
                        events: evs,
                    } => {
                        for ev in evs {
                            log.append(ev)?;
                        }
                        // Live final sweep (spec 2026-07-20, Task 11): the answer
                        // that unblocked the agent may have landed in the last
                        // poll window before the process exited, so observe once
                        // more to journal its `QuestionAnswered`. Idempotent and
                        // count-based, so it never double-writes what `on_tick`
                        // already journaled during the attempt.
                        if live_injectable {
                            let journal = Journal::new(&mut *log);
                            observe_live_channels(run_dir, &current, &journal)?;
                        }
                        // Abandoned-question edge (spec 2026-07-20, Task 11 fix):
                        // a live agent that finished cleanly while a question was
                        // still open (it answered its own way and moved on, or
                        // never waited for the answer) would otherwise leave
                        // silence. Record it so the run report shows what
                        // happened. Gated on a SUCCEEDED finish: a FAILED finish
                        // with an open question is already told by the failure
                        // itself - most notably the question-timeout-without-
                        // default path, whose node-named failure covers it.
                        if live_injectable && status == NodeStatus::Succeeded {
                            let ev = read_all(run_dir)?;
                            if question_asked_count(&ev, &current)
                                > question_answered_count(&ev, &current)
                            {
                                log.append(EventPayload::SupervisorAction {
                                    action: "question_abandoned".into(),
                                    node: Some(current.clone()),
                                    detail: format!(
                                        "live node `{current}` finished with an unanswered question still open"
                                    ),
                                })?;
                            }
                        }
                        // Runtime resume failure (spec 2026-07-20, Task 7): the
                        // resume invocation ran but failed (spawn/transport-init
                        // error, an invalid/expired session, or an agent error on
                        // the resume form - drive cannot cheaply tell an agent-
                        // logic failure from a resume-form failure, so it
                        // downgrades on ANY failure class here, which is safe: a
                        // reprompt retry is merely more expensive). The answer is
                        // already consumed, so downgrade to reprompt and re-run
                        // the SAME answer round via the transcript instead of
                        // failing the node. The follow-up reprompt attempt is not
                        // a resume attempt, so its own failure follows normal
                        // retry/failure handling.
                        if resume_attempt
                            && matches!(status, NodeStatus::Failed | NodeStatus::TimedOut)
                        {
                            log.append(EventPayload::SupervisorAction {
                                action: "interaction_downgraded".into(),
                                node: Some(current.clone()),
                                detail: format!(
                                    "resume re-invocation failed for node `{current}` ({}); retrying the same answer round via reprompt",
                                    output.trim()
                                ),
                            })?;
                            let node_prompt = match &node_kind {
                                NodeKind::AgentTask { prompt, .. } => prompt.clone(),
                                _ => String::new(),
                            };
                            let ov = build_reprompt_override(
                                run_dir,
                                &run_id,
                                &state,
                                cfg,
                                &node_prompt,
                                &events,
                                &current,
                            )?;
                            prompt_overrides.insert(current.clone(), ov);
                            continue;
                        }
                        // Fall to the shared tail (NodeFinished + frontier advance).
                        (status, output)
                    }
                    AttemptOutcome::Suspended { question, options } => {
                        // Declare the question once per suspension
                        // (journal-count based, so it survives the loop
                        // bounce), raise a wake so a waiting supervisor
                        // returns (Task 8), then bounce to the top-of-loop
                        // scan; the next iteration parks.
                        let events = read_all(run_dir)?;
                        if question_asked_count(&events, &current)
                            <= question_answered_count(&events, &current)
                        {
                            let attempt = attempt_started_count(&events, &current) as u32;
                            // Idempotent channel post: skip if a crash between
                            // a prior post_question and its QuestionAsked
                            // already left an unanswered question for this
                            // node in questions.jsonl.
                            if !node_has_unanswered_channel_question(run_dir, &current)? {
                                post_question(
                                    run_dir,
                                    &current,
                                    attempt,
                                    &question,
                                    options.clone(),
                                )?;
                            }
                            log.append(EventPayload::QuestionAsked {
                                node: current.clone(),
                                question,
                                options,
                            })?;
                            crate::event::raise_wake(
                                run_dir,
                                log,
                                WakeTrigger::Anomaly,
                                &current,
                                "interactive question",
                            )?;
                        }
                        continue;
                    }
                }
            }
        } else {
            steps += 1;
            log.append(EventPayload::NodeStarted {
                node: current.clone(),
                attempt: 1,
            })?;
            // Node cache (spec 2026-07-19), through the per-node unit both drive
            // arms share (`cache::probe` / `cache::settle`, spec 2026-08-05
            // section 1.4). `probe` returns a plain `Miss { ctx: None }` for any
            // non-cacheable node, in which case this collapses to the plain
            // execute path. On a hit we skip execution entirely; on a miss we
            // execute and then let `settle` decide whether the result is stored.
            let (cache_ctx, hit) = match cache::probe(
                &playbook,
                &current,
                run_dir,
                &workdir,
                &run_id,
                &state,
                cfg,
                cache::ProbeContext::default(),
            )? {
                cache::CacheProbe::Hit {
                    output,
                    artifacts,
                    events,
                } => {
                    for ev in events {
                        log.append(ev)?;
                    }
                    node_artifacts = artifacts;
                    (None, Some((NodeStatus::Succeeded, output)))
                }
                cache::CacheProbe::Miss { ctx, events } => {
                    for ev in events {
                        log.append(ev)?;
                    }
                    (ctx, None)
                }
            };
            if let Some(hit) = hit {
                hit
            } else {
                let override_prompt = prompt_overrides.remove(&current);
                // The journal borrows `log` for the duration of execute_node so the
                // node can append attempt_started at spawn time; the block scopes
                // that borrow so `log` is free again for the return-batch writes.
                let outcome = {
                    let journal = Journal::new(&mut *log);
                    execute_node(
                        &playbook,
                        run_dir,
                        &workdir,
                        &current,
                        &run_id,
                        &state,
                        cfg,
                        override_prompt,
                        &run_cancel,
                        &env_scrub,
                        &journal,
                        // Non-interactive nodes never resume.
                        None,
                        // ...and never run the live sidecar.
                        None,
                    )?
                };
                let (st, out, evs) = match outcome {
                    AttemptOutcome::Finished {
                        status,
                        output,
                        events,
                    } => (status, output, events),
                    // Only interactive nodes suspend, and they are handled by the
                    // dedicated branch above, never here (this is defensive).
                    AttemptOutcome::Suspended { .. } => {
                        return Err(EngineError::Invalid(format!(
                            "non-interactive node `{current}` produced a question suspension"
                        )));
                    }
                };
                for ev in evs {
                    log.append(ev)?;
                }
                // Declared-artifact capture and cache admission, the same unit
                // the batch path uses.
                let (captured, settle_events) = cache::settle(
                    cache_ctx.as_ref(),
                    &playbook,
                    run_dir,
                    &workdir,
                    &run_id,
                    &current,
                    st,
                    &out,
                );
                node_artifacts = captured;
                for ev in settle_events {
                    log.append(ev)?;
                }
                (st, out)
            }
        };
        log.append(EventPayload::NodeFinished {
            node: current.clone(),
            status: status.as_str().into(),
            attempt: 1,
            output: output.clone(),
            artifacts: node_artifacts,
        })?;

        rebuild_context_md(run_dir)?;

        // Attribute any progress the agent posted while `current` was executing
        // to `current`, before the frontier advances to the successor (B2).
        control_cursor =
            drain_progress_after_execute(run_dir, log, control_cursor, Some(current.as_str()))?;

        // A stop landed while this node was in flight (Task 8): the watcher
        // set the cancel flag and the agent's process tree was killed, so the
        // status just journaled is the wreckage of an interrupted node, not a
        // verdict on the work. Go straight back to the top-of-loop control
        // scan, which applies the pending Abort exactly as it always has and
        // returns Aborted. Without this hop that status would first be routed
        // through the pause/wake/failure handling below and the run would
        // finalize as Paused or Failed instead of Aborted.
        if run_cancel.load(Ordering::SeqCst) && !cancel_short_circuited {
            cancel_short_circuited = true;
            continue;
        }

        // unknown/interrupted halt progression (Phase 2 without supervisor - run pauses).
        if matches!(status, NodeStatus::Unknown | NodeStatus::Interrupted) {
            log.append(EventPayload::RunPaused {
                reason: format!("node `{current}` ended with status {}", status.as_str()),
            })?;
            return Ok(RunResult {
                run_id,
                outcome: RunStatus::Paused,
            });
        }

        // Supervisor mode: a failed/timed-out node raises a wake and waits for a
        // supervisor command instead of autonomously taking a fallback edge.
        if mode == RunMode::Supervised
            && matches!(status, NodeStatus::Failed | NodeStatus::TimedOut)
        {
            match park_for_supervisor(
                root,
                run_dir,
                log,
                cfg,
                &mut playbook,
                &mut current,
                &mut control_cursor,
                &mut prompt_overrides,
                &mut frontier,
                &mut last_applied_patch,
                status,
                &output,
            )? {
                WakeOutcome::Terminal(outcome) => return Ok(RunResult { run_id, outcome }),
                WakeOutcome::Resumed => continue,
            }
        }

        // Check max_loops budget only for condition-nodes and only after
        // their NodeFinished has already been written to the log.
        if let NodeKind::Condition {
            max_loops: Some(limit),
        } = &node_kind
        {
            let visits = cond_visits.entry(current.clone()).or_insert(0);
            *visits += 1;
            if *visits > *limit {
                // Budget exhausted: if the playbook author has a fallback path
                // (fallback edge from this node) - go there, this is a deliberate loop exit.
                let fallback = playbook
                    .edges
                    .iter()
                    .find(|e| e.from == current && e.fallback);
                match fallback {
                    Some(e) => {
                        // The budget-exhausted condition node jumps over its
                        // `fallback` edge without going through
                        // `advance_frontier`, so nothing in the journal
                        // recorded where the run went. `uncounted: true` keeps
                        // accounting exactly as it is: a bounded fallback edge
                        // is still not counted here, which is pre-existing and
                        // deliberately left alone rather than silently fixed in
                        // this change.
                        log.append(EventPayload::EdgeTraversed {
                            from: current.clone(),
                            to: e.to.clone(),
                            via_policy: false,
                            uncounted: true,
                        })?;
                        current = e.to.clone();
                        continue;
                    }
                    None => {
                        // No fallback path - the run fails without waiting for
                        // the hard max_steps limit.
                        log.append(EventPayload::RunFinished {
                            outcome: "failed".into(),
                        })?;
                        return Ok(RunResult {
                            run_id,
                            outcome: RunStatus::Failed,
                        });
                    }
                }
            }
        }

        let state_now = RunState::fold(&read_all(run_dir)?);
        let routes = !parallel::successors(&playbook, &current, &state_now).is_empty();

        // The declared failure policy (spec 2026-07-26): the node failed and
        // nothing takes that failure.
        if !routes && matches!(status, NodeStatus::Failed | NodeStatus::TimedOut) {
            // A declared stop fires even when another branch is still in the
            // frontier: a failure the author called fatal must not be outlived
            // by a sibling and reported as success.
            if playbook.defaults.on_failure == FailurePolicy::Stop {
                return Ok(RunResult {
                    run_id,
                    outcome: stop_on_unhandled_failure(log, &current, &output, &mut frontier)?,
                });
            }
            if let Some(target) = playbook.defaults.on_failure.target_for(&current)
                && !frontier.iter().any(|n| n == target)
            {
                // A route the author declared once instead of drawing it from
                // every node: join the frontier exactly as an explicit failure
                // edge into the same node would have - and be journaled like one.
                journal_policy_route(log, &current, target)?;
                frontier.push(target.to_string());
            }
        }

        advance_frontier(&playbook, &current, &state_now, &mut frontier, &[], log)?;
        if frontier.is_empty() {
            // Branch completed with no ready successors and no other active
            // branches: dead-end (or unready join with no chance) - not finish.
            return Err(EngineError::Invalid(format!(
                "node `{current}` has no outgoing edge and is not finish"
            )));
        }
        // Take the next active head (FIFO - deterministic order
        // by edge declaration).
        current = frontier.remove(0);
    }
}

#[cfg(test)]
mod max_parallel_tests {
    use super::*;

    fn playbook(defaults: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 1\nid: mp\nname: MP\nversion: 1.0.0\n{defaults}\
             nodes:\n  - {{ id: start, type: start }}\n  \
             - {{ id: done, type: finish, outcome: success }}\nedges:\n  \
             - {{ from: start, to: done }}\n"
        ))
        .expect("fixture playbook parses")
    }

    fn cfg(max_parallel: Option<usize>) -> RunConfig {
        RunConfig {
            max_parallel,
            ..Default::default()
        }
    }

    #[test]
    fn neither_playbook_nor_run_declares_a_cap() {
        assert_eq!(
            resolved_max_parallel(&playbook(""), &cfg(None)),
            DEFAULT_MAX_PARALLEL
        );
    }

    #[test]
    fn the_run_cap_applies_when_the_playbook_is_silent() {
        // The RunOptions value arrives here through the persisted RunConfig,
        // which is what a detached driver reads back.
        assert_eq!(resolved_max_parallel(&playbook(""), &cfg(Some(2))), 2);
    }

    #[test]
    fn the_authored_cap_wins_over_the_run_cap() {
        let pb = playbook("defaults:\n  max_parallel: 3\n");
        assert_eq!(resolved_max_parallel(&pb, &cfg(Some(8))), 3);
        assert_eq!(resolved_max_parallel(&pb, &cfg(None)), 3);
    }

    #[test]
    fn a_declared_zero_serializes_instead_of_admitting_nothing() {
        // 0 must never mean "no slots at all", which would wedge every batch.
        assert_eq!(
            resolved_max_parallel(&playbook("defaults:\n  max_parallel: 0\n"), &cfg(None)),
            1
        );
        assert_eq!(resolved_max_parallel(&playbook(""), &cfg(Some(0))), 1);
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;

    fn form() -> Vec<String> {
        vec!["--resume".to_string(), "{session}".to_string()]
    }

    #[test]
    fn resume_when_session_and_form_present() {
        let s = "sess-1".to_string();
        let f = form();
        assert!(matches!(
            resume_decision(Interaction::Resume, "claude", "ask", Some(&s), Some(&f)),
            ResumeChoice::Resume
        ));
    }

    #[test]
    fn downgrade_when_no_session() {
        let f = form();
        match resume_decision(Interaction::Resume, "claude", "ask", None, Some(&f)) {
            ResumeChoice::Reprompt { reason: Some(r) } => {
                assert!(
                    r.contains("session"),
                    "reason names the missing session: {r}"
                );
            }
            other => panic!("expected a downgrade reason, got {other:?}"),
        }
    }

    #[test]
    fn downgrade_when_session_but_no_resume_form() {
        // The defensive branch the reviewer flagged: a captured session but no
        // resume form. No built-in agent reaches it (every agent that captures a
        // session also has a resume form), so it is covered here at unit level.
        let s = "sess-1".to_string();
        match resume_decision(Interaction::Resume, "customx", "ask", Some(&s), None) {
            ResumeChoice::Reprompt { reason: Some(r) } => {
                assert!(
                    r.contains("resume form") && r.contains("customx"),
                    "reason names the missing resume form and the agent: {r}"
                );
            }
            other => panic!("expected a no-resume-form downgrade, got {other:?}"),
        }
    }

    #[test]
    fn reprompt_ceiling_never_downgrades() {
        // A `reprompt` ceiling reprompts with no journaled reason even when a
        // session and form are present.
        let s = "sess-1".to_string();
        let f = form();
        assert!(matches!(
            resume_decision(Interaction::Reprompt, "agy", "ask", Some(&s), Some(&f)),
            ResumeChoice::Reprompt { reason: None }
        ));
    }
}
