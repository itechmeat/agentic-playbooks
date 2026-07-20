//! Node execution: rendering, adapter dispatch, skill materialization, and frontier advance.
//! Split out of `scheduler` for navigability; shares the parent module's imports via `use super::*`.

use super::*;

/// Renders a node's prompt template with the full standard context (compaction
/// summary + uncompacted tail if drive recorded ContextCompacted, otherwise the
/// full context), run hooks, params, prior outputs, and reviews. This is the
/// single rendering sequence shared by `execute_node` (the prompt the agent
/// actually receives) and the drive-loop cache-key computation, so the two can
/// never drift: a prompt that changes changes the key. `run_id` comes from the
/// caller rather than being re-derived from the path, matching every other
/// render site.
pub(crate) fn render_node_prompt(
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    prompt: &str,
) -> Result<String, EngineError> {
    let context =
        build_context_for_render(run_dir, &read_all(run_dir)?, cfg.instruction.as_deref())?;
    let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
        .into_iter()
        .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
        .collect();
    Ok(render(
        prompt,
        &cfg.params,
        cfg.instruction.as_deref(),
        &state.outputs,
        &state.reviews,
        &hooks,
        &context,
    ))
}

/// A single execution of a node. Returns (NodeStatus, output, events).
///
/// The two attempt-lifecycle events are journaled directly through `journal`:
/// `attempt_started` at spawn time (so a crash mid-attempt leaves an open
/// attempt on disk, later folded to interrupted) and `attempt_finished` at
/// return time (carrying `duration_ms`). Every OTHER event (RetryStarted,
/// FallbackTriggered) is still returned in the Vec for drive to write in its
/// return batch - drive remains the sole writer of those. `journal` wraps the
/// same single log in a Mutex, so this stays safe on the parallel batch's
/// worker threads (each append is one atomic line write).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_node(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    override_prompt: Option<String>,
    cancel: &AtomicBool,
    env_scrub: &[String],
    journal: &Journal,
) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError> {
    let node = playbook
        .node(node_id)
        .ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    let mut events: Vec<EventPayload> = Vec::new();
    match &node.kind {
        NodeKind::Start => Ok((NodeStatus::Succeeded, String::new(), events)),
        NodeKind::Prompt { prompt } => {
            let text = match &override_prompt {
                Some(p) => p.clone(),
                None => render_node_prompt(run_dir, run_id, state, cfg, prompt)?,
            };
            Ok((NodeStatus::Succeeded, text, events))
        }
        NodeKind::Condition { .. } => Ok((NodeStatus::Succeeded, String::new(), events)),
        NodeKind::AgentTask {
            prompt,
            profile,
            max_retries,
            timeout_seconds,
            success_check,
            isolation,
            ..
        } => {
            let mut text = match &override_prompt {
                Some(p) => p.clone(),
                None => render_node_prompt(run_dir, run_id, state, cfg, prompt)?,
            };
            let retries = max_retries.or(playbook.defaults.max_retries).unwrap_or(0);
            let timeout = timeout_seconds.map(Duration::from_secs);

            // Autonomy grant (spec 8.5): reaching node execution means the run
            // already cleared the policy/trust gate, where the user consented
            // to the run's effects. An agent-task node's effects always include
            // acting effects (fs_write/network/external), so we hand the agent
            // its non-interactive permission flags; otherwise a headless
            // one-shot agent blocks on approvals it can never receive.
            //
            // The grant is all-or-nothing: any effective effect beyond FsRead
            // yields the full non-interactive permission set (not a per-effect
            // subset). This matches the pessimistic effect model - inference
            // already unions fs_write/network/external onto every acting node,
            // so a narrower declared effect does not narrow the grant. If the
            // effect taxonomy ever gains finer acting effects, revisit this to
            // avoid silently granting full bypass for a narrow declaration.
            let grant_autonomy = apb_core::effects::effective(playbook)
                .iter()
                .any(|e| !matches!(e, apb_core::schema::Effect::FsRead));

            // A single step of the executor chain. For the profile path it carries
            // the invocation fixed in the manifest (call form + binary) rather than
            // re-deriving it from the live config at execution time (spec 3.6).
            struct Step {
                agent: String,
                model: String,
                soul_delivery: Option<String>,
                invocation: Option<crate::invocation::ResolvedInvocation>,
            }

            // A node's executor is always a profile (schema 2). We take the
            // chain/SOUL/skills from the run's immutable manifest (spec 3.6): editing
            // the live profile after the run has started has no effect on the run.
            let _ = profile;
            let manifest = crate::manifest::read(run_dir)?.ok_or_else(|| {
                EngineError::Invalid(format!(
                    "node `{node_id}` has no execution manifest: this run predates agent profiles and cannot be resumed after the schema 2 upgrade - start a fresh run"
                ))
            })?;
            let entry = manifest.for_node(node_id).cloned().ok_or_else(|| {
                EngineError::Invalid(format!(
                    "no manifest entry for node `{node_id}` (no profile bound)"
                ))
            })?;

            let steps: Vec<Step> = entry
                .chain
                .iter()
                .map(|ri| Step {
                    agent: ri.agent_id.clone(),
                    model: ri.model.clone(),
                    soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
                    invocation: Some(ri.clone()),
                })
                .collect();
            let soul_text = Some(entry.soul.clone());
            let skill_names: Vec<String> = entry.skills.iter().map(|s| s.name.clone()).collect();
            let profile_key = Some(entry.key());

            if steps.is_empty() {
                return Err(EngineError::Invalid(format!(
                    "node `{node_id}` has an empty executor chain"
                )));
            }

            // Skill delivery (spec 6.4, completion-plan Task 3). For an isolated
            // node (isolation full|best_effort) skills are materialized as REAL
            // copies from the run snapshot into an isolated per-node workdir
            // (`.agents/skills/<name>` + a `.claude/skills` bridge), and the agent is
            // pointed at it: editing the live skill after the run has started has no
            // effect on the run. For `isolation: none` - an advisory string of names
            // in the shared workdir. Skill content is never embedded into the prompt
            // (only names).
            let isolated = matches!(
                isolation,
                Some(Isolation::Full) | Some(Isolation::BestEffort)
            );
            let skills_mode = if isolated { "materialized" } else { "advisory" };
            if !skill_names.is_empty() {
                text = format!(
                    "{text}\n\nRelevant skills: {} - use them via your skills mechanism",
                    skill_names.join(", ")
                );
            }

            // Connector instruction block (spec 6 step 3): when this node holds
            // grants, tell the agent which connectors/accounts/functions it may
            // call and how. Built only from the run snapshot (manifest non-secret
            // fields + snapshotted ConnectorDocs), so no secret reaches the prompt.
            let grants = manifest.grants_for(node_id);
            if !grants.is_empty() {
                let docs =
                    crate::connector_prompt::load_snapshot_docs(run_dir, &manifest.connectors);
                let block =
                    crate::connector_prompt::instruction_block(grants, &manifest.connectors, &docs);
                if !block.is_empty() {
                    text = format!("{text}\n\n{block}");
                }
            }

            // Connector env isolation (spec 4.3) for every attempt's agent spawn:
            // scrub inherited connector tokens and hand the agent the run-context
            // env that `apb connector call` reads.
            let connector_policy = crate::adapter::ConnectorEnvPolicy {
                scrub: env_scrub.to_vec(),
                run_dir: Some(run_dir.to_path_buf()),
                node_id: Some(node_id.to_string()),
            };

            let mut attempt: u32 = 0;
            let mut last_msg = String::new();
            // The node's final status once all attempts are exhausted: TimedOut if
            // the last attempt was interrupted by a timeout, otherwise Failed.
            let mut last_timed_out = false;
            for (idx, step) in steps.iter().enumerate() {
                if idx > 0 {
                    events.push(EventPayload::FallbackTriggered {
                        node: node_id.into(),
                        from: steps[idx - 1].agent.clone(),
                        to: step.agent.clone(),
                        profile: profile_key.clone(),
                    });
                }
                // The profile path builds the adapter from the fixed invocation
                // (call form + canonical binary from the manifest), so that editing
                // agents.<id>.invocation in the config between start and resume does
                // not silently change the prompt contract. The executor path is unchanged.
                let adapter: Box<dyn crate::adapter::AgentAdapter> = match &step.invocation {
                    Some(ri) => Box::new(crate::adapter::ClaudeAdapter {
                        program: ri.canonical_executable.to_string_lossy().into_owned(),
                        spec: ri.spec.clone(),
                    }),
                    None => adapter_for(&step.agent)?,
                };
                for try_i in 0..=retries {
                    // Cancellation (this branch lost a join:any) - exit with status
                    // Cancelled, not counting this as a failure.
                    if cancel.load(Ordering::Relaxed) {
                        return Ok((NodeStatus::Cancelled, "cancelled".to_string(), events));
                    }
                    attempt += 1;
                    if try_i > 0 {
                        events.push(EventPayload::RetryStarted {
                            node: node_id.into(),
                            attempt,
                        });
                    }
                    // Attempt working directory. For an isolated node - a FRESH
                    // per-attempt directory `work/<node>/<attempt>` with skills
                    // freshly materialized from the snapshot: a hostile/failed
                    // previous attempt cannot slip a modified bundle to the next one
                    // (skills_mode: materialized would then not reflect
                    // reality). For `isolation: none` - the shared workdir.
                    let attempt_workdir: PathBuf = if isolated {
                        let wd = run_dir.join("work").join(node_id).join(attempt.to_string());
                        // Fail-closed: a missing directory is normal, but any other
                        // cleanup error is NOT swallowed - otherwise we would materialize
                        // skills on top of leftovers from the previous (possibly hostile) attempt.
                        match std::fs::remove_dir_all(&wd) {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                        materialize_isolated_skills(run_dir, &entry, &wd)?;
                        wd
                    } else {
                        workdir.to_path_buf()
                    };
                    // Where to stream the attempt's NDJSON events (acp transport); one
                    // file per attempt. The headless field ignores it.
                    let stream_log = run_dir
                        .join("agent-stream")
                        .join(format!("{node_id}-{attempt}.jsonl"));
                    let task = AgentTask {
                        prompt: &text,
                        model: &step.model,
                        workdir: &attempt_workdir,
                        timeout,
                        stream_log: Some(&stream_log),
                        soul: soul_text.as_deref(),
                        grant_autonomy,
                        connector_policy: &connector_policy,
                    };
                    // Spawn-time attempt journaling. The adapter invokes `on_spawn`
                    // right after the agent process starts, so `attempt_started`
                    // (carrying the child pid) is on disk BEFORE the agent does any
                    // work: a crash mid-attempt then leaves an open attempt the
                    // fold maps to interrupted. `spawn_at` records the spawn instant
                    // for `duration_ms`; `spawn_err` carries an append failure from
                    // inside the callback back out so it is not swallowed.
                    let cur_attempt = attempt;
                    let agent_name = step.agent.clone();
                    let soul_del = step.soul_delivery.clone();
                    let smode = Some(skills_mode.to_string());
                    let spawn_at: std::cell::Cell<Option<std::time::Instant>> =
                        std::cell::Cell::new(None);
                    let spawn_err: std::cell::RefCell<Option<EngineError>> =
                        std::cell::RefCell::new(None);
                    let on_spawn = |pid: u32| {
                        spawn_at.set(Some(std::time::Instant::now()));
                        if let Err(e) = journal.append(EventPayload::AttemptStarted {
                            node: node_id.to_string(),
                            attempt: cur_attempt,
                            agent: agent_name.clone(),
                            soul_delivery: soul_del.clone(),
                            skills_mode: smode.clone(),
                            pid: Some(pid),
                        }) {
                            *spawn_err.borrow_mut() = Some(e);
                        }
                    };
                    let outcome = adapter.run_cancellable(&task, cancel, Some(&on_spawn));
                    if let Some(e) = spawn_err.borrow_mut().take() {
                        return Err(e);
                    }
                    let spawn_instant = spawn_at.get();
                    // The spawn itself failed before the callback ran: still journal
                    // a started (pid unknown) so every attempt_finished is preceded
                    // by an attempt_started.
                    if spawn_instant.is_none() {
                        journal.append(EventPayload::AttemptStarted {
                            node: node_id.into(),
                            attempt,
                            agent: step.agent.clone(),
                            soul_delivery: step.soul_delivery.clone(),
                            skills_mode: Some(skills_mode.to_string()),
                            pid: None,
                        })?;
                    }
                    let duration_ms = spawn_instant.map(|t| t.elapsed().as_millis() as u64);
                    match outcome {
                        Ok(report) => {
                            journal.append(EventPayload::AttemptFinished {
                                node: node_id.into(),
                                attempt,
                                status: report.status.as_str().into(),
                                duration_ms,
                            })?;
                            if report.status == NodeStatus::Succeeded {
                                // A deterministic check on top of the self-report (spec 6.2):
                                // a non-zero check exit code makes the node Failed regardless
                                // of the agent's report. We run it in the SAME attempt
                                // workdir the agent worked in (for an isolated node -
                                // attempt_workdir, otherwise the shared workdir), otherwise
                                // the check would validate a directory the agent never wrote to.
                                if let Some(check) = success_check.as_ref() {
                                    // success_check runs only AFTER this branch's agent
                                    // has succeeded (meaning this branch was not
                                    // cancelled) - we do not propagate cancellation here.
                                    let r = run_script(
                                        run_dir,
                                        &attempt_workdir,
                                        check,
                                        "sh",
                                        None,
                                        None,
                                    )?;
                                    if r.status != NodeStatus::Succeeded {
                                        return Ok((
                                            NodeStatus::Failed,
                                            format!("success_check `{check}` failed"),
                                            events,
                                        ));
                                    }
                                }
                                return Ok((NodeStatus::Succeeded, report.summary, events));
                            }
                            last_msg = report.summary;
                            last_timed_out = false;
                        }
                        Err((class, msg)) => {
                            // Cancellation mid-adapter-work: kill returned Transport,
                            // but this is not a failure - mark the node Cancelled.
                            if cancel.load(Ordering::Relaxed) {
                                return Ok((
                                    NodeStatus::Cancelled,
                                    "cancelled".to_string(),
                                    events,
                                ));
                            }
                            last_timed_out = class == ErrorClass::Timeout;
                            let attempt_status = if last_timed_out {
                                "timed_out"
                            } else {
                                "failed"
                            };
                            journal.append(EventPayload::AttemptFinished {
                                node: node_id.into(),
                                attempt,
                                status: attempt_status.into(),
                                duration_ms,
                            })?;
                            last_msg = msg;
                            // A transport error and a timeout break the retry loop for this
                            // executor and go to fallback.
                            if class == ErrorClass::Transport || class == ErrorClass::Timeout {
                                break;
                            }
                        }
                    }
                }
            }
            let final_status = if last_timed_out {
                NodeStatus::TimedOut
            } else {
                NodeStatus::Failed
            };
            Ok((final_status, last_msg, events))
        }
        NodeKind::Script {
            script,
            runner,
            timeout_seconds,
        } => {
            let timeout = timeout_seconds.map(Duration::from_secs);
            // Pass through cancel: in a parallel batch (join:any) the winning
            // branch sets the flag, and a running script is torn down together with
            // its process group - without leaking side effects after a sibling wins.
            let r = run_script(run_dir, workdir, script, runner, timeout, Some(cancel))?;
            Ok((r.status, r.stdout, events))
        }
        NodeKind::Finish { .. } => Ok((NodeStatus::Succeeded, String::new(), events)),
        // human_review is handled inside drive itself (pause until a decision), it
        // never reaches here; this branch is defensive. wait - subphase 7b.
        NodeKind::HumanReview { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (human_review) must be handled by drive"
        ))),
        NodeKind::Wait { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (wait) must be handled by drive"
        ))),
        NodeKind::Playbook { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (playbook) must be handled by drive"
        ))),
    }
}

