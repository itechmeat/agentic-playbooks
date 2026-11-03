//! The guarantees of shipped content, on fixtures of the tests' own (never
//! on the embedded set, which the content tests cover): only the exact
//! shipped digest is trusted, a changed copy is never upgraded on its own,
//! a fork is never `Bundled`, reconcile is idempotent, a skill apb did not
//! install is never overwritten, and the reserved prefix is refused.

use std::collections::BTreeMap;
use std::path::Path;

use super::playbooks::{install_playbook_in, reconcile_playbooks_in, remove_shipped};
use super::skills::{install_skill_in, reconcile_skills_in, remove_skill_in};
use super::*;
use crate::detect::AgentInfo;
use crate::profile_store::PlaybookOrigin;
use crate::trust::{OriginKind, TrustStore};
use crate::versioning::current::{self, MoveCurrent, MovedBy, Surface};
use crate::versioning::{read_current_pointer, written_digest};

const ID: &str = "apb-fixture";

fn yaml(version: &str, title: &str) -> String {
    format!(
        "schema: 2\nid: {ID}\nname: {title}\nversion: {version}\nnodes:\n  - {{ id: s, type: start }}\nedges: []\n"
    )
}

fn shipped(version: &str, title: &str) -> BundledPlaybook {
    let mut files = BTreeMap::new();
    files.insert(
        "playbook.yaml".to_string(),
        yaml(version, title).into_bytes(),
    );
    files.insert(
        "scripts/check.sh".to_string(),
        b"#!/bin/sh\necho ok\n".to_vec(),
    );
    files.insert("evals/suite.yaml".to_string(), b"cases: []\n".to_vec());
    BundledPlaybook::from_files(ID, &files).expect("fixture parses")
}

fn dir(parent: &Path) -> std::path::PathBuf {
    parent.join("playbooks").join(ID)
}

fn current_digest(parent: &Path) -> String {
    let d = dir(parent);
    written_digest(&d.join(read_current_pointer(&d).unwrap())).unwrap()
}

#[test]
fn install_writes_a_version_trusts_exactly_its_digest_and_records_the_move() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();

    let done = install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();

    assert!(!done.no_op);
    assert_eq!(done.version, "1.0.0");
    assert_eq!(done.digest, current_digest(store.path()));
    let trust = TrustStore::load();
    assert_eq!(trust.origin(&done.digest), Some(OriginKind::Bundled));
    let entries: Vec<_> = trust.entries().into_iter().filter(|e| e.id == ID).collect();
    assert_eq!(entries.len(), 1, "only the shipped digest is trusted");
    assert!(dir(store.path()).join("1.0.0/scripts/check.sh").is_file());
    assert!(dir(store.path()).join("evals/suite.yaml").is_file());
    let history = current::history(&dir(store.path()), 5).unwrap();
    let last = &history.entries[0];
    assert_eq!(last.by, Some(MovedBy::Created));
    assert_eq!(last.reason.as_deref(), Some("bundled install"));

    let again = install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    assert!(again.no_op);
    assert_eq!(again.digest, done.digest);
}

#[test]
fn a_pristine_copy_is_upgraded_once_and_the_upgrade_can_be_rolled_back() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();

    let next = [shipped("1.1.0", "Two")];
    let first = reconcile_playbooks_in(store.path(), &next);
    assert_eq!(
        first,
        vec![Reconciled::Updated {
            name: ID.into(),
            from_version: "1.0.0".into(),
            to_version: "1.1.0".into(),
        }]
    );
    assert_eq!(
        TrustStore::load().origin(&current_digest(store.path())),
        Some(OriginKind::Bundled)
    );
    let history = current::history(&dir(store.path()), 1).unwrap();
    assert_eq!(
        history.entries[0].reason.as_deref(),
        Some("bundled upgrade")
    );
    assert!(
        reconcile_playbooks_in(store.path(), &next).is_empty(),
        "reconcile is idempotent"
    );

    // A person rolls back: reconcile never undoes that on its own.
    current::move_current(
        &dir(store.path()),
        &MoveCurrent::new(
            "1.0.0",
            MovedBy::Person {
                surface: Surface::Cli,
            },
        ),
    )
    .unwrap();
    let after = reconcile_playbooks_in(store.path(), &next);
    assert!(
        matches!(&after[..], [Reconciled::UpdateAvailable { installed_version, .. }] if installed_version == "1.0.0"),
        "{after:?}"
    );
    assert_eq!(
        read_current_pointer(&dir(store.path())).as_deref(),
        Some("1.0.0")
    );
}

