//! The move of `current`: its checks, its record, what a crash after each
//! write leaves and what recovery makes of it, the audit by value, a
//! read-only process, and the scrubbed reason.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::fault::{Step, fail_after};
use super::*;

const YAML: &str =
    "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\nedges: []\n";
const CANDIDATE: &str = "1.2.0";

/// A project with playbook `p`: versions 1.0.0, 1.1.0 and 1.2.0, `current`
/// at 1.0.0.
fn project() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join(".apb/playbooks/p");
    for v in ["1.0.0", "1.1.0", CANDIDATE] {
        fs::create_dir_all(dir.join(v)).unwrap();
        fs::write(dir.join(v).join("playbook.yaml"), YAML).unwrap();
    }
    fs::write(dir.join(CURRENT_FILE), "1.0.0").unwrap();
    (t, dir)
}

/// 1.2.0 on trial, with two successes recorded.
fn with_candidate(dir: &Path) {
    fs::write(dir.join("candidate"), CANDIDATE).unwrap();
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::write(
        dir.join("meta").join(format!("{CANDIDATE}.yaml")),
        "created_by: supervisor\nscope: next_runs\nbase_version: 1.0.0\ntrial:\n  successes: 2\n",
    )
    .unwrap();
}

fn person() -> MovedBy {
    MovedBy::Person {
        surface: Surface::Cli,
    }
}

fn promotion() -> MoveCurrent<'static> {
    MoveCurrent {
        to: CANDIDATE,
        expected_from: Some(Some("1.0.0")),
        by: MovedBy::CandidatePromotion {
            run_id: Some("r1".into()),
            basis: "quality".into(),
        },
        reason: None,
        trial: Some(TrialFinal {
            run_id: Some("r1".into()),
            quality: None,
        }),
    }
}

fn cur(dir: &Path) -> Option<String> {
    read_current_pointer(dir)
}

fn entries(dir: &Path) -> Vec<HistoryEntry> {
    history(dir, usize::MAX).unwrap().entries
}

fn trial(dir: &Path, v: &str) -> crate::versioning::TrialRecord {
    read_provenance_in(dir, v).unwrap().trial.unwrap()
}

/// Every file below `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn a_person_moves_current_and_the_history_says_so() {
    let (_t, dir) = project();
    let mut req = MoveCurrent::new("1.1.0", person());
    req.expected_from = Some(Some("1.0.0"));
    req.reason = Some("roll back the flaky retry");
    let moved = move_current(&dir, &req).unwrap();
    assert_eq!(moved.from.as_deref(), Some("1.0.0"));
    assert_eq!(moved.to, "1.1.0");
    assert!(!moved.audit_pending && !moved.awaiting_approval);
    assert_eq!(cur(&dir).as_deref(), Some("1.1.0"));
    assert!(!dir.join(INTENT_FILE).exists());
    assert!(!dir.join(CANDIDATE_LOCK_FILE).exists());
    let h = history(&dir, 10).unwrap();
    assert!(!h.incomplete && !h.pending);
    let e = &h.entries[0];
    assert_eq!(e.change_id, moved.change_id);
    assert_eq!(e.to.as_deref(), Some("1.1.0"));
    assert_eq!(e.by, Some(person()));
    assert_eq!(e.reason.as_deref(), Some("roll back the flaky retry"));
    assert_eq!(e.prev, None);
}

#[test]
fn a_stale_expectation_moves_nothing() {
    let (_t, dir) = project();
    let mut req = MoveCurrent::new("1.1.0", person());
    req.expected_from = Some(Some("1.2.0"));
    let e = move_current(&dir, &req).unwrap_err();
    assert!(
        matches!(&e, MoveError::Stale { actual } if actual.as_deref() == Some("1.0.0")),
        "{e:?}"
    );
    assert_eq!(e.code(), "stale");
    assert_eq!(cur(&dir).as_deref(), Some("1.0.0"));
    assert!(entries(&dir).is_empty());
}

