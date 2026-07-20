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
use crate::control::{Control, read_control_after, read_control_cursor, write_control_cursor};
use crate::error::EngineError;
use crate::event::{
    Event, EventLog, EventPayload, ProfileProvenance, WakeTrigger, now_millis, read_all,
};
use crate::inspect::{should_declare_lost, supervisor_silence_ms, write_supervisor_session};
use crate::manifest::{ManifestProfile, ManifestSkill, RunExecutionManifest};
use crate::parallel::{self, JoinReadiness};
use crate::question::{post_question, read_answers_after, read_questions_after};
use crate::review::read_reviews_after;
use crate::run_config::{
    CacheRunMode, RunConfig, copy_scripts, read_run_config, snapshot_playbook, write_run_config,
};
use crate::script::run_script;
use crate::signals::read_signals_after;
use crate::state::{NodeStatus, RunState, RunStatus};
use crate::workdir::{acquire, acquire_handover};

/// Run mode: autonomous (as in phases 1-3, behavior unchanged) or supervised
/// (the engine stops on a wake event and waits for a command). Defined in
/// `run_config` because it is persisted with the run, re-exported here where
/// every caller expects to find it.
pub use crate::run_config::RunMode;

mod cache;
mod node;
mod patch;
mod prepare;
mod resume;
mod supervisor;

use node::*;
use patch::*;
use prepare::*;
pub use resume::{ResumeDecision, ResumeReason, StartMode, plan_resume};
pub use supervisor::spawn_supervisor_agent;
use supervisor::*;

/// A shared append handle over the run's single event log, used only for the
/// two attempt-lifecycle events that are journaled off the return batch:
/// `attempt_started` at spawn time (so a crash mid-attempt leaves an open
/// attempt on disk) and `attempt_finished` at return time. Every OTHER event
/// keeps drive's direct, return-batch write path.
///
/// The log is wrapped in a `Mutex` so the same handle serves both drive paths:
/// the sequential path holds the sole reference, while the parallel batch path
/// shares `&Journal` across scoped worker threads. Each append is a single
/// atomic line write, so lock contention is irrelevant.
pub(crate) struct Journal<'a> {
    log: std::sync::Mutex<&'a mut EventLog>,
}

impl<'a> Journal<'a> {
    pub(crate) fn new(log: &'a mut EventLog) -> Self {
        Self {
            log: std::sync::Mutex::new(log),
        }
    }

    /// Appends one event under the lock. Poison-tolerant: a worker thread that
    /// panicked mid-append would poison the mutex, but the log file itself is
    /// append-only and a partial line is impossible (a single `writeln!`), so
    /// recovering the inner guard and continuing is safe.
    pub(crate) fn append(&self, payload: EventPayload) -> Result<(), EngineError> {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .append(payload)?;
        Ok(())
    }
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
    /// Expected connector tree digests `name -> tree digest`, captured by the
    /// policy gate (spec 6). Verified verbatim against the live resolution at
    /// run start; a playbook that binds connectors with an empty map is refused
    /// (the gate result must always be passed). Mirrors the profile-bundle pin.
    pub expected_connectors: BTreeMap<String, String>,
    /// Expected connector account digests `"connector/account" -> account
    /// digest`, captured by the policy gate (spec 6). Verified verbatim at run
    /// start alongside `expected_connectors`.
    pub expected_connector_accounts: BTreeMap<String, String>,
    /// Parent run id when this run is a sub-playbook child (spec C).
    pub parent_run: Option<String>,
    /// Sub-playbook nesting depth of THIS run (0 for a top-level run).
    pub depth: usize,
    /// Verified sub-playbook pins from the gate, keyed by playbook-node id.
    pub expected_children: Option<BTreeMap<String, crate::run_config::ChildExpectation>>,
    /// Node-cache policy for the run (spec 2026-07-19). `Auto` by default via
    /// `RunConfig`; the CLI maps `--no-cache`/`--refresh-cache` onto it.
    pub cache: CacheRunMode,
}

/// Defense-in-depth backstop for sub-playbook nesting (spec C). A child that
/// would exceed this depth fails its parent node.
pub const MAX_SUBPLAYBOOK_DEPTH: usize = 5;

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