#[test]
fn an_edited_copy_drops_trust_and_is_never_upgraded_on_its_own() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    let done = install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    // A hand edit of the installed version.
    let file = dir(store.path()).join("1.0.0/playbook.yaml");
    let edited = std::fs::read_to_string(&file)
        .unwrap()
        .replace("One", "Mine");
    std::fs::write(&file, edited).unwrap();

    let digest = current_digest(store.path());
    assert_ne!(digest, done.digest);
    assert!(
        !TrustStore::load().is_approved(&digest),
        "an edit drops trust"
    );

    let next = [shipped("1.1.0", "Two")];
    let out = reconcile_playbooks_in(store.path(), &next);
    assert_eq!(
        out,
        vec![Reconciled::UpdateAvailable {
            name: ID.into(),
            installed_version: "1.0.0".into(),
            embedded_version: "1.1.0".into(),
        }]
    );
    assert_eq!(
        read_current_pointer(&dir(store.path())).as_deref(),
        Some("1.0.0")
    );
    let refused = install_playbook_in(store.path(), &next[0], false).unwrap_err();
    assert_eq!(refused.code(), "modified");

    // Forced, the shipped content becomes a new version; the edit stays.
    let forced = install_playbook_in(store.path(), &next[0], true).unwrap();
    assert_eq!(forced.version, "1.1.0");
    assert!(file.is_file());
    let history = current::history(&dir(store.path()), 1).unwrap();
    assert_eq!(
        history.entries[0].reason.as_deref(),
        Some("bundled replace")
    );
    assert!(
        reconcile_playbooks_in(store.path(), &next).is_empty(),
        "the replaced copy is the shipped one again"
    );
}

#[test]
fn a_person_moving_back_to_the_newest_shipped_version_makes_the_copy_pristine_again() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    reconcile_playbooks_in(store.path(), &[shipped("1.1.0", "Two")]);
    let person = |v: &str| {
        current::move_current(
            &dir(store.path()),
            &MoveCurrent::new(
                v,
                MovedBy::Person {
                    surface: Surface::Cli,
                },
            ),
        )
        .unwrap();
    };
    person("1.0.0");
    person("1.1.0");

    let out = reconcile_playbooks_in(store.path(), &[shipped("1.2.0", "Three")]);
    assert!(
        matches!(&out[..], [Reconciled::Updated { to_version, .. }] if to_version == "1.2.0"),
        "{out:?}"
    );
}

/// A candidate on trial or a frozen playbook is the person's: never
/// pristine, so an upgrade only reports an update.
#[test]
fn a_candidate_or_a_frozen_marker_keeps_a_copy_out_of_pristine() {
    let _cfg = crate::test_config();
    for hold in ["candidate", "frozen"] {
        let store = tempfile::tempdir().unwrap();
        install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
        reconcile_playbooks_in(store.path(), &[shipped("1.1.0", "Two")]);
        let d = dir(store.path());
        match hold {
            "candidate" => {
                std::fs::write(d.join(crate::candidate_pointer::CANDIDATE_FILE), "1.0.0").unwrap()
            }
            _ => std::fs::write(d.join(crate::registry::FROZEN_MARKER), "").unwrap(),
        }
        let next = shipped("1.2.0", "Three");
        assert!(
            matches!(
                playbook_state_in(store.path(), &next),
                InstallState::Modified { .. }
            ),
            "{hold}"
        );
        let out = reconcile_playbooks_in(store.path(), std::slice::from_ref(&next));
        assert!(
            matches!(&out[..], [Reconciled::UpdateAvailable { .. }]),
            "{hold}: {out:?}"
        );
        assert_eq!(read_current_pointer(&d).as_deref(), Some("1.1.0"), "{hold}");
    }
}

/// A crash between writing the trusted version and making it current
/// leaves no `current`: that is apb's own interrupted install, finished by
/// the next install and taken back by remove, never someone else's.
#[test]
fn an_install_interrupted_before_current_is_finished_or_removed() {
    let _cfg = crate::test_config();
    let parent = playbook_store().unwrap();
    let one = shipped("1.0.0", "One");
    install_playbook_in(&parent, &one, false).unwrap();
    std::fs::remove_file(dir(&parent).join("current")).unwrap();
    assert_eq!(playbook_state_in(&parent, &one), InstallState::Absent);

    let done = install_playbook_in(&parent, &one, false).unwrap();
    assert_eq!(done.version, "1.0.0", "the trusted version is reused");
    assert_eq!(
        read_current_pointer(&dir(&parent)).as_deref(),
        Some("1.0.0")
    );

    std::fs::remove_file(dir(&parent).join("current")).unwrap();
    assert!(remove_shipped(&one, 3).unwrap().is_some());
    assert!(!dir(&parent).exists());

    // Versions of someone else without `current` stay someone else's.
    let store = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir(store.path()).join("1.0.0")).unwrap();
    std::fs::write(
        dir(store.path()).join("1.0.0/playbook.yaml"),
        yaml("1.0.0", "Theirs"),
    )
    .unwrap();
    assert_eq!(
        install_playbook_in(store.path(), &one, false)
            .unwrap_err()
            .code(),
        "foreign"
    );
}

#[test]
fn an_equivalent_but_different_text_is_not_the_shipped_digest() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    let done = install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    // The same definition, with a comment: equivalent, but other bytes.
    let file = dir(store.path()).join("1.0.0/playbook.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("# mine\n{text}")).unwrap();
    let digest = current_digest(store.path());
    assert_ne!(digest, done.digest);

    let refused = install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap_err();
    assert_eq!(refused.code(), "modified");
    assert_eq!(TrustStore::load().origin(&digest), None, "never `Bundled`");
}

