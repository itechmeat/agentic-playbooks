pub mod agent_catalog;
pub mod agent_output;
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
pub mod dismiss;
pub mod doctor;
pub mod duration;
pub mod effects;
// --- 0.24.0 eval suites ---
pub mod eval;
// --- end 0.24.0 eval suites ---
// host execution mode (0.23.0)
pub mod execution;
pub mod fingerprint;
pub mod fsutil;
pub mod graphutil;
pub mod judge;
pub mod migration;
pub mod model_check;
pub mod models_table;
pub mod overrides;
pub mod preflight;
pub mod profile;
pub mod profile_store;
pub mod projects;
pub mod registry;
pub mod run_origin;
pub mod schema;
pub mod schema_migrate;
pub mod scope;
pub mod server_auth;
pub mod skills;
pub mod store;
pub mod template;
pub mod trust;
pub mod validate;
pub mod versioning;
pub mod workspace;
pub mod zcode;
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
