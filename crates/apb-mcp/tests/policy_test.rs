use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Lifecycle, OriginKind, TrustStore, write_lifecycle};
use apb_mcp::policy::check_run;

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

fn yaml(id: &str, extra: &str) -> String {
    format!(
        "schema: 1\nid: {id}\nname: {id}\nversion: 1.0.0\n{extra}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n",
    )
}

fn seed_project(root: &Path, id: &str, extra: &str) -> String {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    let y = yaml(id, extra);
    std::fs::write(vdir.join("playbook.yaml"), &y).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    y
}

fn wref(id: &str) -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: id.into(),
        version: None,
    }
}

fn approve(y: &str, id: &str) {
    let mut t = TrustStore::load();
    t.approve(&digest_str(y), id, OriginKind::LocallyApproved)
        .unwrap();
}

#[test]
fn draft_playbook_is_refused() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    let y = seed_project(proj.path(), "d", "");
    approve(&y, "d"); // even approved, but draft - does not run directly
    write_lifecycle(&proj.path().join(".apb/playbooks/d"), Lifecycle::Draft).unwrap();

    let refusal = check_run(proj.path(), &wref("d"), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "draft_requires_trial");
}

#[test]
fn untrusted_needs_acknowledge_and_digest_drift_revokes() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    let y = seed_project(proj.path(), "u", "");
    approve(&y, "u");
    // Approved active - passes.
    assert!(check_run(proj.path(), &wref("u"), false, false).is_ok());

    // Digest drift: edit the version file on disk -> digest shifts -> untrusted.
    let vpath = proj.path().join(".apb/playbooks/u/1.0.0/playbook.yaml");
    let drifted = format!("{y}# drift\n");
    std::fs::write(&vpath, drifted).unwrap();
    let refusal = check_run(proj.path(), &wref("u"), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "untrusted_requires_acknowledge");
    // With acknowledge - passes.
    assert!(check_run(proj.path(), &wref("u"), true, false).is_ok());
}

#[test]
fn cross_workspace_is_refused() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    seed_project(proj.path(), "x", "");
    let foreign = PlaybookRef {
        origin: Origin::Project {
            workspace_id: Some("ws-other".into()),
        },
        id: "x".into(),
        version: None,
    };
    let refusal = check_run(proj.path(), &foreign, true, false).unwrap_err();
    assert_eq!(refusal["policy"], "cross_workspace_requires_plan");
}

#[test]
fn requires_preflight_reports_missing() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    let y = seed_project(proj.path(), "r", "requires:\n  files: [\"Cargo.toml\"]\n");
    approve(&y, "r");
    let refusal = check_run(proj.path(), &wref("r"), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "requires_unmet");
    assert!(
        refusal["missing"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "file:Cargo.toml")
    );
}

#[test]
fn requires_unsafe_path_is_rejected() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    // `../` in requires.files must not turn into an existence oracle.
    let y = seed_project(proj.path(), "esc", "requires:\n  files: [\"../secret\"]\n");
    approve(&y, "esc");
    let refusal = check_run(proj.path(), &wref("esc"), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "requires_unsafe_path");
}

#[test]
fn check_run_returns_verified_digest() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    let _g = EnvGuard;

    let y = seed_project(proj.path(), "d2", "");
    approve(&y, "d2");
    let permit = check_run(proj.path(), &wref("d2"), false, false).unwrap();
    assert_eq!(permit.playbook_digest, digest_str(&y));
}
