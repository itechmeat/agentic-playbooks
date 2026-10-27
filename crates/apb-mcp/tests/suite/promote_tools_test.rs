//! `playbook_make_global`: the plan and its blockers, the three fates of the
//! project copy, trust carried by identical digests, memory moved to the
//! global scope, and profiles copied on request.

use std::path::Path;

use apb_core::registry::{Registry, init_project};
use apb_core::run_memory::{MemoryKey, RememberInput, recall, remember};
use apb_core::scope::Scope;
use apb_core::trust::{OriginKind, TrustStore};
use apb_mcp::catalog::CatalogRequest;
use apb_mcp::promote::{MakeGlobalOpts, Variant, make_global};
use apb_mcp::tools::playbook_catalog;
use serde_json::Value;

/// A playbook whose extra nodes (each `- { id: <x>, ... }` on its own
/// line) run in order between start and done.
fn playbook(id: &str, extra_nodes: &str) -> String {
    let mut chain = vec!["start".to_string()];
    chain.extend(extra_nodes.lines().filter_map(|l| {
        l.split("id: ")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .map(str::to_string)
    }));
    chain.push("done".to_string());
    let edges: String = chain
        .windows(2)
        .map(|w| format!("  - {{ from: {}, to: {} }}\n", w[0], w[1]))
        .collect();
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nparams:\n  - {{ name: who, type: text }}\nnodes:\n  - {{ id: start, type: start }}\n{extra_nodes}  - {{ id: done, type: finish, outcome: success }}\nedges:\n{edges}"
    )
}

fn seed(parent: &Path, id: &str, yaml: &str) {
    let vdir = parent.join("playbooks").join(id).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    std::fs::write(parent.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

fn seed_profile(root: &Path, name: &str, skills: &str) {
    let dir = root.join(".apb/profiles").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        format!("name: {name}\ndescription: d\nexecutor:\n  agent: claude\n  model: m1\n{skills}"),
    )
    .unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();
}

/// A project with an approved project playbook `rel`.
fn project_with(id: &str, yaml: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    init_project(root.path()).unwrap();
    seed(&root.path().join(".apb"), id, yaml);
    let digest = digest_of(&root.path().join(".apb"), id);
    TrustStore::load()
        .approve(&digest, id, OriginKind::LocallyApproved)
        .unwrap();
    root
}

fn digest_of(parent: &Path, id: &str) -> String {
    Registry::open_dir(parent)
        .unwrap()
        .load(id, None)
        .unwrap()
        .trust_digest()
        .unwrap()
}

fn global_parent() -> std::path::PathBuf {
    apb_core::store::global_playbooks_parent().unwrap()
}

fn codes(out: &Value, list: &str) -> Vec<String> {
    out["plan"][list]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_string())
        .collect()
}

fn opts(variant: Option<Variant>) -> MakeGlobalOpts {
    MakeGlobalOpts {
        variant,
        ..Default::default()
    }
}

fn entries_for(root: &Path, id: &str) -> Vec<Value> {
    playbook_catalog(root, &CatalogRequest::default()).unwrap()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["ref"]["id"] == id)
        .cloned()
        .collect()
}

