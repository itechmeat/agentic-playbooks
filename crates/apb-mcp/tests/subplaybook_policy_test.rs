use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_core::profile::ProfileScope;
use apb_core::registry::{Registry, init_project};
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Lifecycle, OriginKind, TrustStore, write_lifecycle};
use apb_engine::run_config::read_run_config;
use apb_mcp::policy::{check_run, preflight};

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

// A child that requires a command that is not on PATH, so its applicability
// preflight (`requires`) fails - proving the child goes through the same
// `check_requires` the parent does.
const CHILD_REQUIRES: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nrequires:\n  commands: [apb-definitely-not-a-real-command-zzz]\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n";

fn set_lifecycle(root: &Path, id: &str, lc: Lifecycle) {
    write_lifecycle(&root.join(".apb/playbooks").join(id), lc).unwrap();
}

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
    assert_eq!(child.scope, ProfileScope::Project);
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
fn child_effects_surface_through_prepare_run_consent() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT_EFFECTS);
    write_pb(dir.path(), "worker", WORKER);
    approve(dir.path(), "parent");
    approve(dir.path(), "worker");

    // GAP 2: the consent surface (preflight -> playbook_prepare_run.plan.effects)
    // shows the WHOLE tree's effects. The child's acting effects surface on the
    // parent's preflight output, proving the recursive union.
    let pf = preflight(dir.path(), "parent", None).expect("preflight");
    assert!(pf.effects.iter().any(|e| e == "fs_write"));
    assert!(pf.effects.iter().any(|e| e == "network"));
    assert!(pf.effects.iter().any(|e| e == "external"));
    // `secrets` is declared-only: it can only reach the parent through the
    // recursive effects union, proving the union (not just node inference).
    assert!(pf.effects.iter().any(|e| e == "secrets"));
}

#[test]
fn gated_run_threads_child_pins_into_parent_config() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    // Control-only parent + child (no agent nodes): the gated run completes
    // synchronously without an executor.
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");

    // The gate produces the pins...
    let permit = check_run(dir.path(), &wref(), false, false).expect("permit");
    assert!(
        permit.children.contains_key("c"),
        "gate pinned child at node c"
    );

    // ...and the MCP run path must thread them into the engine verbatim
    // (anti-TOCTOU). Run through the same tools-layer entry the server uses.
    let out = apb_mcp::tools::playbook_run(
        dir.path(),
        "parent",
        None,
        std::collections::BTreeMap::new(),
        None,
        Some(permit.playbook_digest.clone()),
        Some(permit.profile_bundles.clone()),
        Some(permit.children.clone()),
    )
    .expect("gated run");
    let run_id = out["run_id"].as_str().expect("run_id");

    // The PARENT run's persisted config carries the pins keyed by playbook-node id.
    let run_dir = dir.path().join(".apb/runs").join(run_id);
    let cfg_read = read_run_config(&run_dir).expect("read run config");
    let children = cfg_read
        .expected_children
        .expect("parent config carries expected_children");
    let child = children.get("c").expect("child pinned at node c");
    assert_eq!(child.id, "child");
    assert_eq!(child.scope, ProfileScope::Project);
    assert_eq!(child.version, "1.0.0");
}

// C1 (release blocker): the recursive gate must run the SAME pipeline on each
// child that the parent gets, and every refusal must name the child.

#[test]
fn untrusted_child_digest_refuses_and_acknowledge_allows() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    // Only the parent is approved; the child's own digest is untrusted.
    approve(dir.path(), "parent");

    let refusal = check_run(dir.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "untrusted_requires_acknowledge");
    assert_eq!(refusal["id"], "child", "refusal names the untrusted child");
    assert!(
        refusal["digest"].as_str().is_some_and(|d| !d.is_empty()),
        "refusal carries the child digest"
    );

    // Acknowledging untrusted content lets the whole tree through.
    let permit = check_run(dir.path(), &wref(), true, false).expect("acknowledged permit");
    assert!(permit.children.contains_key("c"));
}

#[test]
fn draft_child_refuses_naming_child() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");
    set_lifecycle(dir.path(), "child", Lifecycle::Draft);

    let refusal = check_run(dir.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "draft_requires_trial");
    assert_eq!(refusal["id"], "child");
}

#[test]
fn retired_child_refuses_naming_child() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");
    set_lifecycle(dir.path(), "child", Lifecycle::Retired);

    let refusal = check_run(dir.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "retired_not_runnable");
    assert_eq!(refusal["id"], "child");
}

#[test]
fn child_with_unmet_requires_refuses_naming_child() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_REQUIRES);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");

    let refusal = check_run(dir.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "requires_unmet");
    assert_eq!(refusal["id"], "child");
    let missing = refusal["missing"].as_array().expect("missing list");
    assert!(
        missing
            .iter()
            .any(|m| m.as_str() == Some("command:apb-definitely-not-a-real-command-zzz")),
        "names the unmet command"
    );
}

#[test]
fn draft_child_also_refuses_through_preflight() {
    // preflight (prepare_run) shares the same tree walk, so child lifecycle is
    // enforced there too even though preflight is read-only about trust.
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _g = setup(cfg.path());

    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");
    set_lifecycle(dir.path(), "child", Lifecycle::Draft);

    let refusal = preflight(dir.path(), "parent", None)
        .err()
        .expect("preflight refuses a draft child");
    assert_eq!(refusal["policy"], "draft_requires_trial");
    assert_eq!(refusal["id"], "child");
}
