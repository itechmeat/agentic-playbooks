use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::schema_migrate::{apply, plan};

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

const NAMED: &str = "schema: 1\nid: ID\nname: W\nversion: 1.0.0\nexecutors:\n  main:\n    agent: claude\n    model: haiku\ndefaults:\n  executor: main\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";

#[test]
fn dedup_same_content_merges() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", &NAMED.replace("id: ID", "id: a"));
    seed_playbook(proj.path(), "b", &NAMED.replace("id: ID", "id: b"));
    let mp = plan(proj.path()).unwrap();
    // The same `main` executor across two playbooks -> a single profile.
    let mains = mp.new_profiles.iter().filter(|p| p.name == "main").count();
    assert_eq!(
        mains, 1,
        "same-content executor should dedup to one profile"
    );
    assert_eq!(mp.playbook_updates.len(), 2);
}

#[test]
fn different_content_gets_hash_suffix() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", &NAMED.replace("id: ID", "id: a"));
    seed_playbook(
        proj.path(),
        "b",
        &NAMED
            .replace("id: ID", "id: b")
            .replace("model: haiku", "model: opus"),
    );
    let mp = plan(proj.path()).unwrap();
    // Different content under the name `main` -> one `main` + one `main-<hash>`.
    assert!(mp.new_profiles.iter().any(|p| p.name == "main"));
    assert!(
        mp.new_profiles.iter().any(|p| p.name.starts_with("main-")),
        "conflicting content needs hash suffix: {:?}",
        mp.new_profiles
    );
}

#[test]
fn apply_creates_profiles_and_new_version_history_untouched() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", &NAMED.replace("id: ID", "id: a"));
    let mp = plan(proj.path()).unwrap();
    apply(proj.path(), &mp, 111).unwrap();

    // The profile was created, SOUL.md is empty.
    assert!(
        proj.path()
            .join(".apb/profiles/main/profile.yaml")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(proj.path().join(".apb/profiles/main/SOUL.md")).unwrap(),
        ""
    );
    // The old version is untouched, current has moved to the new one (1.0.1).
    assert!(
        proj.path()
            .join(".apb/playbooks/a/1.0.0/playbook.yaml")
            .is_file()
    );
    assert!(
        proj.path()
            .join(".apb/playbooks/a/1.0.1/playbook.yaml")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(proj.path().join(".apb/playbooks/a/current"))
            .unwrap()
            .trim(),
        "1.0.1"
    );
    // The new version is marked schema 2 and version was updated to match the directory.
    let migrated =
        fs::read_to_string(proj.path().join(".apb/playbooks/a/1.0.1/playbook.yaml")).unwrap();
    assert!(migrated.contains("schema: 2"));
    assert!(migrated.contains("version: 1.0.1"));
    // Key assertion: the migrated current version ACTUALLY loads via the registry (no VersionMismatch).
    let reg = apb_core::registry::Registry::open(proj.path()).unwrap();
    let loaded = reg.load("a", None).expect("migrated current must load");
    assert_eq!(loaded.version, "1.0.1");
}

#[test]
fn plan_idempotent_after_apply() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", &NAMED.replace("id: ID", "id: a"));
    let mp = plan(proj.path()).unwrap();
    apply(proj.path(), &mp, 111).unwrap();
    // Running plan again on the migrated tree (current -> schema 2, no executors) - empty.
    let mp2 = plan(proj.path()).unwrap();
    assert!(
        mp2.is_empty(),
        "second plan should be empty: {:?}",
        mp2.playbook_updates
    );
}

// A playbook with ONLY supervisor.executor (no defaults/node/named executors).
const SUP_ONLY: &str = "schema: 1\nid: s\nname: W\nversion: 1.0.0\nsupervisor:\n  executor: { agent: claude, model: haiku }\ndefaults:\n  profile: p\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\", profile: p }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n";

#[test]
fn supervisor_only_executor_is_migrated_and_loadable() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    // Profile p already exists (node/defaults); the supervisor executor is inline legacy.
    let pdir = proj.path().join(".apb/profiles/p");
    fs::create_dir_all(&pdir).unwrap();
    fs::write(
        pdir.join("profile.yaml"),
        "name: p\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    fs::write(pdir.join("SOUL.md"), "").unwrap();
    seed_playbook(proj.path(), "s", SUP_ONLY);

    // The plan is NOT empty: supervisor.executor is detected.
    let mp = plan(proj.path()).unwrap();
    assert!(
        !mp.is_empty(),
        "supervisor-only executor must produce a plan"
    );
    apply(proj.path(), &mp, 333).unwrap();

    // The migrated version loads via the registry (supervisor.executor is gone).
    let migrated =
        fs::read_to_string(proj.path().join(".apb/playbooks/s/1.0.1/playbook.yaml")).unwrap();
    assert!(
        !migrated.contains("executor:"),
        "supervisor executor must be gone: {migrated}"
    );
    assert!(
        migrated.contains("profile:"),
        "supervisor must carry a profile now: {migrated}"
    );
    let reg = apb_core::registry::Registry::open(proj.path()).unwrap();
    reg.load("s", None)
        .expect("migrated supervisor playbook must load");
}

#[test]
fn dry_run_writes_nothing() {
    let proj = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    seed_playbook(proj.path(), "a", &NAMED.replace("id: ID", "id: a"));
    let _mp = plan(proj.path()).unwrap();
    // plan writes nothing: no profiles, no new version.
    assert!(!proj.path().join(".apb/profiles/main").exists());
    assert!(!proj.path().join(".apb/playbooks/a/1.0.1").exists());
}