/// Composes the run answer for a finish-with-prompt (spec B). A reduced
/// `agent_task`: the profile chain + SOUL come from the run manifest (identical
/// resolution/trust to an agent_task), the prompt renders with the full
/// standard context, but no skills are delivered and there is no success_check
/// and no isolation. Timeout/retries fall back to `defaults`. Returns
/// (status, answer, events). Like `execute_node`, the two attempt-lifecycle
/// events are journaled directly (`attempt_started` with pid at spawn,
/// `attempt_finished` with `duration_ms` at return) so a crash during the
/// terminal answer composition leaves an open attempt on disk; every other
/// event is returned for drive to write in its return batch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_finish_answer(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    prompt: &str,
    env_scrub: &[String],
    journal: &Journal,
) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError> {
    let context =
        build_context_for_render(run_dir, &read_all(run_dir)?, cfg.instruction.as_deref())?;
    let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
        .into_iter()
        .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
        .collect();
    let text = render(
        prompt,
        &cfg.params,
        cfg.instruction.as_deref(),
        &state.outputs,
        &state.reviews,
        &hooks,
        &context,
    );
    let retries = playbook.defaults.max_retries.unwrap_or(0);
    let timeout = playbook.defaults.timeout_seconds.map(Duration::from_secs);
    let grant_autonomy = apb_core::effects::effective(playbook)
        .iter()
        .any(|e| !matches!(e, apb_core::schema::Effect::FsRead));

    let manifest = crate::manifest::read(run_dir)?.ok_or_else(|| {
        EngineError::Invalid(format!("finish node `{node_id}` has no execution manifest"))
    })?;
    let entry = manifest.for_node(node_id).cloned().ok_or_else(|| {
        EngineError::Invalid(format!("no manifest entry for finish node `{node_id}`"))
    })?;
    if entry.chain.is_empty() {
        return Err(EngineError::Invalid(format!(
            "finish node `{node_id}` has an empty executor chain"
        )));
    }

    // Cancellation happens at drive-loop boundaries (the top-of-loop
    // control.jsonl scan and join:any teardown), never mid-agent: like the
    // inline agent_task path (which drive calls with a fresh `AtomicBool`), this
    // finish-answer agent runs synchronously on the drive thread, so this local
    // token is never set during the call (review I1, accepted as pre-existing).
    let cancel = AtomicBool::new(false);
    // Connector env isolation (spec 4.3): the finish-answer agent is a spawned
    // agent too, so its inherited connector tokens are scrubbed and it gets the
    // run-context env.
    let connector_policy = crate::adapter::ConnectorEnvPolicy {
        scrub: env_scrub.to_vec(),
        run_dir: Some(run_dir.to_path_buf()),
        node_id: Some(node_id.to_string()),
    };
    let mut events: Vec<EventPayload> = Vec::new();
    let mut attempt: u32 = 0;
    let mut last_msg = String::new();
    let mut last_timed_out = false;
    for (idx, ri) in entry.chain.iter().enumerate() {
        if idx > 0 {
            events.push(EventPayload::FallbackTriggered {
                node: node_id.into(),
                from: entry.chain[idx - 1].agent_id.clone(),
                to: ri.agent_id.clone(),
                profile: Some(entry.key()),
            });
        }
        let adapter = crate::adapter::ClaudeAdapter {
            program: ri.canonical_executable.to_string_lossy().into_owned(),
            spec: ri.spec.clone(),
        };
        for try_i in 0..=retries {
            attempt += 1;
            if try_i > 0 {
                events.push(EventPayload::RetryStarted {
                    node: node_id.into(),
                    attempt,
                });
            }
            let stream_log = run_dir
                .join("agent-stream")
                .join(format!("{node_id}-{attempt}.jsonl"));
            let task = AgentTask {
                prompt: &text,
                model: &ri.model,
                workdir,
                timeout,
                stream_log: Some(&stream_log),
                soul: Some(entry.soul.as_str()),
                grant_autonomy,
                connector_policy: &connector_policy,
            };
            // Spawn-time attempt journaling (identical shape to execute_node):
            // `on_spawn` journals attempt_started with the child pid before the
            // agent runs, and records the spawn instant for duration_ms, so a
            // crash during the terminal answer composition leaves an open attempt
            // the fold maps to interrupted.
            let cur_attempt = attempt;
            let agent_name = ri.agent_id.clone();
            let soul_del = Some(soul_delivery_str(ri.soul_delivery));
            let spawn_at: std::cell::Cell<Option<std::time::Instant>> = std::cell::Cell::new(None);
            let spawn_err: std::cell::RefCell<Option<EngineError>> = std::cell::RefCell::new(None);
            let on_spawn = |pid: u32| {
                spawn_at.set(Some(std::time::Instant::now()));
                if let Err(e) = journal.append(EventPayload::AttemptStarted {
                    node: node_id.to_string(),
                    attempt: cur_attempt,
                    agent: agent_name.clone(),
                    soul_delivery: soul_del.clone(),
                    skills_mode: None,
                    pid: Some(pid),
                }) {
                    *spawn_err.borrow_mut() = Some(e);
                }
            };
            let outcome = adapter.run_cancellable(&task, &cancel, Some(&on_spawn));
            if let Some(e) = spawn_err.borrow_mut().take() {
                return Err(e);
            }
            let spawn_instant = spawn_at.get();
            // Spawn failed before the callback ran: still journal a started
            // (pid unknown) so every attempt_finished is preceded by a started.
            if spawn_instant.is_none() {
                journal.append(EventPayload::AttemptStarted {
                    node: node_id.into(),
                    attempt,
                    agent: ri.agent_id.clone(),
                    soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
                    skills_mode: None,
                    pid: None,
                })?;
            }
            let duration_ms = spawn_instant.map(|t| t.elapsed().as_millis() as u64);
            match outcome {
                Ok(report) => {
                    journal.append(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: report.status.as_str().into(),
                        duration_ms,
                    })?;
                    if report.status == NodeStatus::Succeeded {
                        return Ok((NodeStatus::Succeeded, report.summary, events));
                    }
                    last_msg = report.summary;
                    last_timed_out = false;
                }
                Err((class, msg)) => {
                    last_timed_out = class == ErrorClass::Timeout;
                    journal.append(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: if last_timed_out {
                            "timed_out"
                        } else {
                            "failed"
                        }
                        .into(),
                        duration_ms,
                    })?;
                    last_msg = msg;
                    if class == ErrorClass::Transport || class == ErrorClass::Timeout {
                        break;
                    }
                }
            }
        }
    }
    let final_status = if last_timed_out {
        NodeStatus::TimedOut
    } else {
        NodeStatus::Failed
    };
    Ok((final_status, last_msg, events))
}

