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
#[path = "suite/agent_report_test.rs"]
mod agent_report_test;
#[path = "suite/agent_timeout_test.rs"]
mod agent_timeout_test;
#[path = "suite/background_run_test.rs"]
mod background_run_test;
#[path = "suite/background_supervisor_test.rs"]
mod background_supervisor_test;
#[path = "suite/cache_test.rs"]
mod cache_test;
#[path = "suite/child_connector_prepare.rs"]
mod child_connector_prepare;
#[path = "suite/child_run_event_test.rs"]
mod child_run_event_test;
#[path = "suite/connector_asana.rs"]
mod connector_asana;
#[path = "suite/connector_call.rs"]
mod connector_call;
#[path = "suite/connector_e2e.rs"]
mod connector_e2e;
#[path = "suite/connector_healthcheck.rs"]
mod connector_healthcheck;
#[path = "suite/connector_imap.rs"]
mod connector_imap;
#[path = "suite/connector_manifest.rs"]
mod connector_manifest;
#[path = "suite/connector_play_call.rs"]
mod connector_play_call;
#[path = "suite/connector_run.rs"]
mod connector_run;
#[path = "suite/connector_smtp.rs"]
mod connector_smtp;
#[path = "suite/context_compaction_test.rs"]
mod context_compaction_test;
#[path = "suite/context_test.rs"]
mod context_test;
#[path = "suite/control_test.rs"]
mod control_test;
#[path = "suite/detached_driver_test.rs"]
mod detached_driver_test;
#[path = "suite/digest_binding_test.rs"]
mod digest_binding_test;
#[path = "suite/event_test.rs"]
mod event_test;
#[cfg(unix)]
#[path = "suite/finish_answer_test.rs"]
mod finish_answer_test;
#[path = "suite/global_config_test.rs"]
mod global_config_test;
#[path = "suite/global_scope_run_test.rs"]
mod global_scope_run_test;
#[path = "suite/heartbeat_lost_test.rs"]
mod heartbeat_lost_test;
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
#[path = "suite/lineage_test.rs"]
mod lineage_test;
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
#[path = "suite/question_channel_test.rs"]
mod question_channel_test;
#[path = "suite/report_summary_test.rs"]
mod report_summary_test;
#[path = "suite/resume_capture_test.rs"]
mod resume_capture_test;
#[path = "suite/resume_test.rs"]
mod resume_test;
#[path = "suite/retry_test.rs"]
mod retry_test;
#[path = "suite/review_state_test.rs"]
mod review_state_test;
#[path = "suite/review_test.rs"]
mod review_test;
#[path = "suite/runner_registry_test.rs"]
mod runner_registry_test;
#[path = "suite/scheduler_test.rs"]
mod scheduler_test;
#[path = "suite/script_node_test.rs"]
mod script_node_test;
#[path = "suite/script_test.rs"]
mod script_test;
#[path = "suite/state_test.rs"]
mod state_test;
#[path = "suite/stop_run_test.rs"]
mod stop_run_test;
#[path = "suite/subplaybook_run_test.rs"]
mod subplaybook_run_test;
#[path = "suite/success_check_test.rs"]
mod success_check_test;
#[path = "suite/supervised_drive_test.rs"]
mod supervised_drive_test;
#[path = "suite/supervisor_commands_test.rs"]
mod supervisor_commands_test;
#[path = "suite/wait_test.rs"]
mod wait_test;
#[path = "suite/wake_events_test.rs"]
mod wake_events_test;
#[path = "suite/workdir_test.rs"]
mod workdir_test;
