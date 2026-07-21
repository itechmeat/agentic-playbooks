use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::event::{EventPayload, WakeTrigger, now_millis, read_all};
use crate::state::RunState;

/// A wake event handed to the calling code: the first `WakeRaised` after the cursor.
#[derive(Debug, Clone, Serialize)]
pub struct WakeEvent {
    pub seq: u64,
    pub trigger: WakeTrigger,
    pub node: String,
    pub detail: String,
}

/// Checks `run_id` for directory traversal and that the run directory
/// exists. Shared entry point for `wait_wake` and `run_inspect`.
fn resolve_run_dir(root: &Path, run_id: &str) -> Result<PathBuf, EngineError> {
    if !apb_core::registry::is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    Ok(run_dir)
}

/// Blocking poll of `events.jsonl` waiting for the first `WakeRaised` with
/// seq strictly greater than `after_seq` (`None` is equivalent to a cursor of
/// -1, i.e. any seq qualifies).
///
/// If a terminal run event (`RunFinished`/`RunAborted`) is encountered
/// earlier in seq order, the run has ended and there is nothing left to wake
/// for, so `Ok(None)` is returned. Once `timeout` elapses with no match, that
/// is also `Ok(None)`; the decision to wait again is left to the calling
/// tool (4b).
pub fn wait_wake(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    timeout: Duration,
) -> Result<Option<WakeEvent>, EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let cursor: i128 = after_seq.map(i128::from).unwrap_or(-1);
    let deadline = Instant::now() + timeout;
    loop {
        let events = read_all(&run_dir)?;
        for event in &events {
            if i128::from(event.seq) <= cursor {
                continue;
            }
            match &event.payload {
                EventPayload::WakeRaised {
                    trigger,
                    node,
                    detail,
                } => {
                    return Ok(Some(WakeEvent {
                        seq: event.seq,
                        trigger: *trigger,
                        node: node.clone(),
                        detail: detail.clone(),
                    }));
                }
                EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. } => {
                    return Ok(None);
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(Duration::from_millis(50));
    }
}

/// Assembled run summary for an observer: status, nodes, outputs, contents of
/// `context.md`, the full event list, and extracted `WakeRaised`/`SupervisorAction`
/// entries. This is the engine-side collector - the MCP tool `run_inspect`
/// (phase 4b) will be a thin wrapper around it.
pub fn run_inspect(root: &Path, run_id: &str) -> Result<serde_json::Value, EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let events = read_all(&run_dir)?;
    let state = RunState::fold(&events);

    let context = std::fs::read_to_string(run_dir.join("context.md")).unwrap_or_default();

    let nodes: BTreeMap<String, String> = state
        .nodes
        .iter()
        .map(|(node, status)| (node.clone(), status.as_str().to_string()))
        .collect();

    let wakes: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::WakeRaised {
                trigger,
                node,
                detail,
            } => Some(serde_json::json!({
                "seq": e.seq,
                "trigger": trigger,
                "node": node,
                "detail": detail,
            })),
            _ => None,
        })
        .collect();

    let actions: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SupervisorAction {
                action,
                node,
                detail,
            } => Some(serde_json::json!({
                "seq": e.seq,
                "action": action,
                "node": node,
                "detail": detail,
            })),
            _ => None,
        })
        .collect();

    // The pending human-review gate, if any (issue #42 finding 4): the observer
    // sees it through `supervisor_run_inspect` too, not only `run_status`, so a
    // gate is never surfaced in one supervisor path but hidden in the other.
    let pending_review =
        crate::progress::from_run_dir(&run_dir, &events).and_then(|p| p.pending_review);

    Ok(serde_json::json!({
        "run_id": run_id,
        "run_status": state.run_status.as_str(),
        "nodes": nodes,
        "outputs": state.outputs,
        "context": context,
        "wakes": wakes,
        "actions": actions,
        "pending_review": pending_review,
        "events": events,
    }))
}

/// Writes the supervisor report to `run_dir/supervisor/report.md` atomically.
/// An unsafe `run_id` or a missing run directory yields `NotFound`, as in
/// every other function in this module.
pub fn write_supervisor_report(root: &Path, run_id: &str, text: &str) -> Result<(), EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let report_path = run_dir.join("supervisor").join("report.md");
    apb_core::fsutil::atomic_write(&report_path, text.as_bytes())?;
    Ok(())
}

pub fn read_supervisor_report(root: &Path, run_id: &str) -> Result<Option<String>, EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let report_path = run_dir.join("supervisor").join("report.md");
    if !report_path.is_file() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(&report_path)?))
}

/// String representation of the wake trigger for the markdown summary
/// (matches the serde snake_case renaming of `WakeTrigger`).
fn trigger_str(trigger: &WakeTrigger) -> &'static str {
    match trigger {
        WakeTrigger::NodeFailed => "node_failed",
        WakeTrigger::NodeTimeout => "node_timeout",
        WakeTrigger::Anomaly => "anomaly",
    }
}

