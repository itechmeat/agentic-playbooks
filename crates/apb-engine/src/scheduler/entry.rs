//! Every way a run can be started or handed off: foreground, background,
//! supervised, detached, or driven from an already-prepared run directory,
//! plus cancellation and the supervisor command channel.
//!
//! These are the crate's entry points; the loop they hand control to lives in
//! the parent module. Shares the parent module's imports via `use super::*`.

use super::*;

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
    /// Predecessor run id when this run continues an existing run as a new id.
    pub continued_from: Option<String>,
    /// Sub-playbook nesting depth of THIS run (0 for a top-level run).
    pub depth: usize,
    /// Verified sub-playbook pins from the gate, keyed by playbook-node id.
    pub expected_children: Option<BTreeMap<String, crate::run_config::ChildExpectation>>,
    /// Node-cache policy for the run (spec 2026-07-19). `Auto` by default via
    /// `RunConfig`; the CLI maps `--no-cache`/`--refresh-cache` onto it.
    pub cache: CacheRunMode,
    /// How many ready branches this run may execute at the same time (spec
    /// 2026-08-05 section 1.3). Copied verbatim into `RunConfig`, which is what
    /// a detached driver reads back; `Playbook.defaults.max_parallel` still wins
    /// over it, and with neither set the engine uses
    /// [`crate::scheduler::DEFAULT_MAX_PARALLEL`].
    pub max_parallel: Option<usize>,
    /// How long this start may wait for the shared workdir lock when another
    /// write-run already holds it.
    ///
    /// `None` (the default) is the historical behavior: the start fails at
    /// once with `WorkdirBusy` and the caller is left holding the request.
    /// With a duration the start is ADMITTED instead - the run directory, the
    /// playbook snapshot and the run parameters are written and the run id is
    /// returned - and the wait for the lock happens in the drive phase, off
    /// the caller's thread. That is the difference between an event source
    /// being told "busy, your event is your problem" and its event being
    /// persisted as a run that starts when the engine is free.
    pub workdir_queue_wait: Option<Duration>,
}

/// The result of the run's shared preparation (steps 1-5 of phase-3): the registry
/// opened, the playbook loaded and validated, run_dir created, the snapshot and
/// scripts in place, RunStarted recorded. Used by both `run` (synchronously) and
/// `run_background` (preparation is synchronous, `drive` goes onto a separate thread).
pub(crate) struct Prepared {
    pub(crate) playbook: Playbook,
    pub(crate) run_id: String,
    pub(crate) run_dir: std::path::PathBuf,
    pub(crate) log: EventLog,
    pub(crate) cfg: RunConfig,
    // Kept alive to hold the workdir lock until `Prepared` is dropped (at the
    // end of `run`, or at the end of the `run_background` background thread).
    // `PreparedRun::hand_over_workdir_lock` is the one place that reads it: a
    // run handed to a detached driver passes the lock across by pid instead of
    // releasing it.
    pub(crate) guard: Option<crate::workdir::WorkdirGuard>,
    /// Set when preparation deliberately did NOT take a lock the run needs,
    /// because another write-run held it and the caller asked for the start to
    /// be queued rather than refused. `guard` is `None` in that case for a
    /// completely different reason than "this run writes nothing", which is
    /// why the two cannot be collapsed into one field: whoever drives the run
    /// has to claim the lock before the first node, and has this long to do it.
    pub(crate) queued_workdir: Option<Duration>,
    pub(crate) start_node: String,
    pub(crate) mode: RunMode,
    pub(crate) supervisor_expected: bool,
}

impl Prepared {
    /// Claims the workdir lock that preparation left untaken, waiting out the
    /// run that holds it. A no-op for a preparation that already holds the lock
    /// or needs none, so every drive path can call it unconditionally.
    ///
    /// A give-up is journaled before it is returned: the run directory and its
    /// `run_started` already exist, so a queue that runs out has to close the
    /// run out the same way any other start-time refusal does - otherwise the
    /// admitted event would sit in a run that reads as `running` forever.
    fn claim_queued_workdir(&mut self, root: &Path) -> Result<(), EngineError> {
        let Some(wait) = self.queued_workdir.take() else {
            return Ok(());
        };
        let run_dir = self.run_dir.clone();
        let claimed = acquire_queued(root, wait, &mut || {
            // A stop posted while the run sits in the queue must not have to
            // wait for the workdir to free before it takes effect.
            matches!(crate::control::pending_stop_seq(&run_dir), Ok(Some(_)))
        });
        match claimed {
            Ok(guard) => {
                self.guard = guard;
                Ok(())
            }
            Err(e) => {
                self.close_out_unclaimed(&run_dir, &e);
                Err(e)
            }
        }
    }

    /// Writes the outcome of a run that was admitted and never got the
    /// workdir. Two shapes, because the two give-ups are different events: a
    /// stopped run is `run_aborted` (somebody asked for it), a ceiling that
    /// passed is `run_error` plus `run_finished` (the engine gave up).
    ///
    /// Nothing is written over a journal that already has an outcome.
    /// `stop_run` finalizes a run nothing is driving, which is exactly what a
    /// queued run looks like from outside, so the stop that ended this wait
    /// may well have recorded the abort already - and a second outcome would
    /// rewrite what the run did.
    fn close_out_unclaimed(&mut self, run_dir: &Path, e: &EngineError) {
        let already_decided = read_all(run_dir)
            .map(|events| RunState::fold(&events).run_status.is_terminal())
            .unwrap_or(false);
        if already_decided {
            return;
        }
        if matches!(crate::control::pending_stop_seq(run_dir), Ok(Some(_))) {
            let _ = self.log.append(EventPayload::RunAborted {
                reason: e.to_string(),
            });
            return;
        }
        let _ = self.log.append(EventPayload::RunError {
            node: None,
            reason: e.to_string(),
        });
        let _ = self.log.append(EventPayload::RunFinished {
            outcome: "failed".into(),
        });
    }
}

