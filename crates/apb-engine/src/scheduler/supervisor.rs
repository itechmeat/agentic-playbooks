//! Supervisor spawn, control polling, and context rebuild for supervised runs.
//! Split out of `scheduler` for navigability; shares the parent module's imports via `use super::*`.

use super::*;

pub(crate) fn next_supervisor_token() -> String {
    let n = SUPERVISOR_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("sv-{}-{n}", now_millis())
}

/// Spawns a background supervisor agent for run `run_id`: mints a
/// token, persists the session and the baseline `supervisor/spawned_at` (so
/// heartbeat monitoring can detect the agent's silence even before its first
/// poll), builds an English brief and asks the adapter to bring up the process.
/// Does not write events - the sole writer of `events.jsonl` is the `drive`
/// loop. The agent's executor is taken from the `supervisor` binding in the run
/// manifest (profile `supervisor.profile` or `defaults.profile`); no binding ->
/// `None` (nothing to spawn without an executor, and this is not an error -
/// e.g. a playbook without a supervisor profile).
pub fn spawn_supervisor_agent(
    root: &Path,
    run_id: &str,
    playbook: &Playbook,
) -> Result<Option<String>, EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let Some(manifest) = crate::manifest::read(&run_dir)? else {
        return Ok(None);
    };
    let Some(entry) = manifest.for_node("supervisor") else {
        return Ok(None);
    };
    if entry.chain.is_empty() {
        return Ok(None);
    }

    let token = next_supervisor_token();
    // Default capabilities for 4c - restricted by policy - to be refined
    // later (see the carry-over note in the Phase 4c plan).
    let capabilities = vec!["observe".to_string(), "retry".to_string()];
    write_supervisor_session(root, run_id, &token, &capabilities)?;

    apb_core::fsutil::atomic_write(
        &run_dir.join("supervisor").join("spawned_at"),
        now_millis().to_string().as_bytes(),
    )?;

    // The supervisor profile's SOUL and skills are used in full (completion-plan
    // Task 9): SOUL is delivered per the invocation form, skill names go in as an
    // advisory string in the brief (the supervisor works in the project's shared
    // workdir; skill content is never embedded into the prompt).
    let mut brief = format!(
        "You are the background supervisor agent for playbook run `{run_id}` \
         (playbook `{}` version `{}`). Connect to this project's `apb mcp` server and loop: \
         call supervisor_wait_event with token `{token}` to wait for the next wake, then \
         diagnose the run with supervisor_run_inspect, intervene as needed via \
         supervisor_node_retry, supervisor_run_continue_from, supervisor_run_pause, \
         supervisor_run_abort or supervisor_context_append, and once the run reaches a \
         terminal state submit your final findings with supervisor_report.",
        playbook.id, playbook.version,
    );
    if !entry.skills.is_empty() {
        let names: Vec<&str> = entry.skills.iter().map(|s| s.name.as_str()).collect();
        brief = format!(
            "{brief}\n\nRelevant skills: {} - use them via your skills mechanism",
            names.join(", ")
        );
    }
    let soul = if entry.soul.trim().is_empty() {
        None
    } else {
        Some(entry.soul.as_str())
    };

    // Try the executor chain in order: primary spawn failure -> next
    // fallback. We record the actually used element in a diagnostic
    // file (spawn_supervisor_agent does not write events - drive is the sole writer).
    let mut last_err: Option<String> = None;
    for inv in &entry.chain {
        let adapter = crate::adapter::ClaudeAdapter {
            program: inv.canonical_executable.to_string_lossy().into_owned(),
            spec: inv.spec.clone(),
        };
        match adapter.spawn_supervisor(&brief, &inv.model, root, soul) {
            Ok(()) => {
                apb_core::fsutil::atomic_write(
                    &run_dir.join("supervisor").join("executor"),
                    format!("{}:{}", inv.agent_id, inv.model).as_bytes(),
                )?;
                return Ok(Some(token));
            }
            Err((_class, msg)) => last_err = Some(msg),
        }
    }
    Err(EngineError::Adapter(last_err.unwrap_or_else(|| {
        "supervisor spawn failed with no chain element".into()
    })))
}

/// Poll interval for control.jsonl while waiting in supervised mode.
pub(crate) const AWAIT_CONTROL_POLL: Duration = Duration::from_millis(50);

/// Blocks until the first command with seq greater than `cursor` that must be
/// returned to the caller (Retry/ContinueFrom/Pause/Abort/Patch). Used only in
/// supervised mode after a wake event - by that point the run is already
/// stopped on the failed node, so polling at an interval does not lose progress.
/// `ContextAppend` is not terminal here: it is applied in place (logs
/// SupervisorAction + rebuilds context.md) and the wait continues - with the
/// same cursor, so neither the top-of-loop nor the next await_control call will
/// see it again.
pub(crate) fn await_control(
    run_dir: &Path,
    log: &mut EventLog,
    cursor: Option<u64>,
) -> Result<(Control, u64), EngineError> {
    let mut cursor = cursor;
    loop {
        for entry in read_control_after(run_dir, cursor)? {
            match entry.cmd {
                Control::ContextAppend { note } => {
                    log.append(EventPayload::SupervisorAction {
                        action: "context_append".into(),
                        node: None,
                        detail: note,
                    })?;
                    rebuild_context_md(run_dir)?;
                    cursor = Some(entry.seq);
                }
                other => return Ok((other, entry.seq)),
            }
        }
        std::thread::sleep(AWAIT_CONTROL_POLL);
    }
}

/// Rebuilds context.md as a materialized view from events.jsonl. A shared
/// helper for three call sites: after every executed node, on a proactive
/// ContextAppend in the top-of-loop scan, and on a ContextAppend inside
/// await_control - so that {{run.context}} in the next prompt render immediately
/// sees the new note.
pub(crate) fn rebuild_context_md(run_dir: &Path) -> Result<(), EngineError> {
    let ctx_md = build_context(&read_all(run_dir)?);
    apb_core::fsutil::atomic_write(&run_dir.join("context.md"), ctx_md.as_bytes())?;
    Ok(())
}
