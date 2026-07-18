use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_mcp::policy::check_run;
use apb_mcp::tools::{playbook_capture, playbook_catalog, suggestion_dismiss};
use serde_json::json;

use crate::common::env_lock as lock;

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
        }
    }
}

fn setup(cfg: &Path) {
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg);
    }
}

fn good_yaml(id: &str) -> String {
    format!(
        "schema: 1\nid: {id}\nname: {id}\nversion: 1.0.0\ntrigger:\n  when: [\"use when {id}\"]\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n",
    )
}

#[test]
fn capture_creates_draft_with_provenance() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let synopsis =
        json!({ "title": "Nightly cleanup", "trigger": { "when": ["run nightly cleanup"] } });
    let res = playbook_capture(proj.path(), &synopsis, "project", &good_yaml("cleanup")).unwrap();

    assert_eq!(res["lifecycle"], "draft");
    assert_eq!(res["trusted"], false);
    assert_eq!(res["provenance"]["created_by"], "agent-capture");

    // A draft does not pass the run gate.
    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "cleanup".into(),
        version: None,
    };
    let refusal = check_run(proj.path(), &wref, false, false).unwrap_err();
    assert_eq!(refusal["policy"], "draft_requires_trial");
}

#[test]
fn capture_rejects_secret_like_values() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let synopsis = json!({ "title": "Deploy", "token": "abcd1234efgh5678 zz" });
    // A secret directly in the synopsis.
    let synopsis_secret = json!({ "note": "api_key: abcd1234efgh5678" });
    let res =
        playbook_capture(proj.path(), &synopsis_secret, "project", &good_yaml("dep")).unwrap();
    assert_eq!(res["rejected"], "secret_like_value");
    let _ = synopsis;
}

#[test]
fn capture_rejects_duplicate_id() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    let synopsis = json!({ "title": "First", "trigger": { "when": ["do first thing"] } });
    playbook_capture(proj.path(), &synopsis, "project", &good_yaml("dup")).unwrap();
    // Second capture of the same id (a different trigger so possible_duplicate does not fire).
    let synopsis2 =
        json!({ "title": "Second", "trigger": { "when": ["do a totally different thing"] } });
    let res = playbook_capture(proj.path(), &synopsis2, "project", &good_yaml("dup")).unwrap();
    assert_eq!(res["rejected"], "duplicate_id");
}

#[test]
fn dismiss_roundtrip_visible_in_catalog() {
    let _l = lock();
    let cfg = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    setup(cfg.path());
    let _g = EnvGuard;
    init_project(proj.path()).unwrap();

    suggestion_dismiss("save-cleanup-playbook", None).unwrap();
    let cat = playbook_catalog(proj.path(), None, None, None).unwrap();
    let dismissed = cat["dismissed_patterns"].as_array().unwrap();
    assert!(dismissed.iter().any(|p| p == "save-cleanup-playbook"));
}
