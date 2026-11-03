//! Shipped playbooks through the run gate, and the role profiles setup
//! writes: only the exact shipped digest carries the shipped trust; an edit
//! of the shipped copy in place is refused until approved, and a copy an
//! agent saves under its own id is approved as locally authored, like any
//! playbook an agent writes, never with the `Bundled` origin and never
//! upgraded with the defaults. Setup-made profiles carry normal profile
//! trust, never `Bundled`, and keep it across an update of a shipped skill
//! while they stay what setup wrote.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::bundled::testing::{self, playbook_from_files};
use apb_core::bundled::{BundledPlaybook, plan_role_profiles, role_skills};
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::trust::{Kind, OriginKind, TrustStore};
use apb_engine::gate::launch::{LaunchContext, LaunchKind};

const ID: &str = "apb-fixture";

fn fixture() -> BundledPlaybook {
    let yaml = format!(
        "schema: 2\nid: {ID}\nname: Fixture\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: s, to: done }}\n"
    );
    let mut files = BTreeMap::new();
    files.insert("playbook.yaml".to_string(), yaml.into_bytes());
    playbook_from_files(ID, &files).expect("fixture parses")
}

/// Whether the gate refuses `id` for its trust.
fn untrusted(root: &Path, id: &str) -> bool {
    gate(root, id).is_err_and(|v| {
        v["policy"]
            .as_str()
            .is_some_and(|p| p.contains("untrusted"))
    })
}

fn gate(root: &Path, id: &str) -> Result<(), serde_json::Value> {
    let wref = PlaybookRef {
        origin: Origin::Global,
        id: id.into(),
        version: None,
    };
    apb_mcp::policy::check_run(
        root,
        &wref,
        false,
        false,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .map(|_| ())
}

#[test]
fn only_the_shipped_digest_passes_the_gate() {
    let _cfg = crate::common::config_sandbox();
    let project = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(project.path()).unwrap();
    testing::install_playbook(&fixture(), false).unwrap();
    assert!(
        gate(project.path(), ID).is_ok(),
        "the shipped digest is trusted"
    );

    // A copy an agent saves under its own id: approved as locally
    // authored, never with the shipped origin, and no default to upgrade.
    let store = apb_core::store::global_playbooks_parent()
        .unwrap()
        .join("playbooks");
    let yaml = std::fs::read_to_string(store.join(ID).join("1.0.0/playbook.yaml"))
        .unwrap()
        .replace(ID, "my-copy");
    apb_core::versioning::save_definition_by(
        project.path(),
        "my-copy",
        &yaml,
        None,
        true,
        apb_core::launch::floor::SaveChannel::Agent,
    )
    .unwrap();
    let origins: Vec<OriginKind> = TrustStore::load()
        .entries()
        .into_iter()
        .filter(|e| e.kind == Kind::Playbook && e.id.contains("my-copy"))
        .map(|e| e.origin_kind)
        .collect();
    assert!(!origins.is_empty(), "the saved copy is approved");
    assert!(
        origins.iter().all(|o| *o != OriginKind::Bundled),
        "{origins:?}"
    );
    let project_ref = PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: "my-copy".into(),
        version: None,
    };
    assert!(
        apb_mcp::policy::check_run(
            project.path(),
            &project_ref,
            false,
            false,
            &LaunchContext::dashboard(LaunchKind::Start),
        )
        .is_ok(),
        "approved as locally authored"
    );

    // An edit of the shipped playbook in place.
    let file = store.join(ID).join("1.0.0/playbook.yaml");
    let edited = std::fs::read_to_string(&file)
        .unwrap()
        .replace("name: Fixture", "name: Edited");
    std::fs::write(&file, edited).unwrap();
    assert!(untrusted(project.path(), ID), "an edit drops trust");
}

/// Every built-in agent with a default model, as if detected installed.
fn all_agents() -> Vec<apb_core::detect::AgentInfo> {
    apb_core::agents::all()
        .iter()
        .map(|a| apb_core::detect::AgentInfo {
            agent: a.descriptor().id.to_string(),
            installed: true,
            canonical_path: None,
            version: None,
            category: a.descriptor().category,
            models: None,
            providers: None,
            auth: None,
            notes: Vec::new(),
        })
        .collect()
}

