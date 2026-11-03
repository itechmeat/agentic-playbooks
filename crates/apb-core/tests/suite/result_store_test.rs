//! The result snapshot contract shared across languages: canonical manifest
//! bytes and the snapshot id, the citation grammar, and lenient reading of
//! a manifest a newer apb wrote. The expected values come from the shared
//! fixture, which the web results helper reads too.

use apb_core::result_store::{
    Citation, EntryId, OmissionReason, ResultFormat, ResultScope, SnapshotManifest,
    SnapshotOmission, canonical_bytes, snapshot_id,
};
use apb_core::schema::Playbook;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../fixtures/result_manifest.json")).unwrap()
}

#[test]
fn canonical_bytes_and_snapshot_id_match_the_shared_fixture() {
    let fx = fixture();
    let manifest: SnapshotManifest = serde_json::from_value(fx["manifest"].clone()).unwrap();
    let bytes = canonical_bytes(&manifest);
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        fx["canonical"].as_str().unwrap()
    );
    assert_eq!(snapshot_id(&bytes), fx["snapshot_id"].as_str().unwrap());
    // The canonical form is a fixed point: reading it back and writing it
    // again gives the same bytes, so a stored manifest re-hashes to its id.
    let again: SnapshotManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(canonical_bytes(&again), bytes);
}

#[test]
fn citations_parse_and_render_as_the_shared_fixture_says() {
    let fx = fixture();
    for case in fx["citations"].as_array().unwrap() {
        let text = case["ref"].as_str().unwrap();
        let c: Citation = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(c.snapshot_id, fx["snapshot_id"].as_str().unwrap());
        let want = match case["entry"].as_str().unwrap() {
            "output" => EntryId::Output,
            "fields" => EntryId::Fields,
            _ => EntryId::File {
                scope: serde_json::from_value(case["scope"].clone()).unwrap(),
                path: case["path"].as_str().unwrap().to_string(),
            },
        };
        assert_eq!(c.entry, want, "{text}");
        let range = case["range"]
            .as_array()
            .map(|r| (r[0].as_u64().unwrap(), r[1].as_u64().unwrap()));
        assert_eq!(c.range, range, "{text}");
        assert_eq!(c.to_string(), text);
    }
    for text in fx["invalid_citations"].as_array().unwrap() {
        let text = text.as_str().unwrap();
        assert!(text.parse::<Citation>().is_err(), "{text} parsed");
    }
}

/// A manifest a newer apb wrote, with a reason, a limit and an output
/// omission this binary does not know, still loads.
#[test]
fn a_newer_manifest_reads_leniently() {
    let mut v = fixture()["manifest"].clone();
    v["omitted"][1]["reason"] = "quota_exhausted".into();
    v["omitted"][0]["reasons"]["quota_exhausted"] = 4.into();
    v["incomplete"] = "time_limit".into();
    v["output_omitted"] = "policy".into();
    let m: SnapshotManifest = serde_json::from_value(v).unwrap();
    let SnapshotOmission::Item(item) = &m.omitted[1] else {
        panic!("{:?}", m.omitted[1]);
    };
    assert_eq!(item.reason, OmissionReason::Unknown);
    let SnapshotOmission::More(more) = &m.omitted[0] else {
        panic!("{:?}", m.omitted[0]);
    };
    assert_eq!(more.reasons.get(&OmissionReason::Unknown), Some(&4));
    assert!(m.incomplete.is_some() && m.output_omitted.is_some());
}

/// `results:` on a node: scope and format default, and a playbook that
/// declares neither writes neither back.
#[test]
fn a_results_declaration_defaults_and_writes_back_unchanged() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 0.1.0
nodes:
  - { id: s, type: start }
  - id: w
    type: script
    script: "true"
    runner: sh
    results:
      - { path: docs/report.md }
      - { path: "notes/*.txt", scope: run, format: records }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: w }
  - { from: w, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let w = pb.nodes.iter().find(|n| n.id == "w").unwrap();
    let r = w.results.as_ref().unwrap();
    assert_eq!(
        (r[0].scope, r[0].format),
        (ResultScope::Workspace, ResultFormat::Text)
    );
    assert_eq!(
        (r[1].scope, r[1].format),
        (ResultScope::Run, ResultFormat::Records)
    );
    let written = yaml_serde::to_string(w).unwrap();
    assert!(written.contains("path: docs/report.md"), "{written}");
    assert_eq!(written.matches("scope:").count(), 1, "{written}");
    let s = pb.nodes.iter().find(|n| n.id == "s").unwrap();
    assert!(!yaml_serde::to_string(s).unwrap().contains("results"));
    assert!(
        Playbook::from_yaml(&yaml.replace("format: records", "fromat: records")).is_err(),
        "a misspelled key is refused"
    );
}