/// Materializes profile skills as REAL copies from the run snapshot into the
/// isolated per-node workdir (completion-plan Task 3). The source is the snapshot
/// (`run_dir/profiles/<scope>/<name>/skills/<sscope>/<sname>`), NOT the live
/// `.agents/skills`: editing a skill after the run has started has no effect on
/// the run. The `.claude/skills` bridge is aimed at the real copies via symlinks.
/// The workdir is created even without skills (an isolated node execution directory).
pub(crate) fn materialize_isolated_skills(
    run_dir: &Path,
    entry: &ManifestProfile,
    workdir: &Path,
) -> Result<(), EngineError> {
    let skills_parent = workdir.join(".agents/skills");
    std::fs::create_dir_all(&skills_parent)?;
    for sk in &entry.skills {
        let src = run_dir
            .join("profiles")
            .join(&entry.scope)
            .join(&entry.name)
            .join("skills")
            .join(&sk.scope)
            .join(&sk.name);
        copy_tree(&src, &skills_parent.join(&sk.name))?;
    }
    if !entry.skills.is_empty() {
        let claude_parent = workdir.join(".claude/skills");
        // Fail-closed: the isolated node's workdir is fresh, so the
        // `.claude/skills` bridge must be laid down cleanly. Any note here is a
        // real failure (a symlink could not be created, etc.), not a benign case of
        // "already exists/foreign bridge"; silently continuing would mean running the
        // agent without skills visible via `.claude` and passing off an incorrect run as a success.
        let notes = apb_core::skills::ensure_claude_bridge(&skills_parent, &claude_parent);
        if !notes.is_empty() {
            return Err(EngineError::Invalid(format!(
                "isolated skill bridge failed: {}",
                notes.join("; ")
            )));
        }
    }
    Ok(())
}