#[test]
fn an_older_binary_never_moves_a_newer_default_back() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    reconcile_playbooks_in(store.path(), &[shipped("1.1.0", "Two")]);

    let older = shipped("1.0.0", "Changed again");
    let out = reconcile_playbooks_in(store.path(), std::slice::from_ref(&older));
    assert_eq!(
        out,
        vec![Reconciled::NewerInstalled {
            name: ID.into(),
            installed_version: "1.1.0".into(),
            embedded_version: "1.0.0".into(),
        }]
    );
    assert_eq!(
        read_current_pointer(&dir(store.path())).as_deref(),
        Some("1.1.0")
    );
    for force in [false, true] {
        let err = install_playbook_in(store.path(), &older, force).unwrap_err();
        assert_eq!(err.code(), "newer_installed");
    }
    let notes = report::describe("default playbook", |_| String::new(), out);
    assert!(
        notes[0]
            .text
            .contains("newer defaults installed, leaving them")
    );
}

#[test]
fn remove_keeps_a_playbook_that_holds_a_version_of_someone_else() {
    let _cfg = crate::test_config();
    let parent = playbook_store().unwrap();
    let one = shipped("1.0.0", "One");
    install_playbook_in(&parent, &one, false).unwrap();
    // The user's own version after the shipped one, not made current.
    crate::versioning::write_version_in(
        &parent,
        PlaybookOrigin::Global,
        ID,
        &crate::versioning::ShippedVersion {
            yaml: &yaml("1.1.0", "Mine"),
            version: "1.1.0",
            scripts: &[],
            created_by: "user",
        },
        crate::launch::floor::SaveChannel::Person,
    )
    .unwrap();
    assert!(matches!(
        playbook_state_in(&parent, &one),
        InstallState::UpToDate { .. }
    ));

    let kept = remove_shipped(&one, 1).unwrap_err();
    assert_eq!(kept.code(), "own_versions");
    assert!(dir(&parent).join("1.1.0").is_dir());

    std::fs::remove_dir_all(dir(&parent).join("1.1.0")).unwrap();
    assert!(
        remove_shipped(&one, 2).unwrap().is_some(),
        "all shipped: removed"
    );
}

#[test]
fn undoing_a_replace_moves_current_back_to_the_persons_version() {
    let _cfg = crate::test_config();
    let parent = playbook_store().unwrap();
    install_playbook_in(&parent, &shipped("1.0.0", "One"), false).unwrap();
    crate::versioning::write_version_in(
        &parent,
        PlaybookOrigin::Global,
        ID,
        &crate::versioning::ShippedVersion {
            yaml: &yaml("1.0.1", "Mine"),
            version: "1.0.1",
            scripts: &[],
            created_by: "user",
        },
        crate::launch::floor::SaveChannel::Person,
    )
    .unwrap();
    current::move_current(
        &dir(&parent),
        &MoveCurrent::new(
            "1.0.1",
            MovedBy::Person {
                surface: Surface::Cli,
            },
        ),
    )
    .unwrap();
    let next = shipped("1.1.0", "Two");
    let replaced = install_playbook_in(&parent, &next, true).unwrap();
    assert_eq!(
        read_current_pointer(&dir(&parent)).as_deref(),
        Some(replaced.version.as_str())
    );

    let from = super::playbooks::restore_replaced_in(&parent, &next, "1.0.1").unwrap();
    assert_eq!(from, replaced.version);
    assert_eq!(
        read_current_pointer(&dir(&parent)).as_deref(),
        Some("1.0.1")
    );
    assert!(
        matches!(
            playbook_state_in(&parent, &next),
            InstallState::Modified { .. }
        ),
        "the person's copy again, never upgraded on its own"
    );
    assert_eq!(
        super::playbooks::restore_replaced_in(&parent, &next, "1.0.1")
            .unwrap_err()
            .code(),
        "modified",
        "only over the shipped content nobody changed since"
    );
}

#[test]
fn undoing_a_skill_replace_puts_the_persons_copy_back_as_theirs() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    install_skill_in(&dirs, &skill("1.0.0", "one"), false).unwrap();
    std::fs::write(dirs.skills.join("apb-skill/NOTES.md"), "mine").unwrap();
    let next = skill("1.1.0", "two");
    let backup = install_skill_in(&dirs, &next, true)
        .unwrap()
        .backup
        .unwrap();

    let elsewhere = root.path().join(".apb-skill.apb-bundled-backup-1");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(
        super::skills::restore_replaced_in(&dirs, &next, &elsewhere).is_err(),
        "only a backup in the skills directory"
    );
    super::skills::restore_replaced_in(&dirs, &next, &backup).unwrap();
    assert_eq!(
        std::fs::read_to_string(dirs.skills.join("apb-skill/NOTES.md")).unwrap(),
        "mine"
    );
    assert!(!backup.exists());
    assert_eq!(
        install_skill_in(&dirs, &next, false).unwrap_err().code(),
        "foreign",
        "the person's own copy, never touched by apb again"
    );
}

/// [`playbook_state`] of a fixture in the store `parent`.
fn playbook_state_in(parent: &Path, p: &BundledPlaybook) -> InstallState {
    super::playbooks::state_in(parent, p)
}