/// How many `QuestionAsked` events a node already carries. Mirrors
/// `review_requested_count`: drive declares a question once per suspension and
/// this count keeps the declaration loop-resilient (spec 2026-07-20).
fn question_asked_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::QuestionAsked { node: n, .. } if n == node))
        .count()
}

/// How many `QuestionAnswered` events a node already carries. Mirrors
/// `review_decided_count`: the N-th answer for a node is consumed once there
/// are already N `QuestionAnswered` events for it (count-based consumption).
fn question_answered_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(&e.payload, EventPayload::QuestionAnswered { node: n, .. } if n == node),
        )
        .count()
}

/// How many `AttemptStarted` events a node already carries - the number of the
/// current attempt while one is open, used to tag a posted question.
fn attempt_started_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::AttemptStarted { node: n, .. } if n == node))
        .count()
}

/// How many `NodeStarted` / `NodeFinished` events a node already carries. A
/// node visit is "open" when started exceeds finished; the interactive branch
/// uses this to journal `NodeStarted` exactly once per visit (like the wait
/// branch's `wait_started_count`/`wait_ended_count` guard), so re-invocations
/// and park spins within the same visit do not re-declare it.
fn node_started_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node))
        .count()
}

fn node_finished_count(events: &[Event], node: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::NodeFinished { node: n, .. } if n == node))
        .count()
}

/// Whether `questions.jsonl` already carries an unanswered question for this
/// node posted under `attempt`. Guards against a duplicate channel append when
/// drive crashed after `post_question` but before journaling `QuestionAsked`:
/// on resume the journal-count declare-once guard would re-post, but the
/// channel entry is already there. Unanswered questions are the tail beyond the
/// answered count (count-based consumption), so we only inspect those.
fn channel_has_unanswered_question(
    run_dir: &Path,
    node: &str,
    attempt: u32,
) -> Result<bool, EngineError> {
    let questions: Vec<_> = read_questions_after(run_dir, None)?
        .into_iter()
        .filter(|q| q.node == node)
        .collect();
    let answered = read_answers_after(run_dir, None)?
        .into_iter()
        .filter(|a| a.node == node)
        .count();
    Ok(questions
        .iter()
        .skip(answered)
        .any(|q| q.attempt == attempt))
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
    // Kept alive to hold the workdir lock until `Prepared` is dropped (at the
    // end of `run`, or at the end of the `run_background` background thread).
    // `PreparedRun::hand_over_workdir_lock` is the one place that reads it: a
    // run handed to a detached driver passes the lock across by pid instead of
    // releasing it.
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
        StartMode::Rerun,
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
        StartMode::Rerun,
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

    /// Passes the workdir lock this preparation holds (if it took one) to
    /// process `pid`, then lets the preparation go. Used when the run is handed
    /// to a detached driver: the lock moves straight from the preparing process
    /// to the driver process with no window in between, and the driver adopts
    /// it via `workdir::acquire_handover`.
    pub fn hand_over_workdir_lock(self, pid: u32) -> Result<(), EngineError> {
        let mut p = self.0;
        if let Some(guard) = p.guard.take() {
            guard.hand_over(pid)?;
        }
        Ok(())
    }

    /// Marks a prepared run as failed without ever driving it. The run dir and
    /// its `run_started` event already exist at this point, so a preparation
    /// that cannot be handed to a driver must be closed out - otherwise the run
    /// would read as forever `running` to `apb runs`, the dashboard and the
    /// supervisor tools.
    fn abandon(self) {
        let mut p = self.0;
        let _ = p.log.append(EventPayload::RunFinished {
            outcome: "failed".into(),
        });
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
        StartMode::Rerun,
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

/// Re-opens a run that another process prepared but never drove, and drives it
/// to a terminal state. This is the body of the detached driver child
/// (`apb __drive-run`): preparation - the policy gate, the permit, the
/// immutable manifest snapshot - all happened in the parent, and everything
/// this side needs is already in `runs/<id>` (the playbook snapshot, the run
/// config, the manifest, the journal). Nothing is re-resolved from live
/// profile or skill files, so the anti-TOCTOU posture is exactly the one the
/// parent's permit established.
///
/// Refuses a run that has already been driven: replaying nodes against a
/// journal that has moved on is not a resume, and `resume` is the supported
/// way back into a run that already ran.
pub fn drive_run_from_dir(root: &Path, run_id: &str) -> Result<RunResult, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }

    let events = read_all(&run_dir)?;
    let state = RunState::fold(&events);
    if !state.nodes.is_empty() {
        return Err(EngineError::Invalid(format!(
            "run `{run_id}` has already been driven - use resume to continue it"
        )));
    }

    // The snapshot parser rather than `Playbook::from_yaml`: a run dir is a
    // read-only historical record, and the shared parser is the one that
    // tolerates every snapshot shape the engine has written (see `resume`).
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml"))?;
    let playbook = crate::legacy_snapshot::parse_snapshot_playbook(&yaml)?;
    let cfg = read_run_config(&run_dir)?;
    let mut log = EventLog::open(&run_dir)?;

    let start_node = playbook
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Start))
        .ok_or_else(|| EngineError::Invalid("no start node".into()))?
        .id
        .clone();

    // Mirrors prepare's predicate. The parent held this lock through
    // preparation and handed it to us by pid, so we adopt rather than acquire
    // (a plain acquire would see our own live pid and call the workdir busy).
    let is_write = playbook.nodes.iter().any(|n| n.kind.takes_workdir_lock());
    let _guard = if is_write {
        acquire_handover(root)?
    } else {
        None
    };

    let res = drive(
        playbook,
        &run_dir,
        root,
        &mut log,
        &cfg,
        start_node,
        StartMode::Rerun,
        run_id.to_string(),
        cfg.mode,
        cfg.supervisor_expected,
    );
    // Same fallback as `drive_prepared`: an internal error with no terminal
    // record would leave the run `running` forever for any external observer.
    if res.is_err() {
        let _ = log.append(EventPayload::RunFinished {
            outcome: "failed".into(),
        });
    }
    res
}

