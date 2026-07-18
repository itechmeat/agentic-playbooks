//! Node execution: rendering, adapter dispatch, skill materialization, and frontier advance.
//! Split out of `scheduler` for navigability; shares the parent module's imports via `use super::*`.

use super::*;

/// A single execution of a node. Returns (NodeStatus, output, events).
/// Events (AttemptStarted/Finished, RetryStarted, FallbackTriggered) are NOT
/// written here but returned: the caller (drive) writes them - the sole writer of
/// events.jsonl. Thanks to this, execute_node never touches the log and can be
/// run on a background thread for parallel branches (7c-2).
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
) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError> {
    let node = playbook
        .node(node_id)
        .ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    // Context accounting for compaction: summary + the uncompacted tail, if drive
    // recorded ContextCompacted; otherwise the full context. The compaction itself
    // is triggered by drive (the sole writer of the event) - what reaches here is
    // already the finished result.
    let context = build_context_for_render(run_dir, &read_all(run_dir)?)?;
    // Run hooks as map key -> relative endpoint path (for the
    // {{run.hooks.<key>}} template); the monitor is added by the host. We take
    // run_id from the caller (drive) rather than re-deriving it from the path.
    let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
        .into_iter()
        .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
        .collect();
    let mut events: Vec<EventPayload> = Vec::new();
    match &node.kind {
        NodeKind::Start => Ok((NodeStatus::Succeeded, String::new(), events)),
        NodeKind::Prompt { prompt } => {
            let text = match &override_prompt {
                Some(p) => p.clone(),
                None => render(
                    prompt,
                    &cfg.params,
                    cfg.instruction.as_deref(),
                    &state.outputs,
                    &state.reviews,
                    &hooks,
                    &context,
                ),
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
                None => render(
                    prompt,
                    &cfg.params,
                    cfg.instruction.as_deref(),
                    &state.outputs,
                    &state.reviews,
                    &hooks,
                    &context,
                ),
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
                    events.push(EventPayload::AttemptStarted {
                        node: node_id.into(),
                        attempt,
                        agent: step.agent.clone(),
                        soul_delivery: step.soul_delivery.clone(),
                        skills_mode: Some(skills_mode.to_string()),
                    });
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
                    };
                    match adapter.run_cancellable(&task, cancel) {
                        Ok(report) => {
                            events.push(EventPayload::AttemptFinished {
                                node: node_id.into(),
                                attempt,
                                status: report.status.as_str().into(),
                            });
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
                            events.push(EventPayload::AttemptFinished {
                                node: node_id.into(),
                                attempt,
                                status: attempt_status.into(),
                            });
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
    }
}

/// Composes the run answer for a finish-with-prompt (spec B). A reduced
/// `agent_task`: the profile chain + SOUL come from the run manifest (identical
/// resolution/trust to an agent_task), the prompt renders with the full
/// standard context, but no skills are delivered and there is no success_check
/// and no isolation. Timeout/retries fall back to `defaults`. Returns
/// (status, answer, events); drive writes the events (single writer).
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
) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError> {
    let context = build_context_for_render(run_dir, &read_all(run_dir)?)?;
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

    let cancel = AtomicBool::new(false);
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
            events.push(EventPayload::AttemptStarted {
                node: node_id.into(),
                attempt,
                agent: ri.agent_id.clone(),
                soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
                skills_mode: None,
            });
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
            };
            match adapter.run_cancellable(&task, &cancel) {
                Ok(report) => {
                    events.push(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: report.status.as_str().into(),
                    });
                    if report.status == NodeStatus::Succeeded {
                        return Ok((NodeStatus::Succeeded, report.summary, events));
                    }
                    last_msg = report.summary;
                    last_timed_out = false;
                }
                Err((class, msg)) => {
                    last_timed_out = class == ErrorClass::Timeout;
                    events.push(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: if last_timed_out {
                            "timed_out"
                        } else {
                            "failed"
                        }
                        .into(),
                    });
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
pub(crate) fn advance_frontier(
    playbook: &Playbook,
    node: &str,
    state: &RunState,
    frontier: &mut Vec<String>,
    log: &mut EventLog,
) -> Result<(), EngineError> {
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
        if ready && s != node && !runnable.contains(&s) && !frontier.contains(&s) {
            runnable.push(s);
        }
    }
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