/// Recursively copies a skill-snapshot tree. Symlinks are RECREATED as symlinks
/// (not dereferenced), in parity with `content::snapshot_tree`, which
/// preserves in-tree relative symlinks: otherwise a symlinked directory would fail
/// in `fs::copy` with EISDIR and abort the run. `file_type()` from `read_dir` does not
/// follow symlinks, so a symlink is never `is_dir()` - we check it first.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from)?;
                std::os::unix::fs::symlink(&target, &to)?;
            }
            #[cfg(not(unix))]
            {
                // Off unix, skill symlinks are not supported - copy the target instead.
                std::fs::copy(&from, &to)?;
            }
        } else if ft.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// The run id of the latest ChildRunStarted for `node_id`, if any.
pub(crate) fn latest_child_run(events: &[Event], node_id: &str) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ChildRunStarted { node_id: n, run_id } if n == node_id => {
            Some(run_id.clone())
        }
        _ => None,
    })
}

/// Whether a run directory has reached a terminal run status.
///
/// Honest errors (review I7/R1-I9): the child's event log is the sole source of
/// truth for terminality, so an unreadable/corrupt child dir must NOT be guessed
/// at. The old `read_all(..).unwrap_or_default()` folded a read failure into an
/// empty log, which reads as "not terminal" and would make the reattach path in
/// `run_playbook_node` resume the same broken child forever. Returning the read
/// error instead propagates as a hard node/run failure: this cannot loop (no
/// silent reattach) and cannot fake success (no empty-log Running/Succeeded).
/// `read_all` already returns Ok(empty) for a genuinely absent log, so only a
/// real IO/parse fault surfaces here.
pub(crate) fn run_is_terminal(root: &Path, run_id: &str) -> Result<bool, EngineError> {
    let dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&dir)?;
    Ok(matches!(
        RunState::fold(&events).run_status,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
    ))
}

