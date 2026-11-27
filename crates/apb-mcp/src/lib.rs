//! Internal crate of the `apb` command, published only so that
//! `cargo install agentic-playbooks` can build it. It is not a public API:
//! any item may change or disappear in any release, patch releases
//! included. Do not depend on it; use the `apb` command or its MCP server.

// The `testing` feature compiles test seams (the faults an install test
// injects, `tools::trigger::test_hooks`) that no person's apb may carry;
// like apb-core's, it is refused in any build without debug assertions, so
// a release or dist binary never holds one.
#[cfg(all(feature = "testing", not(debug_assertions)))]
compile_error!(
    "the apb-mcp `testing` feature is for test builds only and is refused in a build without debug assertions (a release or dist profile)"
);

// Unit tests run with an isolated global config dir, set before any test
// thread starts (`apb_core::config::test_isolation`).
#[cfg(test)]
apb_core::isolate_test_config!(std::env::temp_dir());

pub mod advisory_tools;
pub mod ask_server;
pub mod catalog;
pub mod catalog_rank;
pub mod instructions;
pub mod plan;
/// The run policy gate lives in the engine (`apb_engine::gate`) so every launch
/// surface - MCP, the dashboard, the CLI - calls the same one; this path is
/// kept for the MCP tool layer and its tests.
pub use apb_engine::gate as policy;
pub mod profile_tools;
pub mod promote;
pub mod server;
pub mod setup;
mod tool_error;
pub mod tools;
mod unadvertised;
