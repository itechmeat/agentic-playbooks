use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_core::registry::{Registry, init_project};
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{OriginKind, TrustStore};
use apb_mcp::policy::check_run;

// Env isolation: TrustStore is keyed on the global config dir, so every test
// runs under its own temp APB_CONFIG_DIR and serializes env mutation.
static ENV_LOCK: Mutex<()> = Mutex::new(());
fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

fn write_pb(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// Approve a playbook's own digest so trust is not the thing under test.
fn approve(root: &Path, id: &str) {
    let reg = Registry::open(root).unwrap();
    let loaded = reg.load(id, None).unwrap();
    let mut store = TrustStore::load();
    store
        .approve(&digest_str(&loaded.yaml), id, OriginKind::LocallyApproved)
        .unwrap();
}

const PARENT: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
const CHILD_OK: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n";
const CHILD_CYCLE: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: parent }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";

// Parent whose own nodes are control-flow only apart from the playbook node,
// pointing at a child whose agent_task carries acting effects and declares
// `secrets` (a declared-only effect that can reach the parent gate ONLY via the
// effects union, not via the parent's own node inference).
const PARENT_EFFECTS: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: worker }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
const WORKER: &str = "schema: 2\nid: worker\nname: worker\nversion: 1.0.0\neffects: [secrets]\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: \"do it\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: f }\n";

fn wref() -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "parent".into(),
        version: None,
    }
}

fn setup(cfg: &Path) -> EnvGuard {
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
    EnvGuard
}

#[test]
fn recursive_permit_pins_child() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");

    let permit = check_run(dir.path(), &wref(), false, false).expect("permit");
    let child = permit.children.get("c").expect("child pinned at node c");
    assert_eq!(child.id, "child");
    assert_eq!(child.scope, "project");
    assert_eq!(child.version, "1.0.0");
    assert!(!child.playbook_digest.is_empty());
    assert!(child.children.is_empty());
}

#[test]
fn cycle_is_refused() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_CYCLE);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");

    let refusal = check_run(dir.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "sub_playbook_cycle");
    let cycle = refusal["cycle"].as_array().expect("cycle path");
    // parent -> child -> parent (the repeated pair closes the cycle).
    assert_eq!(cycle.first().unwrap(), "project/parent");
    assert_eq!(cycle.last().unwrap(), "project/parent");
}

#[test]
fn child_effects_surface_through_parent_gate() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT_EFFECTS);
    write_pb(dir.path(), "worker", WORKER);
    approve(dir.path(), "parent");
    approve(dir.path(), "worker");

    let permit = check_run(dir.path(), &wref(), false, false).expect("permit");
    // The child's acting effects surface on the parent's gate output.
    assert!(permit.effects.iter().any(|e| e == "fs_write"));
    assert!(permit.effects.iter().any(|e| e == "network"));
    assert!(permit.effects.iter().any(|e| e == "external"));
    // `secrets` is declared-only: it can only reach the parent through the
    // recursive effects union, proving the union (not just node inference).
    assert!(permit.effects.iter().any(|e| e == "secrets"));
}
