//! The launch floor and the `launch:` settings: an agent channel never
//! loosens a playbook's launch policy, whatever path it takes (a save, a
//! recreate, a restore, a promotion), a person lowers it only in the editor
//! or at a terminal, the floor is durable before stricter content is
//! runnable, and the settings reader is strict.

use std::path::Path;

use apb_core::launch::LaunchPolicy;
use apb_core::launch::floor::{self, FloorError, FloorKey, SaveChannel};
use apb_core::policy_file::{Policies, parse};
use apb_core::registry::init_project;
use apb_core::versioning::{
    VersioningError, create_draft_in_by, delete_playbook, restore_from_trash,
    restore_from_trash_by, save_definition, save_definition_by,
};

use crate::common::config_sandbox;

fn pb(id: &str, decl: Option<&str>) -> String {
    let decl = decl
        .map(|d| format!("agent_launch: {d}\n"))
        .unwrap_or_default();
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\n{decl}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: done }}\n"
    )
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    dir
}

fn key(root: &Path, id: &str) -> FloorKey {
    FloorKey::read_project(root, id).expect("a save named the workspace")
}

fn floor_of(root: &Path, id: &str) -> Option<LaunchPolicy> {
    floor::floor_of(
        FloorKey::read_project(root, id).as_ref(),
        Some(&root.join(".apb/playbooks").join(id)),
    )
    .unwrap()
}

fn loosened(e: VersioningError) -> bool {
    matches!(e, VersioningError::Launch(FloorError::Loosened { .. }))
}

#[test]
fn an_agent_save_that_loosens_is_refused_and_the_floor_stays() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    save_definition(root, "p", &pb("p", Some("deny")), None, true).unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
    // Dropping the key is a loosening too.
    let e =
        save_definition_by(root, "p", &pb("p", None), None, true, SaveChannel::Agent).unwrap_err();
    assert!(loosened(e));
    let e = save_definition_by(
        root,
        "p",
        &pb("p", Some("ask")),
        None,
        true,
        SaveChannel::Agent,
    )
    .unwrap_err();
    let msg = e.to_string();
    assert!(msg.starts_with("launch_policy_loosened"), "{msg}");
    assert!(msg.contains("apb launch floor p --lower-to ask"), "{msg}");
    // An equal or stricter declaration passes.
    save_definition_by(
        root,
        "p",
        &pb("p", Some("deny")).replace("start }", "start, description: x }"),
        None,
        true,
        SaveChannel::Agent,
    )
    .unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
}

#[test]
fn a_person_save_raises_and_never_lowers_but_the_editor_lowers_once_confirmed() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    save_definition(root, "p", &pb("p", Some("deny")), None, true).unwrap();
    // A person's CLI save of a looser version is kept, the floor holds.
    save_definition(root, "p", &pb("p", Some("ask")), None, true).unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
    // The editor with keys refuses a lowering nobody confirmed, a version
    // that drops the key included.
    for decl in [Some("allow"), None] {
        let e = save_definition_by(
            root,
            "p",
            &pb("p", decl),
            None,
            true,
            SaveChannel::Editor { lower_to: None },
        )
        .unwrap_err();
        assert!(
            matches!(
                e,
                VersioningError::Launch(FloorError::LoweringUnconfirmed { .. })
            ),
            "{e}"
        );
        assert!(e.to_string().contains("--lower-to allow"), "{e}");
    }
    // A confirmation of another level does not count.
    assert!(
        save_definition_by(
            root,
            "p",
            &pb("p", Some("allow")),
            None,
            true,
            SaveChannel::Editor {
                lower_to: Some(LaunchPolicy::Ask)
            },
        )
        .is_err()
    );
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
    // The confirmed lowering sets the floor to what the version declares.
    save_definition_by(
        root,
        "p",
        &pb("p", Some("allow")),
        None,
        true,
        SaveChannel::Editor {
            lower_to: Some(LaunchPolicy::Allow),
        },
    )
    .unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Allow));
    // From there an agent may save `allow` again.
    save_definition_by(root, "p", &pb("p", None), None, true, SaveChannel::Agent).unwrap();
    // The editor raises without any confirmation.
    save_definition_by(
        root,
        "p",
        &pb("p", Some("ask")).replace("start }", "start, description: y }"),
        None,
        true,
        SaveChannel::Editor { lower_to: None },
    )
    .unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Ask));
}

