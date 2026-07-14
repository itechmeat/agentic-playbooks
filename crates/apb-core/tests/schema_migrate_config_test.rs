//! Migration of the legacy global config (executors/default_executor). A
//! separate test binary: it's free to set APB_CONFIG_DIR without racing other
//! tests (they run in their own process). Contains a single, sequential #[test].

use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_core::registry::init_project;
use apb_core::schema_migrate::{apply, plan};

// Both tests mutate the process-global APB_CONFIG_DIR; serialize them so cargo
// doesn't run them concurrently in the same process.
static ENV_LOCK: Mutex<()> = Mutex::new(());
fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn seed_playbook(root: &Path, id: &str, body: &str) {
    let dir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), body).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

// A playbook with no executor of its own: relies on the global default_executor.
const PLAYBOOK: &str = "schema: 1\nid: a\nname: W\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";

#[test]
fn migrate_materializes_default_executor_and_leaves_global_config_intact() {
    let _l = lock();
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    // Legacy global config: default_executor + executors + other keys.
    let legacy = "port: 8080\ndefault_executor: cheap\nexecutors:\n  cheap: { agent: claude, model: haiku }\nagents:\n  claude: {}\n";
    fs::write(cfg.path().join("config.yaml"), legacy).unwrap();

    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", PLAYBOOK);

    let mp = plan(proj.path()).unwrap();
    // default_executor (global) is materialized into a GLOBAL profile.
    assert!(
        mp.new_profiles
            .iter()
            .any(|p| p.name == "cheap" && p.scope == "global"),
        "default_executor -> global profile: {:?}",
        mp.new_profiles
    );
    apply(proj.path(), &mp, 222).unwrap();

    // The profile is created in the GLOBAL directory (config_dir), not in the project.
    assert!(
        cfg.path().join("profiles/cheap/profile.yaml").is_file(),
        "global profile must live in config dir"
    );
    assert!(
        !proj
            .path()
            .join(".apb/profiles/cheap/profile.yaml")
            .is_file(),
        "global profile must not be materialized as a project profile"
    );
    // The new version references defaults.profile with an explicit scope: global.
    let migrated =
        fs::read_to_string(proj.path().join(".apb/playbooks/a/1.0.1/playbook.yaml")).unwrap();
    assert!(
        migrated.contains("name: cheap") && migrated.contains("scope: global"),
        "migrated playbook must default to qualified global profile: {migrated}"
    );

    // We do NOT touch the global config.yaml (shared across all projects):
    // legacy keys remain, but GlobalConfig ignores them and loads without error.
    let after = fs::read_to_string(cfg.path().join("config.yaml")).unwrap();
    assert!(
        after.contains("default_executor"),
        "shared global config must be left intact: {after}"
    );
    assert!(after.contains("port: 8080"));
    assert!(
        apb_core::config::GlobalConfig::load().is_ok(),
        "config with legacy keys must still load"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// A playbook with an EXPLICIT defaults.executor, referencing a global executor by name.
const WF_NAMED_GLOBAL: &str = "schema: 1\nid: b\nname: B\nversion: 1.0.0\ndefaults:\n  executor: shared\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";

#[test]
fn global_executor_becomes_global_profile_and_ref_is_qualified() {
    let _l = lock();
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let legacy = "executors:\n  shared: { agent: claude, model: sonnet }\nagents:\n  claude: {}\n";
    fs::write(cfg.path().join("config.yaml"), legacy).unwrap();

    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "b", WF_NAMED_GLOBAL);

    let mp = plan(proj.path()).unwrap();
    assert!(
        mp.new_profiles
            .iter()
            .any(|p| p.name == "shared" && p.scope == "global"),
        "global executor -> global profile: {:?}",
        mp.new_profiles
    );
    apply(proj.path(), &mp, 333).unwrap();

    // The profile lives under config_dir; the reference is qualified with scope: global.
    assert!(cfg.path().join("profiles/shared/profile.yaml").is_file());
    let migrated =
        fs::read_to_string(proj.path().join(".apb/playbooks/b/1.0.1/playbook.yaml")).unwrap();
    assert!(
        migrated.contains("name: shared") && migrated.contains("scope: global"),
        "ref must be qualified global: {migrated}"
    );

    // The migrated playbook loads and the profile resolves (globally).
    let reg = apb_core::registry::Registry::open(proj.path()).unwrap();
    let loaded = reg.load("b", None).expect("migrated playbook loads");
    let pref = loaded
        .playbook
        .defaults
        .profile
        .clone()
        .expect("defaults.profile present");
    assert_eq!(pref.scope, apb_core::profile::ProfileScope::Global);
    apb_core::profile_store::resolve_profile(
        proj.path(),
        apb_core::profile_store::PlaybookOrigin::Project,
        &pref,
    )
    .expect("global profile resolves");

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

// default_executor references a MISSING executor -> explicit error before any
// writes (review P2): not a silent skip, and not a schema-2 playbook with no binding.
#[test]
fn missing_default_executor_target_is_unresolved_error() {
    let _l = lock();
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    // default_executor points at a missing entry under executors.
    let legacy = "default_executor: ghost\nexecutors:\n  other: { agent: claude, model: haiku }\n";
    fs::write(cfg.path().join("config.yaml"), legacy).unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", PLAYBOOK);

    let r = plan(proj.path());
    assert!(
        r.is_err(),
        "missing default_executor target must be an explicit error, not a silent skip"
    );
    // Nothing was written (the error occurs before apply).
    assert!(!proj.path().join(".apb/playbooks/a/1.0.1").exists());

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