// --- the store: names and the run reservation -----------------------------

use apb_core::result_store::{
    MAX_MANIFEST_BYTES, MAX_RUN_BYTES, MAX_SNAPSHOT_BYTES, Reservation, ResultTxn, Staged,
    is_secret_shaped, read_reservation, write_reservation,
};

#[test]
fn secret_shaped_names_are_matched_in_any_case_and_nothing_else() {
    for name in [
        ".env",
        ".env.local",
        ".ENV",
        "server.pem",
        "tls.key",
        "cert.P12",
        "id_rsa",
        "id_ed25519.pub",
    ] {
        assert!(is_secret_shaped(name), "{name}");
    }
    for name in [
        "report.md",
        "env.txt",
        "keys.md",
        "pem",
        "my_id_rsa",
        "x.key.md",
    ] {
        assert!(!is_secret_shaped(name), "{name}");
    }
}

/// A run directory whose reservation leaves exactly `room` bytes once two
/// manifests are reserved.
fn nearly_full_run(room: u64) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let b = apb_core::fsutil::beneath::Beneath::open(t.path()).unwrap();
    b.mkdir_all("results").unwrap();
    write_reservation(
        &b,
        &Reservation {
            committed_bytes: MAX_RUN_BYTES - 2 * MAX_MANIFEST_BYTES as u64 - room,
            ..Reservation::default()
        },
    )
    .unwrap();
    t
}

#[cfg(unix)]
#[test]
fn transactions_never_reserve_past_the_run_limit() {
    let t = nearly_full_run(10);
    let mut a = ResultTxn::begin(t.path()).unwrap();
    let mut b = ResultTxn::begin(t.path()).unwrap();
    // No room for a third manifest.
    let err = ResultTxn::begin(t.path()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::StorageFull);
    assert!(matches!(
        a.stage_object(b"12345678").unwrap(),
        Staged::Stored(_)
    ));
    assert_eq!(b.stage_object(b"abcdefgh").unwrap(), Staged::RunFull);
    // The same bytes again cost nothing.
    assert!(matches!(
        a.stage_object(b"12345678").unwrap(),
        Staged::Stored(_)
    ));
    let anchor = apb_core::fsutil::beneath::Beneath::open(t.path()).unwrap();
    let r = read_reservation(&anchor).unwrap();
    assert_eq!(r.committed_bytes + r.reserved_bytes, MAX_RUN_BYTES - 2);
    a.abort().unwrap();
    b.end().unwrap();
    let r = read_reservation(&anchor).unwrap();
    assert!(r.open.is_empty());
    assert_eq!(r.reserved_bytes, 0);
}

#[cfg(unix)]
#[test]
fn concurrent_transactions_share_the_run_limit() {
    let t = nearly_full_run(1000);
    // Both begin first (each reserves its manifest room), and both stage
    // before either ends: an ended transaction gives its manifest room
    // back to the run.
    let txns = [
        ResultTxn::begin(t.path()).unwrap(),
        ResultTxn::begin(t.path()).unwrap(),
    ];
    let staged = std::sync::Barrier::new(2);
    let stored: u64 = std::thread::scope(|s| {
        let workers: Vec<_> = txns
            .into_iter()
            .enumerate()
            .map(|(w, mut txn)| {
                let staged = &staged;
                s.spawn(move || {
                    let mut n = 0;
                    for i in 0..40u8 {
                        let text = format!("{w}-{i:02}-{}", "x".repeat(44));
                        if let Staged::Stored(_) = txn.stage_object(text.as_bytes()).unwrap() {
                            n += text.len() as u64;
                        }
                    }
                    staged.wait();
                    txn.end().unwrap();
                    n
                })
            })
            .collect();
        workers.into_iter().map(|h| h.join().unwrap()).sum()
    });
    assert!(stored <= 1000, "{stored}");
    assert!(stored >= 1000 - 50, "{stored}");
}

