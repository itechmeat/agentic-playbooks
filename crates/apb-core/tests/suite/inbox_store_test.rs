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
fn dropped_counter_round_trips_and_persists_across_a_fresh_handle() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);

    assert_eq!(
        box_.dropped_count().unwrap(),
        0,
        "absent counter file reads as 0"
    );

    assert_eq!(box_.note_dropped().unwrap(), 1);
    assert_eq!(box_.note_dropped().unwrap(), 2);
    assert_eq!(box_.dropped_count().unwrap(), 2);

    // A fresh handle over the same directory reads the same persisted count:
    // the counter is not the in-process `IngestState` one, it must be
    // visible from another process (`apb connector doctor`, a second
    // dashboard).
    let reopened = Inbox::at(dir.path(), "echo-hooks", "main").unwrap();
    assert_eq!(reopened.dropped_count().unwrap(), 2);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(box_.dir().join("dropped.count"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "dropped.count must be owner-only");
    }
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

/// Dedupe retention is time-based, not count-based: an id stays deduped for
/// the whole age window regardless of how many distinct deliveries follow it,
/// and is evicted only once the window has passed. The old count window forgot
/// an id after a fixed number of later deliveries, reopening a replay gap.
///
/// `append_bounded` makes the window, the size cap and the wall clock explicit
/// so this drives eviction deterministically rather than sleeping.
#[test]
fn dedupe_is_time_based_and_survives_a_flood_within_the_window() {
    const WINDOW_MS: u64 = 10_000;
    const CAP: usize = 10_000;
    let keep = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: u64::MAX,
    };
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    let t0 = 1_000_000u64;

    // Record "keep" at t0, then 500 distinct deliveries within the window.
    box_.append_bounded("keep", &json!({"k": 0}), &keep, WINDOW_MS, CAP, t0)
        .unwrap();
    for i in 0..500u64 {
        box_.append_bounded(
            &format!("m{i}"),
            &json!({"i": i}),
            &keep,
            WINDOW_MS,
            CAP,
            t0 + 1 + i,
        )
        .unwrap();
    }

    // Still deduped inside the window, no matter how many followed it (a
    // count-based window would have rolled it out already).
    assert!(
        box_.is_duplicate("keep").unwrap(),
        "an id seen now is still deduped after many later distinct deliveries"
    );
    assert_eq!(
        box_.append_bounded("keep", &json!({"k": 1}), &keep, WINDOW_MS, CAP, t0 + 900)
            .unwrap(),
        Appended::Duplicate,
        "a redelivery inside the window is refused"
    );

    // One append past the age window prunes it; only then is it forgotten.
    box_.append_bounded(
        "tick",
        &json!({}),
        &keep,
        WINDOW_MS,
        CAP,
        t0 + WINDOW_MS + 1,
    )
    .unwrap();
    assert!(
        !box_.is_duplicate("keep").unwrap(),
        "evicted only after the age window passes"
    );
    assert!(
        matches!(
            box_.append_bounded(
                "keep",
                &json!({"k": 2}),
                &keep,
                WINDOW_MS,
                CAP,
                t0 + WINDOW_MS + 2
            )
            .unwrap(),
            Appended::Stored(_)
        ),
        "a genuinely fresh delivery past the window is accepted again"
    );
}

/// The size cap is a safety valve independent of age: with an effectively
/// infinite window the index still rolls at the cap, oldest first. And every
/// stored id is a fixed-size hex digest, never the raw provider id.
#[test]
fn the_dedupe_index_size_cap_rolls_and_stores_only_digests() {
    const CAP: usize = 32;
    const NO_AGE_EVICTION: u64 = u64::MAX;
    let keep = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: u64::MAX,
    };
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    for i in 0..(CAP + 5) {
        box_.append_bounded(
            &format!("m{i}"),
            &json!({"i": i}),
            &keep,
            NO_AGE_EVICTION,
            CAP,
            1000 + i as u64,
        )
        .unwrap();
    }

    let raw = std::fs::read_to_string(box_.dir().join("dedupe.idx")).unwrap();
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), CAP, "the index holds the last {CAP}");
    assert!(
        !box_.is_duplicate("m0").unwrap(),
        "the oldest id rolled out"
    );
    assert!(
        box_.is_duplicate(&format!("m{}", CAP + 4)).unwrap(),
        "the newest id stayed"
    );
    for line in &lines {
        let (ts, id) = line.split_once(' ').expect("`<ts> <digest>` per line");
        assert!(ts.parse::<u64>().is_ok(), "a timestamp column: {ts}");
        let hex = id
            .strip_prefix("sha256:")
            .unwrap_or_else(|| panic!("stored id is a sha256 digest, not a raw provider id: {id}"));
        assert_eq!(hex.len(), 64, "fixed-size hex digest: {id}");
        assert!(
            hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "digest body is hex: {id}"
        );
    }
}

