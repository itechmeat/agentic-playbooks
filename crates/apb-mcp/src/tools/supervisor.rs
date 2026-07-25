//! The supervisor surface: everything a supervising agent can do to a run it
//! watches - wait for a wake, inspect, retry, reroute, pause, rebind an
//! executor, patch the playbook, append context, abort, and report.

use std::path::Path;
use std::time::Duration;

use super::run::run_status;
use super::{ToolError, open};
use apb_core::versioning::create_patch_version;
use apb_engine::control::Control;
use apb_engine::{
    post_supervisor_command, run_cancel, run_inspect as engine_run_inspect, touch_heartbeat,
    wait_wake, write_supervisor_report,
};
use serde_json::{Value, json};

/// Blockingly waits for the next wake (or a timeout/run completion) and
/// returns it along with a fresh status. `wake: null` means the run
/// has already finished, or the wait timed out - the agent decides for itself whether to
/// keep looping.
pub fn supervisor_wait_event(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    timeout_ms: Option<u64>,
) -> Result<Value, ToolError> {
    // A liveness mark for the background supervisor before the blocking wait:
    // a signal that the process watching the run is still alive and polling.
    touch_heartbeat(root, run_id)?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(25_000));
    let wake = wait_wake(root, run_id, after_seq, timeout)?;
    let status = run_status(root, run_id)?;
    // Surface the pending human-review gate here too (issue #42 finding 4): a
    // supervisor that wakes on a run must see the gate and its owner-facing
    // instruction so it relays the decision to the user rather than blocking.
    Ok(json!({
        "wake": wake,
        "run_status": status["run_status"],
        "pending_review": status["pending_review"],
        "pending_supervisor": status["pending_supervisor"],
    }))
}

/// A full run summary for the observer (status, nodes, context.md, wakes, actions, events).
pub fn sv_run_inspect(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    Ok(engine_run_inspect(root, run_id)?)
}

pub fn node_retry(
    root: &Path,
    run_id: &str,
    node: &str,
    prompt_override: Option<String>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Retry {
            node: node.to_string(),
            prompt_override,
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

pub fn run_continue_from(root: &Path, run_id: &str, node: &str) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::ContinueFrom {
            node: node.to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

pub fn run_pause(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(root, run_id, Control::Pause)?;
    Ok(json!({ "posted_seq": seq }))
}

/// Posts a `Control::Rebind` to switch a node's executor profile mid-run (issue
/// #45 finding 5). Writes no events - drive journals `profile_rebound` (or
/// `rebind_rejected`) when it applies the command (single-writer). `bundle` is
/// the digest the policy gate (`policy::check_rebind`) verified, pinned so drive
/// re-verifies the re-snapshotted profile against it (anti-TOCTOU). The gate runs
/// at the server boundary before this call, so an untrusted/unresolved profile
/// never reaches here.
pub fn rebind_profile(
    root: &Path,
    run_id: &str,
    node: &str,
    profile: &str,
    scope: apb_core::profile::ProfileScope,
    bundle: &str,
    reason: Option<String>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Rebind {
            node: node.to_string(),
            profile: profile.to_string(),
            scope,
            bundle: bundle.to_string(),
            reason,
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

pub fn run_abort(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    run_cancel(root, run_id)?;
    Ok(json!({ "ok": true }))
}

pub fn context_append(root: &Path, run_id: &str, note: &str) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::ContextAppend {
            note: note.to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

/// Requests interruption of the run's currently RUNNING attempt (finding 7 of
/// issue #42, third item of issue #40). Posts `Control::Interrupt`; the
/// attempt's own poll loop observes it live, SIGKILLs the agent, and journals
/// `attempt_interrupted`. The killed attempt is journaled failed, so ordinary
/// retry/fallback/patch then proceeds at the next attempt boundary - the point
/// being a supervisor can now force the attempt boundary of a wedged attempt
/// (typically after a stall anomaly woke it) rather than waiting out a hang that
/// may never end. Unlike `run_abort` this does NOT stop the run. An interrupt
/// with no attempt running is a harmless no-op. The response reports
/// `posted_seq`; the resulting `control_received`/`attempt_interrupted` events
/// are visible via `supervisor_run_inspect` and `run_events`, so a supervisor
/// can confirm the message was received live.
pub fn interrupt_attempt(
    root: &Path,
    run_id: &str,
    reason: Option<&str>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Interrupt {
            reason: reason.unwrap_or("supervisor interrupt").to_string(),
        },
    )?;
    Ok(json!({ "posted_seq": seq }))
}

/// Creates a patch version of the playbook from patched YAML and posts a run
/// migration command. Writes no events - drive will write them when applying
/// `Control::Patch` (single-writer). The patch's base is the run's active version.
pub fn playbook_patch(
    root: &Path,
    run_id: &str,
    yaml: &str,
    classification: &str,
    continue_from: &str,
) -> Result<Value, ToolError> {
    if !matches!(classification, "improvement" | "workaround") {
        return Err(ToolError::Engine(format!(
            "invalid classification `{classification}`"
        )));
    }
    let (id, base_version) = apb_engine::scheduler::run_playbook_ref(root, run_id)?;
    let version = create_patch_version(root, &id, &base_version, yaml, run_id, classification)?;
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Patch {
            version: version.clone(),
            classification: classification.to_string(),
            continue_from: continue_from.to_string(),
        },
    )?;
    Ok(json!({ "version": version, "posted_seq": seq }))
}

/// Writes the supervisor's final report to `runs/<run_id>/supervisor/report.md`.
pub fn supervisor_report(root: &Path, run_id: &str, text: &str) -> Result<Value, ToolError> {
    write_supervisor_report(root, run_id, text)?;
    Ok(json!({ "ok": true }))
}

/// Extracts the capability list from `playbook.supervisor.policy.capabilities`.
/// Distinguishes an absent key (default) from a present one (exact value):
/// - key absent -> default `["observe", "retry", "rebind", "patch_playbook"]`
///   (all implemented capabilities, see spec 9.5: the default is all)
/// - key present as a sequence -> its strings (empty if empty)
/// - key present as a scalar string -> a single-element list
/// - key present as another type -> empty (deny all)
pub fn supervisor_capabilities(
    root: &Path,
    id: &str,
    version: Option<&str>,
) -> Result<Vec<String>, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, version)?;

    let caps = match loaded
        .playbook
        .supervisor
        .as_ref()
        .and_then(|s| s.policy.as_ref())
        .and_then(|p| p.get("capabilities"))
    {
        None => vec![
            "observe".to_string(),
            "retry".to_string(),
            "rebind".to_string(),
            "patch_playbook".to_string(),
        ],
        Some(v) if v.is_sequence() => v
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Some(v) if v.as_str().is_some() => {
            vec![v.as_str().unwrap().to_string()]
        }
        Some(_) => Vec::new(),
    };

    // A frozen playbook cannot be patched, so never advertise `patch_playbook`:
    // the supervisor still observes and retries within the current run, but the
    // definition is off the table (enforced in core too, this just keeps the
    // advertised capability honest).
    let caps = if reg.is_frozen(id) {
        caps.into_iter().filter(|c| c != "patch_playbook").collect()
    } else {
        caps
    };

    Ok(caps)
}