#[cfg(unix)]
#[test]
fn one_snapshot_holds_at_most_its_limit() {
    let t = tempfile::tempdir().unwrap();
    let mut txn = ResultTxn::begin(t.path()).unwrap();
    let chunk = vec![b'a'; 1024 * 1024];
    let mut stored = 0;
    for i in 0..17u8 {
        let mut text = chunk.clone();
        text[0] = b'a' + i;
        match txn.stage_object(&text).unwrap() {
            Staged::Stored(_) => stored += text.len() as u64,
            Staged::SnapshotFull => break,
            Staged::RunFull => panic!("the run has room"),
        }
    }
    assert_eq!(stored, MAX_SNAPSHOT_BYTES);
    txn.abort().unwrap();
}

// The descriptor-relative IO the result store writes and reads through (`fsutil::beneath`).
mod beneath {
    //! Descriptor-relative access beneath an anchor (`apb_core::fsutil::beneath`):
    //! links swapped in at any component, FIFOs, modes at creation.
    #![cfg(unix)]

    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;
    use std::time::{Duration, Instant};

    use apb_core::fsutil::beneath::{
        Beneath, CappedRead, EntryKind, ListBudget, LockState, ReadRefusal,
    };

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn refusal(r: CappedRead) -> ReadRefusal {
        match r {
            CappedRead::Refused(why) => why,
            CappedRead::Read { bytes, .. } => panic!("read {} bytes", bytes.len()),
        }
    }

