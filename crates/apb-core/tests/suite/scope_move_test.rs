//! Copying a playbook into another scope: the whole definition with the
//! same digest, without the legacy draft, never over an existing or claimed
//! id, never removing through a link, and nothing left behind when the copy
//! fails.

use std::fs;
use std::path::Path;

use apb_core::registry::Registry;
use apb_core::scope_move::{CopyFloors, ScopeMoveError, copy_playbook_to};

const YAML: &str =
    "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\nedges: []\n";

fn seed(parent: &Path) -> std::path::PathBuf {
    let dir = parent.join("playbooks/p");
    fs::create_dir_all(dir.join("1.0.0/scripts")).unwrap();
    fs::write(dir.join("1.0.0/playbook.yaml"), YAML).unwrap();
    fs::write(dir.join("1.0.0/scripts/run.sh"), "echo\n").unwrap();
    fs::write(dir.join("current"), "1.0.0").unwrap();
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::write(dir.join("meta/instruction-draft.md"), "draft").unwrap();
    fs::write(dir.join("meta/1.0.0.yaml"), "created_by: user\n").unwrap();
    dir
}

fn digest(parent: &Path) -> String {
    Registry::open_dir(parent)
        .unwrap()
        .load("p", None)
        .unwrap()
        .trust_digest()
        .unwrap()
}

#[test]
fn the_copy_is_whole_keeps_its_digest_and_leaves_the_draft_out() {
    let _cfg = crate::common::config_sandbox();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());

    let copied = copy_playbook_to(
        &dir,
        dst.path(),
        "p",
        "project",
        &CopyFloors::project_to_global("ws", "p", apb_core::launch::floor::SaveChannel::Person),
    )
    .unwrap();
    assert_eq!(copied, dst.path().join("playbooks/p"));
    assert_eq!(digest(dst.path()), digest(src.path()));
    assert!(copied.join("meta/1.0.0.yaml").is_file());
    assert!(!copied.join("meta/instruction-draft.md").exists());
    assert!(staging_is_empty(dst.path()));
    assert_eq!(
        Registry::open_dir(dst.path()).unwrap().playbook_ids(),
        vec!["p"]
    );
}

#[test]
fn an_existing_id_is_refused() {
    let _cfg = crate::common::config_sandbox();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    seed(dst.path());
    assert!(matches!(
        copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", apb_core::launch::floor::SaveChannel::Person)
        ),
        Err(ScopeMoveError::IdTaken(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_failed_copy_leaves_no_staging_behind() {
    let _cfg = crate::common::config_sandbox();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    // A FIFO cannot be copied: the copy fails part way.
    let fifo = dir.join("1.0.0/scripts/pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", apb_core::launch::floor::SaveChannel::Person)
        )
        .is_err()
    );
    assert!(!dst.path().join("playbooks/p").exists());
    assert!(staging_is_empty(dst.path()));
}

#[cfg(unix)]
#[test]
fn a_linked_meta_directory_never_lets_the_copy_remove_outside_it() {
    let _cfg = crate::common::config_sandbox();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    fs::write(outside.path().join("instruction-draft.md"), "keep me").unwrap();
    fs::remove_dir_all(dir.join("meta")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join("meta")).unwrap();

    assert!(
        copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", apb_core::launch::floor::SaveChannel::Person)
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("instruction-draft.md")).unwrap(),
        "keep me"
    );
    assert!(!dst.path().join("playbooks/p").exists());
}

#[test]
fn an_id_claimed_by_another_writer_is_taken() {
    let _cfg = crate::common::config_sandbox();
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dir = seed(src.path());
    // A new draft claims its id with an empty directory first.
    fs::create_dir_all(dst.path().join("playbooks/p")).unwrap();
    assert!(matches!(
        copy_playbook_to(
            &dir,
            dst.path(),
            "p",
            "project",
            &CopyFloors::project_to_global("ws", "p", apb_core::launch::floor::SaveChannel::Person)
        ),
        Err(ScopeMoveError::IdTaken(_))
    ));
}

/// Nothing of a copy is left in the staging area.
fn staging_is_empty(parent: &Path) -> bool {
    fs::read_dir(parent.join("playbooks-staging")).map_or(true, |mut d| d.next().is_none())
}

// --- the doctor's stale claims (design D4.6) ---

use std::time::{Duration, SystemTime};

use apb_core::fsutil::lock_dir;
use apb_core::scope_move::{ClaimLeft, StaleClaim, stale_claims};

/// The `now` a claim must be more than `STALE_CLAIM_SECS` before to be
/// named: a `now` 700 s ahead of the wall clock ages everything the test
/// just created, and the pointer lock's own 60 s staleness with it.
fn aged_now() -> SystemTime {
    SystemTime::now() + Duration::from_secs(700)
}

/// Sets the modification time of an existing file (write-opened) or
/// directory (read-only opened, the only way a directory opens).
fn set_mtime(path: &Path, at: SystemTime) {
    let f = if path.is_dir() {
        fs::File::open(path).unwrap()
    } else {
        fs::File::options().write(true).open(path).unwrap()
    };
    f.set_modified(at).unwrap();
}

/// The `left` of one claim named for `name`, failing when it is not named.
fn left_of(claims: &[StaleClaim], playbooks: &Path, name: &str) -> ClaimLeft {
    let path = playbooks.join(name);
    claims
        .iter()
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("{name} is not named: {claims:?}"))
        .left
        .clone()
}

#[test]
fn an_old_empty_claim_is_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    fs::create_dir_all(playbooks.join("gone")).unwrap();
    let claims = stale_claims(&playbooks, aged_now());
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].path, playbooks.join("gone"));
    assert_eq!(claims[0].left, ClaimLeft::Empty);
}