/// The parent run's definition origin (from its RunProvenance event), used to
/// resolve a child's `scope: auto` the same way the policy gate does (parent
/// origin first, then global). Defaults to Project when the label is absent.
fn parent_run_origin(run_dir: &Path) -> apb_core::scope::Origin {
    use apb_core::scope::Origin;
    let events = read_all(run_dir).unwrap_or_default();
    for e in &events {
        if let EventPayload::RunProvenance {
            origin: Some(label),
            ..
        } = &e.payload
        {
            return if label == "global" {
                Origin::Global
            } else {
                Origin::Project { workspace_id: None }
            };
        }
    }
    Origin::Project { workspace_id: None }
}

/// Executes a `playbook` node (spec C): starts (or, on resume, reattaches to) a
/// full child run and maps its terminal state to this node's status/output. The
/// child runs in-process, synchronously, with `allow_shared_workdir: true` (the
/// parent already holds the workdir lock; see the module notes). ChildRunStarted
/// is appended here (drive thread, single writer) BEFORE the child is driven.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_playbook_node(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    _playbook: &Playbook,
    cfg: &RunConfig,
    run_id: &str,
    node_id: &str,
    child_ref: &apb_core::schema::QualifiedPlaybookRef,
    node_instruction: Option<&str>,
) -> Result<(NodeStatus, String), EngineError> {
    // Depth backstop.
    if cfg.depth + 1 > MAX_SUBPLAYBOOK_DEPTH {
        return Ok((
            NodeStatus::Failed,
            format!(
                "sub-playbook depth limit ({}) exceeded",
                MAX_SUBPLAYBOOK_DEPTH
            ),
        ));
    }

    // Resume reattach: a still-running child from a prior ChildRunStarted is
    // resumed, not restarted (the event log is the source of truth). The child
    // runs on this drive thread while the parent still holds the workdir lock,
    // so its resume must allow the shared workdir (no second acquire).
    //
    // Single read (review M1): this `events` snapshot is reused below for the
    // instruction render context. No log write happens between the reattach
    // check and that render (the reattach branch returns before any append, and
    // ChildRunStarted is written only much later), so reading once is sound.
    let events = read_all(run_dir)?;
    if let Some(existing) = latest_child_run(&events, node_id)
        && !run_is_terminal(root, &existing)?
    {
        let res = resume_inner(root, &existing, None, false, true)?;
        return Ok(map_child_outcome(root, &existing, res.outcome));
    }

    // Render the node instruction with the parent context; the result is the
    // child's explicit instruction (Part A precedence). Absent -> None (child
    // falls back to its own draft). Reuses the `events` read above (review M1).
    let child_instruction = match node_instruction {
        Some(t) => {
            let context = build_context_for_render(run_dir, &events, cfg.instruction.as_deref())?;
            let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
                .into_iter()
                .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
                .collect();
            let state = RunState::fold(&events);
            Some(render(
                t,
                &cfg.params,
                cfg.instruction.as_deref(),
                &state.outputs,
                &state.reviews,
                &hooks,
                &context,
            ))
        }
        None => None,
    };

    // Resolve the child reference. A gate pin (cfg.expected_children) fixes the
    // scope + version verbatim (anti-TOCTOU); without a pin (CLI path) we live
    // resolve with the same candidate order the policy gate uses: an explicit
    // scope pins the origin, `auto` prefers the parent origin then global.
    use apb_core::profile::ProfileScope;
    use apb_core::scope::{Origin, PlaybookRef, scope_candidates};
    // Fail-closed pins (review I4): `expected_children == None` is the ungated
    // (CLI) path and lives-resolves. But a gated run (`Some(map)`) MUST carry a
    // pin for every playbook node its permit walked; a missing entry means this
    // node was outside the verified tree, so we FAIL the node rather than
    // silently live-resolving unverified content.
    let pin = match &cfg.expected_children {
        None => None,
        Some(map) => match map.get(node_id) {
            Some(p) => Some(p),
            None => {
                return Ok((
                    NodeStatus::Failed,
                    format!(
                        "sub-playbook node `{node_id}`: run permit carried no pin for it; refusing to live-resolve under a gated run"
                    ),
                ));
            }
        },
    };
    let resolved = if let Some(p) = pin {
        // The pin's scope is a resolved origin (never `Auto`), so map it back to
        // a concrete `Origin` (review I2 - no string comparison).
        let origin = match p.scope {
            ProfileScope::Global => Origin::Global,
            _ => Origin::Project { workspace_id: None },
        };
        let cref = PlaybookRef {
            origin,
            id: child_ref.id.clone(),
            version: Some(p.version.clone()),
        };
        apb_core::store::resolve(root, &cref)
            .map_err(|e| EngineError::Invalid(format!("sub-playbook `{}`: {e}", child_ref.id)))?
    } else {
        let candidates = scope_candidates(child_ref.scope, &parent_run_origin(run_dir));
        let mut resolved_opt = None;
        for cand in &candidates {
            let cref = PlaybookRef {
                origin: cand.clone(),
                id: child_ref.id.clone(),
                version: None,
            };
            if let Ok(r) = apb_core::store::resolve(root, &cref) {
                resolved_opt = Some(r);
                break;
            }
        }
        resolved_opt.ok_or_else(|| {
            EngineError::Invalid(format!(
                "sub-playbook `{}` (node `{}`) did not resolve in any candidate scope",
                child_ref.id, node_id
            ))
        })?
    };

    let opts = RunOptions {
        instruction: child_instruction,
        allow_shared_workdir: true,
        parent_run: Some(run_id.to_string()),
        depth: cfg.depth + 1,
        expected_digest: pin.map(|p| p.playbook_digest.clone()),
        expected_profile_bundles: pin.map(|p| p.profile_bundles.clone()),
        expected_children: pin.map(|p| p.children.clone()),
        ..Default::default()
    };

    // Prepare (get the run id) -> record ChildRunStarted -> drive to terminal.
    let t = PrepareTarget {
        definition_parent: resolved.definition_parent.clone(),
        execution_root: resolved.execution_root.clone(),
        origin_label: resolved.origin_label,
    };
    let mut cp = prepare_run_target(&t, &resolved.id, Some(&resolved.version), opts)?;
    let child_run_id = cp.run_id.clone();
    log.append(EventPayload::ChildRunStarted {
        node_id: node_id.to_string(),
        run_id: child_run_id.clone(),
    })?;
    let res = drive(
        cp.playbook.clone(),
        &cp.run_dir,
        &resolved.execution_root,
        &mut cp.log,
        &cp.cfg,
        cp.start_node.clone(),
        StartMode::Rerun,
        cp.run_id.clone(),
        RunMode::Autonomous,
        cp.supervisor_expected,
    )?;
    Ok(map_child_outcome(root, &child_run_id, res.outcome))
}

