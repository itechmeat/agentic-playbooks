use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Lifecycle, OriginKind, TrustStore, write_lifecycle};
use apb_mcp::policy::check_run;

use crate::common::env_lock as lock;

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

const SCRIPTED: &str = "schema: 1\nid: s\nname: s\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: lint, type: script, script: \"scripts/lint.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: lint }\n  - { from: lint, to: done }\n";

fn seed_scripted(root: &Path, script: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/s/1.0.0");
    std::fs::create_dir_all(vdir.join("scripts")).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), SCRIPTED).unwrap();
    std::fs::write(vdir.join("scripts/lint.sh"), script).unwrap();
    std::fs::write(root.join(".apb/playbooks/s/current"), "1.0.0").unwrap();
}

/// An approval covers the scripts a playbook runs, not only its YAML: the
/// same YAML with other scripts (another checkout, a changed script) is
/// content the user never approved.
#[test]
fn approval_does_not_cover_other_or_changed_scripts() {
    let _cfg = crate::common::config_sandbox();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    seed_scripted(a.path(), "echo reviewed\n");
    seed_scripted(b.path(), "echo never-reviewed\n");
    apb_mcp::tools::playbook_approve(a.path(), "s", None, "project", None).unwrap();
    assert!(check_run(a.path(), &wref("s"), false, false).is_ok());

    let refusal = check_run(b.path(), &wref("s"), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "untrusted_requires_acknowledge");

    std::fs::write(
        a.path().join(".apb/playbooks/s/1.0.0/scripts/lint.sh"),
        "echo changed\n",
    )
    .unwrap();
    let refusal = check_run(a.path(), &wref("s"), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "untrusted_requires_acknowledge");
}

/// Scripts that cannot be digested (a symlink out of the version) leave the
/// playbook loadable, to view and fix, but with nothing to approve: the gate
/// refuses it even with an acknowledge.
#[cfg(unix)]
#[test]
fn undigestable_scripts_still_load_but_never_run() {
    let _cfg = crate::common::config_sandbox();
    let a = tempfile::tempdir().unwrap();
    seed_scripted(a.path(), "echo ok\n");
    std::os::unix::fs::symlink(
        "/etc/hostname",
        a.path().join(".apb/playbooks/s/1.0.0/scripts/outside"),
    )
    .unwrap();
    let reg = apb_core::registry::Registry::open(a.path()).unwrap();
    let loaded = reg.load("s", None).unwrap();
    assert!(loaded.trust_digest().is_err());
    let refusal = check_run(a.path(), &wref("s"), true, false).unwrap_err();
    assert_eq!(refusal["policy"], "definition_unreadable", "{refusal}");
}

/// Exports a scripted playbook from a scratch project and imports it into
/// `root`; returns the imported version.
fn import_scripted(root: &Path, script: &str) -> String {
    let source = tempfile::tempdir().unwrap();
    seed_scripted(source.path(), script);
    let bundle = apb_core::bundle::export_bundle(source.path(), "s", None).unwrap();
    init_project(root).unwrap();
    apb_core::bundle::import_bundle(root, &bundle, true, None)
        .unwrap()
        .version
}

/// A version imported with scripts runs only after an approval naming its
/// exact digest: an acknowledge does not stand in for it, and a script
/// changed after the approval brings the refusal back.
#[test]
fn imported_scripts_need_an_approval_by_digest_even_with_acknowledge() {
    use apb_core::versioning::approval::{ApproveError, approve_version};
    let _cfg = crate::common::config_sandbox();
    let a = tempfile::tempdir().unwrap();
    let version = import_scripted(a.path(), "echo imported\n");

    let refusal = check_run(a.path(), &wref("s"), true, false).unwrap_err();
    assert_eq!(refusal["policy"], "imported_scripts_require_approval");
    assert_eq!(refusal["version"], version.as_str());
    assert_eq!(refusal["scripts"][0]["path"], "lint.sh");
    let digest = refusal["digest"].as_str().unwrap().to_string();

    let def_parent = a.path().join(".apb");
    let kind = OriginKind::LocallyApproved;
    assert!(matches!(
        approve_version(&def_parent, "s", None, Some("sha256:0"), kind),
        Err(ApproveError::DigestMismatch { .. })
    ));
    approve_version(&def_parent, "s", None, Some(&digest), kind).unwrap();
    assert!(check_run(a.path(), &wref("s"), false, false).is_ok());

    std::fs::write(
        a.path()
            .join(".apb/playbooks/s")
            .join(&version)
            .join("scripts/lint.sh"),
        "echo changed\n",
    )
    .unwrap();
    let refusal = check_run(a.path(), &wref("s"), true, false).unwrap_err();
    assert_eq!(refusal["policy"], "imported_scripts_require_approval");
}

/// `playbook_approve` on a version imported with scripts: without a digest
/// it answers `digest_required` with the digest and the scripts and changes
/// nothing; a wrong digest is `digest_mismatch`; the right one approves it.
#[test]
fn playbook_approve_needs_the_digest_of_imported_scripts() {
    use apb_mcp::tools::playbook_approve;
    let _cfg = crate::common::config_sandbox();
    let a = tempfile::tempdir().unwrap();
    import_scripted(a.path(), "echo imported\n");

    let asked = playbook_approve(a.path(), "s", None, "project", None).unwrap();
    assert_eq!(asked["error"], "digest_required", "{asked}");
    assert_eq!(asked["scripts"][0]["path"], "lint.sh");
    let digest = asked["digest"].as_str().unwrap().to_string();
    assert_eq!(
        check_run(a.path(), &wref("s"), true, false).unwrap_err()["policy"],
        "imported_scripts_require_approval"
    );

    let wrong = playbook_approve(a.path(), "s", None, "project", Some("sha256:0")).unwrap();
    assert_eq!(wrong["error"], "digest_mismatch", "{wrong}");
    assert_eq!(wrong["actual"], digest.as_str());

    let ok = playbook_approve(a.path(), "s", None, "project", Some(&digest)).unwrap();
    assert_eq!(ok["trusted"], true, "{ok}");
    assert_eq!(ok["digest"], digest.as_str());
    assert!(check_run(a.path(), &wref("s"), false, false).is_ok());
}