#[test]
fn a_person_never_moves_to_the_candidate_and_a_frozen_playbook_refuses() {
    let (_t, dir) = project();
    with_candidate(&dir);
    let e = move_current(&dir, &MoveCurrent::new(CANDIDATE, person())).unwrap_err();
    assert!(matches!(e, MoveError::IsCandidate { .. }), "{e:?}");
    fs::write(dir.join(crate::registry::FROZEN_MARKER), "1").unwrap();
    let e = move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap_err();
    assert!(matches!(e, MoveError::Frozen), "{e:?}");
    // A migration carries the definition as it is: freezing does not stop it.
    move_current(&dir, &MoveCurrent::new("1.1.0", MovedBy::Migration)).unwrap();
    assert_eq!(cur(&dir).as_deref(), Some("1.1.0"));
    let e = move_current(&dir, &MoveCurrent::new("9.9.9", MovedBy::Migration)).unwrap_err();
    assert!(matches!(e, MoveError::NotFound(_)), "{e:?}");
}

#[test]
fn a_move_past_the_candidate_supersedes_it() {
    let (_t, dir) = project();
    with_candidate(&dir);
    let moved = move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
    assert_eq!(moved.superseded_candidate.as_deref(), Some(CANDIDATE));
    assert!(!dir.join("candidate").exists());
    assert_eq!(
        trial(&dir, CANDIDATE).outcome.as_deref(),
        Some("superseded")
    );
    assert_eq!(
        entries(&dir)[0].superseded_candidate.as_deref(),
        Some(CANDIDATE)
    );
}

#[test]
fn a_candidate_promotion_finishes_its_trial_record() {
    let (_t, dir) = project();
    with_candidate(&dir);
    let moved = move_current(&dir, &promotion()).unwrap();
    assert!(!moved.audit_pending);
    assert_eq!(cur(&dir).as_deref(), Some(CANDIDATE));
    assert!(!dir.join("candidate").exists());
    let t = trial(&dir, CANDIDATE);
    assert_eq!(t.outcome.as_deref(), Some("promoted"));
    assert_eq!(t.run_id.as_deref(), Some("r1"));
    assert_eq!(t.successes, 2);
}

#[test]
fn a_version_awaiting_approval_may_become_current_and_says_so() {
    let _l = crate::env_test_lock();
    // The approval is read from a trust store of this test's own.
    let _cfg = ConfigDir::set();
    let (_t, dir) = project();
    fs::create_dir_all(dir.join("1.1.0/scripts")).unwrap();
    fs::write(dir.join("1.1.0/scripts/run.sh"), "echo\n").unwrap();
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::write(
        dir.join("meta/1.1.0.yaml"),
        "created_by: import\nimported:\n  bundle_digest: sha256:00\n  source_id: p\n  source_version: 1.1.0\n  imported_at_ms: 1\n  scripts:\n    - { path: run.sh, sha256: 'sha256:11', bytes: 5 }\n",
    )
    .unwrap();
    let moved = move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
    assert!(moved.awaiting_approval);
    assert_eq!(cur(&dir).as_deref(), Some("1.1.0"));
}

