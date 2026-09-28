//! The supervisor surface: everything a supervising agent can do to a run it
//! watches - wait for a wake, inspect, retry, reroute, pause, rebind an
//! executor, patch the playbook, append context, abort, and report.

use std::path::Path;

use super::run::{run_status, wait_timeout};
use super::{ToolError, open};
use apb_core::versioning::create_patch_version;
use apb_engine::control::Control;
use apb_engine::run_wait::{SupervisorWait, clip_tail, wait_supervisor_event};
use apb_engine::{
    post_supervisor_command, run_cancel, run_inspect as engine_run_inspect, write_supervisor_report,
};
use serde_json::{Value, json};

/// Byte cap for a wake's `detail` in the `supervisor_wait_event` answer. The
/// detail of a failed node is its whole output, which can run to hundreds of
/// kilobytes; the supervisor pays for it on the wake turn and again on every
/// later turn of its conversation. The tail (where a failure usually says
/// what went wrong) is kept; `supervisor_run_inspect` has the full output.
pub const WAKE_DETAIL_MAX_BYTES: usize = 16 * 1024;

/// Blockingly waits for the next wake, a new human-review gate, the end of
/// the run, or the timeout, and returns it along with a fresh status. `wake:
/// null` with a non-terminal `run_status` and no `pending_review` means only
/// that the wait timed out. `next_after_seq` is the cursor to pass on the
/// next call. The supervisor heartbeat stays fresh for the whole wait, so a
/// long timeout is safe (and cheaper: every return is a model turn).
pub fn supervisor_wait_event(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    timeout_ms: Option<u64>,
) -> Result<Value, ToolError> {
    let outcome = wait_supervisor_event(root, run_id, after_seq, wait_timeout(timeout_ms))?;
    supervisor_wait_result(root, run_id, after_seq, &outcome)
}

/// The `supervisor_wait_event` answer for one wait outcome.
pub fn supervisor_wait_result(
    root: &Path,
    run_id: &str,
    after_seq: Option<u64>,
    outcome: &SupervisorWait,
) -> Result<Value, ToolError> {
    let status = run_status(root, run_id)?;
    let (wake, next_after_seq, reason) = match outcome {
        SupervisorWait::Wake(w) => {
            let (detail, clipped) = clip_tail(&w.detail, WAKE_DETAIL_MAX_BYTES);
            let mut v = json!({
                "seq": w.seq,
                "trigger": w.trigger,
                "node": w.node,
                "detail": detail,
            });
            if clipped {
                v["detail_truncated"] = json!(true);
            }
            // The decision model's advisory recommendation on a park wake
            // (issue #165 Part 10); absent otherwise, so the shape is
            // unchanged without it.
            if let Some(t) = &w.triage {
                v["triage"] = json!(t);
            }
            (v, Some(w.seq), "wake")
        }
        SupervisorWait::Review { seq, .. } => (Value::Null, Some(*seq), "review"),
        SupervisorWait::Ended => (Value::Null, after_seq, "ended"),
        SupervisorWait::TimedOut => (Value::Null, after_seq, "timeout"),
    };
    // Surface the pending human-review gate here too (issue #42 finding 4): a
    // supervisor that wakes on a run must see the gate and its owner-facing
    // instruction so it relays the decision to the user rather than blocking.
    Ok(json!({
        "wake": wake,
        "reason": reason,
        "next_after_seq": next_after_seq,
        "run_status": status["run_status"],
        "pending_review": status["pending_review"],
        "pending_supervisor": status["pending_supervisor"],
    }))
}

/// Strings in the `events` list longer than this are elided by default in
/// `supervisor_run_inspect`: they are node outputs and wake details the same
/// answer already carries in `outputs`, `context` and `wakes`.
pub const INSPECT_EVENT_TEXT_MAX_BYTES: usize = 512;

/// A run summary for the observer (status, nodes, outputs, context.md, wakes,
/// actions, events), with the long texts inside `events` elided: every node
/// output would otherwise ship three times (outputs, context, events).
pub fn sv_run_inspect(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    sv_run_inspect_with(root, run_id, false)
}

/// [`sv_run_inspect`]; `full_events` keeps the raw event texts verbatim.
pub fn sv_run_inspect_with(
    root: &Path,
    run_id: &str,
    full_events: bool,
) -> Result<Value, ToolError> {
    let mut v = engine_run_inspect(root, run_id)?;
    if !full_events && let Some(events) = v.get_mut("events").and_then(Value::as_array_mut) {
        for e in events {
            elide_long_strings(e);
        }
    }
    Ok(v)
}

fn elide_long_strings(v: &mut Value) {
    match v {
        Value::String(s) if s.len() > INSPECT_EVENT_TEXT_MAX_BYTES => {
            *s = format!(
                "[{} bytes elided: see outputs, context or wakes; full_events: true for the raw text]",
                s.len()
            );
        }
        Value::Array(a) => a.iter_mut().for_each(elide_long_strings),
        Value::Object(o) => o.values_mut().for_each(elide_long_strings),
        _ => {}
    }
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
///
/// `node` makes the interrupt TARGETED (spec 2026-08-05 section 1.6): only that
/// node's running attempt observes the entry and dies, so a wedged branch of a
/// concurrent fan-out can be broken without touching its healthy siblings. Omit
/// it for the historical broadcast: every attempt running in the run dies.
pub fn interrupt_attempt(
    root: &Path,
    run_id: &str,
    reason: Option<&str>,
    node: Option<&str>,
) -> Result<Value, ToolError> {
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Interrupt {
            reason: reason.unwrap_or("supervisor interrupt").to_string(),
            node: node.map(str::to_string),
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
