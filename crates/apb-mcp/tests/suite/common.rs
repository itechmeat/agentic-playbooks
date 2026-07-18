//! Shared test-only utilities for the consolidated apb-mcp integration
//! binary (see `../main.rs`).
//!
//! Nine of the fourteen former `tests/*.rs` files mutate process-wide env
//! vars (`APB_CONFIG_DIR`, `HOME`, `APB_AGENT_CMD`, `PATH`) to isolate
//! trust-store/config state, and each originally defined its own private
//! `static ENV_LOCK` on the assumption that it ran in its own cargo test
//! process (one file = one binary, so a lock only had to guard tests
//! *within* that file). Consolidating every `tests/*.rs` file into modules
//! of one binary means all their test functions now run as threads in the
//! same process, so separate per-file locks no longer prevent races
//! *between* modules - e.g. one module's test could overwrite
//! `APB_CONFIG_DIR` mid-run of another module's test. Every env-mutating
//! test across all modules must take this ONE shared lock instead.
//!
//! This uses `std::sync::Mutex` rather than `tokio::sync::Mutex` (contrast
//! apb-server's `common.rs`, which uses a tokio mutex): every env-mutating
//! test in this crate is a plain `#[test]` - none are `#[tokio::test]` and
//! none hold the guard across an `.await` - so the crate's existing
//! sync-mutex idiom (poison-tolerant via `unwrap_or_else(|e| e.into_inner())`)
//! is the minimal correct choice; pulling in a tokio runtime here would add
//! nothing.

use std::sync::{Mutex, MutexGuard};

pub static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