    #[test]
    fn a_regular_file_is_read_whole_with_its_digest() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("a/b")).unwrap();
        std::fs::write(t.path().join("a/b/report.md"), "done\n").unwrap();
        let b = Beneath::open(t.path()).unwrap();
        match b.read_regular_capped("a/b/report.md", 1024).unwrap() {
            CappedRead::Read { bytes, digest } => {
                assert_eq!(bytes, b"done\n");
                assert_eq!(digest, apb_core::content::sha256_hex(b"done\n"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            refusal(b.read_regular_capped("a/b/none.md", 1024).unwrap()),
            ReadRefusal::Missing
        );
    }

    #[test]
    fn a_path_through_a_link_is_refused_at_any_component() {
        let t = tempfile::tempdir().unwrap();
        let anchor = t.path().join("tree");
        let outside = t.path().join("outside");
        std::fs::create_dir_all(anchor.join("out")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(anchor.join("out/report.md"), "mine").unwrap();
        std::fs::write(outside.join("report.md"), "a private file").unwrap();
        let b = Beneath::open(&anchor).unwrap();
        let mut budget = ListBudget { remaining: 100 };
        let listed = b.list_dir("out", &mut budget).unwrap();
        assert_eq!(listed.entries.len(), 1);
        // Swapped between two calls: the walk of the next read refuses it.
        // The swap inside one read is covered by the unit tests of
        // `fsutil::beneath`.
        std::fs::rename(anchor.join("out"), anchor.join("moved")).unwrap();
        symlink(&outside, anchor.join("out")).unwrap();
        assert_eq!(
            refusal(b.read_regular_capped("out/report.md", 1024).unwrap()),
            ReadRefusal::Link
        );
        // A link at the last component is refused too.
        symlink(outside.join("report.md"), anchor.join("moved/linked.md")).unwrap();
        assert_eq!(
            refusal(b.read_regular_capped("moved/linked.md", 1024).unwrap()),
            ReadRefusal::Link
        );
        assert!(b.list_dir("out", &mut budget).is_err());
    }

    #[test]
    fn an_anchor_that_is_a_link_is_refused() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("real")).unwrap();
        symlink(t.path().join("real"), t.path().join("anchor")).unwrap();
        assert!(Beneath::open(&t.path().join("real")).is_ok());
        assert!(Beneath::open(&t.path().join("anchor")).is_err());
    }

    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let t = tempfile::tempdir().unwrap();
        let fifo = t.path().join("report.md");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let b = Beneath::open(t.path()).unwrap();
        let started = Instant::now();
        assert_eq!(
            refusal(b.read_regular_capped("report.md", 1024).unwrap()),
            ReadRefusal::NotRegular
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        std::fs::create_dir(t.path().join("dir")).unwrap();
        assert_eq!(
            refusal(b.read_regular_capped("dir", 1024).unwrap()),
            ReadRefusal::NotRegular
        );
    }

    #[test]
    fn a_hard_linked_file_is_refused_without_reading() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("private"), "secret").unwrap();
        std::fs::hard_link(t.path().join("private"), t.path().join("report.md")).unwrap();
        let b = Beneath::open(t.path()).unwrap();
        assert_eq!(
            refusal(b.read_regular_capped("report.md", 1024).unwrap()),
            ReadRefusal::HardLink
        );
    }

    #[test]
    fn a_file_over_the_cap_is_refused_never_cut() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("big"), vec![b'a'; 11]).unwrap();
        std::fs::write(t.path().join("exact"), vec![b'a'; 10]).unwrap();
        let b = Beneath::open(t.path()).unwrap();
        assert_eq!(
            refusal(b.read_regular_capped("big", 10).unwrap()),
            ReadRefusal::TooLarge { size: 11 }
        );
        assert!(matches!(
            b.read_regular_capped("exact", 10).unwrap(),
            CappedRead::Read { bytes, .. } if bytes.len() == 10
        ));
    }

    #[test]
    fn a_path_that_is_not_plain_and_relative_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let b = Beneath::open(t.path()).unwrap();
        for bad in ["../x", "a/../b", "/etc/passwd", "a//b", "./a"] {
            assert!(b.read_regular_capped(bad, 10).is_err(), "{bad}");
            assert!(b.mkdir_all(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn listing_counts_every_entry_against_the_shared_budget() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(t.path().join(format!("f{i}")), "x").unwrap();
        }
        std::fs::create_dir(t.path().join("d")).unwrap();
        symlink("f0", t.path().join("l")).unwrap();
        let b = Beneath::open(t.path()).unwrap();
        let mut budget = ListBudget { remaining: 100 };
        let all = b.list_dir("", &mut budget).unwrap();
        assert!(!all.exhausted);
        assert_eq!(budget.remaining, 93);
        let kind = |n: &str| {
            all.entries
                .iter()
                .find(|e| e.name == n)
                .map(|e| e.kind)
                .unwrap()
        };
        assert_eq!(kind("d"), EntryKind::Dir);
        assert_eq!(kind("l"), EntryKind::Link);
        assert_eq!(kind("f3"), EntryKind::File);

        let mut small = ListBudget { remaining: 3 };
        let part = b.list_dir("", &mut small).unwrap();
        assert!(part.exhausted);
        assert_eq!(part.entries.len(), 3);
        assert_eq!(small.remaining, 0);
        let again = b.list_dir("", &mut small).unwrap();
        assert!(again.exhausted && again.entries.is_empty());
    }

    #[test]
    fn writes_create_private_modes_and_replace_atomically() {
        let t = tempfile::tempdir().unwrap();
        let b = Beneath::open(t.path()).unwrap();
        b.mkdir_all("results/objects/ab").unwrap();
        for d in ["results", "results/objects", "results/objects/ab"] {
            assert_eq!(mode(&t.path().join(d)), 0o700, "{d}");
        }
        b.write_new("results/objects/ab", "obj", b"one").unwrap();
        assert_eq!(mode(&t.path().join("results/objects/ab/obj")), 0o600);
        b.write_new("results/objects/ab", "obj", b"two").unwrap();
        assert_eq!(
            std::fs::read(t.path().join("results/objects/ab/obj")).unwrap(),
            b"two"
        );
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(t.path().join("results/objects/ab"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["obj"]);
        b.mkdir_all("results/objects/ab").unwrap();
    }

    #[test]
    fn a_directory_replaced_by_a_link_between_creation_and_write_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("run");
        let outside = t.path().join("outside");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let b = Beneath::open(&run).unwrap();
        b.mkdir_all("results/staging/tok").unwrap();
        std::fs::remove_dir(run.join("results/staging/tok")).unwrap();
        symlink(&outside, run.join("results/staging/tok")).unwrap();
        assert!(
            b.write_new("results/staging/tok", "txn.json", b"{}")
                .is_err()
        );
        assert!(b.mkdir_all("results/staging/tok/x").is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[test]
    fn rename_and_remove_stay_beneath_and_never_follow_a_link() {
        let t = tempfile::tempdir().unwrap();
        let run = t.path().join("run");
        let outside = t.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "keep").unwrap();
        std::fs::create_dir_all(&run).unwrap();
        let b = Beneath::open(&run).unwrap();
        b.mkdir_all("a").unwrap();
        b.mkdir_all("b").unwrap();
        b.write_new("a", "f", b"x").unwrap();
        b.rename_within("a/f", "b/g").unwrap();
        assert_eq!(std::fs::read(run.join("b/g")).unwrap(), b"x");
        assert_eq!(b.kind("a/f").unwrap(), None);
        assert_eq!(b.kind("b/g").unwrap(), Some(EntryKind::File));

        symlink(&outside, run.join("b/link")).unwrap();
        assert!(b.rename_within("b/g", "b/link/g").is_err());
        // Removing a tree with a link inside removes the link, not its target.
        b.remove_tree("b").unwrap();
        assert_eq!(b.kind("b").unwrap(), None);
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
        b.remove("b/none").unwrap();
    }

    #[test]
    fn a_lock_is_exclusive_and_probing_writes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let b = Beneath::open(t.path()).unwrap();
        b.mkdir_all("results").unwrap();
        assert_eq!(b.probe_lock("results/.lock").unwrap(), LockState::Absent);
        assert!(!t.path().join("results/.lock").exists());
        let held = b.hold_lock("results", ".lock", Duration::ZERO).unwrap();
        assert_eq!(mode(&t.path().join("results/.lock")), 0o600);
        assert_eq!(b.probe_lock("results/.lock").unwrap(), LockState::Held);
        let err = b
            .hold_lock("results", ".lock", Duration::from_millis(50))
            .err()
            .unwrap();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        drop(held);
        assert_eq!(b.probe_lock("results/.lock").unwrap(), LockState::Free);
        let _again = b.hold_lock("results", ".lock", Duration::ZERO).unwrap();
    }
}