#[test]
fn setup_made_role_profiles_are_never_bundled() {
    let _cfg = crate::common::config_sandbox();
    let project = tempfile::tempdir().unwrap();
    let table = apb_core::models_table::builtin();
    let plan = plan_role_profiles(&all_agents(), &table).unwrap();

    let written = apb_mcp::profile_tools::write_role_profiles(project.path(), &plan).unwrap();
    assert_eq!(written.len(), 3);
    // Applying the plan again updates instead of failing.
    apb_mcp::profile_tools::write_role_profiles(project.path(), &plan).unwrap();

    let entries: Vec<_> = TrustStore::load()
        .entries()
        .into_iter()
        .filter(|e| e.kind == Kind::ProfileBundle && e.id.starts_with("apb-"))
        .collect();
    assert!(!entries.is_empty());
    assert!(
        entries.iter().all(|e| e.origin_kind != OriginKind::Bundled),
        "{entries:?}"
    );
}

/// Points `HOME` (the global skills directory) at a test folder; hold the
/// config sandbox first, it serializes env changes.
struct Home(Option<std::ffi::OsString>);

impl Home {
    fn set(path: &Path) -> Self {
        let prev = std::env::var_os("HOME");
        // SAFETY: env mutation is serialized by the sandbox's lock.
        unsafe { std::env::set_var("HOME", path) };
        Home(prev)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            match &self.0 {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// The global role profile `name`: its approval of the live bundle.
fn bundle_origin(root: &Path, name: &str) -> Option<OriginKind> {
    let r = apb_core::profile::QualifiedProfileRef {
        name: name.into(),
        scope: apb_core::profile::ProfileScope::Global,
    };
    let (_, _, bundle) = apb_core::profile_store::compute_bundle(
        root,
        apb_core::profile_store::PlaybookOrigin::Global,
        &r,
    )
    .unwrap();
    TrustStore::load().origin(&bundle)
}

#[test]
fn role_profiles_stay_approved_across_a_shipped_skill_update_while_unchanged() {
    let _cfg = crate::common::config_sandbox();
    let home = tempfile::tempdir().unwrap();
    let _home = Home::set(home.path());
    let project = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(project.path()).unwrap();

    // An earlier binary installed an older apb-review; the rest is current.
    testing::install_skill(&older_skill("apb-review"), false).unwrap();
    for s in apb_core::bundled::skills() {
        if s.name != "apb-review" {
            apb_core::bundled::install_skill(&s.name, false).unwrap();
        }
    }
    apb_core::bundled::install_playbook("apb-code-review", false).unwrap();

    // Setup writes the role profiles with their skills, approved.
    let mut plan = plan_role_profiles(&all_agents(), &apb_core::models_table::builtin()).unwrap();
    for p in &mut plan.profiles {
        p.skills = role_skills(&p.name).iter().map(|s| s.to_string()).collect();
    }
    apb_mcp::profile_tools::write_role_profiles(project.path(), &plan).unwrap();
    assert!(
        !untrusted(project.path(), "apb-code-review"),
        "approved at setup"
    );
    // A person edits the main profile's role prompt by hand: no longer
    // what setup wrote.
    let main_soul = apb_core::config::config_dir()
        .unwrap()
        .join("profiles/apb-main/SOUL.md");
    std::fs::write(&main_soul, "My own role prompt.\n").unwrap();
    assert_eq!(bundle_origin(project.path(), "apb-main"), None);

    let notes = apb_core::bundled::reconcile_defaults();
    assert!(
        notes
            .iter()
            .any(|n| n.text.contains("skill `apb-review` updated")),
        "{notes:?}"
    );
    assert!(notes.iter().all(|n| !n.failed), "{notes:?}");

    assert!(
        !untrusted(project.path(), "apb-code-review"),
        "the reviewer keeps its approval across the shipped skill update"
    );
    assert_eq!(
        bundle_origin(project.path(), "apb-reviewer"),
        Some(OriginKind::AgentGenerated),
        "the origin it had, never `Bundled`"
    );
    assert_eq!(
        bundle_origin(project.path(), "apb-main"),
        None,
        "a profile changed since setup keeps the normal trust drop"
    );
}

/// The shipped skill `name` as an earlier binary shipped it: version 0.9.0
/// and other wording.
fn older_skill(name: &str) -> apb_core::bundled::BundledSkill {
    let real = apb_core::bundled::skill(name).expect("shipped");
    let mut files = real.files.clone();
    let md = String::from_utf8(files["SKILL.md"].clone()).unwrap();
    let older_md = md.replacen(&format!("\"{}\"", real.version), "\"0.9.0\"", 1);
    assert_ne!(older_md, md, "{name}: the fixture names an older version");
    files.insert(
        "SKILL.md".into(),
        format!("{older_md}\nAn older wording.\n").into_bytes(),
    );
    testing::skill_from_files(name, files).unwrap()
}
