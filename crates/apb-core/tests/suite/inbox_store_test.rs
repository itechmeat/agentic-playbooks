//! `apb_core::connector::inbox`: the machine-scoped inbound event store.
//! Every test drives the path-taking constructor (`Inbox::at`) against a
//! tempdir, so none of them touches process env and none needs the shared
//! env lock.

use apb_core::connector::inbox::{Appended, Inbox, Retention};
use serde_json::json;

fn inbox(dir: &tempfile::TempDir) -> Inbox {
    Inbox::at(dir.path(), "echo-hooks", "main").unwrap()
}

#[test]
fn append_read_ack_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);

    assert_eq!(
        box_.append("m1", &json!({"text": "one"})).unwrap(),
        Appended::Stored(1)
    );
    assert_eq!(
        box_.append("m2", &json!({"text": "two"})).unwrap(),
        Appended::Stored(2)
    );
    assert_eq!(
        box_.append("m3", &json!({"text": "three"})).unwrap(),
        Appended::Stored(3)
    );

    // read does not move the cursor: two reads in a row see the same events.
    let (events, cursor) = box_.read("worker", 10).unwrap();
    assert_eq!(
        cursor, 0,
        "an unknown consumer starts before the first event"
    );
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(events[0].body["text"], "one");
    let (again, _) = box_.read("worker", 10).unwrap();
    assert_eq!(again.len(), 3, "read must not consume");

    // limit pages from the cursor forward.
    let (page, _) = box_.read("worker", 2).unwrap();
    assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);

    // ack moves the cursor forward and only forward.
    assert_eq!(box_.ack("worker", 2).unwrap(), 2);
    let (rest, cursor) = box_.read("worker", 10).unwrap();
    assert_eq!(cursor, 2);
    assert_eq!(rest.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3]);
    assert_eq!(
        box_.ack("worker", 1).unwrap(),
        2,
        "ack never moves backwards"
    );

    // a second consumer has its own cursor.
    let (other, cursor) = box_.read("auditor", 10).unwrap();
    assert_eq!(cursor, 0);
    assert_eq!(other.len(), 3, "cursors are per consumer");

    let depth = box_.depth("worker").unwrap();
    assert_eq!(depth.total, 3);
    assert_eq!(depth.pending, 1);
    assert_eq!(depth.cursor, 2);
    assert!(depth.last_received_at.is_some());
}

#[test]
fn a_duplicate_provider_id_is_not_appended() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    assert_eq!(
        box_.append("m1", &json!({"n": 1})).unwrap(),
        Appended::Stored(1)
    );
    assert_eq!(
        box_.append("m1", &json!({"n": 2})).unwrap(),
        Appended::Duplicate,
        "a redelivery of the same provider id is dropped"
    );
    let (events, _) = box_.read("w", 10).unwrap();
    assert_eq!(events.len(), 1, "the duplicate left no second line");
    assert_eq!(events[0].body["n"], 1, "the first delivery is the one kept");
}

#[test]
fn the_dedupe_index_is_bounded() {
    use apb_core::connector::inbox::DEDUPE_WINDOW;
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    // A generous retention keeps every event, so only the index rolls.
    let keep = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: u64::MAX,
    };
    for i in 0..(DEDUPE_WINDOW + 5) {
        box_.append_with(&format!("m{i}"), &json!({"i": i}), &keep)
            .unwrap();
    }
    let raw = std::fs::read_to_string(box_.dir().join("dedupe.idx")).unwrap();
    let lines = raw.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        lines, DEDUPE_WINDOW,
        "the index holds the last {DEDUPE_WINDOW}"
    );
    assert!(!raw.contains("m0\n"), "the oldest ids rolled out");
}