// The `results:` declaration rules V152 to V154 (`validate::results`).
mod validate_results {
    //! The `results:` declaration rules of the validator (V152 to V154).

    use apb_core::result_store::{INTERNAL_RUN_NAMES, MAX_DECLARATIONS};
    use apb_core::schema::Playbook;
    use apb_core::validate::{Severity, ValidationContext, snapshotted_nodes, validate};

    /// A playbook whose node `work` is the given flow mapping body, with a
    /// `results:` block appended.
    fn playbook(work: &str, results: &str) -> String {
        format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, {work}, results: {results} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
        )
    }

    const TASK: &str = "type: agent_task, prompt: hi, profile: x";

    fn codes(yaml: &str) -> Vec<(&'static str, Severity)> {
        let p = Playbook::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
        validate(&p, &ValidationContext::default())
            .issues
            .into_iter()
            .filter(|i| matches!(i.code, "V152" | "V153" | "V154"))
            .map(|i| (i.code, i.severity))
            .collect()
    }

    fn path_codes(path: &str, scope: &str) -> Vec<(&'static str, Severity)> {
        codes(&playbook(
            TASK,
            &format!("[{{ path: {path:?}, scope: {scope} }}]"),
        ))
    }

    #[test]
    fn v152_refuses_results_on_a_node_that_settles_no_work() {
        let wait = "type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 5";
        assert_eq!(
            codes(&playbook(wait, "[]")),
            vec![("V152", Severity::Error)]
        );
        let review = "type: human_review";
        assert_eq!(
            codes(&playbook(review, "[{ path: a.md }]")),
            vec![("V152", Severity::Error)]
        );
        for ok in [
            TASK,
            "type: script, script: \"true\", runner: sh",
            "type: agent_task, prompt: hi, profile: x, list: { items: { inline: [ { id: a } ] } }",
        ] {
            assert!(
                codes(&playbook(ok, "[{ path: report.md }]")).is_empty(),
                "{ok}"
            );
        }
    }

    #[test]
    fn v152_allows_a_finish_only_when_it_composes_an_answer() {
        let with = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success, prompt: sum up, profile: x, results: [] }\nedges:\n  - { from: start, to: done }\n";
        assert!(codes(with).is_empty());
        let without = with.replace(", prompt: sum up, profile: x", "");
        assert_eq!(codes(&without), vec![("V152", Severity::Error)]);
    }

    #[test]
    fn v153_accepts_exact_paths_and_last_segment_globs() {
        for (path, scope) in [
            ("report.md", "workspace"),
            ("docs/out/report.md", "workspace"),
            ("docs/*.md", "workspace"),
            ("notes/day-?.txt", "workspace"),
            (".github/summary.md", "workspace"),
            ("node-outputs/work.md", "run"),
            ("artifacts/*.json", "run"),
            ("résumé/отчёт.md", "workspace"),
        ] {
            assert!(path_codes(path, scope).is_empty(), "{path} ({scope})");
        }
    }

    #[test]
    fn v153_refuses_every_broken_path() {
        for (path, scope) in [
            ("", "workspace"),
            ("/etc/passwd", "workspace"),
            ("docs/**/x.md", "workspace"),
            ("**", "workspace"),
            ("*/report.md", "workspace"),
            ("d?r/report.md", "workspace"),
            ("./report.md", "workspace"),
            ("docs/../secret.md", "workspace"),
            ("..", "workspace"),
            ("docs//x.md", "workspace"),
            ("docs/", "workspace"),
            ("docs/[ab].md", "workspace"),
            ("docs/{a,b/c}.md", "workspace"),
            ("docs\\x.md", "workspace"),
            (".git/HEAD", "workspace"),
            (".git", "workspace"),
            (".apb/runs/x/events.jsonl", "workspace"),
            // A file system that folds case reads these as `.git` and
            // `.apb`.
            (".Git/config", "workspace"),
            (".APB/handoff/x", "workspace"),
            ("Results/reservation.json", "run"),
            ("DEFINITIONS/x.yaml", "run"),
        ] {
            assert_eq!(
                path_codes(path, scope),
                vec![("V153", Severity::Error)],
                "{path:?} ({scope})"
            );
        }
    }

    #[test]
    fn v153_refuses_every_internal_run_name_in_run_scope_only() {
        for name in INTERNAL_RUN_NAMES {
            for path in [name.to_string(), format!("{name}/x.json")] {
                assert_eq!(
                    path_codes(&path, "run"),
                    vec![("V153", Severity::Error)],
                    "{path}"
                );
                assert!(
                    path_codes(&path, "workspace").is_empty(),
                    "{path} is a plain workspace path"
                );
            }
        }
        // `.git` is only reserved in the workspace.
        assert!(path_codes(".git/x", "run").is_empty());
    }

    #[test]
    fn v154_caps_the_number_of_declarations() {
        let list = |n: usize| {
            let items: Vec<String> = (0..n).map(|i| format!("{{ path: f{i}.md }}")).collect();
            format!("[{}]", items.join(", "))
        };
        assert!(codes(&playbook(TASK, &list(MAX_DECLARATIONS))).is_empty());
        assert_eq!(
            codes(&playbook(TASK, &list(MAX_DECLARATIONS + 1))),
            vec![("V154", Severity::Error)]
        );
    }

    #[test]
    fn no_declaration_raises_nothing_and_snapshots_nothing() {
        let yaml = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: hi, profile: x }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";
        assert!(codes(yaml).is_empty());
        assert!(snapshotted_nodes(&Playbook::from_yaml(yaml).unwrap()).is_empty());
    }

    #[test]
    fn snapshotted_nodes_names_every_declaring_node_an_empty_list_included() {
        let p = Playbook::from_yaml(&playbook(TASK, "[]")).unwrap();
        assert_eq!(
            snapshotted_nodes(&p).into_iter().collect::<Vec<_>>(),
            vec!["work".to_string()]
        );
    }
}