#[test]
fn a_dry_run_lists_blockers_and_changes_nothing() {
    let _cfg = crate::common::config_sandbox();
    let nodes = "  - { id: a, type: agent_task, prompt: \"x\", profile: local }\n  - { id: sub, type: playbook, playbook: helper }\n";
    let root = project_with("rel", &playbook("rel", nodes));
    seed_profile(root.path(), "local", "");
    seed(&root.path().join(".apb"), "helper", &playbook("helper", ""));
    seed(&global_parent(), "rel", &playbook("rel", ""));

    let out = make_global(
        root.path(),
        "rel",
        MakeGlobalOpts {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(out["applied"], false);
    let blockers = codes(&out, "blockers");
    for code in [
        "profile_project_only",
        "sub_playbook_not_global",
        "global_id_taken",
    ] {
        assert!(blockers.contains(&code.to_string()), "{code}: {blockers:?}");
    }
    assert!(!blockers.contains(&"variant_fate_required".to_string()));
    assert!(root.path().join(".apb/playbooks/rel").is_dir());
}

#[test]
fn the_fate_of_the_project_copy_must_be_chosen() {
    let _cfg = crate::common::config_sandbox();
    let root = project_with("rel", &playbook("rel", ""));
    let out = make_global(root.path(), "rel", opts(None)).unwrap();
    assert_eq!(out["applied"], false);
    assert_eq!(codes(&out, "blockers"), vec!["variant_fate_required"]);
    assert!(!global_parent().join("playbooks/rel").exists());
}

/// trash: the project copy goes to the trash, the global copy digests the
/// same and stays trusted, the catalog shows one clean entry, and the
/// project's remembered input follows the playbook.
#[test]
fn trash_moves_the_playbook_its_trust_and_its_memory() {
    let _cfg = crate::common::config_sandbox();
    let yaml = playbook("rel", "");
    let root = project_with("rel", &yaml);
    let params = apb_core::schema::Playbook::from_yaml(&yaml).unwrap().params;
    let project_key = MemoryKey::new(Scope::Project, "rel").unwrap();
    remember(
        root.path(),
        &project_key,
        &params,
        "1.0.0",
        RememberInput {
            params: Some([("who".to_string(), "team".to_string())].into()),
            instruction: None,
        },
    )
    .unwrap();
    let digest = digest_of(&root.path().join(".apb"), "rel");

    let out = make_global(root.path(), "rel", opts(Some(Variant::Trash))).unwrap();
    assert_eq!(out["applied"], true, "{out}");
    assert_eq!(out["fit"]["scope"], "global");
    assert_eq!(digest_of(&global_parent(), "rel"), digest);
    assert!(out["fit"]["trusted"].as_bool().unwrap(), "{out}");
    assert!(!root.path().join(".apb/playbooks/rel").exists());
    let trash = apb_core::versioning::list_trash(root.path()).unwrap();
    assert_eq!(trash[0].id, "rel");

    let entries = entries_for(root.path(), "rel");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["shadowed"], false);
    assert_eq!(entries[0]["ambiguous"], false);

    let global_key = MemoryKey::new(Scope::Global, "rel").unwrap();
    let moved = recall(root.path(), &global_key, &params, None).unwrap();
    assert_eq!(
        moved.compatible().collect::<Vec<_>>(),
        vec![("who", "team")]
    );
    let left = recall(root.path(), &project_key, &params, None).unwrap();
    assert!(left.values.is_empty());
}

#[test]
fn retire_shadows_the_project_copy_and_keep_makes_the_pair_ambiguous() {
    let _cfg = crate::common::config_sandbox();
    let a = project_with("ret", &playbook("ret", ""));
    let out = make_global(a.path(), "ret", opts(Some(Variant::Retire))).unwrap();
    assert_eq!(out["applied"], true, "{out}");
    let entries = entries_for(a.path(), "ret");
    let project = entries
        .iter()
        .find(|e| e["ref"]["origin"]["kind"] == "project")
        .unwrap();
    assert_eq!(project["shadowed"], true);
    assert_eq!(project["lifecycle"], "retired");

    let b = project_with("kept", &playbook("kept", ""));
    let out = make_global(b.path(), "kept", opts(Some(Variant::Keep))).unwrap();
    assert_eq!(out["applied"], true, "{out}");
    let entries = entries_for(b.path(), "kept");
    assert_eq!(entries.len(), 2);
    assert!(
        entries.iter().all(|e| e["ambiguous"] == true),
        "{entries:?}"
    );
}

#[test]
fn move_profiles_copies_profiles_but_never_ones_with_project_skills() {
    let _cfg = crate::common::config_sandbox();
    let nodes = "  - { id: a, type: agent_task, prompt: \"x\", profile: local }\n";
    let root = project_with("rel", &playbook("rel", nodes));
    seed_profile(root.path(), "local", "");
    let with_profiles = MakeGlobalOpts {
        variant: Some(Variant::Keep),
        move_profiles: true,
        dry_run: false,
    };
    let out = make_global(root.path(), "rel", with_profiles).unwrap();
    assert_eq!(out["applied"], true, "{out}");
    let global_profiles = apb_core::profile_store::global_dir().unwrap();
    assert!(global_profiles.join("local/profile.yaml").is_file());

    let other = project_with(
        "sk",
        &playbook("sk", nodes.replace("local", "skilled").as_str()),
    );
    seed_profile(
        other.path(),
        "skilled",
        "skills:\n  - { name: mine, scope: project }\n",
    );
    let out = make_global(other.path(), "sk", with_profiles).unwrap();
    assert_eq!(out["applied"], false);
    assert_eq!(codes(&out, "blockers"), vec!["profile_has_project_skills"]);
    assert!(!global_profiles.join("skilled").exists());
}

/// keep: the project copy keeps running here, so it keeps its remembered
/// input; the global playbook gets a copy of it.
#[test]
fn keep_copies_the_memory_instead_of_moving_it() {
    let _cfg = crate::common::config_sandbox();
    let yaml = playbook("kept", "");
    let root = project_with("kept", &yaml);
    let params = apb_core::schema::Playbook::from_yaml(&yaml).unwrap().params;
    let project_key = MemoryKey::new(Scope::Project, "kept").unwrap();
    remember(
        root.path(),
        &project_key,
        &params,
        "1.0.0",
        RememberInput {
            params: Some([("who".to_string(), "team".to_string())].into()),
            instruction: None,
        },
    )
    .unwrap();

    let out = make_global(root.path(), "kept", opts(Some(Variant::Keep))).unwrap();
    assert_eq!(out["applied"], true, "{out}");
    assert!(codes(&out, "actions").contains(&"copy_memory".to_string()));
    let global_key = MemoryKey::new(Scope::Global, "kept").unwrap();
    for key in [project_key, global_key] {
        let r = recall(root.path(), &key, &params, None).unwrap();
        assert_eq!(r.compatible().collect::<Vec<_>>(), vec![("who", "team")]);
    }
}

/// A step that fails after the copy undoes it: the global copy and the
/// memory move are rolled back, so the same call can run again.
#[test]
fn a_failed_step_undoes_the_earlier_ones() {
    let _cfg = crate::common::config_sandbox();
    let nodes = "  - { id: a, type: agent_task, prompt: \"x\", profile: local }\n";
    let root = project_with("rel", &playbook("rel", nodes));
    seed_profile(root.path(), "local", "");
    // The global profile store cannot be created: the profile copy fails
    // after the playbook was copied.
    let global_profiles = apb_core::profile_store::global_dir().unwrap();
    std::fs::write(&global_profiles, "not a directory").unwrap();
    let with_profiles = MakeGlobalOpts {
        variant: Some(Variant::Trash),
        move_profiles: true,
        dry_run: false,
    };
    assert!(make_global(root.path(), "rel", with_profiles).is_err());
    assert!(!global_parent().join("playbooks/rel").exists());
    assert!(root.path().join(".apb/playbooks/rel").is_dir());

    std::fs::remove_file(&global_profiles).unwrap();
    let out = make_global(root.path(), "rel", with_profiles).unwrap();
    assert_eq!(
        out["applied"], true,
        "a retry runs from the same state: {out}"
    );
}

/// An unsafe id never reaches a path, not even the lock's.
#[test]
fn an_unsafe_id_is_only_planned() {
    let _cfg = crate::common::config_sandbox();
    let root = project_with("rel", &playbook("rel", ""));
    let out = make_global(root.path(), "../../Cargo", opts(Some(Variant::Keep))).unwrap();
    assert_eq!(out["applied"], false);
    assert_eq!(codes(&out, "blockers"), vec!["not_found"]);
}

/// A global profile with the same files but a skill of other content
/// changes what the global playbook runs: the plan warns.
#[test]
fn a_global_profile_whose_skill_differs_is_a_shadow_change() {
    let _cfg = crate::common::config_sandbox();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    let nodes = "  - { id: a, type: agent_task, prompt: \"x\", profile: local }\n";
    let root = project_with("rel", &playbook("rel", nodes));
    let skills = "skills:\n  - { name: tool }\n";
    seed_profile(root.path(), "local", skills);
    let global_profile = apb_core::profile_store::global_dir().unwrap().join("local");
    std::fs::create_dir_all(&global_profile).unwrap();
    for f in ["profile.yaml", "SOUL.md"] {
        std::fs::copy(
            root.path().join(".apb/profiles/local").join(f),
            global_profile.join(f),
        )
        .unwrap();
    }
    for (dir, body) in [
        (root.path().join(".agents/skills/tool"), "project"),
        (home.path().join(".agents/skills/tool"), "global"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: tool\n---\n{body}\n"),
        )
        .unwrap();
    }

    let dry = || {
        make_global(
            root.path(),
            "rel",
            MakeGlobalOpts {
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let out = dry();
    // The same skill content on both sides is no change.
    std::fs::copy(
        root.path().join(".agents/skills/tool/SKILL.md"),
        home.path().join(".agents/skills/tool/SKILL.md"),
    )
    .unwrap();
    let same = dry();
    assert!(
        codes(&out, "warnings").contains(&"profile_shadow_change".to_string()),
        "{out}"
    );
    assert!(
        !codes(&same, "warnings").contains(&"profile_shadow_change".to_string()),
        "{same}"
    );
}

/// The profile copy behind make-global checks its own arguments: an unsafe
/// name or a copy onto the same scope is refused before any path is used.
#[test]
fn a_profile_copy_refuses_an_unsafe_name_and_the_same_scope() {
    use apb_core::profile::ProfileScope;
    use apb_mcp::profile_tools::{CopyProfileError, copy_profile_locked};
    let _cfg = crate::common::config_sandbox();
    let root = project_with("rel", &playbook("rel", ""));
    seed_profile(root.path(), "local", "");
    assert!(matches!(
        copy_profile_locked(
            root.path(),
            "../x",
            ProfileScope::Project,
            ProfileScope::Global
        ),
        Err(CopyProfileError::Invalid(_))
    ));
    assert!(matches!(
        copy_profile_locked(
            root.path(),
            "local",
            ProfileScope::Project,
            ProfileScope::Project
        ),
        Err(CopyProfileError::Invalid(_))
    ));
}

/// Points `HOME` at a directory for one test and restores it on every
/// path, a panic included. The config sandbox holds the env lock.
struct HomeGuard(Option<std::ffi::OsString>);

impl HomeGuard {
    fn set(dir: &Path) -> Self {
        let prev = std::env::var_os("HOME");
        // SAFETY: the caller holds the config sandbox, and with it the env lock.
        unsafe { std::env::set_var("HOME", dir) };
        HomeGuard(prev)
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            match self.0.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// A fate that fails after the memory moved puts the memory back as it
/// was: the old draft that seeded the move does not stay behind as a
/// remembered instruction of the project playbook.
#[test]
fn an_undone_move_restores_the_memory_as_it_was() {
    let _cfg = crate::common::config_sandbox();
    let yaml = playbook("rel", "");
    let root = project_with("rel", &yaml);
    let params = apb_core::schema::Playbook::from_yaml(&yaml).unwrap().params;
    let project_key = MemoryKey::new(Scope::Project, "rel").unwrap();
    remember(
        root.path(),
        &project_key,
        &params,
        "1.0.0",
        RememberInput {
            params: Some([("who".to_string(), "team".to_string())].into()),
            instruction: None,
        },
    )
    .unwrap();
    let meta = root.path().join(".apb/playbooks/rel/meta");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("instruction-draft.md"), "old draft").unwrap();
    // The trash cannot be created: the fate fails after the memory moved.
    std::fs::write(root.path().join(".apb/trash"), "not a directory").unwrap();

    assert!(make_global(root.path(), "rel", opts(Some(Variant::Trash))).is_err());
    assert!(!global_parent().join("playbooks/rel").exists());
    let back = recall(root.path(), &project_key, &params, None).unwrap();
    assert_eq!(back.compatible().collect::<Vec<_>>(), vec![("who", "team")]);
    assert_eq!(back.instruction, None);
    let global_key = MemoryKey::new(Scope::Global, "rel").unwrap();
    assert!(
        recall(root.path(), &global_key, &params, None)
            .unwrap()
            .values
            .is_empty()
    );
}
