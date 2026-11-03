//! The tool set of a read-only server (`apb mcp --read-only`).
//!
//! An explicit allowlist, not the tools' `read_only_hint`: a hint describes
//! a tool, while this table decides what a read-only server serves. Every
//! tool of the full server is in exactly one of [`READ_ONLY`] and
//! [`REMOVED_IN_READ_ONLY`] (a test checks it), so a new tool is served
//! read-only only once someone puts it in the first table. A removed tool
//! is not routed at all: unknown to `tools/list` and to a direct
//! `tools/call` alike.
//!
//! The allowlist is the first of three layers. Under it the process guard
//! ([`apb_core::process_mode`]) refuses every write, program start and
//! decision request the allowed tools could still reach, and the end to
//! end test compares the project trees and the config directory before and
//! after a session that called every allowed tool.

/// How a read-only server serves an allowed tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyUse {
    /// As the full server does.
    Full,
    /// With the arguments that would write, start a program or send a
    /// request refused or ignored (see the tool).
    Restricted,
}

/// The tools a read-only server serves.
pub const READ_ONLY: &[(&str, ReadOnlyUse)] = &[
    ("playbook_list", ReadOnlyUse::Full),
    ("playbook_get", ReadOnlyUse::Full),
    ("playbook_validate", ReadOnlyUse::Full),
    ("playbook_howto", ReadOnlyUse::Full),
    ("playbook_interview", ReadOnlyUse::Full),
    ("setup_howto", ReadOnlyUse::Full),
    ("playbook_adopt_report", ReadOnlyUse::Full),
    ("playbook_trash_list", ReadOnlyUse::Full),
    ("playbook_candidate", ReadOnlyUse::Full),
    ("playbook_known_problems", ReadOnlyUse::Full),
    ("playbook_discover_results", ReadOnlyUse::Full),
    ("projects_list", ReadOnlyUse::Full),
    ("profile_list", ReadOnlyUse::Full),
    ("profile_get", ReadOnlyUse::Full),
    ("profile_howto", ReadOnlyUse::Full),
    ("connectors_list", ReadOnlyUse::Full),
    ("trust_list", ReadOnlyUse::Full),
    ("runs_list", ReadOnlyUse::Full),
    ("run_status", ReadOnlyUse::Full),
    ("run_wait", ReadOnlyUse::Full),
    ("run_events", ReadOnlyUse::Full),
    ("run_report", ReadOnlyUse::Full),
    ("run_retro_context", ReadOnlyUse::Full),
    ("run_grades", ReadOnlyUse::Full),
    // `query` is not ranked: ranking asks a decision model.
    ("playbook_catalog", ReadOnlyUse::Restricted),
    // `refresh: true` is refused: a refresh starts every agent's program.
    ("agents_detect", ReadOnlyUse::Restricted),
    // `action: get` only.
    ("playbook_memory", ReadOnlyUse::Restricted),
];

/// Every other tool of the full server: what starts or controls a run,
/// supervises, decides, grades, authors, trusts or discovers.
pub const REMOVED_IN_READ_ONLY: &[&str] = &[
    // Run starts and launch preparation.
    "playbook_run",
    "playbook_trial",
    "playbook_prepare_run",
    "playbook_execute_plan",
    // Run control.
    "run_stop",
    "run_pause",
    "run_resume",
    "run_answer",
    "review_decide",
    "run_task_submit",
    "run_intent_resolve",
    "run_notify",
    "run_progress_report",
    "run_report_html",
    // Supervision.
    "supervisor_wait_event",
    "supervisor_run_inspect",
    "supervisor_report",
    "supervisor_node_retry",
    "supervisor_run_continue_from",
    "supervisor_run_pause",
    "supervisor_run_abort",
    "supervisor_context_append",
    "supervisor_interrupt_attempt",
    "supervisor_rebind_profile",
    "supervisor_patch_playbook",
    // Decisions and grading.
    "decision_ask",
    "run_grade",
    "run_grade_start",
    "run_grade_read",
    "run_grade_submit",
    "known_problem_recheck",
    "known_problem_decide",
    // Authoring and trust.
    "playbook_create",
    "playbook_update",
    "playbook_delete",
    "playbook_trash_restore",
    "playbook_make_global",
    "playbook_capture",
    "playbook_approve",
    "suggestion_dismiss",
    "trust_revoke",
    "profile_write",
    "profile_move",
    "profile_delete",
    "subscriptions_set",
    // Setup: detection starts every agent's program and the sandbox probe
    // starts bubblewrap.
    "setup_plan",
    // Discovery.
    "playbook_discover",
    "playbook_discover_save",
    "playbook_discover_dismiss",
];

/// Whether a read-only server serves `tool`.
pub fn serves(tool: &str) -> bool {
    READ_ONLY.iter().any(|(name, _)| *name == tool)
}

/// The answer of a restricted tool to an argument a read-only server does
/// not take.
pub(crate) fn refused(detail: &str) -> serde_json::Value {
    serde_json::json!({ "error": "read_only", "detail": detail })
}
