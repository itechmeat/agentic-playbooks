//! Single integration-test binary for apb-server. Cargo treats every file
//! directly under `tests/` as its own test binary; each fresh binary costs a
//! macOS security-scan on first spawn. Consolidating the 7 former
//! `tests/*.rs` files into modules under `tests/suite/` and driving them from
//! this one `main.rs` collapses that to a single binary. No content was
//! changed in the moved files - only their location and this file are new.

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
#[path = "../../apb-engine/tests/support/driver_reaper.rs"]
mod driver_reaper;

#[path = "suite/answer_api_test.rs"]
mod answer_api_test;
#[path = "suite/api_test.rs"]
mod api_test;
#[path = "suite/auth_endpoints_test.rs"]
mod auth_endpoints_test;
#[path = "suite/auth_test.rs"]
mod auth_test;
#[path = "suite/bind_interlock_test.rs"]
mod bind_interlock_test;
#[cfg(unix)]
#[path = "suite/budget_api_test.rs"]
mod budget_api_test;
#[path = "suite/connectors_api_test.rs"]
mod connectors_api_test;
#[path = "suite/corrections_api_test.rs"]
mod corrections_api_test;
#[path = "suite/current_api_test.rs"]
mod current_api_test;
#[path = "suite/discover_api_test.rs"]
mod discover_api_test;
#[path = "suite/fit_api_test.rs"]
mod fit_api_test;
#[path = "suite/graph_api_test.rs"]
mod graph_api_test;
#[path = "suite/inbox_api_test.rs"]
mod inbox_api_test;
#[path = "suite/ingest_test.rs"]
mod ingest_test;
#[path = "suite/launch_api_test.rs"]
mod launch_api_test;
#[path = "suite/memory_api_test.rs"]
mod memory_api_test;
#[path = "suite/meta_api_test.rs"]
mod meta_api_test;
#[path = "suite/next_actions_api_test.rs"]
mod next_actions_api_test;
#[path = "suite/notify_api_test.rs"]
mod notify_api_test;
#[path = "suite/passkeys_flow_test.rs"]
mod passkeys_flow_test;
#[path = "suite/problems_api_test.rs"]
mod problems_api_test;
#[path = "suite/profiles_api_test.rs"]
mod profiles_api_test;
#[path = "suite/quality_api_test.rs"]
mod quality_api_test;
#[path = "suite/run_secrets_api_test.rs"]
mod run_secrets_api_test;
#[path = "suite/runs_api_test.rs"]
mod runs_api_test;
#[path = "suite/runs_watch_test.rs"]
mod runs_watch_test;
#[path = "suite/scenario_capture_api_test.rs"]
mod scenario_capture_api_test;
#[path = "support/secret_runs.rs"]
mod secret_runs;
#[path = "suite/stats_api_test.rs"]
mod stats_api_test;
#[path = "suite/suggestions_api_test.rs"]
mod suggestions_api_test;
#[path = "suite/ws_test.rs"]
mod ws_test;