#[test]
fn two_people_with_one_expectation_one_wins() {
    let (_t, dir) = project();
    let results: Vec<_> = std::thread::scope(|s| {
        let a = s.spawn(|| {
            let mut req = MoveCurrent::new("1.1.0", person());
            req.expected_from = Some(Some("1.0.0"));
            move_current(&dir, &req)
        });
        let b = s.spawn(|| {
            let mut req = MoveCurrent::new(CANDIDATE, person());
            req.expected_from = Some(Some("1.0.0"));
            move_current(&dir, &req)
        });
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    let won: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(won.len(), 1, "{results:?}");
    assert!(
        results
            .iter()
            .any(|r| matches!(r, Err(MoveError::Stale { .. })))
    );
    assert_eq!(cur(&dir).as_deref(), Some(won[0].to.as_str()));
    assert!(!history(&dir, 10).unwrap().incomplete);
}

/// A person's move racing another writer that expects the same `current`:
/// exactly one moves, and the record agrees with the pointer.
fn race_person_with(other: MoveCurrent<'static>, setup: impl FnOnce(&Path)) {
    let (_t, dir) = project();
    setup(&dir);
    let results: Vec<_> = std::thread::scope(|s| {
        let a = s.spawn(|| {
            let mut req = MoveCurrent::new("1.1.0", person());
            req.expected_from = Some(Some("1.0.0"));
            move_current(&dir, &req)
        });
        let b = s.spawn(|| move_current(&dir, &other));
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    let h = history(&dir, 10).unwrap();
    assert!(!h.incomplete && !h.pending);
    assert_eq!(cur(&dir), h.entries[0].to);
}

#[test]
fn a_person_racing_a_schema_migration_goes_through_the_lock() {
    let mut migration = MoveCurrent::new(CANDIDATE, MovedBy::Migration);
    migration.expected_from = Some(Some("1.0.0"));
    race_person_with(migration, |_| {});
}

#[test]
fn a_person_racing_a_candidate_promotion_goes_through_the_lock() {
    race_person_with(promotion(), with_candidate);
}

/// What a move leaves when it stops right after `step`, and after the
/// recovery that follows.
fn crash_after(step: Step, req: &MoveCurrent<'_>) -> (tempfile::TempDir, PathBuf) {
    let (t, dir) = project();
    with_candidate(&dir);
    let before = cur(&dir);
    fail_after(Some(step));
    let result = move_current(&dir, req);
    fail_after(None);
    let changed = !matches!(step, Step::Intent | Step::Supersede);
    // A failure before `current` is an error; after it, a move whose record
    // is pending.
    match &result {
        Ok(m) => assert!(changed && m.audit_pending, "{step:?}: {m:?}"),
        Err(e) => assert!(!changed, "{step:?}: {e}"),
    }
    let expected_current = if changed {
        Some(req.to)
    } else {
        before.as_deref()
    };
    assert_eq!(cur(&dir).as_deref(), expected_current, "{step:?}");
    let intent = intent::read(&dir).unwrap();
    assert_eq!(intent.is_some(), step != Step::IntentRemoval, "{step:?}");
    let change_id = intent.map(|i| i.change_id);

    let held = lock(&dir).unwrap();
    recover(&dir, &held).unwrap();
    drop(held);
    assert!(!dir.join(INTENT_FILE).exists(), "{step:?}");
    assert_eq!(cur(&dir).as_deref(), expected_current, "{step:?}");
    let h = history(&dir, 10).unwrap();
    assert!(!h.incomplete && !h.pending, "{step:?}: {h:?}");
    if let Some(id) = change_id {
        let lines: Vec<_> = h.entries.iter().filter(|e| e.change_id == id).collect();
        assert_eq!(lines.len(), 1, "{step:?}: {h:?}");
        let kind = if changed {
            EntryKind::Move
        } else {
            EntryKind::Aborted
        };
        assert_eq!(lines[0].kind, kind, "{step:?}");
    }
    (t, dir)
}

#[test]
fn a_person_move_survives_a_crash_after_each_write() {
    let mut req = MoveCurrent::new("1.1.0", person());
    req.expected_from = Some(Some("1.0.0"));
    for step in [
        Step::Intent,
        Step::Supersede,
        Step::Current,
        Step::History,
        Step::IntentRemoval,
    ] {
        let (_t, dir) = crash_after(step, &req);
        // The candidate is ended only once its pointer is gone, and never
        // left beside a newer `current`.
        let ended = step != Step::Intent;
        assert_eq!(dir.join("candidate").exists(), !ended, "{step:?}");
        let outcome = trial(&dir, CANDIDATE).outcome;
        assert_eq!(outcome.is_some(), ended, "{step:?}");
        let aborted = &entries(&dir)[0];
        if step == Step::Supersede {
            assert_eq!(aborted.superseded_candidate.as_deref(), Some(CANDIDATE));
        }
        if step == Step::Intent {
            assert_eq!(aborted.superseded_candidate, None);
        }
    }
}

#[test]
fn a_candidate_promotion_survives_a_crash_after_each_write() {
    for step in [
        Step::Intent,
        Step::Current,
        Step::PointerClear,
        Step::TrialRecord,
        Step::History,
        Step::IntentRemoval,
    ] {
        let (_t, dir) = crash_after(step, &promotion());
        if step == Step::Intent {
            assert_eq!(read_candidate(&dir).as_deref(), Some(CANDIDATE));
            assert_eq!(trial(&dir, CANDIDATE).outcome, None);
            continue;
        }
        // Rolled forward: the pointer cleared and the trial promoted, also
        // after a crash between `current` and the trial record.
        assert!(!dir.join("candidate").exists(), "{step:?}");
        let t = trial(&dir, CANDIDATE);
        assert_eq!(t.outcome.as_deref(), Some("promoted"), "{step:?}");
        assert_eq!(t.run_id.as_deref(), Some("r1"), "{step:?}");
    }
}

#[test]
fn between_current_and_the_trial_record_the_gate_offers_no_trial() {
    let (_t, dir) = project();
    with_candidate(&dir);
    fail_after(Some(Step::Current));
    let moved = move_current(&dir, &promotion()).unwrap();
    fail_after(None);
    assert!(moved.audit_pending);
    // The pointer still names the new `current`: never a trial.
    assert_eq!(read_candidate(&dir).as_deref(), Some(CANDIDATE));
    let current = crate::schema::Playbook::from_yaml(YAML).unwrap();
    assert_eq!(
        crate::candidate::choose_for_start(&dir, &current, CANDIDATE, 0.0),
        None
    );
    // The next move finishes it first.
    move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
    assert_eq!(trial(&dir, CANDIDATE).outcome.as_deref(), Some("promoted"));
    let h = history(&dir, 10).unwrap();
    assert_eq!(h.entries.len(), 2);
    assert!(!h.incomplete);
}

/// A candidate whose sidecar does not read.
fn with_unreadable_candidate(dir: &Path) {
    with_candidate(dir);
    fs::write(
        dir.join("meta").join(format!("{CANDIDATE}.yaml")),
        "trial: [not, a, record\n",
    )
    .unwrap();
}

#[test]
fn a_trial_record_that_does_not_read_refuses_before_any_write() {
    let mut past = MoveCurrent::new("1.1.0", person());
    past.expected_from = Some(Some("1.0.0"));
    for req in [past, promotion()] {
        let (_t, dir) = project();
        with_unreadable_candidate(&dir);
        let before = snapshot(&dir);
        let e = move_current(&dir, &req).unwrap_err();
        assert!(matches!(e, MoveError::Record(_)), "{e:?}");
        assert_eq!(e.code(), "record_unreadable");
        // The candidate is still on trial, and no intent waits to end it.
        let mut after = snapshot(&dir);
        after.remove(&dir.join(crate::candidate_pointer::CANDIDATE_LOCK_FILE));
        assert_eq!(after, before, "{}", req.to);
        assert_eq!(read_candidate(&dir).as_deref(), Some(CANDIDATE));
        assert!(entries(&dir).is_empty());
    }
}

#[test]
fn recovery_never_promotes_a_version_the_move_does_not_target() {
    let (_t, dir) = project();
    with_candidate(&dir);
    fail_after(Some(Step::Current));
    move_current(&dir, &promotion()).unwrap();
    fail_after(None);
    // A hand edit makes the waiting intent promote another version.
    let path = dir.join(INTENT_FILE);
    let mut intent: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    intent["finish_candidate"]["version"] = "1.1.0".into();
    fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();

    let held = lock(&dir).unwrap();
    let e = recover(&dir, &held).unwrap_err();
    drop(held);
    assert!(e.to_string().contains("remove it by hand"), "{e}");
    assert!(path.exists());
    assert!(read_provenance_in(&dir, "1.1.0").is_none());
    assert_eq!(trial(&dir, CANDIDATE).outcome, None);
}

#[test]
fn an_intent_that_does_not_read_refuses_a_move_and_a_history_read() {
    let (_t, dir) = project();
    fs::write(dir.join(INTENT_FILE), "{not json").unwrap();
    let e = move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap_err();
    assert!(e.to_string().contains("remove it by hand"), "{e}");
    assert_eq!(cur(&dir).as_deref(), Some("1.0.0"));
    let e = history(&dir, 10).unwrap_err();
    assert!(e.to_string().contains("remove it by hand"), "{e}");
}

#[test]
fn an_unset_current_moves_only_for_a_caller_who_saw_it_unset() {
    let (_t, dir) = project();
    fs::remove_file(dir.join(CURRENT_FILE)).unwrap();
    let mut req = MoveCurrent::new("1.1.0", person());
    req.expected_from = Some(Some("1.0.0"));
    let e = move_current(&dir, &req).unwrap_err();
    assert!(matches!(e, MoveError::Stale { actual: None }), "{e:?}");
    req.expected_from = Some(None);
    let moved = move_current(&dir, &req).unwrap();
    assert_eq!((moved.from, moved.to.as_str()), (None, "1.1.0"));
    // Set now, an expectation of unset is stale.
    let mut again = MoveCurrent::new("1.0.0", person());
    again.expected_from = Some(None);
    let e = move_current(&dir, &again).unwrap_err();
    assert!(matches!(e, MoveError::Stale { actual: Some(_) }), "{e:?}");
}

#[test]
fn a_history_read_never_sees_a_move_half_written() {
    let (_t, dir) = project();
    move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
    move_current(&dir, &MoveCurrent::new("1.0.0", person())).unwrap();
    let held = lock(&dir).unwrap();
    // A writer between `current` and its history line.
    fs::write(dir.join(CURRENT_FILE), "1.1.0").unwrap();
    let h = std::thread::scope(|s| {
        let reader = s.spawn(|| history(&dir, 10).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(150));
        fs::write(dir.join(CURRENT_FILE), "1.0.0").unwrap();
        move_current_locked(&dir, &MoveCurrent::new("1.1.0", person()), &held).unwrap();
        drop(held);
        reader.join().unwrap()
    });
    assert!(!h.incomplete, "{h:?}");
    assert_eq!(h.entries[0].to.as_deref(), Some("1.1.0"));
}

/// A playbook directory this process may read but not write: the history
/// still reads, without a lock, while no interrupted move waits.
#[cfg(unix)]
#[test]
fn a_history_reads_from_a_directory_it_may_not_write() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, dir) = project();
    move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes anyway; then there is nothing to check.
    let writable = fs::write(dir.join("probe"), "").is_ok();
    let read = history(&dir, 10);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return;
    }
    let h = read.unwrap();
    assert_eq!(h.entries[0].to.as_deref(), Some("1.1.0"));
    assert!(!h.incomplete && !h.pending, "{h:?}");
    assert!(
        !dir.join(crate::candidate_pointer::CANDIDATE_LOCK_FILE)
            .exists()
    );
}

#[test]
fn the_audit_reads_values_not_file_metadata() {
    let (t, dir) = project();
    move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
    move_current(&dir, &MoveCurrent::new("1.0.0", person())).unwrap();
    move_current(&dir, &MoveCurrent::new(CANDIDATE, person())).unwrap();
    assert!(!history(&dir, 10).unwrap().incomplete);

    // A fresh clone: every file new, the same bytes.
    let clone = t.path().join("clone/.apb/playbooks/p");
    crate::fsutil::copy_tree(&dir, &clone).unwrap();
    let h = history(&clone, 10).unwrap();
    assert!(!h.incomplete && h.entries.len() == 3);

    // An older apb moved `current` to another value without a record.
    fs::write(clone.join(CURRENT_FILE), "1.1.0").unwrap();
    assert!(history(&clone, 10).unwrap().incomplete);

    // A lost line breaks the chain.
    let path = dir.join(HISTORY_FILE);
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| l)
        .collect();
    fs::write(&path, format!("{}\n", kept.join("\n"))).unwrap();
    assert!(history(&dir, 10).unwrap().incomplete);
}

