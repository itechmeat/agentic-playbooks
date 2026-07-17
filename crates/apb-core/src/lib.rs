pub mod bundle;
pub mod config;
pub mod content;
pub mod detect;
pub mod dismiss;
pub mod doctor;
pub mod duration;
pub mod effects;
pub mod fsutil;
pub mod migration;
pub mod models_table;
pub mod overrides;
pub mod profile;
pub mod profile_store;
pub mod projects;
pub mod registry;
pub mod schema;
pub mod schema_migrate;
pub mod scope;
pub mod skills;
pub mod store;
pub mod trust;
pub mod validate;
pub mod versioning;
pub mod workspace;

/// Shared lock for unit tests that touch process-global env
/// (`APB_CONFIG_DIR` etc.): tests within one crate run on parallel threads of
/// the same process, so env mutation needs to be serialized. Poisoning is
/// ignored - the lock only guards against a race on env, not a data invariant.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
