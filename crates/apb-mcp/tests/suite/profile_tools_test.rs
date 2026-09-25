use std::fs;
use std::path::Path;

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{OriginKind, TrustStore};
use apb_mcp::policy::check_run;
use apb_mcp::profile_tools::{self, ExecutorInput};

use crate::common::env_lock as lock;

struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
        }
    }
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    init_project(proj.path()).unwrap();
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    (proj, home, cfg)
}

fn exec() -> ExecutorInput {
    ExecutorInput {
        agent: "claude".into(),
        model: "haiku".into(),
        fallbacks: vec![],
    }
}

fn write_profile(
    root: &Path,
    name: &str,
    skills: &[String],
    expected: Option<&str>,
) -> serde_json::Value {
    profile_tools::profile_write(
        root,
        profile_tools::ProfileWrite {
            name: name.to_string(),
            scope: "project".into(),
            description: "desc".into(),
            soul_md: "role".into(),
            skills: profile_tools::skill_refs(skills),
            executor: exec(),
            expected_digest: expected.map(str::to_string),
            ..Default::default()
        },
    )
    .expect("profile_write ok")
}

fn seed_skill(root: &Path, name: &str, body: &str) {
    let d = root.join(".agents/skills").join(name);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("SKILL.md"), body).unwrap();
}

fn seed_playbook(root: &Path, id: &str, profile: &str) -> String {
    let yaml = format!(
        "schema: 1\nid: {id}\nname: W\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: t, type: agent_task, prompt: \"do\", profile: {profile} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: t }}\n  - {{ from: t, to: done }}\n"
    );
    let dir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("playbook.yaml"), &yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    yaml
}

#[test]
fn profile_write_persists_hermetic_field() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();

    profile_tools::profile_write(
        proj.path(),
        profile_tools::ProfileWrite {
            name: "herm".into(),
            scope: "project".into(),
            description: "desc".into(),
            soul_md: "role".into(),
            executor: exec(),
            hermetic: Some(true),
            ..Default::default()
        },
    )
    .expect("profile_write ok");
    let yaml = fs::read_to_string(proj.path().join(".apb/profiles/herm/profile.yaml")).unwrap();
    assert!(
        yaml.contains("hermetic: true"),
        "expected hermetic: true in written profile.yaml, got:\n{yaml}"
    );
    let doc = apb_core::profile::ProfileDoc::from_yaml(&yaml).unwrap();
    assert!(doc.hermetic);

    profile_tools::profile_write(
        proj.path(),
        profile_tools::ProfileWrite {
            name: "plain".into(),
            scope: "project".into(),
            description: "desc".into(),
            soul_md: "role".into(),
            executor: exec(),
            hermetic: Some(false),
            ..Default::default()
        },
    )
    .expect("profile_write ok");
    let yaml_plain =
        fs::read_to_string(proj.path().join(".apb/profiles/plain/profile.yaml")).unwrap();
    let doc_plain = apb_core::profile::ProfileDoc::from_yaml(&yaml_plain).unwrap();
    assert!(
        !doc_plain.hermetic,
        "hermetic: false (or omitted) must parse back as false; yaml:\n{yaml_plain}"
    );
}

/// F15: an update that does not mention `hermetic` (MCP `profile_write` without
/// the field, `apb profile write` without the flag) keeps the stored flag
/// instead of silently turning hermetic isolation off.
#[test]
fn profile_update_without_hermetic_keeps_stored_flag() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    let write = |desc: &str, expected: Option<String>| {
        profile_tools::profile_write(
            proj.path(),
            profile_tools::ProfileWrite {
                name: "herm".into(),
                scope: "project".into(),
                description: desc.into(),
                soul_md: "role".into(),
                executor: exec(),
                expected_digest: expected,
                ..Default::default()
            },
        )
        .expect("profile_write ok")
    };
    let created = profile_tools::profile_write(
        proj.path(),
        profile_tools::ProfileWrite {
            name: "herm".into(),
            scope: "project".into(),
            description: "desc".into(),
            soul_md: "role".into(),
            executor: exec(),
            hermetic: Some(true),
            ..Default::default()
        },
    )
    .expect("profile_write ok");
    let digest = created["profile_digest"].as_str().unwrap().to_string();
    write("updated", Some(digest));
    let yaml = fs::read_to_string(proj.path().join(".apb/profiles/herm/profile.yaml")).unwrap();
    let doc = apb_core::profile::ProfileDoc::from_yaml(&yaml).unwrap();
    assert_eq!(doc.description, "updated");
    assert!(
        doc.hermetic,
        "an update that omits hermetic must keep it; yaml:\n{yaml}"
    );
}