#[test]
fn a_young_empty_claim_is_not_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    fs::create_dir_all(playbooks.join("gone")).unwrap();
    // Real now: a creation in progress is younger than the threshold.
    assert!(stale_claims(&playbooks, SystemTime::now()).is_empty());
}

#[test]
fn a_directory_with_current_is_not_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    fs::create_dir_all(playbooks.join("gone")).unwrap();
    fs::write(playbooks.join("gone").join("current"), "1.0.0").unwrap();
    assert!(stale_claims(&playbooks, aged_now()).is_empty());
}

#[test]
fn a_link_is_not_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("real");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&playbooks).unwrap();
    std::os::unix::fs::symlink(&target, playbooks.join("gone")).unwrap();
    // The target aged: a claim that read the link's time through it would
    // name this. `symlink_metadata` never follows it.
    set_mtime(&target, SystemTime::now() - Duration::from_secs(700));
    assert!(stale_claims(&playbooks, SystemTime::now()).is_empty());
}

#[test]
fn an_old_empty_claim_whose_id_lock_is_held_is_not_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    let gone = playbooks.join("gone");
    fs::create_dir_all(&gone).unwrap();
    let now = SystemTime::now();
    set_mtime(&gone, now - Duration::from_secs(700));
    let _lock = lock_dir(&playbooks, "gone.lock").unwrap();
    // Held: less than 60 s before now (the lock's own staleness rule).
    let lock_path = playbooks.join("gone.lock");
    set_mtime(&lock_path, now - Duration::from_secs(30));
    let token = fs::read_to_string(&lock_path).unwrap();
    assert!(stale_claims(&playbooks, now).is_empty());
    // Read only: the lock file and its token are exactly as they were.
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), token);
}

#[test]
fn an_old_empty_claim_with_a_stale_id_lock_is_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    let gone = playbooks.join("gone");
    fs::create_dir_all(&gone).unwrap();
    let now = SystemTime::now();
    set_mtime(&gone, now - Duration::from_secs(700));
    // A killed holder's lock file, older than the lock's staleness rule.
    let lock_path = playbooks.join("gone.lock");
    fs::write(&lock_path, "dead-token").unwrap();
    set_mtime(&lock_path, now - Duration::from_secs(120));
    let claims = stale_claims(&playbooks, now);
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].left, ClaimLeft::Empty);
    // Read only: the lock file a person may want to look at stays.
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), "dead-token");
}

#[test]
fn only_the_exact_draft_names_are_debris() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    // The four names the draft creation writes (design D4.6 item 1).
    let debris = |dir: &Path| {
        fs::create_dir_all(dir.join(".tmp-1.0.0-123")).unwrap();
        fs::write(dir.join(".candidate.lock"), "t").unwrap();
        fs::write(dir.join("current.intent.json"), "{}").unwrap();
        fs::write(dir.join("current-history.jsonl"), "").unwrap();
    };
    let gone = playbooks.join("gone");
    debris(&gone);
    let gone_env = playbooks.join("gone-env");
    debris(&gone_env);
    fs::write(gone_env.join(".env"), "X=1").unwrap();
    let gone_tmp = playbooks.join("gone-tmp");
    debris(&gone_tmp);
    fs::create_dir_all(gone_tmp.join(".tmp-x")).unwrap();
    let gone_notes = playbooks.join("gone-notes");
    debris(&gone_notes);
    fs::write(gone_notes.join("notes.txt"), "mine").unwrap();

    let claims = stale_claims(&playbooks, aged_now());
    assert_eq!(claims.len(), 4, "{claims:?}");
    assert_eq!(left_of(&claims, &playbooks, "gone"), ClaimLeft::DraftDebris);
    assert_eq!(left_of(&claims, &playbooks, "gone-env"), ClaimLeft::Other);
    assert_eq!(left_of(&claims, &playbooks, "gone-tmp"), ClaimLeft::Other);
    assert_eq!(left_of(&claims, &playbooks, "gone-notes"), ClaimLeft::Other);
}

#[test]
fn a_version_without_current_is_other() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    let gone = playbooks.join("gone");
    fs::create_dir_all(gone.join("1.0.0")).unwrap();
    fs::write(gone.join("1.0.0/playbook.yaml"), YAML).unwrap();
    let claims = stale_claims(&playbooks, aged_now());
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].left, ClaimLeft::Other);
}

/// A copy that stages longer than its id lock's staleness: its staged copy
/// beside the store keeps the empty claim from being offered for removal.
#[test]
fn an_empty_claim_with_a_staged_copy_is_staging() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    fs::create_dir_all(playbooks.join("gone")).unwrap();
    fs::create_dir_all(playbooks.join("gone-b")).unwrap();
    fs::create_dir_all(base.path().join("playbooks-staging/gone-123456789")).unwrap();
    let claims = stale_claims(&playbooks, aged_now());
    assert_eq!(left_of(&claims, &playbooks, "gone"), ClaimLeft::Staging);
    // Another id that shares the prefix is not this staged copy's.
    assert_eq!(left_of(&claims, &playbooks, "gone-b"), ClaimLeft::Empty);
}

#[test]
fn a_claim_whose_pointer_lock_is_held_is_not_named() {
    let base = tempfile::tempdir().unwrap();
    let playbooks = base.path().join("playbooks");
    let gone = playbooks.join("gone");
    fs::create_dir_all(&gone).unwrap();
    let now = aged_now();
    // The draft's pointer lock, fresh against `now`: the claim is old
    // enough to be named, the lock's hold keeps it back.
    let lock_path = gone.join(".candidate.lock");
    fs::write(&lock_path, "token").unwrap();
    set_mtime(&lock_path, now - Duration::from_secs(30));
    assert!(stale_claims(&playbooks, now).is_empty());
}
