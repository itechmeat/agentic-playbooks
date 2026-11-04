//! Single integration-test binary for apb-cli. Cargo treats every file
//! directly under `tests/` as its own test binary; each fresh binary costs a
//! macOS security-scan on first spawn. Consolidating the 10 former
//! `tests/*.rs` files into modules under `tests/suite/` and driving them from
//! this one `main.rs` collapses that to a single binary. No content was
//! changed in the moved files - only their location and this file are new,
//! except: `include_str!` relative paths adjusted for the extra directory
//! depth, and `stdio_profile_e2e_test.rs`'s former `#![cfg(unix)]` inner
//! attribute converted to the outer `#[cfg(unix)]` below on its `mod` line.
//!
//! `suite/common.rs` is the one way these tests spawn `apb`: it sandboxes the
//! global config dir so a test run never writes the real `~/.config/apb`.
//! `suite/real_config_guard_test.rs` enforces that.

// Config isolation: before any test thread starts, the real config dir is
// recorded as forbidden and `XDG_CONFIG_HOME` points at a sandbox that every
// spawned process inherits (`apb_core::config::test_isolation`).
apb_core::isolate_test_config!(env!("CARGO_TARGET_TMPDIR"));

// Billed use (`apb_core::billing`): the stand-in agents these tests start
// have an unknown cost basis, which the default `ask` would hold. Set for
// the whole process before any test thread starts (and inherited by every
// `apb` it spawns); the billing tests clear it or write their own settings.
#[used]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
static BILLING_DEFAULT: extern "C" fn() = billing_default;

extern "C" fn billing_default() {
    // SAFETY: runs before `main`, so no other thread exists yet.
    unsafe { std::env::set_var("APB_TEST_BILLING_DEFAULT", "allow") };
}

#[path = "suite/common.rs"]
mod common;

