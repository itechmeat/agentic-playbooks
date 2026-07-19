//! Advisory tools: the onboarding howto flag, subscriptions_set, adopt codes,
//! independence of the catalog revision from the profile count. Env is global - under a
//! shared Mutex; PATH is set to an empty temp dir for the duration of each test (detection
//! finds no one - model codes are deterministic), then restored by `Ctx`'s `Drop` impl.
//!
//! Restoring PATH matters specifically because of test consolidation: this file used to run
//! in its own process, so clobbering PATH for the whole process lifetime was harmless. Now
//! all modules share one process, and other modules' tests shell out to real commands (e.g.
//! `trial_tools_test`'s `git`, and scripts using `touch`) - leaving PATH pointed at an empty
//! dir after this file's tests broke `touch` for every test that ran after these in the same
//! binary (observed directly: `supervisor_tools_test`'s flaky-agent script failed every time
//! with `touch: command not found` once these tests had run first). APB_CONFIG_DIR/HOME/
//! APB_NO_REGISTRY are still just removed (not restored to a prior value) on Drop, matching
//! the `EnvGuard` idiom the other env-mutating files in this suite already use for those same
//! two vars - every test that depends on them sets its own value before reading it, so an
//! absent value is always safe, whereas PATH is read by code this test doesn't control.

use std::ffi::OsString;
use std::path::Path;

use apb_mcp::advisory_tools;

use crate::common::env_lock as lock;

struct Ctx {
    _proj: tempfile::TempDir,
    _cfg: tempfile::TempDir,
    _home: tempfile::TempDir,
    _bin: tempfile::TempDir,
    root: std::path::PathBuf,
    orig_path: Option<OsString>,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        unsafe {
            match self.orig_path.take() {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
            std::env::remove_var("APB_NO_REGISTRY");
        }
    }
}

fn setup() -> Ctx {
    let proj = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let orig_path = std::env::var_os("PATH");
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
        std::env::set_var("HOME", home.path());
        std::env::set_var("PATH", bin.path()); // no agent is installed
        std::env::set_var("APB_NO_REGISTRY", "1");
    }
    Ctx {
        root: proj.path().to_path_buf(),
        _proj: proj,
        _cfg: cfg,
        _home: home,
        _bin: bin,
        orig_path,
    }
}

fn seed_profile(root: &Path, name: &str, agent: &str, skills: &str) {
    let dir = root.join(".apb/profiles").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let yaml =
        format!("name: {name}\ndescription: d\nexecutor:\n  agent: {agent}\n  model: m1\n{skills}");
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();
}

