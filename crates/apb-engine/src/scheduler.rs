use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use apb_core::config::{GlobalConfig, SoulDelivery};
use apb_core::migration::validate_migration;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::{Registry, is_safe_segment};
use apb_core::schema::{Isolation, NodeKind, Outcome, Playbook, WaitFor};
use apb_core::store::ResolvedPlaybook;
use apb_core::validate::{Severity, ValidationContext, validate};
use apb_core::versioning::{promote_policy, promote_version, should_promote};
use serde::Serialize;

use crate::adapter::{AgentAdapter, AgentTask, ErrorClass, adapter_for};
use crate::context::{build_context, build_context_for_render, render};
use crate::control::{Control, read_control_after};
use crate::error::EngineError;
use crate::event::{
    Event, EventLog, EventPayload, ProfileProvenance, WakeTrigger, now_millis, read_all,
};
use crate::inspect::{should_declare_lost, supervisor_silence_ms, write_supervisor_session};
use crate::manifest::{ManifestProfile, ManifestSkill, RunExecutionManifest};
use crate::parallel::{self, JoinReadiness};
use crate::review::read_reviews_after;
use crate::run_config::{RunConfig, copy_scripts, snapshot_playbook, write_run_config};
use crate::script::run_script;
use crate::signals::read_signals_after;
use crate::state::{NodeStatus, RunState, RunStatus};
use crate::workdir::acquire;

/// Run mode: autonomous (as in phases 1-3, behavior unchanged) or
/// supervised (the engine stops on a wake event and waits for a command).
mod node;
mod patch;
mod prepare;
mod supervisor;

use node::*;
use patch::*;
use prepare::*;
pub use supervisor::spawn_supervisor_agent;
use supervisor::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    #[default]
    Autonomous,
    Supervised,
}

#[derive(Debug, Default)]
pub struct RunOptions {
    pub instruction: Option<String>,
    pub params: BTreeMap<String, String>,
    pub allow_shared_workdir: bool,
    pub mode: RunMode,
    /// The run waits for an external background agent - the engine spawns it
    /// itself after preparation and will watch its heartbeat (see `drive`). For
    /// requests without an external agent (supervise:"self", regular autonomous) stays false.
    pub supervisor_expected: bool,
    /// Limit of supervisor patches within one run. `None` gives a value of 5.
    pub max_patches_per_run: Option<u32>,
    /// Context size threshold in bytes for compaction (spec 8.5). `None`/0
    /// means compaction is disabled.
    pub context_max_bytes: Option<usize>,
    /// Model used for context compaction. `None` -> "haiku".
    pub context_compact_model: Option<String>,
    /// Run-level overrides (spec 11): different models/executors without a new version.
    pub overrides: Option<apb_core::overrides::RunOverrides>,
    /// Expected definition digest (spec 9): if set, the engine checks it against
    /// the YAML's digest right after loading and refuses to run on a
    /// mismatch. Closes the TOCTOU gap between the policy/preflight trust check and
    /// the actual load (the file could have changed in between).
    pub expected_digest: Option<String>,
    /// Expected profile bundles `<scope>/<name> -> bundle_digest`, captured by
    /// the bundle gate (spec 5.1). If set, the engine checks them against the
    /// bundle recomputed from the snapshot and refuses on a mismatch - this closes
    /// the TOCTOU gap between the gate (policy::check_run) and the snapshot (a
    /// skill/profile could have changed in between). The CLI path does not pass
    /// them and does not change the semantics.
    pub expected_profile_bundles: Option<BTreeMap<String, String>>,
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

#[derive(Debug)]
pub struct RunResult {
    pub run_id: String,
    pub outcome: RunStatus,
}

fn review_decided_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ReviewDecided { node: n, .. } if n == node))
        .count()
}

fn review_requested_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node: n, .. } if n == node),
        )
        .count()
}

fn wait_started_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::WaitStarted { node: n, .. } if n == node))
        .count()
}

fn wait_ended_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(&e.payload, EventPayload::WaitSignalled { node: n } if n == node)
                || matches!(&e.payload, EventPayload::WaitTimeout { node: n } if n == node)
        })
        .count()
}

/// How many times the wait node's waiting resolved specifically via a SIGNAL
/// (not a timeout) - i.e. how many webhook signals this node has already
/// consumed across past visits.
fn wait_signalled_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::WaitSignalled { node: n } if n == node))
        .count()
}