/// Maps a child run's terminal status to the parent node's (status, output).
///
/// Honest errors (review I7/R1-I9): on a Succeeded child we must read its event
/// log to compose the answer. The old `read_all(..).unwrap_or_default()` turned
/// an unreadable/corrupt child dir into an empty log, which then yielded node
/// SUCCESS with an empty answer - a corrupted run masquerading as a legit
/// promptless finish. We now distinguish the two: a genuine read failure FAILS
/// the parent node with a diagnostic naming the child run id and the error,
/// while a successful read whose `run_answer` is None (a promptless finish, a
/// legitimately empty answer) stays Succeeded with "".
fn map_child_outcome(root: &Path, child_run_id: &str, outcome: RunStatus) -> (NodeStatus, String) {
    match outcome {
        RunStatus::Succeeded => {
            let dir = root.join(".apb/runs").join(child_run_id);
            match read_all(&dir) {
                Ok(events) => {
                    let answer = crate::progress::run_answer(&dir, &events).unwrap_or_default();
                    (NodeStatus::Succeeded, answer)
                }
                Err(e) => (
                    NodeStatus::Failed,
                    format!(
                        "sub-playbook child run `{child_run_id}` succeeded but its events could not be read: {e}"
                    ),
                ),
            }
        }
        other => (
            NodeStatus::Failed,
            format!(
                "sub-playbook child run `{child_run_id}` ended {}",
                other.as_str()
            ),
        ),
    }
}