#[test]
fn reconcile_skips_while_another_process_holds_the_lock_and_runs_after() {
    let _cfg = crate::test_config();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    // An older copy of a real shipped playbook, as an earlier binary left it.
    let real = super::playbook("apb-code-review").expect("shipped");
    let mut files = BTreeMap::new();
    files.insert(
        "playbook.yaml".to_string(),
        real.yaml
            .replacen(&format!("version: {}", real.version), "version: 0.9.0", 1)
            .replacen("name: ", "name: Older ", 1)
            .into_bytes(),
    );
    let older = BundledPlaybook::from_files(&real.id, &files).unwrap();
    assert_eq!(older.version, "0.9.0");
    let parent = playbook_store().unwrap();
    install_playbook_in(&parent, &older, false).unwrap();

    let held = hold().unwrap();
    assert!(try_hold().unwrap().is_none(), "busy");
    assert!(reconcile_defaults().is_empty(), "skipped while busy");
    assert_eq!(
        read_current_pointer(&parent.join("playbooks").join(&real.id)).as_deref(),
        Some("0.9.0")
    );
    drop(held);

    let notes = reconcile_defaults();
    assert!(
        notes
            .iter()
            .any(|n| !n.failed && n.text.contains("apb-code-review")),
        "{notes:?}"
    );
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A reconcile installs nothing: on a machine where no default was ever
/// installed it writes only its own lock.
#[test]
fn reconcile_on_a_machine_without_defaults_installs_nothing() {
    let _cfg = crate::test_config();
    let home = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(home.path());
    let config = std::path::PathBuf::from(std::env::var_os("APB_CONFIG_DIR").unwrap());

    assert!(reconcile_defaults().is_empty());

    assert!(files_under(home.path()).is_empty(), "nothing under HOME");
    let written = files_under(&config);
    assert!(
        written.iter().all(|p| p.ends_with(LOCK_FILE)),
        "{written:?}"
    );
}

/// Points `HOME` at a test folder (the global skills directory lives under
/// it) and restores it on drop. Hold the env lock (`test_config`) first.
struct HomeGuard(Option<std::ffi::OsString>);

impl HomeGuard {
    fn set(path: &Path) -> Self {
        let prev = std::env::var_os("HOME");
        // SAFETY: env mutation is serialized by the env test lock the
        // caller holds.
        unsafe { std::env::set_var("HOME", path) };
        Self(prev)
    }
}

impl Drop for HomeGuard {
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

#[test]
fn a_later_version_by_someone_else_keeps_the_copy_from_upgrading() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    // Someone else's version after the shipped one, not made current.
    crate::versioning::write_version_in(
        store.path(),
        PlaybookOrigin::Global,
        ID,
        &crate::versioning::ShippedVersion {
            yaml: &yaml("1.5.0", "Mine"),
            version: "1.5.0",
            scripts: &[],
            created_by: "user",
        },
        crate::launch::floor::SaveChannel::Person,
    )
    .unwrap();

    let out = reconcile_playbooks_in(store.path(), &[shipped("2.0.0", "Two")]);
    assert!(
        matches!(&out[..], [Reconciled::UpdateAvailable { .. }]),
        "{out:?}"
    );
}

#[test]
fn a_playbook_apb_did_not_write_is_foreign() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    let vdir = dir(store.path()).join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join("playbook.yaml"), yaml("1.0.0", "Mine")).unwrap();
    std::fs::write(dir(store.path()).join("current"), "1.0.0").unwrap();

    let err = install_playbook_in(store.path(), &shipped("1.0.0", "One"), true).unwrap_err();
    assert_eq!(err.code(), "foreign");
    assert!(reconcile_playbooks_in(store.path(), &[shipped("1.0.0", "One")]).is_empty());
}

#[test]
fn the_reserved_prefix_is_refused_on_every_create_path_and_a_fork_is_not_bundled() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    let project = tempfile::tempdir().unwrap();
    crate::registry::init_project(project.path()).unwrap();

    // A save, a capture and a scope copy under the reserved id.
    let saved = crate::versioning::save_definition(
        project.path(),
        "apb-mine",
        &yaml("1.0.0", "x"),
        None,
        true,
    );
    assert!(
        matches!(&saved, Err(crate::versioning::VersioningError::Validation(i)) if i[0].code == RESERVED_ID_CODE),
        "{saved:?}"
    );
    let drafted = crate::versioning::create_draft_in(
        store.path(),
        "apb-mine",
        &yaml("1.0.0", "x"),
        PlaybookOrigin::Global,
    );
    assert!(drafted.is_err());
    // An import of a bundle that names a reserved id.
    let imported = crate::bundle::import_bundle(
        project.path(),
        &crate::bundle::PlaybookBundle {
            origin: None,
            apb_bundle: 1,
            id: "apb-mine".to_string(),
            version: "1.0.0".to_string(),
            playbook: yaml("1.0.0", "x").replace(ID, "apb-mine"),
            layout: None,
            scripts: Vec::new(),
        },
        true,
        None,
    );
    assert!(
        matches!(
            &imported,
            Err(crate::bundle::BundleError::Versioning(
                crate::versioning::VersioningError::Validation(i)
            )) if i[0].code == RESERVED_ID_CODE
        ),
        "{imported:?}"
    );
    assert!(!project.path().join(".apb/playbooks/apb-mine").exists());
    let copied = crate::scope_move::copy_playbook_to(
        &dir(store.path()),
        project.path().join(".apb").as_path(),
        ID,
        "global",
        &crate::scope_move::CopyFloors::project_to_global(
            "ws",
            ID,
            crate::launch::floor::SaveChannel::Agent,
        ),
    );
    assert!(matches!(
        copied,
        Err(crate::scope_move::ScopeMoveError::ReservedId(_))
    ));

    // A fork under the user's own id: allowed, and never `Bundled`.
    let fork_yaml = std::fs::read_to_string(dir(store.path()).join("1.0.0/playbook.yaml"))
        .unwrap()
        .replace(ID, "my-fork");
    crate::versioning::create_draft_in(store.path(), "my-fork", &fork_yaml, PlaybookOrigin::Global)
        .unwrap();
    let fork_dir = store.path().join("playbooks/my-fork");
    let fork_digest = written_digest(&fork_dir.join("1.0.0")).unwrap();
    assert!(
        !TrustStore::load().is_approved(&fork_digest),
        "a fork starts untrusted"
    );
    crate::versioning::save_definition_by(
        project.path(),
        "my-copy",
        &fork_yaml,
        None,
        true,
        crate::launch::floor::SaveChannel::Agent,
    )
    .unwrap();
    let copy_digest = written_digest(&project.path().join(".apb/playbooks/my-copy/1.0.0")).unwrap();
    assert_ne!(
        TrustStore::load().origin(&copy_digest),
        Some(OriginKind::Bundled),
        "a saved copy never carries the shipped origin"
    );
}

