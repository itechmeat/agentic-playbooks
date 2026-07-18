//! Shared test-only utilities for the consolidated apb-core integration
//! binary (see `../main.rs`).
//!
//! Six of the twenty-three former `tests/*.rs` files mutate process-wide env
//! vars (`APB_CONFIG_DIR`, `HOME`, `PATH`, `APB_PROBE_TIMEOUT_MS`) or the
//! process-wide current directory to isolate config/detect/profile state.
//! Five of them (`models_table_test.rs`, `profile_resolve_test.rs`,
//! `doctor_test.rs`, `schema_migrate_config_test.rs`, `detect_test.rs`) each
//! defined their own private `static ENV_LOCK`, on the assumption that they
//! ran in their own cargo test process (one file = one binary, so a lock
//! only had to guard tests *within* that file). The sixth
//! (`config_test.rs`'s `global_config_load_paths`) mutated `APB_CONFIG_DIR`
//! with no lock at all - safe only because it ran in a process by itself.
//! Consolidating every `tests/*.rs` file into modules of one binary means
//! all their test functions now run as threads in the same process, so
//! separate (or absent) per-file locks no longer prevent races *between*
//! modules - e.g. one module's test could overwrite `APB_CONFIG_DIR` mid-run
//! of another module's test. Every env-mutating test across all modules must
//! take this ONE shared lock instead.
//!
//! This uses `std::sync::Mutex` rather than `tokio::sync::Mutex`: apb-core is
//! a no-async domain layer, and every test in this crate (env-mutating or
//! not) is a plain `#[test]` - none are `#[tokio::test]` and none hold the
//! guard across an `.await` - so the crate's existing sync-mutex idiom
//! (poison-tolerant via `unwrap_or_else(|e| e.into_inner())`) is the minimal
//! correct choice; pulling in a tokio runtime here would add nothing.

use std::sync::{Mutex, MutexGuard};

pub static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