#[test]
fn a_keyless_editor_never_lowers_and_points_to_the_terminal() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    save_definition(root, "p", &pb("p", Some("deny")), None, true).unwrap();
    let e = save_definition_by(
        root,
        "p",
        &pb("p", Some("allow")),
        None,
        true,
        SaveChannel::KeylessEditor,
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            VersioningError::Launch(FloorError::LoweringNeedsTerminal { .. })
        ),
        "{e}"
    );
    let msg = e.to_string();
    assert!(
        msg.starts_with("launch_floor_lowering_needs_terminal"),
        "{msg}"
    );
    assert!(msg.contains("apb launch floor p --lower-to allow"), "{msg}");
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
    // Deleted and recreated in a keyless editor: the floor holds too.
    delete_playbook(root, "p", 7).unwrap();
    assert!(
        save_definition_by(
            root,
            "p",
            &pb("p", None),
            None,
            true,
            SaveChannel::KeylessEditor
        )
        .is_err()
    );
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
}

#[test]
fn delete_and_recreate_or_restore_keeps_the_floor() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    save_definition(root, "p", &pb("p", Some("deny")), None, true).unwrap();
    delete_playbook(root, "p", 1).unwrap();
    // An agent recreating the id with a looser policy is refused, both by a
    // create and by a capture.
    assert!(loosened(
        save_definition_by(root, "p", &pb("p", None), None, true, SaveChannel::Agent).unwrap_err()
    ));
    assert!(loosened(
        create_draft_in_by(
            &root.join(".apb"),
            "p",
            &pb("p", None),
            apb_core::profile_store::PlaybookOrigin::Project,
            SaveChannel::Agent,
        )
        .unwrap_err()
    ));
    // A person recreates it: the floor still holds `deny`.
    save_definition(root, "p", &pb("p", None), None, true).unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
    // Deleted again and restored, by a person and refused for an agent
    // whose restored `current` is looser.
    delete_playbook(root, "p", 2).unwrap();
    assert!(loosened(
        restore_from_trash_by(root, "p-2", SaveChannel::Agent).unwrap_err()
    ));
    restore_from_trash(root, "p-2").unwrap();
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Deny));
}

#[test]
fn a_store_without_a_record_reads_its_scan_and_a_delete_keeps_it() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    // A version written outside apb's save path (a checkout): no record.
    let v = root.join(".apb/playbooks/p/1.0.0");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(v.join("playbook.yaml"), pb("p", Some("ask"))).unwrap();
    std::fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
    apb_core::workspace::ensure_id(root).unwrap();
    assert!(floor::read().unwrap().is_empty(), "reads write nothing");
    assert_eq!(floor_of(root, "p"), Some(LaunchPolicy::Ask));
    delete_playbook(root, "p", 3).unwrap();
    assert_eq!(
        floor::read().unwrap().get(&key(root, "p").as_string()),
        Some(&LaunchPolicy::Ask),
        "the delete wrote the scanned floor before the store left"
    );
}

#[test]
fn a_copy_to_global_carries_the_floor_to_the_machine_wide_key() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    save_definition(root, "p", &pb("p", Some("deny")), None, true).unwrap();
    // A later looser version by a person: the store's current is `allow`.
    save_definition(root, "p", &pb("p", Some("allow")), None, true).unwrap();
    let floors = apb_core::scope_move::CopyFloors::project_to_global(
        &apb_core::workspace::read_id(root).unwrap(),
        "p",
        SaveChannel::Person,
    );
    let global = tempfile::tempdir().unwrap();
    apb_core::scope_move::copy_playbook_to(
        &root.join(".apb/playbooks/p"),
        global.path(),
        "p",
        "project",
        &floors,
    )
    .unwrap();
    assert_eq!(
        floor::read().unwrap().get("global/p"),
        Some(&LaunchPolicy::Deny)
    );
}

/// The floor is written before the rename that makes a version runnable: a
/// publication that fails after the floor write leaves the floor stricter
/// and nothing published (fail closed).
#[test]
fn a_failed_publication_after_the_floor_write_leaves_the_floor() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    let k = FloorKey::project_of(root, "p").unwrap();
    let store = root.join(".apb/playbooks/p");
    let authority = floor::SaveAuthority {
        key: k.clone(),
        channel: SaveChannel::Agent,
    };
    let out = floor::publish(&authority, &store, Some(LaunchPolicy::Deny), || {
        Err::<(), &str>("the rename failed")
    })
    .unwrap();
    assert_eq!(out, Err("the rename failed"));
    assert_eq!(
        floor::read().unwrap().get(&k.as_string()),
        Some(&LaunchPolicy::Deny)
    );
}

