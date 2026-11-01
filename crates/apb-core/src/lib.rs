pub mod agent_actions;
pub mod agent_catalog;
pub mod agent_output;
pub mod agents;
pub mod baseline;
pub mod budget;
pub mod bundle;
pub mod cache;
pub mod candidate;
pub mod candidate_pointer;
pub mod clock;
pub mod config;
pub mod connector;
pub mod content;
pub mod decision_probe;
pub mod decision_thresholds;
pub mod decisions;
pub mod detect;
pub mod discover;
pub mod dismiss;
pub mod doctor;
pub mod duration;
pub mod edge_address;
pub mod effects;
// --- 0.24.0 eval suites ---
pub mod eval;
// --- end 0.24.0 eval suites ---
// host execution mode (0.23.0)
pub mod execution;
pub mod fingerprint;
pub mod fork;
pub mod fsutil;
pub mod git;
pub mod graphutil;
pub mod isolation;
pub mod judge;
pub mod known_problems;
pub mod launch;
pub mod list;
pub mod migration;
pub mod model_check;
pub mod models_table;
pub mod notify;
pub mod output_limits;
pub mod overrides;
pub mod policy_file;
pub mod preflight;
pub mod process_env;
pub mod process_mode;
pub mod profile;
pub mod profile_store;
pub mod project_checks;
pub mod projects;
pub mod publish;
pub mod quality;
pub mod registry;
pub mod run_memory;
pub mod run_notes;
pub mod run_origin;
pub mod run_params;
pub mod schema;
pub mod schema_migrate;
pub mod scope;
pub mod scope_move;
pub mod scrub;
pub mod secret;
pub mod server_auth;
pub mod skills;
pub mod stats;
pub mod step_env;
pub mod store;
pub mod template;
pub mod text;
pub mod token_shape;
pub mod trust;
pub mod url_component;
pub mod validate;
pub mod versioning;
pub mod workspace;
// The zcode agent module, at the path its callers have always used.
pub use agents::zcode;
pub mod zcode_tasks_index;

/// Shared lock for unit tests that touch process-global env
/// (`APB_CONFIG_DIR` etc.): tests within one crate run on parallel threads of
/// the same process, so env mutation needs to be serialized. Poisoning is
/// ignored - the lock only guards against a race on env, not a data invariant.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A config dir of a unit test's own: `APB_CONFIG_DIR` points at a fresh
/// directory under [`env_test_lock`] until drop, so a test that writes
/// launch floors, trust or the projects registry never touches the
/// developer's real config dir.
#[cfg(test)]
pub(crate) struct TestConfig {
    _dir: tempfile::TempDir,
    prev: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(crate) fn test_config() -> TestConfig {
    let lock = env_test_lock();
    let dir = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("APB_CONFIG_DIR");
    // SAFETY: env mutation is serialized by the lock, held until drop.
    unsafe { std::env::set_var("APB_CONFIG_DIR", dir.path()) };
    TestConfig {
        _dir: dir,
        prev,
        _lock: lock,
    }
}

#[cfg(test)]
impl Drop for TestConfig {
    fn drop(&mut self) {
        // SAFETY: still under the lock (the guard drops after this body).
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                None => std::env::remove_var("APB_CONFIG_DIR"),
            }
        }
    }
}