pub fn run(
    root: &Path,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<RunResult, EngineError> {
    let mut p = prepare_run(root, id, version, opts)?;
    p.claim_queued_workdir(root)?;
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
    p.claim_queued_workdir(&resolved.execution_root)?;
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

pub(crate) fn prepare_supervised_background_target(
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

/// Drives a prepared run until a terminal state. `drive` itself now converts
/// every internal error into a logged `RunError` + `run_finished(failed)`
/// (issue #42 finding 3), so it no longer returns `Err` in practice; the
/// fallback below is kept as a defensive backstop in case a future change to
/// `drive` reopens an `Err` path - without it, such a run would stay stuck in
/// `Running` forever for an external observer.
pub fn drive_prepared(root: &Path, prepared: PreparedRun) -> Result<RunResult, EngineError> {
    let mut p = prepared.0;
    // A run admitted into the workdir queue waits HERE, on the background
    // thread, not on the caller's - the whole point of admitting it was that
    // the caller (a webhook bridge, the dashboard) already has its run id and
    // is gone. `claim_queued_workdir` journals its own failure.
    p.claim_queued_workdir(root)?;
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
    //
    // A run admitted into the workdir queue stacks a second wait on top of the
    // handover race: its parent never held the lock, so what this poll is
    // really waiting out is the unrelated write-run that made the run queue.
    // The persisted ceiling covers both; without it a queued detached run
    // would give up after the five seconds meant for a sub-millisecond
    // handover.
    let is_write = playbook.nodes.iter().any(|n| n.kind.takes_workdir_lock());
    let _guard = if is_write {
        let wait = cfg
            .workdir_queue_wait_ms
            .map(Duration::from_millis)
            .unwrap_or_default();
        acquire_handover_within(root, wait)?
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
    // Same defensive backstop as `drive_prepared`: `drive` no longer returns
    // `Err` in practice (issue #42 finding 3), but without this an internal
    // error with no terminal record would leave the run `running` forever for
    // any external observer.
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
pub(crate) fn hand_to_detached_driver(
    root: &Path,
    prepared: PreparedRun,
) -> Result<String, EngineError> {
    let run_id = prepared.run_id().to_string();
    let pid = match crate::driver::spawn_detached_driver(root, &run_id, None, false, false) {
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

/// Closes every open attempt whose process is provably gone, as a drive starts
/// over an existing run dir (spec 2026-08-05 section 2.4, issue #71 item 4).
/// Returns the nodes it reaped, in journal order.
///
/// The gap this fills: an attempt whose DRIVER died leaves an `attempt_started`
/// that nothing will ever close, because the only process that could write the
/// matching `attempt_finished` is gone. `liveness::lost_nodes` could see it, but
/// only as a read-time report - so the run read as forever in-flight, and
/// recovering the node needed a human to notice and rerun. Writing the
/// process-table judgment into the journal once, here, makes it a replayable
/// fact and lets the node re-enter scheduling like any other unfinished work.
///
/// Why this is the whole implementation, with no second frontier
/// reconstruction: the node's own re-entry is already covered twice over.
/// `plan_resume` treats a node whose journal ends mid-attempt as interrupted
/// work and restarts it (its filter is `Running | Interrupted`, and closing the
/// attempt moves the fold from the second to the first - the same decision), and
/// a node reached on some OTHER branch comes back through
/// `node::restore_frontier`, whose `pending_heads` reconstruction keys on
/// non-terminal status, which an `interrupted` closure does not change. So the
/// reap adds the missing journal record and changes no routing.
///
/// Boundaries, all of them deliberate:
///
///   * only a PROVABLY dead pid is reaped ([`crate::liveness::dead_open_attempts`]
///     carries the module's bias): an attempt with no journaled pid stays open,
///     because unknown is not dead, and a live pid is left to the driver claim
///     and the workdir lock, which are what decide run ownership;
///   * this runs at drive ENTRY only. A live drive waits on its own child and
///     cannot miss its exit, so there is no watchdog and no mid-drive reaping;
///   * a driverless run is not reaped until someone drives it again. Autonomous
///     reaping of an abandoned run is out of scope (an external watchdog), and
///     `apb doctor --run` plus `run_status` remain how such a run is seen.
///
/// `duration_ms` is `None` rather than the attempt's age: the journal records
/// when the attempt STARTED, and nothing on disk says when its process died, so
/// any number here would be invented. `partial_output` is `None` for the same
/// honest reason - the mid-work text died with the process and was never
/// journaled.
pub(super) fn reap_dead_attempts(
    events: &[Event],
    log: &mut EventLog,
) -> Result<Vec<String>, EngineError> {
    let dead = crate::liveness::dead_open_attempts(events);
    if dead.is_empty() {
        return Ok(Vec::new());
    }
    let journal = Journal::new(log);
    let mut reaped = Vec::with_capacity(dead.len());
    for a in dead {
        journal_interrupted_attempt(&journal, &a.node, a.attempt, None, None, "", None)?;
        reaped.push(a.node);
    }
    Ok(reaped)
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