/// A duplicate id is refused before the floor is touched: an agent that
/// names an existing playbook cannot lock other agents out of it.
#[test]
fn a_duplicate_draft_leaves_the_floor_untouched() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    save_definition(root, "p", &pb("p", None), None, true).unwrap();
    let e = create_draft_in_by(
        &root.join(".apb"),
        "p",
        &pb("p", Some("deny")),
        apb_core::profile_store::PlaybookOrigin::Project,
        SaveChannel::Agent,
    )
    .unwrap_err();
    assert!(matches!(e, VersioningError::Conflict(_)), "{e}");
    assert!(floor::read().unwrap().is_empty(), "{:?}", floor::read());
    assert_eq!(floor_of(root, "p"), None);
}

/// A playbook deleted before its floor was ever recorded (a delete by an
/// older apb leaves only the trash entry) is not recreated looser: a key
/// without a record also scans the id's trash entries.
#[test]
fn a_recreate_scans_the_trash_of_a_key_without_a_record() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    let v = root.join(".apb/trash/deploy-1700000000000/1.0.0");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(v.join("playbook.yaml"), pb("deploy", Some("deny"))).unwrap();
    // Another id that shares the prefix is not this one's.
    let other = root.join(".apb/trash/deploy-x-1700000000001/1.0.0");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("playbook.yaml"), pb("deploy-x", Some("ask"))).unwrap();
    assert_eq!(
        floor::store_scan(&root.join(".apb/playbooks/deploy")),
        Some(LaunchPolicy::Deny)
    );
    assert!(loosened(
        save_definition_by(
            root,
            "deploy",
            &pb("deploy", None),
            None,
            true,
            SaveChannel::Agent
        )
        .unwrap_err()
    ));
    save_definition(root, "deploy", &pb("deploy", None), None, true).unwrap();
    assert_eq!(floor_of(root, "deploy"), Some(LaunchPolicy::Deny));
}

/// The workspace id of a floor key is the one registered at this root in
/// the projects registry: an id written into `.apb/workspace.local` by hand,
/// another project's or a malformed one, keys nothing.
#[test]
fn a_workspace_id_not_registered_here_keys_nothing() {
    let _c = config_sandbox();
    let mine = project();
    let other = project();
    save_definition(other.path(), "p", &pb("p", Some("deny")), None, true).unwrap();
    let theirs = apb_core::workspace::read_id(other.path()).unwrap();
    assert!(FloorKey::read_project(other.path(), "p").is_some());
    let file = mine.path().join(".apb/workspace.local");
    for forged in [theirs.as_str(), "global", "ws-NOTHEX"] {
        std::fs::write(&file, forged).unwrap();
        assert_eq!(FloorKey::read_project(mine.path(), "p"), None, "{forged}");
        assert_eq!(apb_core::projects::registered_id(mine.path()), None);
    }
    // A writer refuses an id another live project holds, and a malformed one.
    std::fs::write(&file, &theirs).unwrap();
    assert!(FloorKey::project_of(mine.path(), "p").is_err());
    std::fs::write(&file, "global").unwrap();
    assert!(FloorKey::project_of(mine.path(), "p").is_err());
}

#[test]
fn a_damaged_floor_file_refuses_saves() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    let path = floor::path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();
    let e =
        save_definition_by(root, "p", &pb("p", None), None, true, SaveChannel::Agent).unwrap_err();
    assert!(e.to_string().starts_with("launch_floor_invalid"), "{e}");
}

