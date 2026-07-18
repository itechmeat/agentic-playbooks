use std::fs;
use std::path::Path;

use apb_core::profile::{ProfileScope, QualifiedProfileRef, SkillRef};
use apb_core::profile_store::{PlaybookOrigin, ProfileError, compute_bundle, resolve_profile};
use apb_core::skills::{ensure_claude_bridge, resolve_skill};

// Env mutations (HOME, APB_CONFIG_DIR) are serialized: integration tests run
// in parallel within the same process.
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

fn profile_yaml(name: &str, skills: &str) -> String {
    format!(
        "name: {name}\ndescription: d\nexecutor:\n  agent: claude\n  model: claude-opus-4-8\n{skills}"
    )
}

fn seed_profile(profiles_parent: &Path, name: &str, yaml: &str, soul: &str) {
    let dir = profiles_parent.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("profile.yaml"), yaml).unwrap();
    fs::write(dir.join("SOUL.md"), soul).unwrap();
}

fn seed_skill(skills_parent: &Path, name: &str, body: &str) {
    let dir = skills_parent.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), body).unwrap();
}

/// Prepares a project + temporary HOME + APB_CONFIG_DIR. Returns the tempdirs
/// so they live until the end of the test.
fn setup() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join(".apb/profiles")).unwrap();
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    (proj, home, cfg)
}

#[test]
fn auto_prefers_project_then_global_for_project_origin() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, cfg) = setup();
    seed_profile(
        &proj.path().join(".apb/profiles"),
        "p",
        &profile_yaml("p", ""),
        "proj soul",
    );
    seed_profile(
        &cfg.path().join("profiles"),
        "p",
        &profile_yaml("p", ""),
        "glob soul",
    );

    let r = QualifiedProfileRef {
        name: "p".into(),
        scope: ProfileScope::Auto,
    };
    let loaded = resolve_profile(proj.path(), PlaybookOrigin::Project, &r).unwrap();
    assert_eq!(loaded.scope, ProfileScope::Project);
    assert_eq!(loaded.soul, "proj soul");
}

#[test]
fn global_origin_never_sees_project_profiles() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, _cfg) = setup();
    seed_profile(
        &proj.path().join(".apb/profiles"),
        "only",
        &profile_yaml("only", ""),
        "",
    );

    let r = QualifiedProfileRef {
        name: "only".into(),
        scope: ProfileScope::Auto,
    };
    let err = resolve_profile(proj.path(), PlaybookOrigin::Global, &r).unwrap_err();
    assert!(matches!(err, ProfileError::NotFound(_)));
}

#[test]
fn explicit_scope_project_in_global_playbook_is_error() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, _cfg) = setup();
    let r = QualifiedProfileRef {
        name: "x".into(),
        scope: ProfileScope::Project,
    };
    let err = resolve_profile(proj.path(), PlaybookOrigin::Global, &r).unwrap_err();
    assert!(matches!(err, ProfileError::ScopeForbidden(_)));
}

#[test]
fn global_profile_cannot_use_project_skills() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, home, cfg) = setup();
    // The global profile references a skill with scope: project.
    let yaml = profile_yaml("g", "skills:\n  - { name: s, scope: project }\n");
    seed_profile(&cfg.path().join("profiles"), "g", &yaml, "");
    seed_skill(&proj.path().join(".agents/skills"), "s", "x");
    seed_skill(&home.path().join(".agents/skills"), "s", "x");

    let r = QualifiedProfileRef {
        name: "g".into(),
        scope: ProfileScope::Global,
    };
    let err = compute_bundle(proj.path(), PlaybookOrigin::Project, &r).unwrap_err();
    assert!(matches!(err, ProfileError::ScopeForbidden(_)));
}

#[test]
fn project_skill_shadows_global_same_name() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, home, _cfg) = setup();
    seed_skill(
        &proj.path().join(".agents/skills"),
        "dup",
        "project version",
    );
    seed_skill(&home.path().join(".agents/skills"), "dup", "global version");

    let s = SkillRef {
        name: "dup".into(),
        scope: ProfileScope::Auto,
    };
    let resolved = resolve_skill(proj.path(), ProfileScope::Project, &s).unwrap();
    assert_eq!(resolved.scope, ProfileScope::Project);
}

#[test]
fn same_name_two_scopes_coexist() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, cfg) = setup();
    seed_profile(
        &proj.path().join(".apb/profiles"),
        "reviewer",
        &profile_yaml("reviewer", ""),
        "P",
    );
    seed_profile(
        &cfg.path().join("profiles"),
        "reviewer",
        &profile_yaml("reviewer", ""),
        "G",
    );

    let p = resolve_profile(
        proj.path(),
        PlaybookOrigin::Project,
        &QualifiedProfileRef {
            name: "reviewer".into(),
            scope: ProfileScope::Project,
        },
    )
    .unwrap();
    let g = resolve_profile(
        proj.path(),
        PlaybookOrigin::Project,
        &QualifiedProfileRef {
            name: "reviewer".into(),
            scope: ProfileScope::Global,
        },
    )
    .unwrap();
    assert_eq!(p.soul, "P");
    assert_eq!(g.soul, "G");
    assert_ne!(p.profile_digest, g.profile_digest);
}