#[test]
fn a_history_limit_keeps_the_newest() {
    let (_t, dir) = project();
    for to in ["1.1.0", "1.0.0", CANDIDATE] {
        move_current(&dir, &MoveCurrent::new(to, person())).unwrap();
    }
    let h = history(&dir, 2).unwrap();
    assert_eq!(h.entries.len(), 2);
    assert_eq!(h.entries[0].to.as_deref(), Some(CANDIDATE));
}

crate::read_only_test! {
    fn a_read_only_process_neither_moves_nor_recovers() before setup = {
        let (t, dir) = project();
        // A move whose record waits: recovering it needs only the history
        // line and the intent's removal.
        fail_after(Some(Step::Current));
        let moved = move_current(&dir, &MoveCurrent::new("1.1.0", person())).unwrap();
        fail_after(None);
        assert!(moved.audit_pending);
        let held = lock(&dir).unwrap();
        (t, dir, held)
    } then {
        let (_t, dir, held) = setup;
        let before = snapshot(&dir);
        let req = MoveCurrent::new(CANDIDATE, person());
        assert!(matches!(move_current_locked(&dir, &req, &held), Err(MoveError::ReadOnly)));
        assert!(matches!(recover(&dir, &held), Err(MoveError::ReadOnly)));
        assert!(matches!(move_current(&dir, &req), Err(MoveError::ReadOnly)));
        let h = history(&dir, 10).unwrap();
        assert!(h.pending, "{h:?}");
        assert_eq!(snapshot(&dir), before);
        // Its drop would remove the lock file.
        std::mem::forget(held);
    }
}