/// A provider id may contain a newline or be arbitrarily long: hashing it to a
/// fixed-size digest before it reaches the newline-delimited index keeps the
/// index uncorrupted and each append's rewrite bounded, while still deduping.
#[test]
fn dedupe_hashes_ids_so_a_newline_or_a_huge_id_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);

    // An embedded newline neither corrupts the index nor defeats dedupe.
    let nl_id = "abc\ndef";
    assert!(matches!(
        box_.append(nl_id, &json!({"n": 1})).unwrap(),
        Appended::Stored(_)
    ));
    assert_eq!(
        box_.append(nl_id, &json!({"n": 2})).unwrap(),
        Appended::Duplicate,
        "an id with a newline still dedupes"
    );
    let raw = std::fs::read_to_string(box_.dir().join("dedupe.idx")).unwrap();
    assert_eq!(
        raw.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "the embedded newline did not add a phantom index line"
    );

    // Two distinct long ids do not collide.
    let a = "x".repeat(200_000);
    let b = format!("{}y", "x".repeat(199_999));
    assert!(matches!(
        box_.append(&a, &json!({})).unwrap(),
        Appended::Stored(_)
    ));
    assert!(
        matches!(box_.append(&b, &json!({})).unwrap(), Appended::Stored(_)),
        "a different long id is not treated as a duplicate"
    );
    assert_eq!(
        box_.append(&a, &json!({})).unwrap(),
        Appended::Duplicate,
        "the first long id is still recognized"
    );
}

/// An ack for a seq beyond the current head is clamped, never persisted above
/// the head, so events that arrive later through that seq stay pending instead
/// of being silently swallowed.
#[test]
fn ack_beyond_head_does_not_swallow_future_events() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    for i in 1..=3u32 {
        box_.append(&format!("m{i}"), &json!({"i": i})).unwrap();
    }
    assert_eq!(
        box_.ack("worker", 100).unwrap(),
        3,
        "ack clamps to the head rather than persisting a cursor above it"
    );
    box_.append("m4", &json!({"i": 4})).unwrap();
    box_.append("m5", &json!({"i": 5})).unwrap();
    let (pending, cursor) = box_.read("worker", 100).unwrap();
    assert_eq!(cursor, 3);
    assert_eq!(
        pending.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![4, 5],
        "events after the clamped ack are still delivered, not skipped"
    );
    assert_eq!(box_.depth("worker").unwrap().pending, 2);
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
fn retention_never_empties_the_log_even_when_every_event_is_acked_and_expired() {
    let dir = tempfile::tempdir().unwrap();
    let box_ = inbox(&dir);
    let keep = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: u64::MAX,
    };
    for i in 1..=3u32 {
        box_.append_with(&format!("m{i}"), &json!({"i": i}), &keep)
            .unwrap();
    }
    // Ack the whole log up to its head. The cursor now covers every event
    // that exists.
    let cursor_before = box_.ack("worker", 3).unwrap();
    assert_eq!(cursor_before, 3);

    // (a) A zero-length age window makes every event that predates this append
    // read as acked and expired. Retention must still keep the newest event
    // rather than emptying events.jsonl.
    let age_only = Retention {
        max_bytes: 64 * 1024 * 1024,
        max_age_ms: 0,
    };
    box_.append_with("m4", &json!({"i": 4}), &age_only).unwrap();
    let (events, _) = box_.read("someone_else", 100).unwrap();
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![4],
        "the newest event survives even after everything before it was acked and expired"
    );

    // (b) seq keeps counting up from the survivor: it does not restart at 1
    // because the log was never actually emptied.
    assert_eq!(
        box_.append("m5", &json!({"i": 5})).unwrap(),
        Appended::Stored(5),
        "seq continues monotonically after retention, it does not restart at 1"
    );
    let depth = box_.depth("worker").unwrap();
    assert_eq!(
        depth.cursor, 3,
        "the cursor is where the clamped ack left it"
    );
    assert_eq!(depth.total, 2, "seqs 4 and 5 survive");
    assert_eq!(
        depth.pending, 2,
        "the cursor sits below the surviving range, so both survivors are pending"
    );
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