#[test]
fn two_concurrent_appenders_get_unique_sequence_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().to_path_buf();
    let mut handles = Vec::new();
    for worker in 0..2u32 {
        let base = base.clone();
        handles.push(std::thread::spawn(move || {
            let box_ = Inbox::at(&base, "echo-hooks", "main").unwrap();
            let mut seqs = Vec::new();
            for i in 0..25u32 {
                match box_.append(&format!("w{worker}-{i}"), &serde_json::json!({"w": worker})) {
                    Ok(Appended::Stored(seq)) => seqs.push(seq),
                    other => panic!("append failed: {other:?}"),
                }
            }
            seqs
        }));
    }
    let mut all: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    all.sort_unstable();
    let unique = {
        let mut u = all.clone();
        u.dedup();
        u
    };
    assert_eq!(all.len(), 50);
    assert_eq!(unique.len(), 50, "every seq must be unique: {all:?}");
    assert_eq!(all, (1..=50).collect::<Vec<u64>>(), "and gapless from 1");

    let box_ = Inbox::at(&base, "echo-hooks", "main").unwrap();
    let (events, _) = box_.read("w", 1000).unwrap();
    assert_eq!(
        events.len(),
        50,
        "every line survived the concurrent appends"
    );
}

#[test]
fn retention_drops_acked_entries_first_then_the_oldest_by_size() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    let keep = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: u64::MAX,
    };
    for i in 1..=6u32 {
        box_.append_with(&format!("m{i}"), &json!({"i": i}), &keep)
            .unwrap();
    }
    box_.ack("worker", 3).unwrap();

    // Age cap alone: everything acked is older than a zero-length window, so
    // seqs 1..=3 go and the unacked tail stays.
    let age_only = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: 0,
    };
    box_.append_with("m7", &json!({"i": 7}), &age_only).unwrap();
    let (events, _) = box_.read("fresh", 100).unwrap();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![4, 5, 6, 7],
        "acked entries past the age window go, unacked ones stay"
    );

    // Size cap: unacked entries are dropped oldest first when nothing else fits.
    let tiny = Retention {
        max_bytes: 1,
        max_age_ms: u64::MAX,
    };
    box_.append_with("m8", &json!({"i": 8}), &tiny).unwrap();
    let (events, _) = box_.read("fresh", 100).unwrap();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![8],
        "the size cap keeps the newest entry and nothing else"
    );

    // Sequence numbers never restart, even after retention emptied the file.
    assert_eq!(
        box_.append("m9", &json!({"i": 9})).unwrap(),
        Appended::Stored(9)
    );

    // Depth is derived from the surviving range, not from a scan, so it stays
    // correct after retention moved the front of the log.
    let depth = box_.depth("worker").unwrap();
    assert_eq!(depth.total, 2, "seqs 8 and 9 survive");
    assert_eq!(
        depth.pending, 2,
        "the cursor sits below the surviving range"
    );
    assert_eq!(depth.cursor, 3);
}

#[test]
fn every_inbox_file_is_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    box_.append("m1", &json!({"a": 1})).unwrap();
    box_.ack("worker", 1).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in ["events.jsonl", "dedupe.idx", "cursors.yaml"] {
            let path = box_.dir().join(file);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{file} must be owner-only, got {mode:o}"
            );
        }
    }
    let leftovers: Vec<_> = std::fs::read_dir(box_.dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with(".tmp") || n.ends_with(".lock")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp or lock files left behind: {leftovers:?}"
    );
}

#[test]
fn unsafe_path_segments_and_consumer_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    for (connector, account) in [("../etc", "main"), ("echo-hooks", ".."), ("", "main")] {
        let err = Inbox::at(dir.path(), connector, account)
            .unwrap_err()
            .to_string();
        assert!(!err.contains('!'), "no exclamation marks: {err}");
    }
    let box_ = inbox(&dir);
    box_.append("m1", &json!({})).unwrap();
    assert!(
        box_.read("Bad Consumer", 10).is_err(),
        "a consumer name is an identifier"
    );
    assert!(
        box_.ack("../escape", 1).is_err(),
        "and cannot escape the cursor map"
    );
}

#[test]
fn listing_accounts_reports_what_exists() {
    use apb_core::connector::inbox::list_accounts;
    let dir = tempfile::tempdir().unwrap();
    assert!(list_accounts(dir.path(), "echo-hooks").is_empty());
    Inbox::at(dir.path(), "echo-hooks", "main")
        .unwrap()
        .append("m1", &json!({}))
        .unwrap();
    Inbox::at(dir.path(), "echo-hooks", "backup")
        .unwrap()
        .append("m1", &json!({}))
        .unwrap();
    assert_eq!(
        list_accounts(dir.path(), "echo-hooks"),
        vec!["backup".to_string(), "main".to_string()],
        "sorted, one entry per account directory"
    );
}