fn skill(version: &str, body: &str) -> BundledSkill {
    let mut files = BTreeMap::new();
    files.insert(
        "SKILL.md".to_string(),
        format!("---\nname: apb-skill\nmetadata:\n  version: \"{version}\"\n---\n{body}\n")
            .into_bytes(),
    );
    files.insert("references/a.md".to_string(), b"ref\n".to_vec());
    BundledSkill::from_files("apb-skill", files).unwrap()
}

fn skill_dirs(root: &Path) -> SkillDirs {
    SkillDirs {
        skills: root.join("agents-skills"),
        claude: Some(root.join("claude-skills")),
    }
}

#[test]
fn a_skill_installs_with_its_sidecar_and_bridge_and_upgrades_while_pristine() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());

    let done = install_skill_in(&dirs, &skill("1.0.0", "one"), false).unwrap();
    assert_eq!(done.version, "1.0.0");
    assert!(dirs.skills.join("apb-skill/SKILL.md").is_file());
    assert!(dirs.skills.join(".apb-skill.apb-bundled.json").is_file());
    let link = dirs.claude.as_ref().unwrap().join("apb-skill");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        install_skill_in(&dirs, &skill("1.0.0", "one"), false)
            .unwrap()
            .no_op
    );

    let next = [skill("1.1.0", "two")];
    let out = reconcile_skills_in(&dirs, &next);
    assert_eq!(
        out,
        vec![Reconciled::Updated {
            name: "apb-skill".into(),
            from_version: "1.0.0".into(),
            to_version: "1.1.0".into(),
        }]
    );
    assert!(reconcile_skills_in(&dirs, &next).is_empty(), "idempotent");
}

#[test]
fn a_changed_skill_is_reported_and_replaced_only_when_forced() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    install_skill_in(&dirs, &skill("1.0.0", "one"), false).unwrap();
    std::fs::write(dirs.skills.join("apb-skill/NOTES.md"), "mine").unwrap();

    let next = skill("1.1.0", "two");
    let out = reconcile_skills_in(&dirs, std::slice::from_ref(&next));
    assert!(
        matches!(&out[..], [Reconciled::UpdateAvailable { .. }]),
        "{out:?}"
    );
    assert!(dirs.skills.join("apb-skill/NOTES.md").is_file());
    assert_eq!(
        install_skill_in(&dirs, &next, false).unwrap_err().code(),
        "modified"
    );
    let replaced = install_skill_in(&dirs, &next, true).unwrap();
    assert!(!dirs.skills.join("apb-skill/NOTES.md").exists());
    let backup = replaced.backup.expect("the changed copy is kept");
    assert!(backup.join("NOTES.md").is_file(), "{}", backup.display());
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".apb-skill.apb-bundled-backup-")
    );
    assert!(reconcile_skills_in(&dirs, &[next]).is_empty());
}

/// A replace whose write of the shipped copy fails puts the changed copy
/// back: the skill is never left missing.
#[test]
fn a_failed_replace_puts_the_changed_copy_back() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    install_skill_in(&dirs, &skill("1.0.0", "one"), false).unwrap();
    std::fs::write(dirs.skills.join("apb-skill/NOTES.md"), "mine").unwrap();
    // Another install of the same skill holds its lock: the write fails.
    std::fs::write(
        dirs.skills
            .join(crate::tree_install::lock_name("apb-skill")),
        "held",
    )
    .unwrap();

    let err = install_skill_in(&dirs, &skill("1.1.0", "two"), true).unwrap_err();
    assert_eq!(err.code(), "io", "{err}");
    assert_eq!(
        std::fs::read_to_string(dirs.skills.join("apb-skill/NOTES.md")).unwrap(),
        "mine",
        "the changed copy is back in place"
    );
    let aside: Vec<String> = std::fs::read_dir(&dirs.skills)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("apb-bundled-backup"))
        .collect();
    assert!(aside.is_empty(), "{aside:?}");
}

