//! Single integration-test binary for apb-engine. Cargo compiles every file
//! directly under `tests/` into its own test binary, and on macOS each fresh
//! binary pays a per-spawn security scan. Collapsing the 46 former
//! `tests/*.rs` files into modules under `tests/suite/`, driven from this one
//! `main.rs`, reduces that to a single integration binary.
//!
//! Content of the moved files is unchanged except for three mechanical fixes
//! forced by the move:
//!   * The former `#![cfg(unix)]` inner attribute on `process_group_test.rs`,
//!     `finish_answer_test.rs`, and `profile_run_test.rs` is converted to an
//!     outer `#[cfg(unix)]` on the `mod` line below.
//!   * Every former private per-file `static ENV_LOCK` (six files) is replaced
//!     with the single shared lock in `suite/common/mod.rs`, and every test
//!     that mutates process-wide env now takes that one lock. Running as
//!     modules of one process means separate per-file locks no longer prevent
//!     races *between* files (see `suite/common/mod.rs`).
//!   * The 16 files that used `mod common;` now say `use crate::common;`, so
//!     there is exactly one `common` module compiled once (declared here).

#[path = "suite/common/mod.rs"]
mod common;

// Config isolation (issue #165 P1 open item 5): every engine test runs with
// an isolated global config dir, so a developer's real
// `~/.config/apb/decisions.yaml` (or any other global file) can never make a
// test call a provider or read personal settings. Many tests set
// `APB_CONFIG_DIR` themselves and remove it afterwards, which would fall back
// to `XDG_CONFIG_HOME` or `HOME`; pointing `XDG_CONFIG_HOME` at a sandbox for
// the whole process, before any test thread starts, closes that fallback.
// Tests that need their own config dir still set `APB_CONFIG_DIR`, which wins.
#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
static ISOLATE_CONFIG: extern "C" fn() = isolate_config;