/// zcode profiles are stored with the bare model id (the legacy
/// `zai-individual/` prefix is accepted and dropped, in fallbacks too), and a
/// model or plan outside apb's allowlist is refused with a clear message.
#[test]
fn profile_write_normalizes_and_gates_zcode_models() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    let write = |name: &str, model: &str| {
        profile_tools::profile_write(
            proj.path(),
            profile_tools::ProfileWrite {
                name: name.into(),
                scope: "project".into(),
                description: "d".into(),
                soul_md: "role".into(),
                executor: ExecutorInput {
                    agent: "zcode".into(),
                    model: model.into(),
                    fallbacks: vec![("zcode".into(), "zai-individual/GLM-5.3".into())],
                },
                ..Default::default()
            },
        )
    };
    write("legacy", "zai-individual/glm-5.3-flash@HIGH").expect("legacy spelling accepted");
    let yaml = fs::read_to_string(proj.path().join(".apb/profiles/legacy/profile.yaml")).unwrap();
    let doc = apb_core::profile::ProfileDoc::from_yaml(&yaml).unwrap();
    assert_eq!(doc.executor.model, "GLM-5.3-Flash@high");
    assert_eq!(doc.executor.fallbacks[0].model, "GLM-5.3");
    assert!(!yaml.contains("zai-individual"), "{yaml}");

    for (name, bad) in [("turbo", "GLM-5-Turbo"), ("start", "zai-start/GLM-5.3")] {
        let err = write(name, bad).unwrap_err().to_string();
        assert!(err.contains("not allowed"), "{bad}: {err}");
        assert!(err.contains("GLM-5.3-Flash"), "{bad}: {err}");
        assert!(!proj.path().join(".apb/profiles").join(name).exists());
    }
}