#[test]
fn an_older_binary_leaves_a_newer_skill_alone() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    install_skill_in(&dirs, &skill("1.1.0", "two"), false).unwrap();

    let older = skill("1.0.0", "one");
    let out = reconcile_skills_in(&dirs, std::slice::from_ref(&older));
    assert!(
        matches!(&out[..], [Reconciled::NewerInstalled { installed_version, .. }] if installed_version == "1.1.0"),
        "{out:?}"
    );
    for force in [false, true] {
        let err = install_skill_in(&dirs, &older, force).unwrap_err();
        assert_eq!(err.code(), "newer_installed");
    }
    assert!(
        std::fs::read_to_string(dirs.skills.join("apb-skill/SKILL.md"))
            .unwrap()
            .contains("two")
    );
}

#[test]
fn a_copy_left_without_its_sidecar_is_recognised_and_repaired() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    let s = skill("1.0.0", "one");
    install_skill_in(&dirs, &s, false).unwrap();
    // A crash between the tree and the sidecar.
    let sidecar = dirs.skills.join(".apb-skill.apb-bundled.json");
    std::fs::remove_file(&sidecar).unwrap();

    assert!(reconcile_skills_in(&dirs, std::slice::from_ref(&s)).is_empty());
    assert!(sidecar.is_file(), "the sidecar is written again");
    let out = reconcile_skills_in(&dirs, &[skill("1.1.0", "two")]);
    assert!(matches!(&out[..], [Reconciled::Updated { .. }]), "{out:?}");
}

#[test]
fn remove_takes_only_an_unchanged_skill_and_its_own_bridge() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    let s = skill("1.0.0", "one");
    install_skill_in(&dirs, &s, false).unwrap();
    std::fs::write(dirs.skills.join("apb-skill/NOTES.md"), "mine").unwrap();
    assert_eq!(remove_skill_in(&dirs, &s).unwrap_err().code(), "modified");
    assert!(dirs.skills.join("apb-skill/NOTES.md").is_file());

    std::fs::remove_file(dirs.skills.join("apb-skill/NOTES.md")).unwrap();
    assert!(remove_skill_in(&dirs, &s).unwrap());
    assert!(!dirs.skills.join("apb-skill").exists());
    assert!(!dirs.skills.join(".apb-skill.apb-bundled.json").exists());
    let link = dirs.claude.as_ref().unwrap().join("apb-skill");
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert!(
        !remove_skill_in(&dirs, &s).unwrap(),
        "absent: nothing to do"
    );
}

#[test]
fn a_skill_apb_did_not_install_is_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let dirs = skill_dirs(root.path());
    let theirs = dirs.skills.join("apb-skill");
    std::fs::create_dir_all(&theirs).unwrap();
    std::fs::write(theirs.join("SKILL.md"), "theirs").unwrap();

    for force in [false, true] {
        let err = install_skill_in(&dirs, &skill("1.0.0", "one"), force).unwrap_err();
        assert_eq!(err.code(), "foreign");
    }
    assert!(reconcile_skills_in(&dirs, &[skill("1.0.0", "one")]).is_empty());
    assert_eq!(
        std::fs::read_to_string(theirs.join("SKILL.md")).unwrap(),
        "theirs"
    );
    assert!(!dirs.skills.join(".apb-skill.apb-bundled.json").exists());
}

fn agent(id: &str, installed: bool) -> AgentInfo {
    AgentInfo {
        agent: id.to_string(),
        installed,
        canonical_path: None,
        version: None,
        category: crate::detect::AgentCategory::Vendor,
        models: None,
        providers: None,
        auth: None,
        notes: Vec::new(),
    }
}

/// Two built-in vendor agents of different vendors, from the agent
/// registry, so the test names none.
fn two_vendor_agents() -> (String, String) {
    let table = crate::models_table::builtin();
    let ids: Vec<String> = crate::agents::all()
        .iter()
        .map(|a| a.descriptor().id.to_string())
        .filter(|id| {
            crate::models_table::agent_vendor(id).is_some()
                && default_model(&agent(id, true), &table).is_some()
        })
        .collect();
    let first = ids[0].clone();
    let other = ids
        .iter()
        .find(|id| {
            crate::models_table::agent_vendor(id) != crate::models_table::agent_vendor(&first)
        })
        .expect("two vendors in the registry")
        .clone();
    (first, other)
}

#[test]
fn the_reviewer_is_another_vendor_when_one_is_usable() {
    let table = crate::models_table::builtin();
    let (a, b) = two_vendor_agents();
    let plan = plan_role_profiles(&[agent(&a, true), agent(&b, true)], &table).unwrap();
    let names: Vec<&str> = plan.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec![ROLE_MAIN, ROLE_REVIEWER, ROLE_RESEARCHER]);
    assert_eq!(plan.independence, Independence::SeparateAgent);
    assert_eq!(plan.profiles[0].agent, a);
    assert_eq!(plan.profiles[1].agent, b);
    assert!(plan.profiles[1].fallbacks.iter().all(|f| f.agent != a));
    // Models come from the table only.
    assert_eq!(
        Some(plan.profiles[0].model.clone()),
        default_model(&agent(&a, true), &table)
    );
}