#[path = "suite/advisory_cli_test.rs"]
mod advisory_cli_test;
#[path = "suite/answer_cli_test.rs"]
mod answer_cli_test;
#[path = "suite/ask_server_cli_test.rs"]
mod ask_server_cli_test;
#[cfg(unix)]
#[path = "suite/billing_cli_test.rs"]
mod billing_cli_test;
#[cfg(unix)]
#[path = "suite/budget_cli_test.rs"]
mod budget_cli_test;
// Unix-only: a shell script stands in for the codex CLI setup detects.
#[cfg(unix)]
#[path = "suite/bundled_global_scenario_test.rs"]
mod bundled_global_scenario_test;
#[path = "suite/bundled_scenarios_test.rs"]
mod bundled_scenarios_test;
#[path = "suite/cache_cli.rs"]
mod cache_cli;
#[path = "suite/cli_test.rs"]
mod cli_test;
#[path = "suite/connector_cli.rs"]
mod connector_cli;
#[path = "suite/demo_playbooks_test.rs"]
mod demo_playbooks_test;
// Unix-only: a shell script stands in for git on PATH.
#[cfg(unix)]
#[path = "suite/daily_cleanup_script_test.rs"]
mod daily_cleanup_script_test;
#[path = "suite/decisions_cli_test.rs"]
mod decisions_cli_test;
// Unix-only: the module drives process groups through
// `std::os::unix::process::CommandExt` and inspects them with `ps`/`kill`.
#[cfg(unix)]
#[path = "suite/detached_driver_test.rs"]
mod detached_driver_test;
#[path = "suite/discover_cli_test.rs"]
mod discover_cli_test;
#[cfg(unix)]
#[path = "suite/doctor_agents_cli_test.rs"]
mod doctor_agents_cli_test;
#[path = "suite/grade_cli_test.rs"]
mod grade_cli_test;
#[path = "suite/hook_cli_test.rs"]
mod hook_cli_test;
#[cfg(unix)]
#[path = "suite/host_mode_cli_test.rs"]
mod host_mode_cli_test;
#[path = "suite/http_service.rs"]
mod http_service;
#[path = "suite/ingest_cli_test.rs"]
mod ingest_cli_test;
#[path = "suite/intent_cli_test.rs"]
mod intent_cli_test;
// Unix-only: reads a private file's permission bits (`PermissionsExt`).
#[cfg(unix)]
#[path = "suite/evidence_cli_test.rs"]
mod evidence_cli_test;
// Unix-only: kills run drivers by process group.
#[cfg(unix)]
#[path = "suite/journal_corpus_test.rs"]
mod journal_corpus_test;
#[cfg(unix)]
#[path = "suite/list_send_cli_test.rs"]
mod list_send_cli_test;
#[path = "suite/live_smoke_test.rs"]
mod live_smoke_test;
#[path = "suite/mcp_cli_test.rs"]
mod mcp_cli_test;
#[cfg(unix)]
#[path = "suite/mcp_read_only_cli_test.rs"]
mod mcp_read_only_cli_test;
#[path = "suite/mcp_supervise_test.rs"]
mod mcp_supervise_test;
#[path = "suite/next_actions_cli_test.rs"]
mod next_actions_cli_test;
#[path = "suite/notify_cli_test.rs"]
mod notify_cli_test;
#[path = "suite/official_connectors_gate.rs"]
mod official_connectors_gate;
#[cfg(unix)]
#[path = "suite/pause_cli_test.rs"]
mod pause_cli_test;
#[path = "suite/pause_waker_cli_test.rs"]
mod pause_waker_cli_test;
#[path = "suite/phase9_cli_test.rs"]
mod phase9_cli_test;
#[path = "suite/problems_cli_test.rs"]
mod problems_cli_test;
#[path = "suite/profile_cli_test.rs"]
mod profile_cli_test;
#[path = "suite/projects_cli_test.rs"]
mod projects_cli_test;
// Unix-only: a shell script stands in for git on PATH.
#[cfg(unix)]
#[path = "suite/launch_cli_test.rs"]
mod launch_cli_test;
#[cfg(unix)]
#[path = "suite/publish_cli_test.rs"]
mod publish_cli_test;
#[cfg(unix)]
#[path = "suite/quota_pause_cli_test.rs"]
mod quota_pause_cli_test;
#[path = "suite/real_config_guard_test.rs"]
mod real_config_guard_test;
#[path = "suite/release_workflow_test.rs"]
mod release_workflow_test;
#[path = "suite/run_cli_test.rs"]
mod run_cli_test;
#[path = "suite/run_doctor_cli_test.rs"]
mod run_doctor_cli_test;
#[path = "suite/run_outputs_cli_test.rs"]
mod run_outputs_cli_test;
#[path = "suite/run_secrets_cli_test.rs"]
mod run_secrets_cli_test;
#[path = "suite/run_wait_cli_test.rs"]
mod run_wait_cli_test;
#[path = "suite/runs_detail_cli_test.rs"]
mod runs_detail_cli_test;
#[path = "suite/runs_search_cli_test.rs"]
mod runs_search_cli_test;
#[path = "suite/sandbox_exec_test.rs"]
mod sandbox_exec_test;
#[cfg(unix)]
#[path = "suite/scenario_fixture.rs"]
mod scenario_fixture;
#[cfg(unix)]
#[path = "suite/scenario_mutation_test.rs"]
mod scenario_mutation_test;
#[cfg(unix)]
#[path = "suite/scenario_run_test.rs"]
mod scenario_run_test;
#[cfg(unix)]
#[path = "suite/scenario_smoke_test.rs"]
mod scenario_smoke_test;
#[path = "../../apb-server/tests/support/secret_runs.rs"]
mod secret_runs;
#[path = "suite/server_key_cli_test.rs"]
mod server_key_cli_test;
#[cfg(unix)]
#[path = "suite/setup_cli_test.rs"]
mod setup_cli_test;
#[path = "suite/stats_cli_test.rs"]
mod stats_cli_test;
// --- 0.24.0 eval suites ---
#[path = "suite/eval_cli_test.rs"]
mod eval_cli_test;
// --- end 0.24.0 eval suites ---
#[path = "suite/passkey_cli_test.rs"]
mod passkey_cli_test;
#[path = "suite/read_only_unattended_cli_test.rs"]
mod read_only_unattended_cli_test;
#[cfg(unix)]
#[path = "suite/stdio_profile_e2e_test.rs"]
mod stdio_profile_e2e_test;
#[path = "suite/suggestions_cli_test.rs"]
mod suggestions_cli_test;
#[path = "suite/supervise_cli_test.rs"]
mod supervise_cli_test;
#[path = "suite/version_cli_test.rs"]
mod version_cli_test;
