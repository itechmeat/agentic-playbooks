//! Single integration-test binary for apb-cli. Cargo treats every file
//! directly under `tests/` as its own test binary; each fresh binary costs a
//! macOS security-scan on first spawn. Consolidating the 10 former
//! `tests/*.rs` files into modules under `tests/suite/` and driving them from
//! this one `main.rs` collapses that to a single binary. No content was
//! changed in the moved files - only their location and this file are new,
//! except: `include_str!` relative paths adjusted for the extra directory
//! depth, and `stdio_profile_e2e_test.rs`'s former `#![cfg(unix)]` inner
//! attribute converted to the outer `#[cfg(unix)]` below on its `mod` line.

#[path = "suite/advisory_cli_test.rs"]
mod advisory_cli_test;
#[path = "suite/cache_cli.rs"]
mod cache_cli;
#[path = "suite/cli_test.rs"]
mod cli_test;
#[path = "suite/connector_cli.rs"]
mod connector_cli;
#[path = "suite/demo_playbooks_test.rs"]
mod demo_playbooks_test;
// Unix-only: the module drives process groups through
// `std::os::unix::process::CommandExt` and inspects them with `ps`/`kill`.
#[cfg(unix)]
#[path = "suite/detached_driver_test.rs"]
mod detached_driver_test;
#[path = "suite/live_smoke_test.rs"]
mod live_smoke_test;
#[path = "suite/mcp_cli_test.rs"]
mod mcp_cli_test;
#[path = "suite/mcp_supervise_test.rs"]
mod mcp_supervise_test;
#[path = "suite/official_connectors_gate.rs"]
mod official_connectors_gate;
#[path = "suite/phase9_cli_test.rs"]
mod phase9_cli_test;
#[path = "suite/profile_cli_test.rs"]
mod profile_cli_test;
#[path = "suite/projects_cli_test.rs"]
mod projects_cli_test;
#[path = "suite/run_cli_test.rs"]
mod run_cli_test;
#[path = "suite/run_doctor_cli_test.rs"]
mod run_doctor_cli_test;
#[cfg(unix)]
#[path = "suite/stdio_profile_e2e_test.rs"]
mod stdio_profile_e2e_test;
#[path = "suite/supervise_cli_test.rs"]
mod supervise_cli_test;
