use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_core::registry::init_project;
use apb_core::scope::digest_str;
use apb_core::trust::{Lifecycle, OriginKind, TrustStore, write_lifecycle};
use apb_mcp::tools::playbook_catalog;

// Tests in this file touch the process-global APB_CONFIG_DIR - serialize them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn playbook_yaml(id: &str) -> String {
    format!(
        "schema: 1\nid: {id}\nname: {id}\nversion: 1.0.0\ntrigger:\n  when: [\"use when {id}\"]\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n",
    )
}

/// Seeds a playbook into the catalog at `<parent>/playbooks/<id>/1.0.0`.
fn seed(parent: &Path, id: &str) -> String {
    let vdir = parent.join("playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    let yaml = playbook_yaml(id);
    std::fs::write(vdir.join("playbook.yaml"), &yaml).unwrap();
    std::fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
    yaml
}

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

#[test]
fn catalog_lists_both_scopes_with_trust_aware_shadowing() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    // Global review: active + approved.
    let global_yaml = seed(cfg.path(), "review");
    let mut trust = TrustStore::load();
    trust
        .approve(
            &digest_str(&global_yaml),
            "review",
            OriginKind::LocallyApproved,
        )
        .unwrap();

    // Project review: draft (untrusted).
    init_project(proj.path()).unwrap();
    seed(&proj.path().join(".apb"), "review");
    write_lifecycle(&proj.path().join(".apb/playbooks/review"), Lifecycle::Draft).unwrap();

    let cat = playbook_catalog(proj.path(), None, None, None).unwrap();
    let entries = cat["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "both scopes present");

    let proj_entry = entries
        .iter()
        .find(|e| e["ref"]["origin"]["kind"] == "project")
        .unwrap();
    let glob_entry = entries
        .iter()
        .find(|e| e["ref"]["origin"]["kind"] == "global")
        .unwrap();

    // An untrusted project entry does not hide the approved global one: it shadows itself.
    assert_eq!(
        proj_entry["shadowed"], true,
        "untrusted project must be shadowed"
    );
    assert_eq!(
        glob_entry["shadowed"], false,
        "approved global must not be shadowed"
    );
    assert_eq!(proj_entry["lifecycle"], "draft");
    assert_eq!(glob_entry["trusted"], true);
}

#[test]
fn catalog_revision_unchanged_roundtrip() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    init_project(proj.path()).unwrap();
    seed(&proj.path().join(".apb"), "alpha");

    let first = playbook_catalog(proj.path(), None, None, None).unwrap();
    let rev = first["catalog_revision"].as_str().unwrap().to_string();
    let second = playbook_catalog(proj.path(), None, Some(&rev), None).unwrap();
    assert_eq!(second["unchanged"], true);
    assert_eq!(second["catalog_revision"], rev);
}

#[test]
fn broken_playbook_lands_in_diagnostics_not_error() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    init_project(proj.path()).unwrap();
    seed(&proj.path().join(".apb"), "good");
    // Broken playbook: current points to a version, but the yaml is invalid.
    let bad = proj.path().join(".apb/playbooks/bad/1.0.0");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("playbook.yaml"), "this: is: not: valid: playbook").unwrap();
    std::fs::write(proj.path().join(".apb/playbooks/bad/current"), "1.0.0").unwrap();

    let cat = playbook_catalog(proj.path(), None, None, None).unwrap();
    let entries = cat["entries"].as_array().unwrap();
    let diags = cat["diagnostics"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["ref"]["id"] == "good"),
        "good playbook present"
    );
    assert!(
        diags.iter().any(|d| d["id"] == "bad"),
        "bad playbook in diagnostics, not fatal"
    );
}

#[test]
fn both_approved_is_ambiguous_not_silently_shadowed() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    // Both scopes: same id, both active (default) + approved.
    let gy = seed(cfg.path(), "review");
    init_project(proj.path()).unwrap();
    let py = seed(&proj.path().join(".apb"), "review");
    let mut trust = TrustStore::load();
    trust
        .approve(&digest_str(&gy), "review", OriginKind::LocallyApproved)
        .unwrap();
    trust
        .approve(&digest_str(&py), "review", OriginKind::LocallyApproved)
        .unwrap();

    let cat = playbook_catalog(proj.path(), None, None, None).unwrap();
    let entries = cat["entries"].as_array().unwrap();
    let proj_entry = entries
        .iter()
        .find(|e| e["ref"]["origin"]["kind"] == "project")
        .unwrap();
    let glob_entry = entries
        .iter()
        .find(|e| e["ref"]["origin"]["kind"] == "global")
        .unwrap();
    assert_eq!(proj_entry["ambiguous"], true, "both approved -> ambiguous");
    assert_eq!(glob_entry["ambiguous"], true);
    assert_eq!(proj_entry["shadowed"], false, "neither silently shadowed");
    assert_eq!(glob_entry["shadowed"], false);
}
