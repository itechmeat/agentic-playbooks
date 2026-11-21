//! Blocking MCP client sessions for apb's connector calls: one server
//! conversation per session, run on its own thread.
//!
//! The client speaks the protocol and returns plain `serde_json::Value`
//! data. It knows nothing about runs, grants, pins or the send ledger;
//! every apb rule lives in the engine.

// The `testing` feature (the stub server) exists for test builds only: a
// build without debug assertions refuses it, so no release or dist binary
// can carry it, whatever else was built with it.
#[cfg(all(feature = "testing", not(debug_assertions)))]
compile_error!(
    "the apb-mcp-client `testing` feature is for test builds only and is refused in a build without debug assertions (a release or dist profile)"
);

// Unit tests run with an isolated global config dir, set before any test
// thread starts (`apb_core::config::test_isolation`).
#[cfg(test)]
apb_core::isolate_test_config!(std::env::temp_dir());

mod frame;
mod session;
mod stdio;
mod tap;

#[cfg(feature = "testing")]
pub mod testing;

pub use frame::{ArgumentsRefusal, MAX_ARGUMENT_BYTES, MAX_ARGUMENT_DEPTH, check_arguments};
pub use session::{
    CallFailure, Ended, Era, Failure, FailureKind, GroupEnd, Lifecycle, REPLY_CLEANUP_ALLOWANCE,
    SUPPORTED_VERSIONS, Server, Session, ToolAnswer,
};
pub use stdio::{EnvValue, ProgramFile, ProgramRefusal, StartFrom, StdioServer};
pub use tap::{MAX_LINE_BYTES, MAX_SESSION_BYTES};