/// `APB_CONFIG_DIR` pointed at a fresh directory while it lives. The
/// caller holds `crate::env_test_lock()`.
struct ConfigDir(tempfile::TempDir);

impl ConfigDir {
    fn set() -> Self {
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: every test that reads or writes the environment holds
        // `env_test_lock`, so no other thread touches it meanwhile.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        ConfigDir(cfg)
    }
}

impl Drop for ConfigDir {
    fn drop(&mut self) {
        // SAFETY: as in `set`, under `env_test_lock`.
        unsafe { std::env::remove_var("APB_CONFIG_DIR") };
    }
}

/// A connector whose account holds `value` as a secret, for the project
/// at `root`.
fn configure_secret(cfg: &Path, root: &Path, value: &str) {
    let connector = cfg.join("connectors/mock-crm");
    fs::create_dir_all(&connector).unwrap();
    fs::write(
        connector.join("connector.yaml"),
        "name: mock-crm\nversion: 0.1.0\naccount_fields:\n  - name: base_url\n    required: true\n  - name: token\n    required: true\n    secret: true\nfunctions:\n  - name: list_items\n    description: List items\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/items\"\n",
    )
    .unwrap();
    let accounts = crate::connector::config::project_config_path(root, "mock-crm");
    fs::create_dir_all(accounts.parent().unwrap()).unwrap();
    fs::write(
        &accounts,
        "accounts:\n  - name: main\n    base_url: https://crm.example.com\n    token: \"{{env.APB_CURRENT_TEST_VALUE}}\"\n",
    )
    .unwrap();
    fs::write(
        root.join(".apb/secrets.env"),
        format!("APB_CURRENT_TEST_VALUE={value}\n"),
    )
    .unwrap();
}