/// Whether a node is slow (external work - agent or script), such that it
/// makes sense to execute it in parallel with other branches.
pub(crate) fn is_agent_or_script(playbook: &Playbook, node: &str) -> bool {
    matches!(
        playbook.node(node).map(|n| &n.kind),
        Some(NodeKind::AgentTask { .. }) | Some(NodeKind::Script { .. })
    )
}

/// Context compaction (spec 8.5): if enabled (cfg.context_max_bytes) and the
/// full context exceeds the threshold, old sections are compacted by a cheap model
/// into context_compact.md, and a ContextCompacted event is returned, which drive
/// writes (the sole writer of the log). The context_compact.md file is a
/// materialized artifact outside the primary log, so writing it directly here does
/// not violate the single-writer-of-events invariant. The summary does NOT go into
/// the log (a non-deterministic LLM output), which preserves replay determinism.
/// Returns None when compaction is disabled, the threshold is not exceeded, or
/// everything old is already compacted (idempotent on resume). A model failure is
/// not critical: it also returns None then, and the run works on the full context.
pub(crate) fn maybe_compact_context(
    run_dir: &Path,
    workdir: &Path,
    cfg: &RunConfig,
    events: &[Event],
    env_scrub: &[String],
) -> Result<Option<EventPayload>, EngineError> {
    let Some(max_bytes) = cfg.context_max_bytes else {
        return Ok(None);
    };
    if max_bytes == 0 || build_context(events).len() <= max_bytes {
        return Ok(None);
    }
    // We keep the tail at roughly half the limit and compact the rest.
    let Some(boundary) = crate::context::compaction_boundary(events, max_bytes / 2) else {
        return Ok(None);
    };
    let prev = crate::context::latest_compaction(events);
    let prev_up_to = prev.as_ref().map(|(_, s)| *s).unwrap_or(0);
    if boundary <= prev_up_to {
        // Everything old is already compacted - nothing left to compact.
        return Ok(None);
    }
    let prev_summary = prev
        .as_ref()
        .map(|(f, _)| std::fs::read_to_string(run_dir.join(f)).unwrap_or_default())
        .unwrap_or_default();
    let newly_old = crate::context::sections_between(events, prev_up_to, boundary);
    let model = cfg
        .context_compact_model
        .clone()
        .unwrap_or_else(|| "haiku".to_string());
    let adapter = adapter_for("claude-code")?;
    let prompt = format!(
        "Summarize the following playbook run context concisely, preserving key facts, \
         decisions, and outputs that later steps may need. Keep it to a few short \
         paragraphs. Do not add commentary or preamble.\n\n{prev_summary}\n\n{newly_old}"
    );
    // Compaction is synchronous inside drive: without a timeout, a hung model would
    // stall the entire run. We bound it with a finite deadline; on overrun (as with
    // any model error) compaction is not critical - we work on the full context.
    const COMPACTION_TIMEOUT: Duration = Duration::from_secs(120);
    // Connector env isolation (spec 4.3): scrub inherited connector tokens even
    // from this internal summarizer. It performs no connector calls, so it gets
    // no run-context env.
    let connector_policy = crate::adapter::ConnectorEnvPolicy {
        scrub: env_scrub.to_vec(),
        run_dir: None,
        node_id: None,
    };
    let task = AgentTask {
        prompt: &prompt,
        model: &model,
        workdir,
        timeout: Some(COMPACTION_TIMEOUT),
        stream_log: None,
        soul: None,
        // Context compaction only summarizes text; it needs no file or network
        // access, so it stays in the default permission posture.
        grant_autonomy: false,
        connector_policy: &connector_policy,
    };
    let summary = match adapter.run(&task) {
        Ok(report) => report.summary,
        Err(_) => return Ok(None),
    };
    let compact_file = "context_compact.md";
    apb_core::fsutil::atomic_write(&run_dir.join(compact_file), summary.as_bytes())?;
    Ok(Some(EventPayload::ContextCompacted {
        compact_file: compact_file.to_string(),
        model,
        up_to_seq: boundary,
    }))
}

