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

/// A private global config dir (`APB_CONFIG_DIR`) for one test, holding
/// [`ENV_LOCK`] for its lifetime and restoring the previous value on drop.
/// Any test that writes global state - a playbook save approves the saved
/// digest in `trust.json` - must hold one, or the write lands in the
/// developer's real `~/.config/apb`.
pub struct ConfigSandbox {
    _dir: tempfile::TempDir,
    prev: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

pub fn config_sandbox() -> ConfigSandbox {
    let lock = env_lock();
    let dir = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("APB_CONFIG_DIR");
    // SAFETY: env mutation is serialized by ENV_LOCK, held until drop.
    unsafe { std::env::set_var("APB_CONFIG_DIR", dir.path()) };
    ConfigSandbox {
        _dir: dir,
        prev,
        _lock: lock,
    }
}

impl Drop for ConfigSandbox {
    fn drop(&mut self) {
        // SAFETY: still under ENV_LOCK (the guard drops after this body).
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
    }
}