#[test]
fn one_usable_agent_reviews_as_the_same_agent_and_says_so() {
    let table = crate::models_table::builtin();
    let (a, b) = two_vendor_agents();
    let plan = plan_role_profiles(&[agent(&a, true), agent(&b, false)], &table).unwrap();
    assert_eq!(plan.independence, Independence::SameAgent);
    assert_eq!(plan.profiles[1].agent, a);
    assert_eq!(plan.skipped[0].agent, b);
    assert_eq!(
        plan_role_profiles(&[agent(&a, false)], &table).unwrap_err(),
        PlanError::NoUsableAgent
    );
}

#[test]
fn subscriptions_order_the_agents() {
    let mut table = crate::models_table::builtin();
    let (a, b) = two_vendor_agents();
    table.subscriptions = vec![crate::models_table::Subscription {
        agent: b.clone(),
        plan: None,
        coverage: Default::default(),
    }];
    let plan = plan_role_profiles(&[agent(&a, true), agent(&b, true)], &table).unwrap();
    assert_eq!(plan.profiles[0].agent, b);
    assert_eq!(plan.profiles[1].agent, a);
}

#[test]
fn frontmatter_version_reads_metadata_then_top_level() {
    assert_eq!(
        super::frontmatter_version("---\nmetadata:\n  version: \"2.1.0\"\n---\nx").as_deref(),
        Some("2.1.0")
    );
    assert_eq!(
        super::frontmatter_version("---\nversion: 3.0.0\n---\nx").as_deref(),
        Some("3.0.0")
    );
    assert_eq!(super::frontmatter_version("no frontmatter"), None);
}

/// Whatever the embedded set holds parses and keeps to the reserved
/// prefix (an entry that does not is dropped from the listing, so this
/// compares the listing with the folders).
#[test]
fn every_embedded_entry_is_listed() {
    let folders = |section: &str| -> Vec<String> { super::embedded(section).into_keys().collect() };
    let listed: Vec<String> = playbooks().into_iter().map(|p| p.id).collect();
    assert_eq!(
        listed,
        folders("playbooks"),
        "a shipped playbook does not parse"
    );
    let listed: Vec<String> = skills().into_iter().map(|s| s.name).collect();
    assert_eq!(listed, folders("skills"), "a shipped skill has no SKILL.md");
}

/// Every file under `dir` with its bytes, to show a read wrote nothing.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

use crate::versioning::current::fault::{Step, fail_after};

/// A person's rollback a crash interrupted after `current` moved (its
/// history line not yet written) is settled before any decision: reconcile
/// keeps it, and the read-only state already reads it so without writing.
#[test]
fn an_interrupted_person_rollback_survives_reconcile() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    let d = dir(store.path());
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    let next = shipped("1.1.0", "Two");
    reconcile_playbooks_in(store.path(), std::slice::from_ref(&next));
    assert_eq!(read_current_pointer(&d).as_deref(), Some("1.1.0"));

    fail_after(Some(Step::Current));
    let moved = current::move_current(
        &d,
        &MoveCurrent::new(
            "1.0.0",
            MovedBy::Person {
                surface: Surface::Cli,
            },
        ),
    );
    fail_after(None);
    assert!(moved.unwrap().audit_pending);
    assert!(current::intent_pending(&d));

    let before = snapshot(&d);
    assert_eq!(
        playbook_state_in(store.path(), &next),
        InstallState::Modified {
            installed: "1.0.0".into()
        }
    );
    assert_eq!(snapshot(&d), before, "reading the state writes nothing");

    let out = reconcile_playbooks_in(store.path(), std::slice::from_ref(&next));
    assert!(
        matches!(&out[..], [Reconciled::UpdateAvailable { installed_version, .. }] if installed_version == "1.0.0"),
        "{out:?}"
    );
    assert_eq!(read_current_pointer(&d).as_deref(), Some("1.0.0"));
    assert!(!current::intent_pending(&d), "recovered");
}