extern "C" fn isolate_config() {
    // The sandbox helper below runs this binary inside the sandbox; the
    // order of `.init_array` entries is not guaranteed, so the helper's
    // process must not get this setup (the real `apb __sandbox-exec` never
    // does). A panic here would abort the process, which is acceptable for a
    // test harness.
    #[cfg(target_os = "linux")]
    if std::fs::read("/proc/self/cmdline").is_ok_and(|raw| {
        raw.split(|b| *b == 0)
            .nth(1)
            .is_some_and(|a| a == b"__sandbox-exec" || a == b"__test-abstract-connect")
    }) {
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("apb-engine-test-xdg-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: runs before `main`, so no other thread exists yet.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// The sandbox helper (`apb __sandbox-exec`): the engine runs its own
// executable inside an isolation sandbox, which under this suite is this test
// binary. When bwrap starts it that way, it applies the scope and execs the
// agent before the test harness ever runs, exactly as `apb` does. The same
// hook serves one probe of `sandbox_run_test`.
#[cfg(target_os = "linux")]
#[used]
#[unsafe(link_section = ".init_array")]
static SANDBOX_HELPER: extern "C" fn() = sandbox_helper;

#[cfg(target_os = "linux")]
extern "C" fn sandbox_helper() {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(raw) = std::fs::read("/proc/self/cmdline") else {
        return;
    };
    let args: Vec<std::ffi::OsString> = raw
        .split(|b| *b == 0)
        .map(|a| std::ffi::OsStr::from_bytes(a).to_os_string())
        .collect();
    let end = args.len() - usize::from(args.last().is_some_and(|a| a.is_empty()));
    match args.get(1).and_then(|a| a.to_str()) {
        Some("__sandbox-exec") => {
            std::process::exit(apb_engine::sandbox::scope::exec_main(&args[2..end]));
        }
        // A probe the sandbox tests run inside the sandbox: connect to the
        // abstract unix socket named by the next argument (0: connected).
        Some("__test-abstract-connect") => {
            use std::os::linux::net::SocketAddrExt as _;
            let name = args
                .get(2)
                .map(|a| a.as_bytes().to_vec())
                .unwrap_or_default();
            let ok = std::os::unix::net::SocketAddr::from_abstract_name(&name)
                .and_then(|addr| std::os::unix::net::UnixStream::connect_addr(&addr))
                .is_ok();
            std::process::exit(if ok { 0 } else { 3 });
        }
        _ => {}
    }
}

#[path = "suite/acp_adapter_test.rs"]
mod acp_adapter_test;
#[path = "suite/acp_config_test.rs"]
mod acp_config_test;
#[cfg(unix)]
#[path = "suite/adapter_bounded_waits_test.rs"]
mod adapter_bounded_waits_test;
#[cfg(unix)]
#[path = "suite/adapter_output_test.rs"]
mod adapter_output_test;
#[path = "suite/adapter_test.rs"]
mod adapter_test;
#[cfg(unix)]
#[path = "suite/agent_contract/mod.rs"]
mod agent_contract;
#[cfg(unix)]
#[path = "suite/agent_env_live_test.rs"]
mod agent_env_live_test;
#[cfg(unix)]
#[path = "suite/agent_env_run_test.rs"]
mod agent_env_run_test;
#[cfg(unix)]
#[path = "suite/agent_env_spawn_test.rs"]
mod agent_env_spawn_test;
#[cfg(unix)]
#[path = "suite/agent_probe_test.rs"]
mod agent_probe_test;
#[path = "suite/agent_report_test.rs"]
mod agent_report_test;
#[path = "suite/agent_timeout_test.rs"]
mod agent_timeout_test;
#[cfg(unix)]
#[path = "suite/attempt_briefing_test.rs"]
mod attempt_briefing_test;
#[cfg(unix)]
#[path = "suite/attempt_notes_test.rs"]
mod attempt_notes_test;
#[cfg(unix)]
#[path = "suite/attempt_usage_test.rs"]
mod attempt_usage_test;
#[path = "suite/background_run_test.rs"]
mod background_run_test;
#[path = "suite/background_supervisor_test.rs"]
mod background_supervisor_test;
#[cfg(unix)]
#[path = "suite/baseline_test.rs"]
mod baseline_test;
#[cfg(unix)]
#[path = "suite/bounded_output_test.rs"]
mod bounded_output_test;
#[cfg(unix)]
#[path = "suite/budget_gate_test.rs"]
mod budget_gate_test;
#[path = "suite/cache_test.rs"]
mod cache_test;
#[cfg(unix)]
#[path = "suite/candidate_quality_test.rs"]
mod candidate_quality_test;
#[cfg(unix)]
#[path = "suite/candidate_trial_test.rs"]
mod candidate_trial_test;
#[path = "suite/child_connector_prepare.rs"]
mod child_connector_prepare;
#[path = "suite/child_run_event_test.rs"]
mod child_run_event_test;
#[path = "suite/connector_asana.rs"]
mod connector_asana;
#[path = "suite/connector_call.rs"]
mod connector_call;
#[path = "suite/connector_contract_inbox.rs"]
mod connector_contract_inbox;
#[path = "suite/connector_discord.rs"]
mod connector_discord;
#[path = "suite/connector_e2e.rs"]
mod connector_e2e;
#[path = "suite/connector_gitlab.rs"]
mod connector_gitlab;
#[path = "suite/connector_healthcheck.rs"]
mod connector_healthcheck;
#[path = "suite/connector_imap.rs"]
mod connector_imap;
#[path = "suite/connector_inbox.rs"]
mod connector_inbox;
#[path = "suite/connector_manifest.rs"]
mod connector_manifest;
#[path = "suite/connector_play_call.rs"]
mod connector_play_call;
#[path = "suite/connector_run.rs"]
mod connector_run;
#[path = "suite/connector_slack.rs"]
mod connector_slack;
#[path = "suite/connector_smtp.rs"]
mod connector_smtp;
#[path = "suite/connector_youtrack.rs"]
mod connector_youtrack;
#[path = "suite/connector_zulip.rs"]
mod connector_zulip;
#[path = "suite/consent_test.rs"]
mod consent_test;
#[path = "suite/context_compaction_test.rs"]
mod context_compaction_test;
#[path = "suite/context_test.rs"]
mod context_test;
#[path = "suite/control_liveness_test.rs"]
mod control_liveness_test;
#[path = "suite/control_test.rs"]
mod control_test;
#[path = "suite/decision_test.rs"]
mod decision_test;
#[cfg(unix)]
#[path = "suite/decision_uses_test.rs"]
mod decision_uses_test;
#[path = "suite/detached_driver_test.rs"]
mod detached_driver_test;
#[path = "suite/digest_binding_test.rs"]
mod digest_binding_test;
#[path = "suite/event_test.rs"]
mod event_test;
#[cfg(unix)]
#[path = "suite/failure_class_test.rs"]
mod failure_class_test;
#[path = "suite/failure_reason_test.rs"]
mod failure_reason_test;
#[path = "suite/failure_stop_test.rs"]
#[cfg(unix)]
mod failure_stop_test;
#[cfg(unix)]
#[path = "suite/finish_answer_test.rs"]
mod finish_answer_test;
#[cfg(unix)]
#[path = "suite/finish_context_test.rs"]
mod finish_context_test;
#[path = "suite/fit_test.rs"]
mod fit_test;
#[cfg(unix)]
#[path = "suite/fork_failure_test.rs"]
mod fork_failure_test;
#[cfg(unix)]
#[path = "suite/git_hardening_test.rs"]
mod git_hardening_test;
#[path = "suite/global_config_test.rs"]
mod global_config_test;
#[path = "suite/global_scope_run_test.rs"]
mod global_scope_run_test;
#[path = "suite/goal_criteria_test.rs"]
mod goal_criteria_test;
#[path = "suite/grade_rules_test.rs"]
mod grade_rules_test;
#[path = "suite/grade_session_test.rs"]
mod grade_session_test;
#[path = "suite/heartbeat_lost_test.rs"]
mod heartbeat_lost_test;
#[cfg(unix)]
#[path = "suite/host_mode_test.rs"]
mod host_mode_test;
#[path = "suite/inspect_wait_test.rs"]
mod inspect_wait_test;
#[path = "suite/instruction_precedence_test.rs"]
mod instruction_precedence_test;
#[path = "suite/interaction_defaults_test.rs"]
mod interaction_defaults_test;
#[path = "suite/interactive_live_test.rs"]
mod interactive_live_test;
#[path = "suite/interactive_reprompt_test.rs"]
mod interactive_reprompt_test;
#[path = "suite/interactive_timeout_test.rs"]
mod interactive_timeout_test;
#[cfg(unix)]
#[path = "suite/join_loop_test.rs"]
mod join_loop_test;
#[cfg(unix)]
#[path = "suite/join_revive_test.rs"]
mod join_revive_test;
#[path = "suite/journal_compat_test.rs"]
mod journal_compat_test;
#[path = "suite/judge_test.rs"]
mod judge_test;
#[cfg(unix)]
#[path = "suite/known_problems_test.rs"]
mod known_problems_test;
#[path = "suite/lineage_test.rs"]
mod lineage_test;
#[cfg(unix)]
#[path = "suite/list_resume_test.rs"]
mod list_resume_test;
#[cfg(unix)]
#[path = "suite/list_run_test.rs"]
mod list_run_test;
#[path = "suite/list_runs_resilient_test.rs"]
mod list_runs_resilient_test;
#[path = "suite/loop_edges_test.rs"]
mod loop_edges_test;
#[path = "suite/marker_test.rs"]
mod marker_test;
#[path = "suite/max_loops_test.rs"]
mod max_loops_test;
#[path = "suite/migrate_test.rs"]
mod migrate_test;
#[path = "suite/missing_input_test.rs"]
mod missing_input_test;
#[path = "suite/model_grade_test.rs"]
mod model_grade_test;
#[cfg(unix)]
#[cfg(unix)]
#[path = "suite/named_outputs_test.rs"]
mod named_outputs_test;
#[path = "suite/notify_send_test.rs"]
mod notify_send_test;
#[path = "suite/notify_transitions_test.rs"]
mod notify_transitions_test;
#[cfg(unix)]
#[path = "suite/operator_pause_test.rs"]
mod operator_pause_test;
#[path = "suite/output_field_test.rs"]
mod output_field_test;
#[path = "suite/overrides_run_test.rs"]
mod overrides_run_test;
#[path = "suite/parallel_cancel_test.rs"]
mod parallel_cancel_test;
#[path = "suite/parallel_concurrency_test.rs"]
mod parallel_concurrency_test;
#[path = "suite/parallel_e2e_test.rs"]
mod parallel_e2e_test;
#[cfg(unix)]
#[path = "suite/process_group_test.rs"]
mod process_group_test;
#[cfg(unix)]
#[path = "suite/profile_run_test.rs"]
mod profile_run_test;
#[path = "suite/progress_api_test.rs"]
mod progress_api_test;
#[path = "suite/prompt_cache_test.rs"]
mod prompt_cache_test;
#[cfg(unix)]
#[path = "suite/protect_test.rs"]
mod protect_test;
#[path = "suite/quality_common.rs"]
mod quality_common;
#[path = "suite/question_channel_test.rs"]
mod question_channel_test;
#[path = "suite/quota_pause_test.rs"]
mod quota_pause_test;
#[cfg(unix)]
#[path = "suite/reap_test.rs"]
mod reap_test;
#[cfg(unix)]
#[path = "suite/rebind_test.rs"]
mod rebind_test;
#[path = "suite/report_html_test.rs"]
mod report_html_test;
#[path = "suite/report_summary_test.rs"]
mod report_summary_test;
#[path = "suite/resume_capture_test.rs"]
mod resume_capture_test;
#[path = "suite/resume_test.rs"]
mod resume_test;
#[path = "suite/retry_test.rs"]
mod retry_test;
#[cfg(unix)]
#[path = "suite/review_panel_test.rs"]
mod review_panel_test;
#[path = "suite/review_state_test.rs"]
mod review_state_test;
#[path = "suite/review_test.rs"]
mod review_test;
#[cfg(unix)]
#[path = "suite/run_estimate_test.rs"]
mod run_estimate_test;
#[path = "suite/run_outcome_list_test.rs"]
mod run_outcome_list_test;
#[path = "suite/run_outcome_test.rs"]
mod run_outcome_test;
#[path = "suite/run_provenance_test.rs"]
mod run_provenance_test;
#[path = "suite/run_retro_test.rs"]
mod run_retro_test;
#[path = "suite/run_wait_test.rs"]
mod run_wait_test;
#[path = "suite/runner_registry_test.rs"]
mod runner_registry_test;
#[cfg(target_os = "linux")]
#[path = "suite/sandbox_run_test.rs"]
mod sandbox_run_test;
#[path = "suite/scheduler_test.rs"]
mod scheduler_test;
#[path = "suite/script_node_test.rs"]
mod script_node_test;
#[path = "suite/script_test.rs"]
mod script_test;
#[path = "suite/send_intent_test.rs"]
mod send_intent_test;
#[cfg(unix)]
#[path = "suite/session_handoff_test.rs"]
mod session_handoff_test;
#[path = "suite/shell_portability_test.rs"]
mod shell_portability_test;
#[path = "suite/state_test.rs"]
mod state_test;
#[cfg(unix)]
#[path = "suite/status_file_test.rs"]
mod status_file_test;
#[path = "suite/stop_run_test.rs"]
mod stop_run_test;
#[path = "suite/subplaybook_run_test.rs"]
mod subplaybook_run_test;
#[path = "suite/success_check_test.rs"]
mod success_check_test;
#[path = "suite/supervised_drive_test.rs"]
mod supervised_drive_test;
#[path = "suite/supervisor_channel_delivery_test.rs"]
mod supervisor_channel_delivery_test;
#[path = "suite/supervisor_commands_test.rs"]
mod supervisor_commands_test;
#[cfg(unix)]
#[path = "suite/token_economy_test.rs"]
mod token_economy_test;
#[cfg(unix)]
#[path = "suite/tool_exposure_test.rs"]
mod tool_exposure_test;
#[path = "suite/transcript_codex_test.rs"]
mod transcript_codex_test;
#[path = "suite/transcript_read_test.rs"]
mod transcript_read_test;
#[cfg(unix)]
#[path = "suite/transcript_test.rs"]
mod transcript_test;
#[path = "suite/unhandled_failure_test.rs"]
mod unhandled_failure_test;
#[path = "suite/wait_test.rs"]
mod wait_test;
#[path = "suite/wake_events_test.rs"]
mod wake_events_test;
#[path = "suite/workdir_test.rs"]
mod workdir_test;
#[path = "suite/worktree_test.rs"]
mod worktree_test;
#[cfg(unix)]
#[path = "suite/writing_brief_test.rs"]
mod writing_brief_test;
#[cfg(unix)]
#[path = "suite/zcode_adapter_test.rs"]
mod zcode_adapter_test;
