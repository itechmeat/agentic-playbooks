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

/// Sets or clears process env vars for one test, restoring them on drop;
/// hold the config sandbox first, it serializes env changes.
struct Env(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Env {
    fn set(vars: &[(&'static str, Option<&std::ffi::OsStr>)]) -> Self {
        let prev = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            // SAFETY: env mutation is serialized by the sandbox's lock.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        Env(prev)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            // SAFETY: as in `set`.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

fn stub(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A machine with two agent CLIs as stubs, its own HOME and PATH, on which
/// setup planned and applied everything; hold the config sandbox first.
struct SetupMachine {
    _env: Env,
    _home: tempfile::TempDir,
    _bin: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl SetupMachine {
    fn applied() -> Self {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        stub(
            bin.path(),
            "claude",
            "case \"$1\" in\n  --version) echo '2.1.283 (Claude Code)' ;;\n  mcp) printf '%s' '{}' > \"$HOME/.claude.json\" ;;\nesac\n",
        );
        stub(
            bin.path(),
            "codex",
            "case \"$1\" in\n  --version) echo 'codex-cli 0.157.0' ;;\nesac\n",
        );
        let path = std::ffi::OsString::from(format!("{}:/usr/bin:/bin", bin.path().display()));
        let env = Env::set(&[
            ("HOME", Some(home.path().as_os_str())),
            ("PATH", Some(&path)),
            ("XDG_CONFIG_HOME", None),
            ("CODEX_HOME", None),
            ("CLAUDE_CONFIG_DIR", None),
            ("APB_RUN_ID", None),
            ("APB_HOST_TASK", None),
            (
                "APB_TEST_GITHUB_TOKEN",
                Some(std::ffi::OsStr::new("not-a-real-token-value")),
            ),
        ]);
        let project = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(project.path()).unwrap();
        let m = SetupMachine {
            _env: env,
            _home: home,
            _bin: bin,
            project,
        };
        let out = m.apply();
        assert_eq!(
            out.status,
            apb_mcp::setup::ApplyStatus::Applied,
            "{:?}",
            out.failed
        );
        m
    }

    fn root(&self) -> &Path {
        self.project.path()
    }

    /// Plans and applies, as `apb setup apply` does.
    fn apply(&self) -> apb_mcp::setup::ApplyOutcome {
        let none = apb_mcp::setup::ReplaceModified::None;
        let plan = apb_mcp::setup::plan(self.root(), None, &none).unwrap();
        assert!(plan.ready, "{:?}", plan.refusal);
        apb_mcp::setup::apply(self.root(), None, &none, &plan.digest)
    }
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

/// N skills of one role profile updated by one apply: the approval is
/// judged once every skill holds the shipped files, so the profile keeps
/// it (judged per skill, the first update would see the second skill still
/// old and the second would find the approval already gone).
#[test]
fn one_apply_that_updates_several_skills_of_a_role_keeps_its_approval() {
    let _cfg = crate::common::config_sandbox();
    let m = SetupMachine::applied();
    let origin = bundle_origin(m.root(), "apb-main").expect("approved at setup");

    // An earlier binary's copies of two of the main profile's skills,
    // untouched since, with the profile approved as it stood then.
    let skills = ["apb-delivery", "apb-usage"];
    for name in skills {
        assert!(role_skills("apb-main").contains(&name));
        apb_core::bundled::remove_skill(name).unwrap();
        testing::install_skill(&older_skill(name), false).unwrap();
    }
    let r = apb_core::profile::QualifiedProfileRef {
        name: "apb-main".into(),
        scope: apb_core::profile::ProfileScope::Global,
    };
    let (_, _, bundle) = apb_core::profile_store::compute_bundle(
        m.root(),
        apb_core::profile_store::PlaybookOrigin::Global,
        &r,
    )
    .unwrap();
    TrustStore::load()
        .approve_kind(&bundle, "apb-main", Kind::ProfileBundle, origin)
        .unwrap();

    let out = m.apply();
    assert_eq!(
        out.status,
        apb_mcp::setup::ApplyStatus::Applied,
        "{:?}",
        out.failed
    );
    let upgraded: Vec<&str> = out
        .done
        .iter()
        .filter(|d| d.kind == apb_mcp::setup::ActionKind::UpgradeSkill)
        .map(|d| d.target.as_str())
        .collect();
    assert_eq!(upgraded, skills, "both in one apply");
    assert!(out.notes.is_empty(), "{:?}", out.notes);
    assert_eq!(
        bundle_origin(m.root(), "apb-main"),
        Some(origin),
        "the main profile keeps its approval, with the origin it had"
    );
}

/// E1 end to end: setup on a machine with two agents installs the shipped
/// defaults and the role profiles bound to those agents (the reviewer to
/// the other one), and then every shipped playbook passes
/// the run gate for a person's start (the dashboard's click answers its
/// `agent_launch: ask`) and its readiness report names no missing or
/// unapproved profile or skill.
#[test]
fn every_shipped_playbook_runs_on_what_setup_wrote() {
    let _cfg = crate::common::config_sandbox();
    let m = SetupMachine::applied();
    let project = &m.project;

    // The role profiles run on the detected agents, the reviewer on the
    // other one of the two.
    let agent = |name: &str| {
        let r = apb_core::profile::QualifiedProfileRef {
            name: name.into(),
            scope: apb_core::profile::ProfileScope::Global,
        };
        apb_core::profile_store::resolve_profile(
            project.path(),
            apb_core::profile_store::PlaybookOrigin::Global,
            &r,
        )
        .unwrap()
        .doc
        .executor
        .agent
    };
    let (main, reviewer) = (agent("apb-main"), agent("apb-reviewer"));
    for a in [&main, &reviewer, &agent("apb-researcher")] {
        assert!(["claude", "codex"].contains(&a.as_str()), "{a}");
    }
    assert_ne!(main, reviewer, "the reviewer is the other agent");

    // The issue triage reads and labels issues through the official
    // connector, which a person installs and gives an account (setup does
    // not): installed here, as AGENT-SETUP says.
    apb_core::connector::install::install_official("github", false).unwrap();
    let accounts = apb_core::connector::config::global_config_path("github").unwrap();
    std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
    std::fs::write(
        &accounts,
        "accounts:\n  - name: default\n    default: true\n    api_base: https://api.github.com\n    token: \"{{env.APB_TEST_GITHUB_TOKEN}}\"\n",
    )
    .unwrap();
    // The person approves that account, as the approve surface records it.
    let account = apb_core::connector::config::load_merged(project.path(), "github")
        .unwrap()
        .remove(0);
    TrustStore::load()
        .approve_kind(
            &apb_core::connector::config::account_digest(&account),
            &apb_core::trust::account_trust_id("github", "default"),
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .unwrap();
    let shipped = apb_core::bundled::playbooks();
    assert!(!shipped.is_empty());
    for p in &shipped {
        if let Err(v) = gate(project.path(), &p.id) {
            panic!("{} is refused at the gate: {v}", p.id);
        }
        let report = apb_mcp::advisory_tools::playbook_adopt_report(
            project.path(),
            Some(&p.id),
            Some("global"),
        )
        .unwrap();
        let findings = report["playbooks"][0]["findings"].as_array().unwrap();
        for f in findings {
            let code = f["code"].as_str().unwrap_or("");
            assert!(
                ![
                    "profile_missing",
                    "skill_missing",
                    "profile_unresolved",
                    "untrusted",
                    "playbook_unloadable"
                ]
                .contains(&code),
                "{}: {f}",
                p.id
            );
        }
    }
}

#[test]
fn setup_plan_is_read_inside_a_run_step_and_apply_is_refused() {
    let _cfg = crate::common::config_sandbox();
    let _env = Env::set(&[("APB_RUN_ID", Some(std::ffi::OsStr::new("r-step")))]);
    let project = tempfile::tempdir().unwrap();
    let plan = apb_mcp::tools::setup_plan(project.path(), false).expect("read in a step");
    let digest = plan["digest"].as_str().unwrap();
    let none = apb_mcp::setup::ReplaceModified::None;
    let out = apb_mcp::setup::apply(project.path(), None, &none, digest);
    assert_eq!(out.status, apb_mcp::setup::ApplyStatus::Refused);
    assert!(
        out.refusal.unwrap().message.contains("r-step"),
        "the refusal names the run"
    );
    let removed = apb_mcp::setup::remove(project.path()).unwrap_err();
    assert!(removed.contains("r-step"), "{removed}");
}