fn seed_skill(root: &Path, name: &str) {
    let dir = root.join(".agents/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "v1").unwrap();
}

fn seed_playbook(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    std::fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

#[test]
fn howto_flags_uninitialized_then_declined_clears_it() {
    let _l = lock();
    let _c = setup();
    let h1 = advisory_tools::profile_howto().unwrap();
    assert_eq!(h1["subscriptions_uninitialized"], serde_json::json!(true));
    assert!(
        h1["models_table"]["models"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    );

    advisory_tools::subscriptions_set(Vec::new(), true).unwrap();
    let h2 = advisory_tools::profile_howto().unwrap();
    assert!(
        h2.get("subscriptions_uninitialized").is_none(),
        "declined must clear the flag"
    );
}

#[test]
fn subscriptions_set_writes_overlay_and_configures() {
    let _l = lock();
    let _c = setup();
    let subs = vec![apb_core::models_table::Subscription {
        agent: "claude".into(),
        plan: Some("max".into()),
        coverage: apb_core::models_table::Coverage::Full,
    }];
    advisory_tools::subscriptions_set(subs, false).unwrap();
    let t = apb_core::models_table::load_merged().unwrap();
    assert_eq!(t.subscriptions.len(), 1);
    assert_eq!(
        apb_core::models_table::onboarding::read().unwrap(),
        apb_core::models_table::OnboardingState::Configured
    );
    let h = advisory_tools::profile_howto().unwrap();
    assert!(h.get("subscriptions_uninitialized").is_none());
}

#[test]
fn adopt_report_emits_expected_codes() {
    let _l = lock();
    let c = setup();
    // goodp: a valid profile (the skill exists), NOT trusted, agent outside the top six.
    seed_skill(&c.root, "s1");
    seed_profile(&c.root, "goodp", "customx", "skills:\n  - s1\n");
    // The pipeline references goodp and a nonexistent ghost.
    let playbook = "schema: 1\nid: wf1\nname: W\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: \"do\", profile: goodp }\n  - { id: b, type: agent_task, prompt: \"do\", profile: ghost }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: done }\n";
    seed_playbook(&c.root, "wf1", playbook);

    let report = advisory_tools::playbook_adopt_report(&c.root, Some("wf1")).unwrap();
    let findings = report["playbooks"][0]["findings"].as_array().unwrap();
    let codes: Vec<&str> = findings.iter().filter_map(|f| f["code"].as_str()).collect();
    assert!(
        codes.contains(&"profile_missing"),
        "ghost -> profile_missing: {codes:?}"
    );
    assert!(
        codes.contains(&"untrusted"),
        "goodp bundle untrusted: {codes:?}"
    );
    assert!(
        codes.contains(&"model_unverifiable"),
        "customx agent unverifiable: {codes:?}"
    );
}

#[test]
fn adopt_report_flags_missing_skill() {
    let _l = lock();
    let c = setup();
    // The profile references a skill that does not exist -> compute_bundle -> skill_missing.
    seed_profile(&c.root, "p2", "claude", "skills:\n  - nope\n");
    let playbook = "schema: 1\nid: wf2\nname: W\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: \"do\", profile: p2 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: done }\n";
    seed_playbook(&c.root, "wf2", playbook);
    let report = advisory_tools::playbook_adopt_report(&c.root, Some("wf2")).unwrap();
    let codes: Vec<String> = report["playbooks"][0]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["code"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        codes.iter().any(|c| c == "skill_missing"),
        "expected skill_missing, got {codes:?}"
    );
}

#[test]
fn adopt_report_explicit_missing_id_is_error_finding() {
    let _l = lock();
    let c = setup();
    // The registry must exist (Registry::open requires .apb); the playbook does not.
    std::fs::create_dir_all(c.root.join(".apb/playbooks")).unwrap();
    // An explicit id for a nonexistent playbook - NOT an empty report, but a diagnostic.
    let report = advisory_tools::playbook_adopt_report(&c.root, Some("ghostwf")).unwrap();
    let w = &report["playbooks"][0];
    assert_eq!(w["id"], serde_json::json!("ghostwf"));
    let codes: Vec<&str> = w["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"playbook_unloadable"),
        "missing id must yield playbook_unloadable: {codes:?}"
    );
}

#[test]
fn adopt_report_all_mode_flags_unloadable_playbook() {
    let _l = lock();
    let c = setup();
    // A broken playbook.yaml in all-mode -> a diagnostic on its id, not a silent skip.
    seed_playbook(&c.root, "broke", "this: is: not: valid: yaml: ][");
    let report = advisory_tools::playbook_adopt_report(&c.root, None).unwrap();
    let found = report["playbooks"].as_array().unwrap().iter().any(|w| {
        w["id"] == serde_json::json!("broke")
            && w["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["code"] == serde_json::json!("playbook_unloadable"))
    });
    assert!(
        found,
        "all-mode must flag the unloadable playbook: {report}"
    );
}

#[test]
fn catalog_revision_independent_of_profile_count() {
    let _l = lock();
    let c = setup();
    let playbook = "schema: 1\nid: wf1\nname: W\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";
    seed_playbook(&c.root, "wf1", playbook);

    let before = apb_mcp::catalog::build(&c.root, None, None, None, Vec::new());
    let rev_before = before["catalog_revision"].as_str().unwrap().to_string();
    assert_eq!(before["profiles_hint"]["count"], serde_json::json!(0));

    // Add a profile - the catalog revision does not change, the hint grows.
    seed_skill(&c.root, "s1");
    seed_profile(&c.root, "goodp", "claude", "skills:\n  - s1\n");
    let after = apb_mcp::catalog::build(&c.root, None, None, None, Vec::new());
    assert_eq!(
        after["catalog_revision"].as_str().unwrap(),
        rev_before,
        "revision must not depend on profiles"
    );
    assert_eq!(after["profiles_hint"]["count"], serde_json::json!(1));
}
