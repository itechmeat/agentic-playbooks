//! Single integration-test binary for apb-server. Cargo treats every file
//! directly under `tests/` as its own test binary; each fresh binary costs a
//! macOS security-scan on first spawn. Consolidating the 7 former
//! `tests/*.rs` files into modules under `tests/suite/` and driving them from
//! this one `main.rs` collapses that to a single binary. No content was
//! changed in the moved files - only their location and this file are new.

#[path = "suite/common.rs"]
mod common;

#[path = "suite/api_test.rs"]
mod api_test;
#[path = "suite/connectors_api_test.rs"]
mod connectors_api_test;
#[path = "suite/input_draft_api_test.rs"]
mod input_draft_api_test;
#[path = "suite/meta_api_test.rs"]
mod meta_api_test;
#[path = "suite/profiles_api_test.rs"]
mod profiles_api_test;
#[path = "suite/runs_api_test.rs"]
mod runs_api_test;
#[path = "suite/runs_watch_test.rs"]
mod runs_watch_test;
#[path = "suite/ws_test.rs"]
mod ws_test;
