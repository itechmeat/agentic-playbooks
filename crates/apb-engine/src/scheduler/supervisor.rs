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
    // Connector env isolation (spec 4.3): the supervisor is a spawned agent, so
    // scrub the union of installed connector tokens and hand it the run-context
    // env (its node id in the manifest is `supervisor`).
    let connector_policy = crate::adapter::ConnectorEnvPolicy {
        scrub: apb_core::connector::resolve::all_referenced_env_names(root),
        run_dir: Some(run_dir.clone()),
        node_id: Some("supervisor".to_string()),
    };
    let mut last_err: Option<String> = None;
    for inv in &entry.chain {
        let adapter = crate::adapter::ClaudeAdapter {
            program: inv.canonical_executable.to_string_lossy().into_owned(),
            spec: inv.spec.clone(),
        };
        match adapter.spawn_supervisor(&brief, &inv.model, root, soul, &connector_policy) {
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
/// see it again. `Progress` is likewise applied in place: it logs `RunProgress`
/// (stamped with `current_node`) and the wait continues.
pub(crate) fn await_control(
    run_dir: &Path,
    log: &mut EventLog,
    cursor: Option<u64>,
    current_node: &str,
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
                Control::Progress { done, total, label } => {
                    log.append(EventPayload::RunProgress {
                        node_id: current_node.to_string(),
                        done,
                        total,
                        label,
                    })?;
                    cursor = Some(entry.seq);
                }
                other => return Ok((other, entry.seq)),
            }
        }
        std::thread::sleep(AWAIT_CONTROL_POLL);
    }
}

/// Drains the `Control::Progress` commands pending right after a node finished
/// executing, stamping each `RunProgress` with `node` (the node that just ran)
/// and advancing `cursor` past them. Stops at the first non-Progress command,
/// leaving it untouched for the top-of-loop scan.
///
/// B2: a report the agent posts during `execute_node` of node A is otherwise
/// only drained at the next top-of-loop, by which point `current` has advanced
/// to the successor B, so the report is stamped with B. Draining here, before
/// frontier advancement, keeps the attribution on A. The remaining drains
/// (top-of-loop, await_control) still catch commands that arrive between nodes.
pub(crate) fn drain_progress_after_execute(
    run_dir: &Path,
    log: &mut EventLog,
    cursor: Option<u64>,
    node: &str,
) -> Result<Option<u64>, EngineError> {
    let mut cursor = cursor;
    for entry in read_control_after(run_dir, cursor)? {
        match entry.cmd {
            Control::Progress { done, total, label } => {
                log.append(EventPayload::RunProgress {
                    node_id: node.to_string(),
                    done,
                    total,
                    label,
                })?;
                cursor = Some(entry.seq);
            }
            _ => break,
        }
    }
    Ok(cursor)
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

#[cfg(test)]
mod tests {
    use crate::control::{Control, post_control};
    use crate::event::{EventLog, EventPayload, read_all};

    /// B2: a progress command posted while node `a` was executing is drained by
    /// `drain_progress_after_execute` with `node_id: "a"`, even though the next
    /// top-of-loop drain would have stamped the successor. This is the exact
    /// mechanism `post_supervisor_command` uses (a `Control::Progress` entry in
    /// the run's control channel); folding the resulting events proves the
    /// attribution deterministically, with no sleeps or timing races.
    #[test]
    fn drain_stamps_progress_with_the_executing_node() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();

        // The agent reports progress mid-execution of node `a`.
        post_control(
            &run_dir,
            Control::Progress {
                done: 2,
                total: 5,
                label: Some("chapter 2 of 5".into()),
            },
        )
        .unwrap();

        let cursor = super::drain_progress_after_execute(&run_dir, &mut log, None, "a").unwrap();
        assert_eq!(cursor, Some(0));

        let events = read_all(&run_dir).unwrap();
        let progress = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RunProgress {
                    node_id,
                    done,
                    total,
                    ..
                } => Some((node_id.clone(), *done, *total)),
                _ => None,
            })
            .expect("a RunProgress event must have been written");
        assert_eq!(progress, ("a".to_string(), 2, 5));
    }

    /// The drain consumes only the leading run of progress commands and stops at
    /// the first non-progress command, leaving it for the top-of-loop scan
    /// (Pause/Abort must not be swallowed here).
    #[test]
    fn drain_stops_at_first_non_progress_command() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();

        post_control(
            &run_dir,
            Control::Progress {
                done: 1,
                total: 3,
                label: None,
            },
        )
        .unwrap();
        post_control(&run_dir, Control::Pause).unwrap();

        let cursor = super::drain_progress_after_execute(&run_dir, &mut log, None, "a").unwrap();
        // Consumed the progress at seq 0, stopped before the pause at seq 1.
        assert_eq!(cursor, Some(0));
        let progress_count = read_all(&run_dir)
            .unwrap()
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::RunProgress { .. }))
            .count();
        assert_eq!(progress_count, 1);
    }
}