fn last_wait_started_ts(events: &[Event], node: &str) -> Option<u128> {
    events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node: n, .. } if n == node))
        .map(|e| e.ts)
}

/// The result of the run's shared preparation (steps 1-5 of phase-3): the registry
/// opened, the playbook loaded and validated, run_dir created, the snapshot and
/// scripts in place, RunStarted recorded. Used by both `run` (synchronously) and
/// `run_background` (preparation is synchronous, `drive` goes onto a separate thread).
struct Prepared {
    playbook: Playbook,
    run_id: String,
    run_dir: std::path::PathBuf,
    log: EventLog,
    cfg: RunConfig,
    // The field is not read directly - it keeps the workdir lock alive until `Prepared`
    // is dropped (at the end of `run` or at the end of the `run_background` background
    // thread). dead_code here is not about a partial-move bug: even after the
    // closure capture is fixed (see `let mut p = p;` in run_background), rustc still
    // does not consider the field "read" - it is used only through the Drop side
    // effect, not through an explicit read of the value, so the attribute remains
    // necessary for a clean build.
    #[allow(dead_code)]
    guard: Option<crate::workdir::WorkdirGuard>,
    start_node: String,
    mode: RunMode,
    supervisor_expected: bool,
}

pub fn run(
    root: &Path,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<RunResult, EngineError> {
    let mut p = prepare_run(root, id, version, opts)?;
    // `p.guard` lives until the end of this function (dropped together with `p`
    // after drive returns) - the workdir lock is held for the whole synchronous run,
    // just as before the refactor.
    drive(
        p.playbook.clone(),
        &p.run_dir,
        root,
        &mut p.log,
        &p.cfg,
        p.start_node.clone(),
        p.run_id.clone(),
        p.mode,
        p.supervisor_expected,
    )
}

/// Synchronous run of an already-resolved playbook (spec 3): the definition may
/// live in the global store, while execution happens in `execution_root`. The
/// equivalent of `run` for a resolved target; blocks until terminal and returns the result.
pub fn run_resolved(
    resolved: &ResolvedPlaybook,
    mut opts: RunOptions,
) -> Result<RunResult, EngineError> {
    // Tie the expected digest to what the resolver read (anti-TOCTOU).
    opts.expected_digest
        .get_or_insert_with(|| resolved.digest.clone());
    let t = PrepareTarget {
        definition_parent: resolved.definition_parent.clone(),
        execution_root: resolved.execution_root.clone(),
        origin_label: resolved.origin_label,
    };
    let mut p = prepare_run_target(&t, &resolved.id, Some(&resolved.version), opts)?;
    drive(
        p.playbook.clone(),
        &p.run_dir,
        &resolved.execution_root,
        &mut p.log,
        &p.cfg,
        p.start_node.clone(),
        p.run_id.clone(),
        p.mode,
        p.supervisor_expected,
    )
}

/// An opaque handle to a run that is prepared but not yet driven. It exists to
/// separate preparation (synchronous, with fast error returns) from the actual
/// `drive` loop - the caller decides for itself WHERE to call `drive_prepared`
/// (on a thread of the current process - `run_background`, or in a separate OS
/// process - CLI `--supervise`, see `apb-cli`, which needs the run to survive the
/// exit of the CLI invocation itself).
pub struct PreparedRun(Prepared);

impl PreparedRun {
    pub fn run_id(&self) -> &str {
        &self.0.run_id
    }
}

/// Synchronous preparation of a supervised background run: registration,
/// validation, creating run_dir, the snapshot, the workdir lock, and the initial
/// spawn of the background agent (if the run expects one) - everything that must
/// happen BEFORE `drive_prepared` goes off to actually run the run. Factored out
/// of `run_background` so that the same path can be used by the CLI, which
/// needs to learn the `run_id` before the drive loop starts, but the drive loop
/// itself must survive the CLI process exiting (see `PreparedRun`).
pub fn prepare_supervised_background(
    root: &Path,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<PreparedRun, EngineError> {
    let t = PrepareTarget {
        definition_parent: root.join(".apb"),
        execution_root: root.to_path_buf(),
        origin_label: "project",
    };
    prepare_supervised_background_target(&t, id, version, opts)
}

fn prepare_supervised_background_target(
    t: &PrepareTarget,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<PreparedRun, EngineError> {
    let p = prepare_run_target(t, id, version, opts)?;

    // Initial spawn of the background agent: only for runs that explicitly
    // expect one (CLI --supervise, future web requests) - not for supervise:"self"
    // (there the supervisor is the calling MCP session, not a separate process) and
    // not for regular background runs without expecting a supervisor. Best
    // effort: a spawn failure (no executor, agent program not found) must not
    // bring down the run itself - drive will continue without external oversight,
    // and heartbeat monitoring (if supervisor_expected) will log SupervisorLost
    // and attempt a respawn itself.
    if p.supervisor_expected && p.mode == RunMode::Supervised {
        let _ = spawn_supervisor_agent(&t.execution_root, &p.run_id, &p.playbook);
    }

    Ok(PreparedRun(p))
}

/// Drives a prepared run until a terminal state (or an internal engine
/// error). If `drive` returned `Err` without a terminal record, appends
/// `run_finished(failed)` itself - without this fallback the run would stay
/// stuck in `Running` forever for an external observer (drive already writes a
/// terminal event on all `Ok(..)` paths: Succeeded/Failed/Paused/Aborted;
/// we only reach here on an internal error, e.g. from `execute_node`).
pub fn drive_prepared(root: &Path, prepared: PreparedRun) -> Result<RunResult, EngineError> {
    let mut p = prepared.0;
    let res = drive(
        p.playbook.clone(),
        &p.run_dir,
        root,
        &mut p.log,
        &p.cfg,
        p.start_node.clone(),
        p.run_id.clone(),
        p.mode,
        p.supervisor_expected,
    );
    if res.is_err() {
        let _ = p.log.append(EventPayload::RunFinished {
            outcome: "failed".into(),
        });
    }
    // The guard (if any) is dropped here together with `p` - the workdir lock
    // is released once the run finishes.
    res
}

/// A background (non-blocking) run on a separate thread of the CURRENT process.
/// Suitable for callers whose process lives for the whole run (a web server,
/// `apb mcp`) - the thread will not outlive the process itself exiting. For the CLI
/// `--supervise`, whose process must exit right after printing `run_id`, a
/// different scheme is used - see `prepare_supervised_background` +
/// `drive_prepared`, driven in a separate OS process.
pub fn run_background(
    root: &Path,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<String, EngineError> {
    let prepared = prepare_supervised_background(root, id, version, opts)?;
    let run_id = prepared.run_id().to_string();
    let root_owned = root.to_path_buf();

    std::thread::spawn(move || {
        let _ = drive_prepared(&root_owned, prepared);
    });

    Ok(run_id)
}

/// A background run of an already-resolved playbook (spec 3): the definition may
/// live in the global store, while execution happens in the project's `execution_root`.
/// As with `run_background`, the thread lives in the current process - suitable for
/// `apb mcp` and the web server.
pub fn run_background_resolved(
    resolved: &ResolvedPlaybook,
    mut opts: RunOptions,
) -> Result<String, EngineError> {
    // Tie the expected digest to what the resolver read (anti-TOCTOU).
    opts.expected_digest
        .get_or_insert_with(|| resolved.digest.clone());
    let t = PrepareTarget {
        definition_parent: resolved.definition_parent.clone(),
        execution_root: resolved.execution_root.clone(),
        origin_label: resolved.origin_label,
    };
    let prepared =
        prepare_supervised_background_target(&t, &resolved.id, Some(&resolved.version), opts)?;
    let run_id = prepared.run_id().to_string();
    let exec_root = resolved.execution_root.clone();

    std::thread::spawn(move || {
        let _ = drive_prepared(&exec_root, prepared);
    });

    Ok(run_id)
}

/// Posts a cancel command to control.jsonl of an already-running (or already
/// finished) run. Does not wait for an actual stop - `drive` will see the Abort at
/// the nearest iteration boundary. Idempotent: a repeated call just appends
/// another Abort, which is harmless.
pub fn run_cancel(root: &Path, run_id: &str) -> Result<(), EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    crate::control::post_control(
        &run_dir,
        Control::Abort {
            reason: "run_cancel".into(),
        },
    )?;
    Ok(())
}

/// Posts an arbitrary supervisor command (Retry/ContinueFrom/Pause/Abort/
/// ContextAppend) to control.jsonl of an already-running (or already finished)
/// run. Unlike `run_cancel`, it is not fixed to Abort - it is used by the MCP
/// supervisor tools (Phase 4b) for all command types. Does not wait for
/// actual application - `drive` will see the command at the nearest iteration
/// boundary (top-of-loop) or in `await_control` if the run is currently on a wake.
/// Returns the seq of the recorded entry.
pub fn post_supervisor_command(
    root: &Path,
    run_id: &str,
    cmd: Control,
) -> Result<u64, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    crate::control::post_control(&run_dir, cmd)
}

/// The shared execution loop: advances the run from `start_node` to a finish node,
/// a pause, or exhausting the step limit. Used by both `run` and `resume`.
#[allow(clippy::too_many_arguments)]
fn drive(
    mut playbook: Playbook,
    run_dir: &Path,
    root: &Path,
    log: &mut EventLog,
    cfg: &RunConfig,
    start_node: String,
    run_id: String,
    mode: RunMode,
    supervisor_expected: bool,
) -> Result<RunResult, EngineError> {
    let workdir = root.to_path_buf();
    let mut current = start_node;
    // The other active branch heads (besides `current`). A linear run keeps the
    // frontier empty - behavior identical to the old single `current`. A fork
    // (several unconditional outgoing edges) puts the extra targets here; when the
    // `current` branch runs into a not-yet-ready join or a dead end, we take the next one.
    let mut frontier: Vec<String> = Vec::new();
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
    let mut control_cursor: Option<u64> = None;
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
        // Top-of-loop scan of control.jsonl - works in BOTH modes (Autonomous
        // and Supervised), at the boundary of every iteration, before executing a node.
        //
        // The `control_cursor` is shared with `await_control` below - it is one and
        // the same monotonic sequence of consuming control.jsonl, which is what
        // prevents double application: once a command has advanced the cursor here,
        // `await_control` will never see it again (it reads after the same cursor), and
        // vice versa.
        //
        // Proactively (without a wake) only "stop" commands are handled here -
        // Pause (-> RunPaused, exit Paused), Abort (-> RunAborted, exit
        // Aborted) - and ContextAppend (not terminal: logs SupervisorAction +
        // rebuilds context.md, the cursor advances, the scan continues).
        //
        // Retry/ContinueFrom outside a wake is a caller error (the supervisor
        // should only send them in response to WakeRaised), but we do not lose them:
        // the scan STOPS at such an entry, without advancing the cursor past it.
        // The command stays in control.jsonl with a seq greater than the current cursor and
        // will be consumed by `await_control` on the nearest wake of that same node. This
        // was exactly the Phase 4a bug: the cursor advanced past ANY entry, including
        // Retry/ContinueFrom, silently losing them.
        let mut patch_applied = false;
        for entry in read_control_after(run_dir, control_cursor)? {
            match entry.cmd {
                Control::Abort { reason } => {
                    log.append(EventPayload::RunAborted { reason })?;
                    return Ok(RunResult {
                        run_id,
                        outcome: RunStatus::Aborted,
                    });
                }
                Control::Pause => {
                    log.append(EventPayload::RunPaused {
                        reason: "supervisor pause".into(),
                    })?;
                    return Ok(RunResult {
                        run_id,
                        outcome: RunStatus::Paused,
                    });
                }
                Control::ContextAppend { note } => {
                    log.append(EventPayload::SupervisorAction {
                        action: "context_append".into(),
                        node: None,
                        detail: note,
                    })?;
                    rebuild_context_md(run_dir)?;
                    control_cursor = Some(entry.seq);
                }
                Control::Patch {
                    version,
                    classification,
                    continue_from,
                } => {
                    control_cursor = Some(entry.seq);
                    match apply_patch(
                        root,
                        run_dir,
                        log,
                        cfg,
                        &mut playbook,
                        &mut current,
                        PatchCommand {
                            version,
                            classification,
                            continue_from,
                        },
                    )? {
                        PatchResult::Applied(applied) => {
                            last_applied_patch = Some(*applied);
                            patch_applied = true;
                            break;
                        }
                        PatchResult::Rejected => {}
                        PatchResult::Paused => {
                            return Ok(RunResult {
                                run_id,
                                outcome: RunStatus::Paused,
                            });
                        }
                    }
                }
                Control::Progress { done, total, label } => {
                    log.append(EventPayload::RunProgress {
                        node_id: current.clone(),
                        done,
                        total,
                        label,
                    })?;
                    control_cursor = Some(entry.seq);
                }
                Control::Retry { .. } | Control::ContinueFrom { .. } => {
                    // Valid only inside await_control, in response to a wake -
                    // we do not advance the cursor, the command remains unconsumed.
                    break;
                }
            }
        }

        if patch_applied {
            // A migration - linear continuation from continue_from; we do not
            // carry over the previous parallel branches.
            frontier.clear();
            continue;
        }

        // Heartbeat monitoring: only for runs explicitly expecting an external
        // background agent (`supervisor_expected`). Silence is measured either from
        // the last heartbeat, or (as long as the agent has never checked in) from the
        // moment it was spawned - `supervisor_silence_ms` covers both cases.
        // The threshold is configurable via `APB_SUPERVISOR_HEARTBEAT_MS` (tests
        // set a tiny value so they don't have to wait a real minute).
        // A respawn happens at most once for the whole drive loop - repeated
        // losses within an already-logged one do not respawn again (4c:
        // one retry attempt, after that it is a matter of manual review).
        if supervisor_expected {
            let silence = supervisor_silence_ms(root, &run_id)?;
            let threshold_ms: u128 = std::env::var("APB_SUPERVISOR_HEARTBEAT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60_000u128);
            if should_declare_lost(silence, threshold_ms, supervisor_lost_logged) {
                log.append(EventPayload::SupervisorLost {
                    detail: "supervisor heartbeat lost".into(),
                })?;
                supervisor_lost_logged = true;
                let _ = spawn_supervisor_agent(root, &run_id, &playbook);
            }
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
                let (st, out, evs) = execute_finish_answer(
                    &playbook, run_dir, &workdir, &current, &run_id, &state, cfg, p,
                )?;
                for ev in evs {
                    log.append(ev)?;
                }
                if st != NodeStatus::Succeeded {
                    log.append(EventPayload::NodeFinished {
                        node: current.clone(),
                        status: st.as_str().into(),
                        attempt: 1,
                        output: out.clone(),
                    })?;
                    log.append(EventPayload::RunFinished {
                        outcome: "failed".into(),
                    })?;
                    return Ok(RunResult {
                        run_id,
                        outcome: RunStatus::Failed,
                    });
                }
                out
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
        // node (current or an agent_task in the frontier) actually substitutes
        // {{run.context}}. Triggered by drive (the sole writer) so that the
        // ContextCompacted event lands in the log before the context is read in
        // execute_node. Disabled if context_max_bytes is not set.
        let renders_context = matches!(
            node_kind,
            NodeKind::AgentTask { .. } | NodeKind::Prompt { .. }
        ) || frontier.iter().any(|n| {
            matches!(
                playbook.node(n).map(|x| &x.kind),
                Some(NodeKind::AgentTask { .. })
            )
        });
        if renders_context
            && let Some(ev) = maybe_compact_context(run_dir, &workdir, cfg, &read_all(run_dir)?)?
        {
            log.append(ev)?;
        }

        // Concurrent fast path (autonomous only): if, together with current, the
        // frontier has >= 2 ready slow nodes (agent_task/script), execute
        // them CONCURRENTLY on threads. drive remains the sole writer of
        // events: execute_node never touches the log, it returns events instead; drive
        // writes them as threads finish (order = finish order,
        // spec 8.5). Supervised mode never enters here (wake/await is per single
        // node), and neither do human_review/wait/condition (they are not slow and/or they wait).
        if mode == RunMode::Autonomous {
            let mut batch: Vec<String> = Vec::new();
            if is_agent_or_script(&playbook, &current) {
                batch.push(current.clone());
            }
            for n in &frontier {
                if is_agent_or_script(&playbook, n) && !batch.contains(n) {
                    batch.push(n.clone());
                }
            }
            if batch.len() >= 2 {
                steps += batch.len();
                for n in &batch {
                    log.append(EventPayload::NodeStarted {
                        node: n.clone(),
                        attempt: 1,
                    })?;
                }
                // A shared cancel flag: once join:any is ready we set it, and
                // still-running branches kill their processes (7c-3).
                let cancel = Arc::new(AtomicBool::new(false));
                let (tx, rx) = mpsc::channel();
                for n in &batch {
                    let playbook_c = playbook.clone();
                    let rd = run_dir.to_path_buf();
                    let wd = workdir.clone();
                    let rid = run_id.clone();
                    let st = state.clone();
                    let cfg_c = cfg.clone();
                    let node = n.clone();
                    let op = prompt_overrides.remove(n);
                    let tx = tx.clone();
                    let cancel_c = Arc::clone(&cancel);
                    std::thread::spawn(move || {
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
                        );
                        let _ = tx.send((node, res));
                    });
                }
                drop(tx);
                // Collect completions in readiness order and write their events.
                let mut batch_statuses: Vec<NodeStatus> = Vec::new();
                for (node, res) in rx {
                    let (status, output, evs) = res?;
                    for ev in evs {
                        log.append(ev)?;
                    }
                    log.append(EventPayload::NodeFinished {
                        node: node.clone(),
                        status: status.as_str().into(),
                        attempt: 1,
                        output,
                    })?;
                    batch_statuses.push(status);
                    // If this branch successfully fed a join:any - cancel the others.
                    if status == NodeStatus::Succeeded {
                        let state_peek = RunState::fold(&read_all(run_dir)?);
                        let feeds_ready_any = parallel::successors(&playbook, &node, &state_peek)
                            .into_iter()
                            .any(|s| {
                                parallel::is_join(&playbook, &s)
                                    && parallel::join_mode(&playbook, &s) == parallel::JoinMode::Any
                                    && matches!(
                                        parallel::join_readiness(&playbook, &s, &state_peek),
                                        JoinReadiness::ReadySuccess
                                    )
                            });
                        if feeds_ready_any {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    }
                }
                rebuild_context_md(run_dir)?;
                // unknown/interrupted in any branch - pause the run (as in the
                // sequential path).
                if batch_statuses
                    .iter()
                    .any(|s| matches!(s, NodeStatus::Unknown | NodeStatus::Interrupted))
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
                let state_now = RunState::fold(&read_all(run_dir)?);
                for node in &batch {
                    advance_frontier(&playbook, node, &state_now, &mut frontier, log)?;
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
        let (status, output) = if let NodeKind::HumanReview { options } = &node_kind {
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
                if review_requested_count(&events, &current) <= decided {
                    log.append(EventPayload::ReviewRequested {
                        node: current.clone(),
                        options: options.clone(),
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
            let start_ts =
                last_wait_started_ts(&read_all(run_dir)?, &current).unwrap_or_else(now_millis);
            let elapsed = now_millis().saturating_sub(start_ts);
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
                parallel::join_readiness(&playbook, &current, &state),
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
        } else {
            steps += 1;
            log.append(EventPayload::NodeStarted {
                node: current.clone(),
                attempt: 1,
            })?;
            let override_prompt = prompt_overrides.remove(&current);
            let (st, out, evs) = execute_node(
                &playbook,
                run_dir,
                &workdir,
                &current,
                &run_id,
                &state,
                cfg,
                override_prompt,
                &AtomicBool::new(false),
            )?;
            for ev in evs {
                log.append(ev)?;
            }
            (st, out)
        };
        log.append(EventPayload::NodeFinished {
            node: current.clone(),
            status: status.as_str().into(),
            attempt: 1,
            output: output.clone(),
        })?;

        rebuild_context_md(run_dir)?;

        // Attribute any progress the agent posted while `current` was executing
        // to `current`, before the frontier advances to the successor (B2).
        control_cursor = drain_progress_after_execute(run_dir, log, control_cursor, &current)?;

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

        // Supervisor mode: a failed/timed-out node raises wake and waits
        // for a supervisor command instead of autonomously taking a fallback.
        // Autonomous mode skips this block - the branch below (next_node) is untouched.
        if mode == RunMode::Supervised
            && matches!(status, NodeStatus::Failed | NodeStatus::TimedOut)
        {
            let trigger = if status == NodeStatus::TimedOut {
                WakeTrigger::NodeTimeout
            } else {
                WakeTrigger::NodeFailed
            };
            log.append(EventPayload::WakeRaised {
                trigger,
                node: current.clone(),
                detail: output.clone(),
            })?;

            loop {
                let (cmd, seq) = await_control(run_dir, log, control_cursor, &current)?;
                control_cursor = Some(seq);
                match cmd {
                    // await_control applies ContextAppend and Progress in-place
                    // and never returns them (see its implementation above) - these
                    // branches are defensive, in case of future refactoring: we do
                    // not fail the run, just wait for the next command on the same node.
                    Control::ContextAppend { .. } => continue,
                    Control::Progress { .. } => continue,
                    Control::Retry {
                        node,
                        prompt_override,
                    } => {
                        log.append(EventPayload::SupervisorAction {
                            action: "node_retry".into(),
                            node: Some(node.clone()),
                            detail: prompt_override.clone().unwrap_or_default(),
                        })?;
                        if let Some(p) = prompt_override {
                            prompt_overrides.insert(node.clone(), p);
                        }
                        current = node;
                        break;
                    }
                    Control::ContinueFrom { node } => {
                        log.append(EventPayload::SupervisorAction {
                            action: "run_continue_from".into(),
                            node: Some(node.clone()),
                            detail: String::new(),
                        })?;
                        current = node;
                        break;
                    }
                    Control::Patch {
                        version,
                        classification,
                        continue_from,
                    } => {
                        match apply_patch(
                            root,
                            run_dir,
                            log,
                            cfg,
                            &mut playbook,
                            &mut current,
                            PatchCommand {
                                version,
                                classification,
                                continue_from,
                            },
                        )? {
                            PatchResult::Applied(applied) => {
                                last_applied_patch = Some(*applied);
                                frontier.clear();
                                break;
                            }
                            PatchResult::Rejected => continue,
                            PatchResult::Paused => {
                                return Ok(RunResult {
                                    run_id,
                                    outcome: RunStatus::Paused,
                                });
                            }
                        }
                    }
                    Control::Pause => {
                        log.append(EventPayload::RunPaused {
                            reason: "supervisor pause".into(),
                        })?;
                        return Ok(RunResult {
                            run_id,
                            outcome: RunStatus::Paused,
                        });
                    }
                    Control::Abort { reason } => {
                        log.append(EventPayload::RunAborted { reason })?;
                        return Ok(RunResult {
                            run_id,
                            outcome: RunStatus::Aborted,
                        });
                    }
                }
            }
            continue;
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
        advance_frontier(&playbook, &current, &state_now, &mut frontier, log)?;
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

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub playbook: String,
    pub status: String,
    pub started_ts: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<crate::progress::ProgressSummary>,
}

pub fn list_runs(root: &Path) -> Result<Vec<RunSummary>, EngineError> {
    let runs_dir = root.join(".apb/runs");
    let mut out = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&runs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        // One corrupted/legacy run (for example, events.jsonl with old schema,
        // where `ts` was a string or a truncated record) should not crash
        // the entire listing - skip such a directory and show the rest.
        let events = match read_all(&entry.path()) {
            Ok(events) => events,
            Err(_) => continue,
        };
        if events.is_empty() {
            continue;
        }
        let state = RunState::fold(&events);
        let (playbook, started_ts) = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunStarted { playbook, .. } => Some((playbook.clone(), e.ts)),
                _ => None,
            })
            .unwrap_or_else(|| (run_id.clone(), 0));
        let progress = crate::progress::from_run_dir(&entry.path(), &events);
        out.push(RunSummary {
            run_id,
            playbook,
            status: state.run_status.as_str().into(),
            started_ts,
            progress,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.started_ts));
    Ok(out)
}

/// Verifies agent binary fingerprints from the manifest against current ones (spec 3.6). On
/// mismatch - an `environment drift` error unless `allow` permits
/// continuing (then the fact is written as an event). Without manifest (executor-path) -
/// no-op.
fn check_environment_drift(run_dir: &Path, allow: bool) -> Result<Vec<EventPayload>, EngineError> {
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

    let state = RunState::fold(&read_all(&run_dir)?);
    let start_node = match from_node {
        Some(n) => n.to_string(),
        None => state
            .last_node
            .clone()
            .ok_or_else(|| EngineError::Invalid("nothing to resume from".into()))?,
    };
    log.append(EventPayload::RunPaused {
        reason: format!("resume from `{start_node}`"),
    })?;
    let is_write = playbook
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }));
    let _guard = if is_write {
        acquire(root, false)?
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
        start_node,
        run_id.to_string(),
        RunMode::Autonomous,
        supervisor_expected,
    )
}