/// The supervisor report a human will see: if a report was explicitly
/// submitted (`supervisor/report.md`) - it is returned as is; otherwise a
/// markdown auto-summary is built from the run's events - the final status,
/// the list of wake events (`WakeRaised`), and supervisor interventions
/// (`SupervisorAction`). Empty sections are omitted.
pub fn supervisor_report_or_summary(root: &Path, run_id: &str) -> Result<String, EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    if let Some(report) = read_supervisor_report(root, run_id)? {
        return Ok(report);
    }

    let events = read_all(&run_dir)?;
    let state = RunState::fold(&events);

    let mut out = String::new();
    let _ = write!(
        out,
        "# Supervisor report\n\nrun_status: {}\n\n",
        state.run_status.as_str()
    );

    let mut wakes_body = String::new();
    for e in &events {
        if let EventPayload::WakeRaised {
            trigger,
            node,
            detail,
        } = &e.payload
        {
            let _ = writeln!(
                wakes_body,
                "- {} on `{node}`: {detail}",
                trigger_str(trigger)
            );
        }
    }
    if !wakes_body.is_empty() {
        let _ = write!(out, "## Wakes\n\n{wakes_body}\n");
    }

    let mut actions_body = String::new();
    for e in &events {
        if let EventPayload::SupervisorAction {
            action,
            node,
            detail,
        } = &e.payload
        {
            let node_str = node.as_deref().unwrap_or("-");
            let _ = writeln!(actions_body, "- {action} on `{node_str}`: {detail}");
        }
    }
    if !actions_body.is_empty() {
        let _ = write!(out, "## Interventions\n\n{actions_body}\n");
    }

    Ok(out)
}

/// Persistent supervisor session on disk: lets a separate `apb mcp` process
/// (for a background supervisor agent, Task 4) validate a token without
/// sharing the in-memory session table with the process that issued that
/// token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    /// Irreversible token fingerprint (`sha256:<hex>`), NOT the token itself:
    /// the secret is never written to disk. Validation compares the
    /// fingerprint of the presented token (see `find_session_by_token`), so a
    /// reversible form is not needed.
    pub token_hash: String,
    pub capabilities: Vec<String>,
}

/// Writes the supervisor session to `run_dir/supervisor/session.json`
/// atomically. Only the token's fingerprint reaches disk, not the token itself.
pub fn write_supervisor_session(
    root: &Path,
    run_id: &str,
    token: &str,
    capabilities: &[String],
) -> Result<(), EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let session_path = run_dir.join("supervisor").join("session.json");
    let session = PersistedSession {
        token_hash: apb_core::content::sha256_hex(token.as_bytes()),
        capabilities: capabilities.to_vec(),
    };
    let bytes = serde_json::to_vec(&session).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write(&session_path, &bytes)?;
    Ok(())
}

/// Scans `.apb/runs/*/supervisor/session.json` looking for a session with
/// the given token. Broken/unreadable files for individual runs are skipped,
/// so one corrupted run does not break resolution for the rest (same as
/// `list_runs`). A missing runs directory or no match yields `Ok(None)`.
pub fn find_session_by_token(
    root: &Path,
    token: &str,
) -> Result<Option<(String, Vec<String>)>, EngineError> {
    let runs_dir = root.join(".apb/runs");
    if !runs_dir.is_dir() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(&runs_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let is_dir = match entry.file_type() {
            Ok(ft) => ft.is_dir(),
            Err(_) => continue,
        };
        if !is_dir {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        let session_path = entry.path().join("supervisor").join("session.json");
        let bytes = match std::fs::read(&session_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let session: PersistedSession = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Compare the fingerprint of the presented token with the stored
        // fingerprint: the token itself is never stored on disk.
        if session.token_hash == apb_core::content::sha256_hex(token.as_bytes()) {
            return Ok(Some((run_id, session.capabilities)));
        }
    }
    Ok(None)
}

/// Updates the background supervisor's liveness marker: writes the current
/// unix timestamp (ms) to `run_dir/supervisor/heartbeat`.
pub fn touch_heartbeat(root: &Path, run_id: &str) -> Result<(), EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let heartbeat_path = run_dir.join("supervisor").join("heartbeat");
    apb_core::fsutil::atomic_write(&heartbeat_path, now_millis().to_string().as_bytes())?;
    Ok(())
}

/// Age of the last heartbeat in milliseconds. A missing or
/// unreadable/unparseable file yields `Ok(None)` (the supervisor may simply
/// not have started polling yet; this is not an error).
pub fn heartbeat_age_ms(root: &Path, run_id: &str) -> Result<Option<u128>, EngineError> {
    let run_dir = resolve_run_dir(root, run_id)?;
    let heartbeat_path = run_dir.join("supervisor").join("heartbeat");
    if !heartbeat_path.is_file() {
        return Ok(None);
    }
    let text = match std::fs::read_to_string(&heartbeat_path) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    let stored: u128 = match text.trim().parse() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    Ok(Some(now_millis().saturating_sub(stored)))
}

/// Pure decision function: is it time to declare the supervisor lost.
/// A missing heartbeat (`None`) is not a loss: the supervisor agent may
/// simply not have started polling yet. An already-logged loss is not
/// repeated.
pub fn should_declare_lost(age_ms: Option<u128>, threshold_ms: u128, already_logged: bool) -> bool {
    matches!(age_ms, Some(a) if a > threshold_ms) && !already_logged
}

/// Background supervisor silence in milliseconds: if a heartbeat has already
/// touched liveness - its age (`heartbeat_age_ms`); otherwise, if the spawn
/// moment is known (`supervisor/spawned_at`) - the time since that moment,
/// so that "the supervisor agent never started polling" can be detected even
/// before the first heartbeat. If neither heartbeat nor spawned_at is
/// recorded (the run does not wait on a supervisor at all) - `None`.
pub fn supervisor_silence_ms(root: &Path, run_id: &str) -> Result<Option<u128>, EngineError> {
    if let Some(age) = heartbeat_age_ms(root, run_id)? {
        return Ok(Some(age));
    }
    let run_dir = resolve_run_dir(root, run_id)?;
    let spawned_at_path = run_dir.join("supervisor").join("spawned_at");
    if !spawned_at_path.is_file() {
        return Ok(None);
    }
    let text = match std::fs::read_to_string(&spawned_at_path) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    let spawned_at: u128 = match text.trim().parse() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    Ok(Some(now_millis().saturating_sub(spawned_at)))
}