/// Adds the ready successors of a finished node `node` to the frontier. A
/// join target is added only if it is ready (otherwise the branch waits at the
/// join). On a ready join:any it cancels the other unfinished frontier branches
/// (marking them cancelled). The sole writer of events (cancelled) is the
/// calling drive, so the single-writer invariant is preserved.
/// The ready successors a node hands the frontier: its outgoing edges evaluated
/// against the folded status and outputs, dropping the node itself and any join
/// that is not yet ready. Pure - it reads state and writes nothing, so a resume
/// can ask "would advancing past this node have anything to run" WITHOUT any
/// journal side effect. `advance_frontier` layers the join:any cancellation and
/// the frontier writes on top of this.
pub(crate) fn seed_successors(playbook: &Playbook, node: &str, state: &RunState) -> Vec<String> {
    let mut runnable: Vec<String> = Vec::new();
    for s in parallel::successors(playbook, node, state) {
        let ready = if parallel::is_join(playbook, &s) {
            !matches!(
                parallel::join_readiness(playbook, &s, state),
                JoinReadiness::NotReady
            )
        } else {
            true
        };
        if ready && s != node && !runnable.contains(&s) {
            runnable.push(s);
        }
    }
    runnable
}

pub(crate) fn advance_frontier(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    frontier: &mut Vec<String>,
    log: &mut EventLog,
) -> Result<(), EngineError> {
    let mut runnable: Vec<String> = seed_successors(playbook, node, state)
        .into_iter()
        .filter(|s| !frontier.contains(s))
        .collect();
    if let Some(join) = runnable
        .iter()
        .find(|s| {
            parallel::is_join(playbook, s)
                && parallel::join_mode(playbook, s) == parallel::JoinMode::Any
        })
        .cloned()
    {
        for other in std::mem::take(frontier) {
            if !parallel::is_join(playbook, &other) {
                log.append(EventPayload::NodeFinished {
                    node: other,
                    status: "cancelled".into(),
                    attempt: 1,
                    output: String::new(),
                    artifacts: Vec::new(),
                })?;
            }
        }
        runnable.retain(|s| s == &join);
    }
    for s in runnable {
        if !frontier.contains(&s) {
            frontier.push(s);
        }
    }
    Ok(())
}
