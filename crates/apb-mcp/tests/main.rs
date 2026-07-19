//! Single integration-test binary for apb-mcp. Cargo treats every file
//! directly under `tests/` as its own test binary; each fresh binary costs a
//! macOS security-scan on first spawn. Consolidating the 14 former
//! `tests/*.rs` files into modules under `tests/suite/` and driving them from
//! this one `main.rs` collapses that to a single binary. No content was
//! changed in the moved files - only their location and this file are new,
//! except: `include_str!` relative paths adjusted for the extra directory
//! depth, `profile_e2e_test.rs`'s former `#![cfg(unix)]` inner attribute
//! converted to the outer `#[cfg(unix)]` below on its `mod` line, and every
//! former private per-file `static ENV_LOCK` replaced with calls into the
//! single shared lock in `suite/common.rs` (see that file for why: running
//! as modules of one process means separate per-file locks no longer
//! prevent races between files).

#[path = "suite/common.rs"]
mod common;

#[path = "suite/advisory_tools_test.rs"]
mod advisory_tools_test;
#[path = "suite/capture_tools_test.rs"]
mod capture_tools_test;
#[path = "suite/catalog_tools_test.rs"]
mod catalog_tools_test;
#[path = "suite/connector_policy.rs"]
mod connector_policy;
#[path = "suite/patch_tool_test.rs"]
mod patch_tool_test;
#[path = "suite/policy_test.rs"]
mod policy_test;
#[cfg(unix)]
#[path = "suite/profile_e2e_test.rs"]
mod profile_e2e_test;
#[path = "suite/profile_tools_test.rs"]
mod profile_tools_test;
#[path = "suite/read_tools_test.rs"]
mod read_tools_test;
#[path = "suite/review_tool_test.rs"]
mod review_tool_test;
#[path = "suite/run_tools_test.rs"]
mod run_tools_test;
#[path = "suite/subplaybook_policy_test.rs"]
mod subplaybook_policy_test;
#[path = "suite/supervisor_tools_test.rs"]
mod supervisor_tools_test;
#[path = "suite/trial_tools_test.rs"]
mod trial_tools_test;
#[path = "suite/write_tools_test.rs"]
mod write_tools_test;
