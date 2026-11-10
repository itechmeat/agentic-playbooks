//! The reverse links over temp projects, each test in its own child
//! process (its config dir and installation key are its own).

use super::*;
use serde_json::json;

fn child(name: &str, f: impl FnOnce(&Path)) {
    apb_core::process_mode::testing::in_plain_child(name, || {
        let cfg = tempfile::tempdir().unwrap();
        // SAFETY: a child process runs this one test on one thread.
        unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
        apb_core::run_origin::ensure_key().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".apb/runs")).unwrap();
        f(project.path());
    });
}

/// A run `id` started at `ts`, its journal last written `age` seconds
/// before now, with a launch record naming `starter` (verified or not).
fn seed(root: &Path, id: &str, ts: u64, age: u64, starter: Option<(&str, bool)>) {
    let dir = root.join(".apb/runs").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("events.jsonl");
    std::fs::write(
        &journal,
        format!(
            "{}\n",
            json!({"seq": 1, "ts": ts, "type": "run_started", "playbook": "fix", "version": "1.0.0"})
        ),
    )
    .unwrap();
    if let Some((run_id, verified)) = starter {
        let manifest = json!({
            "profiles": [], "node_bindings": {},
            "launch": {
                "kind": "start", "initiator": "run_step:cli", "policy": "allow", "sources": [],
                "started_by": { "run_id": run_id, "node": "triage", "attempt": 2, "verified": verified, "chain": 1 }
            }
        });
        std::fs::write(dir.join("manifest.yaml"), manifest.to_string()).unwrap();
    }
    apb_core::run_origin::stamp(&dir, id).unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
    std::fs::File::options()
        .append(true)
        .open(&journal)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Newest first, unverified records and other starters left out, at most
/// 50, and a scan of every directory is complete.
#[test]
fn started_runs_are_the_verified_ones_newest_first() {
    child(
        "run_view::started::tests::started_runs_are_the_verified_ones_newest_first",
        |root| {
            seed(root, "old", 1, 5000, None);
            seed(root, "t", 10, 4000, None);
            for i in 0..60u64 {
                seed(
                    root,
                    &format!("f-{i:02}"),
                    100 + i,
                    3000 - i,
                    Some(("t", true)),
                );
            }
            seed(root, "forged", 300, 10, Some(("t", false)));
            seed(root, "other", 301, 9, Some(("u", true)));
            let s = started_runs(root, "t");
            assert!(!s.truncated);
            assert_eq!(s.runs.len(), STARTED_RUNS_MAX);
            assert_eq!(s.runs[0].run_id, "f-59");
            assert_eq!(s.runs[0].node.as_deref(), Some("triage"));
            assert_eq!(s.runs[0].attempt, Some(2));
            assert!(s.runs.iter().all(|r| r.run_id.starts_with("f-")));
            assert_eq!(started_runs(root, "u").runs.len(), 1);
            assert_eq!(started_runs(root, "nobody").runs.len(), 0);
        },
    );
}

/// 520 runs newer than the starter and the started run among the oldest:
/// the scan stops at its bound without it, and says so.
#[test]
fn a_scan_that_stops_at_its_bound_is_truncated() {
    child(
        "run_view::started::tests::a_scan_that_stops_at_its_bound_is_truncated",
        |root| {
            seed(root, "t", 10, 9000, None);
            seed(root, "f-first", 11, 8000, Some(("t", true)));
            for i in 0..520u64 {
                seed(root, &format!("n-{i:03}"), 20 + i, 7000 - i, None);
            }
            let s = started_runs(root, "t");
            assert!(s.truncated);
            assert!(s.runs.is_empty(), "{s:?}");
            assert!(started_runs_line(&s).contains("more may exist beyond the newest 500 runs"));
        },
    );
}

/// A run started before the starter but written recently sits among the
/// newest 500 journals while the started run is beyond them: journal
/// order says nothing about start times, so the scan still says it may
/// have missed runs.
#[test]
fn an_old_run_written_late_does_not_hide_the_truncation() {
    child(
        "run_view::started::tests::an_old_run_written_late_does_not_hide_the_truncation",
        |root| {
            seed(root, "t", 10, 9000, None);
            seed(root, "f-first", 11, 8000, Some(("t", true)));
            seed(root, "old-busy", 1, 5, None);
            for i in 0..510u64 {
                seed(root, &format!("n-{i:03}"), 20 + i, 7000 - i, None);
            }
            let s = started_runs(root, "t");
            assert!(s.runs.is_empty(), "{s:?}");
            assert!(s.truncated, "the omitted started run went unreported");
        },
    );
}

#[test]
fn the_starter_line_names_what_is_known() {
    let by = StartedBy {
        run_id: "t-1".into(),
        workspace_id: None,
        node: Some("triage".into()),
        attempt: Some(2),
        host_task: None,
        verified: true,
        chain: 1,
    };
    assert_eq!(
        started_by_line(&by, None),
        "run t-1 (node triage, attempt 2)"
    );
    let forged = StartedBy {
        verified: false,
        node: None,
        attempt: None,
        ..by
    };
    assert_eq!(started_by_line(&forged, None), "run t-1, not verified");
}