#[test]
fn name_dir_mismatch_and_casefold_collision_rejected() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, _cfg) = setup();
    // Name inside the file != directory name.
    seed_profile(
        &proj.path().join(".apb/profiles"),
        "dirname",
        &profile_yaml("othername", ""),
        "",
    );
    let err = resolve_profile(
        proj.path(),
        PlaybookOrigin::Project,
        &QualifiedProfileRef {
            name: "dirname".into(),
            scope: ProfileScope::Project,
        },
    )
    .unwrap_err();
    assert!(matches!(err, ProfileError::NameMismatch { .. }));

    // Case-fold collision between two directories. Only reproducible on a
    // case-sensitive filesystem; on macOS (APFS, case-insensitive by default)
    // the names collapse into a single directory and there is no physical
    // collision - so we skip the check there (the logic is still covered
    // on Linux CI).
    let p2 = tempfile::tempdir().unwrap();
    let profiles2 = p2.path().join(".apb/profiles");
    fs::create_dir_all(&profiles2).unwrap();
    seed_profile(&profiles2, "reviewer", &profile_yaml("reviewer", ""), "");
    seed_profile(&profiles2, "Reviewer", &profile_yaml("Reviewer", ""), "");
    let distinct = fs::read_dir(&profiles2)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().to_lowercase() == "reviewer")
        .count();
    if distinct >= 2 {
        let err2 = resolve_profile(
            p2.path(),
            PlaybookOrigin::Project,
            &QualifiedProfileRef {
                name: "reviewer".into(),
                scope: ProfileScope::Project,
            },
        )
        .unwrap_err();
        assert!(matches!(err2, ProfileError::CaseFoldCollision(_)));
    }
}

#[test]
fn bundle_changes_when_skill_changes() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, _cfg) = setup();
    let yaml = profile_yaml("b", "skills:\n  - coding-standards\n");
    seed_profile(&proj.path().join(".apb/profiles"), "b", &yaml, "soul");
    seed_skill(
        &proj.path().join(".agents/skills"),
        "coding-standards",
        "v1",
    );

    let r = QualifiedProfileRef {
        name: "b".into(),
        scope: ProfileScope::Project,
    };
    let (p1, _s1, bundle1) = compute_bundle(proj.path(), PlaybookOrigin::Project, &r).unwrap();

    fs::write(
        proj.path().join(".agents/skills/coding-standards/SKILL.md"),
        "v2 changed",
    )
    .unwrap();
    let (p2, _s2, bundle2) = compute_bundle(proj.path(), PlaybookOrigin::Project, &r).unwrap();

    assert_ne!(bundle1, bundle2);
    // profile_digest doesn't change - only the skill changed (comparing against the pre-edit value).
    assert_eq!(p2.profile_digest, p1.profile_digest);
}

#[test]
fn unsafe_skill_name_is_rejected() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, _cfg) = setup();
    // Unsafe names (directory traversal, absolute path) are rejected BEFORE any
    // filesystem access - otherwise one could read/delete/overwrite other
    // directories via `dir.join(name)`.
    for bad in ["../evil", "a/b", "/tmp/victim", "..", ".hidden"] {
        let s = SkillRef {
            name: bad.into(),
            scope: ProfileScope::Auto,
        };
        let err = resolve_skill(proj.path(), ProfileScope::Project, &s).unwrap_err();
        assert!(
            matches!(err, ProfileError::Invalid(_)),
            "name `{bad}` must be rejected, got {err:?}"
        );
    }
}

#[test]
fn claude_bridge_idempotent_and_respects_real_dirs() {
    let _l = lock();
    let _g = EnvGuard;
    let (proj, _home, _cfg) = setup();
    let skills_parent = proj.path().join(".agents/skills");
    let claude_parent = proj.path().join(".claude/skills");
    seed_skill(&skills_parent, "bridged", "x");
    seed_skill(&skills_parent, "conflict", "x");
    // A real directory in .claude/skills with the same name is left untouched.
    fs::create_dir_all(claude_parent.join("conflict")).unwrap();

    let notes1 = ensure_claude_bridge(&skills_parent, &claude_parent);
    assert!(notes1.iter().any(|n| n.contains("conflict")));
    let link = claude_parent.join("bridged");
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // Idempotency: the second call doesn't duplicate anything or fail.
    let _notes2 = ensure_claude_bridge(&skills_parent, &claude_parent);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
