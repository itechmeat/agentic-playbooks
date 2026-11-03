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
pub mod tools;