/// Prepares a run and hands it to a DETACHED driver process, returning the
/// run_id as soon as the child is spawned. Unlike `run_background`, whose
/// drive thread dies with the calling process, the run started here survives
/// its launcher - which is what `apb mcp` needs, since its process is bound to
/// a chat session that can be killed at any moment.
pub fn start_detached(
    root: &Path,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<String, EngineError> {
    let prepared = prepare_supervised_background(root, id, version, opts)?;
    hand_to_detached_driver(root, prepared)
}

/// `start_detached` for an already-resolved playbook (spec 3): the definition
/// may live in the global store while execution happens in the project root.
pub fn start_detached_resolved(
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
    hand_to_detached_driver(&resolved.execution_root, prepared)
}

/// Spawns the driver child for an already prepared run and moves the workdir
/// lock across to it. The order matters: the child is spawned first (so we
/// know its pid), and the lock is rewritten immediately afterwards while we
/// still own it - `acquire_handover` on the child side covers the case where
/// the child looks at the lock before that write lands.
fn hand_to_detached_driver(root: &Path, prepared: PreparedRun) -> Result<String, EngineError> {
    let run_id = prepared.run_id().to_string();
    let pid = match crate::driver::spawn_detached_driver(root, &run_id, None, false) {
        Ok(pid) => pid,
        Err(e) => {
            // No driver was started, so nothing will ever move this run. Close
            // it out rather than leaving it stuck in `running`.
            prepared.abandon();
            return Err(EngineError::Invalid(format!(
                "cannot start the detached run driver: {e}"
            )));
        }
    };
    // Publish the driver BEFORE returning, for the same reason the lock is
    // handed over by pid here: until `driver.pid` names the child, this run
    // reads as having no driver at all, and a stop landing in that window
    // finalizes a run that is about to execute (see `publish_driver_pid`).
    crate::driver::publish_driver_pid(&root.join(".apb/runs").join(&run_id), pid);
    // Best effort, and deliberately not fatal: the child is already running, so
    // failing the call here would hide a run that is genuinely under way. If
    // the rewrite fails, our guard releases the lock as it drops instead, and
    // the child's `acquire_handover` simply takes a free lock - a hair's
    // breadth of a window rather than a lost run.
    let _ = prepared.hand_over_workdir_lock(pid);
    Ok(run_id)
}

/// Resumes a run in a DETACHED driver process and returns that process's pid
/// immediately. The caller is expected to have already computed the resume
/// decision (`plan_resume`) for its acknowledgement: the child re-derives the
/// same decision from the same journal.
pub fn resume_detached(
    root: &Path,
    run_id: &str,
    from_node: Option<&str>,
) -> Result<u32, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    if !root.join(".apb/runs").join(run_id).is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let pid = crate::driver::spawn_detached_driver(root, run_id, from_node, true)
        .map_err(|e| EngineError::Invalid(format!("cannot start the detached run driver: {e}")))?;
    // As in `hand_to_detached_driver`: name the driver before returning, so a
    // stop issued the moment this call comes back sees a live driver instead of
    // finalizing a run the child is about to drive.
    crate::driver::publish_driver_pid(&root.join(".apb/runs").join(run_id), pid);
    Ok(pid)
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
    // Propagate the abort into any non-terminal sub-playbook children (spec C):
    // an operator abort of the parent must reach a child that is blocking the
    // parent (e.g. a child paused on human_review).
    crate::stop::abort_children(root, run_id)?;
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
    start_mode: StartMode,
    run_id: String,
    mode: RunMode,
    supervisor_expected: bool,
) -> Result<RunResult, EngineError> {
    // Publish which OS process is driving this run, for as long as the drive
    // lasts (Task 7). Every drive invocation takes one - the CLI's synchronous
    // run, the in-process background thread, and the detached driver child
    // alike - and the guard removes the file on every exit path, so
    // `driver.pid` present means "a process claims to be driving this run".
    let _driver_pid = crate::driver::DriverPidGuard::claim(run_dir);
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
    let mut current = match start_mode {
        // Restart interrupted work, or an explicit `--from-node` re-run: the
        // start node is executed by the loop below.
        StartMode::Rerun => start_node,
        // Advance past an already-finished node without re-executing it: seed
        // the frontier by evaluating its outgoing edges against the folded
        // status and outputs (exactly the normal post-node advancement), then
        // start from the first ready successor.
        StartMode::After => {
            let state = RunState::fold(&read_all(run_dir)?);
            advance_frontier(&playbook, &start_node, &state, &mut frontier, log)?;
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
    let run_cancel = Arc::new(AtomicBool::new(false));
    let _abort_watcher =
        crate::stop::AbortWatcher::spawn(run_dir, control_cursor, Arc::clone(&run_cancel));
    // True once the loop has already taken the cancel short-circuit below, so
    // a pathological case (an unconsumable Retry queued ahead of the Abort
    // stops the top-of-loop scan before it reaches it) degrades to the old
    // behavior instead of spinning.
    let mut cancel_short_circuited = false;
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
        // Set to the node named by a Retry/ContinueFrom the scan stopped at
        // without consuming it: everything queued BEHIND that entry - including
        // a pending Abort - is unreachable for this scan. See the cancel check
        // below the loop.
        let mut blocked_by: Option<String> = None;
        let pending_control = read_control_after(run_dir, control_cursor)?;
        for entry in pending_control.iter().cloned() {
            match entry.cmd {
                Control::Abort { reason } => {
                    // Effect first, cursor persisted last: if `log.append` errs
                    // (ordinary I/O failure), the entry must NOT be marked
                    // applied - it has to resurface on the next drive rather
                    // than being silently dropped. Persisted before the return
                    // (once the effect has actually happened) so a resumed
                    // drive never sees this same terminal entry again (Task 4
                    // completion-plan defect 1 - a stale stop command re-firing
                    // on resume).
                    log.append(EventPayload::RunAborted { reason })?;
                    write_control_cursor(run_dir, entry.seq)?;
                    return Ok(RunResult {
                        run_id,
                        outcome: RunStatus::Aborted,
                    });
                }
                Control::Pause => {
                    // Same ordering and reasoning as Abort above.
                    log.append(EventPayload::RunPaused {
                        reason: "supervisor pause".into(),
                    })?;
                    write_control_cursor(run_dir, entry.seq)?;
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
                    write_control_cursor(run_dir, entry.seq)?;
                }
                Control::Patch {
                    version,
                    classification,
                    continue_from,
                } => {
                    // Effect first (`apply_patch` can itself err on ordinary
                    // I/O - unreadable events.jsonl, a bad snapshot read), then
                    // persist the cursor only once it has actually returned
                    // Ok: an error here must leave the entry unconsumed so it
                    // resurfaces on the next drive instead of being silently
                    // dropped.
                    let result = apply_patch(
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
                    )?;
                    control_cursor = Some(entry.seq);
                    write_control_cursor(run_dir, entry.seq)?;
                    match result {
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
                    write_control_cursor(run_dir, entry.seq)?;
                }
                Control::Retry { ref node, .. } | Control::ContinueFrom { ref node } => {
                    // Valid only inside await_control, in response to a wake -
                    // we do not advance the cursor, the command remains unconsumed.
                    blocked_by = Some(node.clone());
                    break;
                }
            }
        }

        // A stop that the scan above cannot reach. The watcher reads the raw
        // control file and so DOES see an Abort queued behind an unconsumable
        // Retry/ContinueFrom; the scan stops short of it and never applies it.
        // The flag then stayed latched for the rest of the drive: every later
        // node returned `Cancelled` instantly, `Cancelled` is neither Unknown
        // nor Interrupted, so it fell through to edge selection, matched
        // nothing, and the drive failed with "has no outgoing edge" - which
        // `drive_prepared` stamped as run_finished(failed). An operator who
        // asked for a stop got a FAILED run.
        //
        // The flag being set is proof that an Abort is pending, so finalize as
        // aborted here instead - and consume the abort properly, cursor and
        // all. Skipping the cursor forward past the Retry is what the scalar
        // cursor forces (see `write_control_cursor`), and it is the right
        // trade here: the run is stopping, so a queued Retry has nothing left
        // to retry. What it must not be is silent, hence the
        // `retry_superseded_by_stop` record ahead of the terminal event. NOT
        // advancing would be far worse than losing the Retry: this arm does
        // not consume anything, so every later resume would re-enter it and
        // append another RunAborted, forever.
        //
        // If the Abort is not in `pending_control` (the watcher saw an append
        // that landed after our read), fall through rather than invent a seq:
        // the next iteration re-reads control and this arm fires with the real
        // entry in hand.
        if let Some(blocked_node) = blocked_by.as_ref()
            && run_cancel.load(Ordering::SeqCst)
            && let Some((abort_seq, reason)) = pending_control.iter().find_map(|e| match &e.cmd {
                Control::Abort { reason } => Some((e.seq, reason.clone())),
                _ => None,
            })
        {
            log.append(EventPayload::SupervisorAction {
                action: "retry_superseded_by_stop".into(),
                node: Some(blocked_node.clone()),
                detail: format!(
                    "a pending stop was applied before this command could be consumed, so it was discarded: {reason}"
                ),
            })?;
            log.append(EventPayload::RunAborted { reason })?;
            write_control_cursor(run_dir, abort_seq)?;
            return Ok(RunResult {
                run_id,
                outcome: RunStatus::Aborted,
            });
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
                    log.append(EventPayload::NodeFinished {
                        node: current.clone(),
                        status: st.as_str().into(),
                        attempt: 1,
                        output: out.clone(),
                        artifacts: Vec::new(),
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

        // Concurrent fast path (autonomous only): if, together with current, the
        // frontier has >= 2 ready slow nodes (agent_task/script), execute
        // them CONCURRENTLY on threads. drive remains the sole writer of
        // events: execute_node never touches the log, it returns events instead; drive
        // writes them as threads finish (order = finish order,
        // spec 8.5). Supervised mode never enters here (wake/await is per single
        // node), and neither do human_review/wait/condition (they are not slow and/or they wait).
        // An interactive `current` never enters the concurrent path: it may park
        // on a question, and the concurrent batch (which runs on worker threads
        // that cannot write events or park) has no place to do that. It runs
        // through the sequential park path below instead. Interactive frontier
        // nodes are likewise excluded from any batch and picked up as `current`
        // on a later iteration.
        if mode == RunMode::Autonomous && !is_interactive(&playbook, &current) {
            let mut batch: Vec<String> = Vec::new();
            if is_agent_or_script(&playbook, &current) {
                batch.push(current.clone());
            }
            for n in &frontier {
                if is_agent_or_script(&playbook, n)
                    && !is_interactive(&playbook, n)
                    && !batch.contains(n)
                {
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
                let mut batch_statuses: Vec<NodeStatus> = Vec::new();
                // The batch shares ONE journal (the drive's log behind a Mutex)
                // so each worker thread appends its own attempt_started at spawn
                // time and attempt_finished at return, while the collector on this
                // thread appends the returned RetryStarted/FallbackTriggered and
                // the per-node NodeFinished through the same journal. Scoped
                // threads let the workers borrow `&Journal` without a 'static
                // bound; the block scopes the borrow so `log` is free again for
                // the frontier writes below. Each append is one atomic line
                // write, so the shared lock is uncontended in practice.
                {
                    let journal = Journal::new(&mut *log);
                    let (tx, rx) = mpsc::channel();
                    std::thread::scope(|scope| -> Result<(), EngineError> {
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
                            journal.append(EventPayload::NodeFinished {
                                node: node.clone(),
                                status: status.as_str().into(),
                                attempt: 1,
                                output,
                                // The concurrent batch path does not run through the
                                // node cache, so it captures no declared artifacts.
                                artifacts: Vec::new(),
                            })?;
                            batch_statuses.push(status);
                            // If this branch successfully fed a join:any - cancel the others.
                            if status == NodeStatus::Succeeded {
                                let state_peek = RunState::fold(&read_all(run_dir)?);
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
                                                        &state_peek
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
        //
        // Declared artifacts to record on the shared `NodeFinished` below. Only
        // the node-cache branch sets it (captured on a store, replayed on a
        // hit); every other branch leaves it empty.
        let mut node_artifacts: Vec<apb_core::cache::ArtifactRef> = Vec::new();
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
            let events = read_all(run_dir)?;
            let answered = question_answered_count(&events, &current);
            // A pending (asked-but-unanswered) question means we are parked.
            if question_asked_count(&events, &current) > answered {
                let for_node: Vec<_> = read_answers_after(run_dir, None)?
                    .into_iter()
                    .filter(|a| a.node == current)
                    .collect();
                match for_node.into_iter().nth(answered) {
                    Some(entry) => {
                        // The next answer arrived: consume it (count-based) and
                        // re-invoke with the answer appended (Task 6/7 refine
                        // into the full transcript / session resume).
                        log.append(EventPayload::QuestionAnswered {
                            node: current.clone(),
                            answer: entry.answer.clone(),
                            answered_by: entry.answered_by.clone(),
                        })?;
                        let node_prompt = match &node_kind {
                            NodeKind::AgentTask { prompt, .. } => prompt.clone(),
                            _ => String::new(),
                        };
                        let base = render_node_prompt(run_dir, &run_id, &state, cfg, &node_prompt)?;
                        prompt_overrides.insert(
                            current.clone(),
                            format!("{base}\n\n## prior answer\n{}", entry.answer),
                        );
                        // Fall through to run the next attempt below.
                    }
                    None => {
                        // Still waiting: bounce to the top-of-loop control scan
                        // so control commands keep being applied, then poll again.
                        std::thread::sleep(AWAIT_CONTROL_POLL);
                        continue;
                    }
                }
            }
            // Run one agent attempt. NodeStarted is journaled once per node
            // visit (guarded by started-vs-finished counts, like the wait
            // branch): the first attempt of a visit emits it; a re-invocation
            // after an answer does not (the visit is still open).
            if node_started_count(&events, &current) <= node_finished_count(&events, &current) {
                steps += 1;
                log.append(EventPayload::NodeStarted {
                    node: current.clone(),
                    attempt: 1,
                })?;
            }
            let override_prompt = prompt_overrides.remove(&current);
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
                    // Fall to the shared tail (NodeFinished + frontier advance).
                    (status, output)
                }
                AttemptOutcome::Suspended { question, options } => {
                    // Declare the question once per suspension (journal-count
                    // based, so it survives the loop bounce), raise a wake so a
                    // waiting supervisor returns (Task 8), then bounce to the
                    // top-of-loop scan; the next iteration parks.
                    let events = read_all(run_dir)?;
                    if question_asked_count(&events, &current)
                        <= question_answered_count(&events, &current)
                    {
                        let attempt = attempt_started_count(&events, &current) as u32;
                        // Idempotent channel post: skip if a crash between a
                        // prior post_question and its QuestionAsked already left
                        // this exact question in questions.jsonl.
                        if !channel_has_unanswered_question(run_dir, &current, attempt)? {
                            post_question(run_dir, &current, attempt, &question, options.clone())?;
                        }
                        log.append(EventPayload::QuestionAsked {
                            node: current.clone(),
                            question,
                            options,
                        })?;
                        log.append(EventPayload::WakeRaised {
                            trigger: WakeTrigger::Anomaly,
                            node: current.clone(),
                            detail: "interactive question".into(),
                        })?;
                    }
                    continue;
                }
            }
        } else {
            steps += 1;
            log.append(EventPayload::NodeStarted {
                node: current.clone(),
                attempt: 1,
            })?;
            // Node cache (spec 2026-07-19). `prepare` returns `None` for any
            // non-cacheable node, in which case this collapses to the plain
            // execute path. On a hit we skip execution entirely; on a miss we
            // execute and then let `admit` decide whether the result is stored.
            //
            // Agent-task key parts come from the run's own immutable manifest
            // snapshot (bundle digest + primary agent/model + the node's
            // connector digests), and the prompt is rendered via the same
            // shared helper `execute_node` uses so the key can never drift from
            // what the agent receives. A script node leaves all of these empty
            // (Task 5 behavior, unchanged).
            let mut rendered_prompt: Option<String> = None;
            let mut bundle_digest: Option<String> = None;
            let mut agent_model: Option<(String, String)> = None;
            let mut connector_digests: Vec<String> = Vec::new();
            if let NodeKind::AgentTask { prompt, .. } = &node_kind
                && let Some((bundle, agent, model, digests)) =
                    cache::agent_key_parts(run_dir, &current)
            {
                rendered_prompt = Some(render_node_prompt(run_dir, &run_id, &state, cfg, prompt)?);
                bundle_digest = Some(bundle);
                agent_model = Some((agent, model));
                connector_digests = digests;
            }
            let cache_ctx = cache::prepare(
                &playbook,
                &current,
                &workdir,
                run_dir,
                cfg,
                rendered_prompt.as_deref(),
                bundle_digest.as_deref(),
                agent_model.as_ref().map(|(a, m)| (a.as_str(), m.as_str())),
                connector_digests,
            );
            // A hit is only taken once its declared artifacts are restored to
            // the workspace. If restore fails (a missing or tampered artifact
            // object), we drop the hit entirely and fall through to the miss
            // path, so a failed hit never leaves a `NodeCacheHit` without the
            // files it promised (no partial event pair).
            // A node that already finished once in this run (a loop
            // re-execution) must NOT replay a cached verdict: skip the lookup so
            // each iteration runs the node again. The store side is unchanged -
            // a fresh execution still admits/stores below. Detected from the
            // folded state (`state` predates this iteration's NodeStarted, so a
            // terminal status means a prior NodeFinished for this node).
            let already_finished = state.nodes.get(&current).is_some_and(|st| st.is_finished());
            let lookup = if already_finished {
                None
            } else {
                cache_ctx.as_ref().and_then(|c| c.lookup(cfg))
            };
            let hit = match lookup {
                Some(entry) => {
                    let ctx = cache_ctx.as_ref().expect("a hit implies a cache ctx");
                    match cache::restore_artifacts(&entry, ctx.store(), run_dir, &workdir) {
                        Ok(()) => Some(entry),
                        Err(_) => None,
                    }
                }
                None => None,
            };
            if let Some(entry) = hit {
                let ctx = cache_ctx.as_ref().expect("a hit implies a cache ctx");
                log.append(EventPayload::NodeCacheHit {
                    node: current.clone(),
                    key: ctx.key.clone(),
                    source_run: entry.record.provenance.run_id.clone(),
                })?;
                node_artifacts = entry.record.artifacts.clone();
                (NodeStatus::Succeeded, entry.output)
            } else {
                if let Some(ctx) = &cache_ctx {
                    log.append(EventPayload::NodeCacheMiss {
                        node: current.clone(),
                        key: ctx.key.clone(),
                    })?;
                }
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
                if let Some(ctx) = &cache_ctx
                    && st == NodeStatus::Succeeded
                {
                    // Scan the run log for this node's connector calls (written
                    // out of band by the connector-call subprocess) and verify
                    // each against the read_only set in the run's connector
                    // snapshot. A script node makes none, so this is (true,
                    // false) - identical to Task 5's admission.
                    let (calls_ok, had_calls) = cache::verify_connector_calls(run_dir, &current);
                    let node = playbook.node(&current).expect("a cache ctx implies a node");
                    // Capture the node's declared output artifacts. A capture
                    // error (an unreadable matched file or a path escaping its
                    // scope root) rejects admission outright: storing a record
                    // that references artifacts we could not read would be a
                    // lie. It never fails the run.
                    match cache::capture_artifacts(node, run_dir, &workdir) {
                        Ok(captured) => {
                            node_artifacts = captured.iter().map(|(a, _)| a.clone()).collect();
                            log.append(ctx.admit(
                                &workdir, &run_id, &playbook, &out, calls_ok, had_calls, &captured,
                            ))?;
                        }
                        Err(reason) => {
                            log.append(EventPayload::NodeCacheRejected {
                                node: current.clone(),
                                reason: format!("artifact capture failed: {reason}"),
                            })?;
                        }
                    }
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
        control_cursor = drain_progress_after_execute(run_dir, log, control_cursor, &current)?;

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
                // `await_control` applies ContextAppend/Progress in place (and
                // persists their own cursor internally) before ever returning;
                // whatever it DOES return (Retry/ContinueFrom/Pause/Abort/Patch)
                // is applied right here in each arm below. Cursor persistence
                // happens AFTER the arm's effect (log.append/apply_patch), not
                // before: those calls can themselves err on ordinary I/O
                // conditions, and persisting first would permanently drop the
                // entry (it would never resurface on the next drive) exactly
                // when its effect failed to happen.
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
                        control_cursor = Some(seq);
                        write_control_cursor(run_dir, seq)?;
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
                        control_cursor = Some(seq);
                        write_control_cursor(run_dir, seq)?;
                        current = node;
                        break;
                    }
                    Control::Patch {
                        version,
                        classification,
                        continue_from,
                    } => {
                        let result = apply_patch(
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
                        )?;
                        control_cursor = Some(seq);
                        write_control_cursor(run_dir, seq)?;
                        match result {
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
                        // The function returns right below, so there is no
                        // further loop iteration to read the in-memory
                        // `control_cursor` back - only the persisted file
                        // matters here (read by the next drive's init).
                        write_control_cursor(run_dir, seq)?;
                        return Ok(RunResult {
                            run_id,
                            outcome: RunStatus::Paused,
                        });
                    }
                    Control::Abort { reason } => {
                        log.append(EventPayload::RunAborted { reason })?;
                        write_control_cursor(run_dir, seq)?;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run: Option<String>,
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
        let parent_run = crate::run_config::read_run_config(&entry.path())
            .ok()
            .and_then(|c| c.parent_run);
        out.push(RunSummary {
            run_id,
            playbook,
            status: state.run_status.as_str().into(),
            started_ts,
            progress,
            parent_run,
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
    resume_inner(root, run_id, from_node, allow_environment_drift, false)
}

/// Shared implementation behind `resume`/`resume_with`. `allow_shared_workdir`
/// mirrors `RunOptions::allow_shared_workdir`: a sub-playbook child reattached
/// on resume runs on the parent's drive thread while the parent still holds the
/// PID-keyed workdir lock, so its own resume must skip a second acquire
/// (which would return WorkdirBusy). The public entry points pass `false`.
fn resume_inner(
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
