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
pub mod list;
pub mod migration;
pub mod model_check;
pub mod models_table;
pub mod notify;
pub mod output_limits;
pub mod overrides;
pub mod preflight;
pub mod process_env;
pub mod profile;
pub mod profile_store;
pub mod project_checks;
pub mod projects;
pub mod quality;
pub mod registry;
pub mod run_memory;
pub mod run_notes;
pub mod run_origin;
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