/// A person's lowering and an agent's save are serialized by the floor
/// lock: the save that waited while a confirmed editor lowering held the
/// lock is checked against the floor as it is once the lock frees, never
/// against a stale read. Had it run first, it would have been refused.
#[test]
fn a_lowering_and_a_save_are_serialized() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path().to_path_buf();
    save_definition(&root, "p", &pb("p", Some("deny")), None, true).unwrap();
    let k = key(&root, "p");
    let (inside_tx, inside_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lowering = {
        let store = root.join(".apb/playbooks/p");
        let authority = floor::SaveAuthority {
            key: k.clone(),
            channel: SaveChannel::Editor {
                lower_to: Some(LaunchPolicy::Allow),
            },
        };
        std::thread::spawn(move || {
            floor::publish(&authority, &store, Some(LaunchPolicy::Allow), || {
                inside_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok::<(), ()>(())
            })
        })
    };
    // The lowering holds the floor lock, its floor already written.
    inside_rx.recv().unwrap();
    let saver = {
        let root = root.clone();
        std::thread::spawn(move || {
            save_definition_by(
                &root,
                "p",
                &pb("p", Some("allow")),
                None,
                true,
                SaveChannel::Agent,
            )
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!saver.is_finished(), "the save waits on the floor lock");
    release_tx.send(()).unwrap();
    lowering.join().unwrap().unwrap().unwrap();
    saver
        .join()
        .unwrap()
        .expect("checked against the lowered floor");
}

#[test]
fn the_launch_section_is_strict() {
    let m = parse("launch:\n  agent: ask\n  playbooks:\n    \"ws1/deploy\": allow\n    \"global/triage\": deny\n").unwrap();
    assert_eq!(m.launch.agent, Some(LaunchPolicy::Ask));
    assert_eq!(
        m.launch.playbooks.get("global/triage"),
        Some(&LaunchPolicy::Deny)
    );
    let project = tempfile::tempdir().unwrap();
    let file = apb_core::policy_file::project_path(project.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "launch:\n  playbooks: { deploy: deny, \"global:triage\": ask }\n",
    )
    .unwrap();
    let p = Policies::load_with(Some(project.path()), None)
        .unwrap()
        .project;
    assert_eq!(p.launch.playbooks.get("deploy"), Some(&LaunchPolicy::Deny));
    // An unknown key, an unknown value, a key of the wrong form.
    let e = parse("launch:\n  agents: ask\n").unwrap_err();
    assert!(e.contains("agents"), "{e}");
    let e = parse("launch:\n  agent: maybe\n").unwrap_err();
    assert!(e.contains("maybe"), "{e}");
    let e = parse("launch:\n  playbooks: { deploy: deny }\n").unwrap_err();
    assert!(e.contains("`deploy` is not a playbook key"), "{e}");
    std::fs::write(&file, "launch:\n  playbooks: { \"ws1/deploy\": deny }\n").unwrap();
    let e = Policies::load_with(Some(project.path()), None).unwrap_err();
    assert!(e.message.contains("not a playbook key"), "{e}");
}

/// `apb import` is a person's publication: an imported `deny` version
/// raises the floor of the importing project's key, and a later import of a
/// looser version never lowers it.
#[test]
fn an_import_raises_the_floor_and_never_lowers_it() {
    let _c = config_sandbox();
    let src = project();
    let dst = project();
    save_definition(src.path(), "p", &pb("p", Some("deny")), None, true).unwrap();
    let bundle = apb_core::bundle::export_bundle(src.path(), "p", None).unwrap();
    apb_core::bundle::import_bundle(dst.path(), &bundle, true, None).unwrap();
    assert_eq!(floor_of(dst.path(), "p"), Some(LaunchPolicy::Deny));
    assert_eq!(
        floor::read()
            .unwrap()
            .get(&key(dst.path(), "p").as_string()),
        Some(&LaunchPolicy::Deny),
        "the import wrote the record"
    );
    save_definition(
        src.path(),
        "p",
        &pb("p", Some("allow")).replace("start }", "start, description: z }"),
        None,
        true,
    )
    .unwrap();
    let looser = apb_core::bundle::export_bundle(src.path(), "p", None).unwrap();
    apb_core::bundle::import_bundle(dst.path(), &looser, true, None).unwrap();
    assert_eq!(floor_of(dst.path(), "p"), Some(LaunchPolicy::Deny));
}

/// `apb migrate` writes a new schema-2 version: a publication that raises
/// the floor from what it declares before the version appears.
#[test]
fn a_migration_publishes_through_the_floor() {
    let _c = config_sandbox();
    let dir = project();
    let root = dir.path();
    let v = root.join(".apb/playbooks/m/1.0.0");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("playbook.yaml"),
        "schema: 1\nid: m\nname: M\nversion: 1.0.0\nagent_launch: deny\nexecutors:\n  main:\n    agent: claude\n    model: haiku\ndefaults:\n  executor: main\nnodes:\n  - { id: start, type: start }\n  - { id: t, type: agent_task, prompt: \"do\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: t }\n  - { from: t, to: done }\n",
    )
    .unwrap();
    std::fs::write(root.join(".apb/playbooks/m/current"), "1.0.0").unwrap();
    let plan = apb_core::schema_migrate::plan(root).unwrap();
    assert_eq!(plan.playbook_updates.len(), 1);
    apb_core::schema_migrate::apply(root, &plan, 1).unwrap();
    let new = &plan.playbook_updates[0].new_version;
    let migrated = std::fs::read_to_string(
        root.join(".apb/playbooks/m")
            .join(new)
            .join("playbook.yaml"),
    )
    .unwrap();
    assert_eq!(floor::declared_of(&migrated), Some(LaunchPolicy::Deny));
    assert_eq!(
        floor::read().unwrap().get(&key(root, "m").as_string()),
        Some(&LaunchPolicy::Deny),
        "the migration wrote the record before its version"
    );
}