#[test]
fn a_configured_secret_in_a_reason_is_never_kept() {
    let _l = crate::env_test_lock();
    let cfg = ConfigDir::set();
    let (t, dir) = project();
    let value = "violet hedgehog sings";
    configure_secret(cfg.0.path(), t.path(), value);
    let reason = format!("rolled back, the token {value} was rotated");

    // The intent holds the scrubbed reason.
    let mut req = MoveCurrent::new("1.1.0", person());
    req.reason = Some(&reason);
    fail_after(Some(Step::Intent));
    move_current(&dir, &req).unwrap_err();
    fail_after(None);
    let intent = fs::read_to_string(dir.join(INTENT_FILE)).unwrap();
    assert!(!intent.contains(value), "{intent}");
    assert!(intent.contains("rolled back"), "{intent}");

    move_current(&dir, &req).unwrap();
    let file = fs::read_to_string(dir.join(HISTORY_FILE)).unwrap();
    assert!(!file.contains(value), "{file}");
    let kept = history(&dir, 1).unwrap().entries[0].reason.clone().unwrap();
    assert!(kept.starts_with("rolled back"), "{kept}");

    // And a long one is cut.
    let long = "x".repeat(1000);
    let mut req = MoveCurrent::new("1.0.0", person());
    req.reason = Some(&long);
    move_current(&dir, &req).unwrap();
    let kept = history(&dir, 1).unwrap().entries[0].reason.clone().unwrap();
    assert_eq!(kept.chars().count(), REASON_MAX);
}
