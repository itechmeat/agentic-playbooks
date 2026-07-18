//! Shared test-only utilities for the consolidated apb-server integration
//! binary (see `../main.rs`).
//!
//! `meta_api_test` and `profiles_api_test` each mutate process-wide env vars
//! (`HOME`, `APB_CONFIG_DIR`) and were originally written on the assumption
//! that they ran in their own cargo test process, so no lock was needed.
//! Consolidating every `tests/*.rs` file into one binary means their test
//! functions now run as threads in the same process, and cargo test runs
//! test functions in parallel by default - so without serialization these
//! two race on the shared env and fail intermittently (observed directly
//! during consolidation: `agents_models_and_skills_endpoints` failed when
//! `profiles_list_then_create_then_trusted` overwrote `HOME`/`APB_CONFIG_DIR`
//! mid-run). Any test that mutates process env must take this lock for the
//! duration of its run.
//!
//! This uses `tokio::sync::Mutex` rather than `std::sync::Mutex`: both
//! affected tests hold the guard across several `.await` points (the router
//! calls under test), and clippy's `await_holding_lock` correctly flags a
//! std mutex guard held that way as a potential executor stall. The
//! async-aware guard is fine to hold across awaits.

use tokio::sync::{Mutex, MutexGuard};

pub static ENV_LOCK: Mutex<()> = Mutex::const_new(());

pub async fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}