#[test]
fn write_create_then_conflict_on_double_create() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    write_profile(proj.path(), "p", &[], None);
    let err = profile_tools::profile_write(
        proj.path(),
        profile_tools::ProfileWrite {
            name: "p".into(),
            scope: "project".into(),
            description: "d".into(),
            soul_md: "role".into(),
            executor: exec(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("conflict"),
        "expected conflict, got {err}"
    );
}

#[test]
fn write_update_requires_matching_expected_digest() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    let created = write_profile(proj.path(), "p", &[], None);
    let digest = created["profile_digest"].as_str().unwrap();

    // Wrong expected_digest -> conflict.
    let stale = profile_tools::profile_write(
        proj.path(),
        profile_tools::ProfileWrite {
            name: "p".into(),
            scope: "project".into(),
            description: "changed".into(),
            soul_md: "role".into(),
            executor: exec(),
            expected_digest: Some("sha256:wrong".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(stale.to_string().contains("conflict"));

    // Correct expected_digest -> ok.
    write_profile(proj.path(), "p", &[], Some(digest));
}

#[test]
fn concurrent_writes_one_wins() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    let root = proj.path();
    let results = std::thread::scope(|s| {
        let a = s.spawn(|| {
            profile_tools::profile_write(
                root,
                profile_tools::ProfileWrite {
                    name: "p".into(),
                    scope: "project".into(),
                    description: "d".into(),
                    soul_md: "role".into(),
                    executor: exec(),
                    ..Default::default()
                },
            )
        });
        let b = s.spawn(|| {
            profile_tools::profile_write(
                root,
                profile_tools::ProfileWrite {
                    name: "p".into(),
                    scope: "project".into(),
                    description: "d".into(),
                    soul_md: "role".into(),
                    executor: exec(),
                    ..Default::default()
                },
            )
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let oks = [results.0.is_ok(), results.1.is_ok()]
        .iter()
        .filter(|x| **x)
        .count();
    assert_eq!(oks, 1, "exactly one create must win");
}

#[test]
fn write_autoapproves_bundle_and_skill_edit_untrusts_next_run() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    seed_skill(proj.path(), "cs", "v1");
    write_profile(proj.path(), "arch", &["cs".to_string()], None);
    let playbook_yaml = seed_playbook(proj.path(), "w", "arch");

    // Approve the playbook itself (otherwise the gate would stop on it before the profile).
    let mut trust = TrustStore::load();
    trust
        .approve(
            &digest_str(&playbook_yaml),
            "w",
            OriginKind::LocallyApproved,
        )
        .unwrap();

    let wref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "w".into(),
        version: None,
    };
    // The profile was just created by the profile tool -> its bundle is auto-approved.
    assert!(
        check_run(proj.path(), &wref, false, false).is_ok(),
        "trusted profile should pass"
    );

    // Edit the skill -> the bundle changes -> the next run is untrusted.
    fs::write(proj.path().join(".agents/skills/cs/SKILL.md"), "v2 changed").unwrap();
    let refusal = check_run(proj.path(), &wref, false, false).unwrap_err();
    assert_eq!(refusal["policy"], "untrusted_profile_requires_acknowledge");
}

#[test]
fn playbook_profile_bundles_change_on_skill_edit() {
    // Plan-binding mechanism (PlanPayload.profiles): the <scope/name, bundle>
    // pairs must change when a skill is edited, otherwise the prepare/execute
    // reconciliation would not catch the drift.
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    seed_skill(proj.path(), "cs", "v1");
    write_profile(proj.path(), "arch", &["cs".to_string()], None);
    seed_playbook(proj.path(), "w", "arch");

    let before = apb_mcp::policy::playbook_profile_bundles(proj.path(), "w", None, false);
    assert!(!before.is_empty(), "expected at least one bundle");
    fs::write(proj.path().join(".agents/skills/cs/SKILL.md"), "v2 changed").unwrap();
    let after = apb_mcp::policy::playbook_profile_bundles(proj.path(), "w", None, false);
    assert_ne!(
        before, after,
        "skill edit must change the plan-bound bundles"
    );
}

#[test]
fn delete_blocked_by_reference_unless_forced() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    write_profile(proj.path(), "arch", &[], None);
    seed_playbook(proj.path(), "w", "arch");

    let err = profile_tools::profile_delete(proj.path(), "arch", "project", false).unwrap_err();
    assert!(
        err.to_string().contains("referenced"),
        "expected referenced error, got {err}"
    );

    // With force - it is deleted.
    profile_tools::profile_delete(proj.path(), "arch", "project", true).expect("force delete ok");
}

/// Rewriting a profile does not approve skill content that changed since the
/// profile was last approved: the write produced profile.yaml and SOUL.md,
/// not the skills. The changed skills come back for the user's consent.
#[test]
fn a_rewrite_does_not_approve_drifted_skill_content() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _h, _c) = setup();
    seed_skill(proj.path(), "helper", "v1");
    let first = write_profile(proj.path(), "dev", &["helper".to_string()], None);

    seed_skill(proj.path(), "helper", "v2 from upstream");
    let rewritten = write_profile(
        proj.path(),
        "dev",
        &["helper".to_string()],
        first["profile_digest"].as_str(),
    );
    let bundle = rewritten["bundle_digest"].as_str().unwrap();
    assert!(
        !TrustStore::load().is_approved(bundle),
        "drifted skill content must stay unapproved: {rewritten}"
    );
    assert_eq!(
        rewritten["skills_unapproved"][0]["skill"], "project/helper",
        "{rewritten}"
    );
}