/// A crash between writing the shipped version and moving `current` is one
/// interrupted move: recovery records it as aborted, the read-only state
/// still offers the upgrade without writing, and the next run reuses the
/// written version instead of writing another.
#[test]
fn a_crash_between_the_version_write_and_the_move_recovers() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    let d = dir(store.path());
    install_playbook_in(store.path(), &shipped("1.0.0", "One"), false).unwrap();
    let next = shipped("1.1.0", "Two");

    fail_after(Some(Step::Target));
    let out = reconcile_playbooks_in(store.path(), std::slice::from_ref(&next));
    fail_after(None);
    assert!(matches!(&out[..], [Reconciled::Failed { .. }]), "{out:?}");
    assert_eq!(read_current_pointer(&d).as_deref(), Some("1.0.0"));
    assert!(d.join("1.1.0/playbook.yaml").is_file());
    assert!(current::intent_pending(&d));

    let before = snapshot(&d);
    assert_eq!(
        playbook_state_in(store.path(), &next),
        InstallState::Pristine {
            installed: "1.0.0".into()
        }
    );
    assert_eq!(snapshot(&d), before, "reading the state writes nothing");

    let out = reconcile_playbooks_in(store.path(), std::slice::from_ref(&next));
    assert_eq!(
        out,
        vec![Reconciled::Updated {
            name: ID.into(),
            from_version: "1.0.0".into(),
            to_version: "1.1.0".into(),
        }]
    );
    assert_eq!(
        crate::registry::list_versions(&d).unwrap(),
        vec!["1.0.0".to_string(), "1.1.0".to_string()],
        "the written version is reused"
    );
    let history = current::history(&d, 10).unwrap();
    assert!(!history.incomplete && !history.pending, "{history:?}");
    assert_eq!(history.entries[1].kind, current::EntryKind::Aborted);

    // A first install the same crash interrupted has no `current` at all:
    // its own trusted version makes it apb's, and installing finishes it.
    let fresh = tempfile::tempdir().unwrap();
    let one = shipped("1.0.0", "One");
    fail_after(Some(Step::Target));
    assert!(install_playbook_in(fresh.path(), &one, false).is_err());
    fail_after(None);
    assert_eq!(read_current_pointer(&dir(fresh.path())), None);
    assert_eq!(playbook_state_in(fresh.path(), &one), InstallState::Absent);
    let done = install_playbook_in(fresh.path(), &one, false).unwrap();
    assert_eq!(done.version, "1.0.0");
    assert_eq!(
        read_current_pointer(&dir(fresh.path())).as_deref(),
        Some("1.0.0")
    );

    // A crash before the version was written leaves no playbook behind.
    let empty = tempfile::tempdir().unwrap();
    fail_after(Some(Step::Intent));
    assert!(install_playbook_in(empty.path(), &one, false).is_err());
    fail_after(None);
    assert!(!dir(empty.path()).exists());
}

/// A newer binary's first install a crash interrupted before `current`
/// existed holds newer shipped content: an older binary refuses to install
/// over it on every path and leaves it for the newer one to finish.
#[test]
fn an_older_binary_refuses_a_newer_interrupted_install() {
    let _cfg = crate::test_config();
    let store = tempfile::tempdir().unwrap();
    let d = dir(store.path());
    let newer = shipped("2.0.0", "Two");
    fail_after(Some(Step::Target));
    assert!(install_playbook_in(store.path(), &newer, false).is_err());
    fail_after(None);
    assert_eq!(read_current_pointer(&d), None);

    let older = shipped("1.0.0", "One");
    for force in [false, true] {
        let err = install_playbook_in(store.path(), &older, force).unwrap_err();
        assert_eq!(err.code(), "newer_installed", "force {force}");
    }
    assert!(reconcile_playbooks_in(store.path(), std::slice::from_ref(&older)).is_empty());
    assert_eq!(read_current_pointer(&d), None, "nothing moved back");
    assert_eq!(
        crate::registry::list_versions(&d).unwrap(),
        vec!["2.0.0".to_string()]
    );
    assert_eq!(
        playbook_state_in(store.path(), &older),
        InstallState::UpToDate {
            installed: "2.0.0".into()
        }
    );

    let done = install_playbook_in(store.path(), &newer, false).unwrap();
    assert_eq!(done.version, "2.0.0");
}

/// A family is the vendor of the model a profile runs, not of the agent:
/// an aggregator agent running the builder's family is not an independent
/// reviewer, and the fallbacks rank another known family first, an unknown
/// family next and the profile's own family last (subscription order kept
/// within a rank).
#[test]
fn the_reviewer_and_the_fallbacks_rank_families_by_the_model() {
    let mut table = crate::models_table::builtin();
    let (a, b) = two_vendor_agents();
    let vendor_a = crate::models_table::agent_vendor(&a).unwrap();
    // An aggregator: no vendor of its own and no closed list.
    let m = crate::agents::all()
        .iter()
        .map(|x| x.descriptor().id.to_string())
        .find(|id| {
            crate::models_table::agent_vendor(id).is_none()
                && crate::models_table::static_models_for_agent(id, &table).is_none()
        })
        .expect("an aggregator agent in the registry");
    let same_family = table
        .models
        .iter()
        .find(|r| r.vendor == vendor_a)
        .expect("a table row of the builder's vendor")
        .id
        .clone();
    let mut aggregator = agent(&m, true);
    aggregator.models = Some(crate::detect::ModelsInventory {
        items: vec![same_family.clone()],
        authority: crate::detect::Authority::Full,
    });
    table.subscriptions = [&a, &m, &b]
        .into_iter()
        .map(|id| crate::models_table::Subscription {
            agent: id.clone(),
            plan: None,
            coverage: Default::default(),
        })
        .collect();
    let plan = plan_role_profiles(&[agent(&a, true), aggregator, agent(&b, true)], &table).unwrap();
    let (main, reviewer) = (&plan.profiles[0], &plan.profiles[1]);
    assert_eq!(main.agent, a);
    assert_eq!(
        reviewer.agent, b,
        "the aggregator runs the builder's family"
    );
    let order: Vec<&str> = main.fallbacks.iter().map(|f| f.agent.as_str()).collect();
    assert_eq!(order, [b.as_str(), m.as_str()], "another family first");
    assert_eq!(plan.profiles[2].fallbacks, main.fallbacks);
}
