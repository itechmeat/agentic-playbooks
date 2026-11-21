//! The integration tests of `apb-mcp-client`, one binary.

// Every test runs with an isolated global config dir
// (`apb_core::config::test_isolation`).
apb_core::isolate_test_config!(env!("CARGO_TARGET_TMPDIR"));

#[path = "suite/stdio_test.rs"]
mod stdio_test;
