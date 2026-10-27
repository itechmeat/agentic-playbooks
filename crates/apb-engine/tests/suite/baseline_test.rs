//! The baseline check end to end: the before snapshot is taken once, ahead
//! of the first node that can act, and survives pauses, crashes and resumes;
//! the after snapshot and the comparisons are journaled at the finish; an
//! unreadable source, a changed tree, a tree that cannot be checked or a
//! changed input reads as unknown, never as a value; `enforce` fails the
//! run on anything but met.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventPayload, read_all, read_journal};
use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
use apb_engine::scheduler::{RunOptions, RunResult, resume, run};
use apb_engine::state::RunStatus;

use crate::common;

const DEADLINE: Duration = Duration::from_secs(60);
const POLL_STEP: Duration = Duration::from_millis(20);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < DEADLINE, "timed out waiting for: {what}");
        std::thread::sleep(POLL_STEP);
    }
}

/// start -> note (a prompt, which cannot act) -> work (a script) -> done.
fn linear(id: &str, head: &str) -> String {
    format!(
        r#"
schema: 2
id: {id}
name: Baseline
version: 1.0.0
{head}
nodes:
  - {{ id: start, type: start }}
  - {{ id: note, type: prompt, prompt: "look first" }}
  - {{ id: work, type: script, script: "scripts/work.sh", runner: sh }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: note }}
  - {{ from: note, to: work }}
  - {{ from: work, to: done }}
"#
    )
}

/// A script check over `state.json` in the tree, observing `count` against
/// `equals: 0`.
fn count_check(enforce: bool) -> String {
    format!(
        "baseline_check:\n  enforce: {enforce}\n  checks:\n    - id: deals\n      script: scripts/count.sh\n      observe:\n        - {{ id: count, type: number, path: count, expect: {{ equals: 0 }} }}"
    )
}

/// Records each read in the run dir (outside the tree) and prints the state.
const COUNT_SH: &str =
    "echo \"$APB_BASELINE_PHASE\" >> \"$APB_RUN_DIR/reads.log\"\ncat state.json\n";
/// Moves the tree to the next state, waiting while `slow` exists.
const WORK_SH: &str = "trap 'exit 1' TERM\ntouch work_running\nwhile [ -f slow ]; do sleep 0.05; done\ncp next.json state.json\n";

/// Runs git in `root`, failing the test when it fails.
fn git(root: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

/// Seeds playbook `id` in a git work tree with one commit: a script or a
/// project check reads only a tree it can check before and after.
fn seed(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    git(root, &["init", "-q"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "seed"]);
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    for (name, body) in scripts {
        fs::write(vdir.join("scripts").join(name), body).unwrap();
    }
}

/// Seeds the count playbook with the tree going from `before` to `after`.
fn seed_count(root: &Path, id: &str, enforce: bool, before: u32, after: u32) {
    seed(
        root,
        id,
        &linear(id, &count_check(enforce)),
        &[("count.sh", COUNT_SH), ("work.sh", WORK_SH)],
    );
    fs::write(root.join("state.json"), format!("{{\"count\": {before}}}")).unwrap();
    fs::write(root.join("next.json"), format!("{{\"count\": {after}}}")).unwrap();
}

fn run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    root.join(".apb/runs").join(run_id)
}

fn events(root: &Path, run_id: &str) -> Vec<Event> {
    read_all(&run_dir(root, run_id)).unwrap()
}

fn run_ok(root: &Path, id: &str) -> (RunResult, Vec<Event>) {
    let res = run(root, id, None, RunOptions::default()).unwrap();
    let ev = events(root, &res.run_id);
    (res, ev)
}

/// `(phase, status, reason)` of every snapshot.
fn observed(events: &[Event]) -> Vec<(String, String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BaselineObserved {
                phase,
                status,
                reason,
                ..
            } => Some((phase.clone(), status.clone(), reason.clone())),
            _ => None,
        })
        .collect()
}

fn befores(events: &[Event]) -> usize {
    observed(events).iter().filter(|o| o.0 == "before").count()
}

/// `(observation, verdict, regressed, detail)` of every comparison.
fn compared(events: &[Event]) -> Vec<(String, String, Option<bool>, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BaselineCompared {
                observation,
                verdict,
                regressed,
                detail,
                ..
            } => Some((
                observation.clone(),
                verdict.clone(),
                *regressed,
                detail.clone().unwrap_or_default(),
            )),
            _ => None,
        })
        .collect()
}

fn reads(root: &Path, run_id: &str) -> Vec<String> {
    fs::read_to_string(run_dir(root, run_id).join("reads.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn position(events: &[Event], pred: impl Fn(&EventPayload) -> bool) -> usize {
    events.iter().position(|e| pred(&e.payload)).expect("event")
}

fn run_errors(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunError { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

/// Cuts the journal right after `work`'s `node_started`, as a driver that
/// died there would leave it; `keep` filters the kept lines.
fn crash_after_work_started(root: &Path, run_id: &str, keep: impl Fn(&str) -> bool) {
    let path = run_dir(root, run_id).join("events.jsonl");
    let raw = fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in raw.lines() {
        if keep(line) {
            out.push_str(line);
            out.push('\n');
        }
        if line.contains("\"type\":\"node_started\"") && line.contains("\"node\":\"work\"") {
            break;
        }
    }
    fs::write(&path, out).unwrap();
}

#[test]
fn the_before_snapshot_is_taken_once_ahead_of_the_first_node_that_can_act() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "b1", false, 37, 20);
    let (res, ev) = run_ok(root, "b1");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let phases: Vec<(String, String)> = observed(&ev).into_iter().map(|o| (o.0, o.1)).collect();
    assert_eq!(
        phases,
        [
            ("before".to_string(), "observed".to_string()),
            ("after".to_string(), "observed".to_string())
        ]
    );
    let before = position(
        &ev,
        |p| matches!(p, EventPayload::BaselineObserved { phase, .. } if phase == "before"),
    );
    let note = position(
        &ev,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "note"),
    );
    let work = position(
        &ev,
        |p| matches!(p, EventPayload::NodeStarted { node, .. } if node == "work"),
    );
    assert!(
        note < before && before < work,
        "note {note}, before {before}, work {work}"
    );
    // Both snapshots come before the finish node's checkpoint.
    let compared_at = position(&ev, |p| matches!(p, EventPayload::BaselineCompared { .. }));
    let finish = position(
        &ev,
        |p| matches!(p, EventPayload::NodeFinished { node, .. } if node == "done"),
    );
    assert!(compared_at < finish);
    assert_eq!(reads(root, &res.run_id), ["before", "after"]);
}

/// The crm-cleanup shape: 37 open deals without an owner, 20 after a
/// partial failure. Not met, the change is visible, and a report-only
/// baseline leaves the run's outcome alone.
#[test]
fn a_partial_cleanup_reads_not_met_with_the_change_visible() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "crm", false, 37, 20);
    let (res, ev) = run_ok(root, "crm");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_eq!(
        compared(&ev),
        [(
            "count".into(),
            "not_met".into(),
            Some(false),
            "37 -> 20".into()
        )]
    );
    let rule = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::BaselineCompared { rule, enforced, .. } => {
                Some((rule.clone(), *enforced))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(rule, ("equals 0".to_string(), false));
}

#[test]
fn enforce_fails_a_run_on_not_met_or_unknown_and_report_only_does_not() {
    // Not met under enforce.
    let dir = tempfile::tempdir().unwrap();
    seed_count(dir.path(), "e1", true, 37, 20);
    let (res, ev) = run_ok(dir.path(), "e1");
    assert_eq!(res.outcome, RunStatus::Failed);
    assert!(
        run_errors(&ev)
            .iter()
            .any(|r| r.contains("baseline check `deals` observation `count` not_met: 37 -> 20")),
        "{:?}",
        run_errors(&ev)
    );
    // Met under enforce.
    let dir = tempfile::tempdir().unwrap();
    seed_count(dir.path(), "e2", true, 37, 0);
    assert_eq!(run_ok(dir.path(), "e2").0.outcome, RunStatus::Succeeded);
    // Unknown (the check script fails) under enforce, then report only.
    for (enforce, want) in [(true, RunStatus::Failed), (false, RunStatus::Succeeded)] {
        let dir = tempfile::tempdir().unwrap();
        seed_count(dir.path(), "e3", enforce, 37, 0);
        let vdir = dir.path().join(".apb/playbooks/e3/1.0.0/scripts");
        fs::write(vdir.join("count.sh"), "echo broken\nexit 2\n").unwrap();
        let (res, ev) = run_ok(dir.path(), "e3");
        assert_eq!(res.outcome, want, "enforce {enforce}");
        assert_eq!(compared(&ev)[0].1, "unknown");
        assert_eq!(observed(&ev)[0].1, "unknown");
    }
}

/// A `--now` pause and a drain pause between the two snapshots: the resume
/// reuses the journaled before snapshot and never reads it again.
#[test]
fn a_pause_between_the_snapshots_reuses_the_before_snapshot() {
    let _lock = common::env_lock();
    let _grace = common::EnvVar::set("APB_PAUSE_GRACE_MS", "5000");
    for mode in [PauseMode::Now, PauseMode::Drain] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        seed_count(&root, "p", false, 3, 0);
        fs::write(root.join("slow"), "").unwrap();
        let (tx, rx) = mpsc::channel();
        let r = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "p", None, RunOptions::default()));
        });
        poll_until("the work node to start", || {
            root.join("work_running").is_file().then_some(())
        });
        let run_id = fs::read_dir(root.join(".apb/runs"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .find(|n| n.starts_with("p-"))
            .unwrap();
        request_pause(
            &root,
            &run_id,
            &PauseRequest {
                by: Some(PauseBy::Operator),
                mode,
                ..PauseRequest::default()
            },
        )
        .unwrap();
        if mode == PauseMode::Drain {
            fs::remove_file(root.join("slow")).unwrap();
        }
        let paused: Result<RunResult, EngineError> = rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(paused.unwrap().outcome, RunStatus::Paused, "{mode:?}");
        let _ = fs::remove_file(root.join("slow"));
        let res = resume(&root, &run_id, None).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded, "{mode:?}");
        let ev = events(&root, &run_id);
        assert_eq!(befores(&ev), 1, "{mode:?}");
        assert_eq!(reads(&root, &run_id), ["before", "after"], "{mode:?}");
        assert_eq!(compared(&ev)[0].1, "met", "{mode:?}");
    }
}

#[test]
fn a_crash_after_an_acting_node_started_does_not_retake_the_before_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "c", false, 5, 0);
    let (first, _) = run_ok(root, "c");
    crash_after_work_started(root, &first.run_id, |_| true);
    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &first.run_id);
    assert_eq!(befores(&ev), 1);
    // The first run read before and after; the resume read only after.
    assert_eq!(reads(root, &first.run_id), ["before", "after", "after"]);
    assert_eq!(compared(&ev).last().unwrap().1, "met");
}

/// A journal an older apb wrote: the playbook names a baseline, but no
/// before snapshot exists and an acting node already started.
#[test]
fn a_journal_without_a_before_snapshot_reads_missed_and_compares_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "m", false, 5, 0);
    let (first, _) = run_ok(root, "m");
    crash_after_work_started(root, &first.run_id, |l| !l.contains("\"type\":\"baseline_"));
    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &first.run_id);
    let obs = observed(&ev);
    assert_eq!(obs[0].0, "before");
    assert_eq!(obs[0].1, "missed");
    let c = &compared(&ev)[0];
    assert_eq!((c.1.as_str(), c.2), ("unknown", None));
    assert!(c.3.contains("before snapshot"), "{c:?}");
    // The resume never read a before snapshot.
    assert_eq!(reads(root, &first.run_id), ["before", "after", "after"]);
}

#[test]
fn a_check_that_changes_the_git_tree_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "g", false, 5, 0);
    let vdir = root.join(".apb/playbooks/g/1.0.0/scripts");
    fs::write(
        vdir.join("count.sh"),
        "echo touched > touched.txt\ncat state.json\n",
    )
    .unwrap();
    git(root, &["add", "state.json", "next.json"]);
    git(root, &["commit", "-q", "-m", "state"]);
    let (_, ev) = run_ok(root, "g");
    let obs = observed(&ev);
    assert_eq!(obs[0].1, "unknown");
    assert_eq!(
        obs[0].2.as_deref(),
        Some("the check changed the working tree")
    );
    assert_eq!(compared(&ev)[0].1, "unknown");
}

/// The digest covers what defines the command, so the comparison is unknown
/// whatever the runner made of either side (npm need not be installed).
#[test]
fn a_test_definition_edited_mid_run_makes_the_comparison_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let head =
        "baseline_check:\n  checks:\n    - id: tests\n      project_checks: { only: [test] }";
    seed(
        root,
        "pc",
        &linear("pc", head),
        &[(
            "work.sh",
            "printf '%s' '{\"scripts\":{\"test\":\"exit 1\"}}' > package.json\n",
        )],
    );
    fs::write(
        root.join("package.json"),
        r#"{"scripts":{"test":"exit 0"}}"#,
    )
    .unwrap();
    let (_, ev) = run_ok(root, "pc");
    let digests: Vec<String> = ev
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BaselineObserved { input_digest, .. } => Some(input_digest.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(digests.len(), 2);
    assert_ne!(digests[0], digests[1]);
    let c = &compared(&ev)[0];
    assert_eq!((c.0.as_str(), c.1.as_str()), ("npm_test", "unknown"));
    assert!(c.3.contains("input changed"), "{c:?}");
}

/// The dependency-security-fix shape: `cargo test` already had a failing
/// test; the run adds a new one. Worse than the baseline under `enforce`
/// fails the run, naming the new test.
#[test]
fn a_new_failing_cargo_test_under_enforce_fails_the_run_naming_it() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let head = "baseline_check:\n  enforce: true\n  checks:\n    - id: tests\n      project_checks: { only: [test] }";
    seed(
        root,
        "dep",
        &linear("dep", head),
        &[(
            "work.sh",
            "printf '\\n#[test]\\nfn new_bug() { assert!(false) }\\n' >> src/lib.rs\n",
        )],
    );
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"baseline_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "#[test]\nfn fine() {}\n#[test]\nfn old_bug() { assert!(false) }\n",
    )
    .unwrap();
    // cargo writes its lockfile and build output: ignored, as a project
    // would, so the check leaves the tree as it found it.
    fs::write(root.join(".gitignore"), "Cargo.lock\n").unwrap();
    let target = tempfile::tempdir().unwrap();
    let out = {
        let _target = common::EnvVar::set("CARGO_TARGET_DIR", target.path());
        // CI often forces color on: the check must read the failures anyway.
        let _color = common::EnvVar::set("CARGO_TERM_COLOR", "always");
        run(root, "dep", None, RunOptions::default())
    };
    let res = out.unwrap();
    let ev = events(root, &res.run_id);
    assert_eq!(res.outcome, RunStatus::Failed);
    let c = &compared(&ev)[0];
    assert_eq!(
        (c.0.as_str(), c.1.as_str(), c.2),
        ("cargo_test", "not_met", Some(true))
    );
    assert!(
        c.3.contains("new failing test(s): unittests src/lib.rs [baseline_fixture]: new_bug"),
        "{c:?}"
    );
    assert!(
        run_errors(&ev).iter().any(|r| r.contains("new_bug")),
        "{:?}",
        run_errors(&ev)
    );
}

/// A resume `from_node` runs an acting node again, so the old after
/// snapshot is stale: a new one is taken and the comparisons journaled again.
#[test]
fn a_resume_from_a_node_retakes_the_after_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "r", true, 37, 20);
    let (first, ev) = run_ok(root, "r");
    assert_eq!(first.outcome, RunStatus::Failed);
    assert_eq!(compared(&ev)[0].1, "not_met");
    fs::write(root.join("next.json"), "{\"count\": 0}").unwrap();
    let res = resume(root, &first.run_id, Some("work")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(root, &first.run_id);
    let afters = observed(&ev).iter().filter(|o| o.0 == "after").count();
    assert_eq!((befores(&ev), afters), (1, 2));
    let verdicts: Vec<String> = compared(&ev).into_iter().map(|c| c.1).collect();
    assert_eq!(verdicts, ["not_met", "met"]);
    assert_eq!(reads(root, &first.run_id), ["before", "after", "after"]);
}

/// A supervisor patch that changes `baseline_check` is refused once a node
/// that can act has started, and applies before that.
#[test]
fn a_patch_changing_the_baseline_after_an_acting_node_started_is_refused() {
    use apb_engine::control::Control;
    use apb_engine::scheduler::{
        RunMode, drive_prepared, post_supervisor_command, prepare_supervised_background,
    };
    let patched = |early: bool| -> Vec<EventPayload> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        seed_count(&root, "pt", false, 3, 0);
        let prepared = prepare_supervised_background(
            &root,
            "pt",
            None,
            RunOptions {
                mode: RunMode::Supervised,
                ..Default::default()
            },
        )
        .unwrap();
        let run_id = prepared.run_id().to_string();
        let version = apb_core::versioning::create_patch_version(
            &root,
            "pt",
            "1.0.0",
            &linear("pt", &count_check(false).replace("equals: 0", "equals: 1")),
            &run_id,
            "workaround",
        )
        .unwrap();
        let patch = Control::Patch {
            version,
            classification: "workaround".into(),
            continue_from: "work".into(),
        };
        if early {
            post_supervisor_command(&root, &run_id, patch).unwrap();
            drive_prepared(&root, prepared).unwrap();
        } else {
            fs::write(root.join("slow"), "").unwrap();
            let r = root.clone();
            let handle = std::thread::spawn(move || drive_prepared(&r, prepared));
            poll_until("the work node to start", || {
                root.join("work_running").is_file().then_some(())
            });
            post_supervisor_command(&root, &run_id, patch).unwrap();
            fs::remove_file(root.join("slow")).unwrap();
            handle.join().unwrap().unwrap();
        }
        events(&root, &run_id)
            .into_iter()
            .map(|e| e.payload)
            .collect()
    };
    let late = patched(false);
    let reason = late
        .iter()
        .find_map(|e| match e {
            EventPayload::PatchRejected { reason } => Some(reason.clone()),
            _ => None,
        })
        .expect("the late patch is refused");
    assert!(reason.contains("baseline_check"), "{reason}");
    let early = patched(true);
    assert!(
        early
            .iter()
            .any(|e| matches!(e, EventPayload::PatchApplied { .. })),
        "{early:?}"
    );
}

/// A `continued_from` successor inherits its predecessor's before snapshot:
/// the state before the whole piece of work, not before the second half.
#[test]
fn a_continued_run_inherits_the_before_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "cf", false, 37, 20);
    let (first, _) = run_ok(root, "cf");
    fs::write(root.join("next.json"), "{\"count\": 0}").unwrap();
    let res = run(
        root,
        "cf",
        None,
        RunOptions {
            continued_from: Some(first.run_id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let ev = events(root, &res.run_id);
    let inherited = ev
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::BaselineObserved {
                phase,
                inherited_from,
                ..
            } if phase == "before" => Some(inherited_from.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(inherited.as_deref(), Some(first.run_id.as_str()));
    assert_eq!(reads(root, &res.run_id), ["after"]);
    let c = &compared(&ev)[0];
    assert_eq!((c.1.as_str(), c.3.as_str()), ("met", "37 -> 0"));
}

/// A predecessor without a before snapshot of the check leaves the
/// successor's before unknown: a fresh read now would describe the tree
/// halfway through the work, and the comparison would look observed.
#[test]
fn a_continued_run_whose_predecessor_has_no_before_snapshot_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "cu", false, 37, 20);
    let (first, _) = run_ok(root, "cu");
    let path = run_dir(root, &first.run_id).join("events.jsonl");
    let raw = fs::read_to_string(&path).unwrap();
    let kept: String = raw
        .lines()
        .filter(|l| !l.contains("\"type\":\"baseline_"))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&path, kept).unwrap();
    let res = run(
        root,
        "cu",
        None,
        RunOptions {
            continued_from: Some(first.run_id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let ev = events(root, &res.run_id);
    let before = &observed(&ev)[0];
    assert_eq!(
        (before.0.as_str(), before.1.as_str()),
        ("before", "unknown"),
        "{before:?}"
    );
    assert!(
        before
            .2
            .as_deref()
            .unwrap_or("")
            .contains("has no before snapshot of this check"),
        "{before:?}"
    );
    assert_eq!(compared(&ev)[0].1, "unknown");
    assert_eq!(reads(root, &res.run_id), ["after"]);
}

/// An older binary that does not know the baseline events skips them: each
/// is followed by a checkpoint, so the journal still reads.
#[test]
fn an_older_binary_skips_the_baseline_events_up_to_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_count(root, "old", false, 3, 0);
    let (res, _) = run_ok(root, "old");
    let path = run_dir(root, &res.run_id).join("events.jsonl");
    let raw = fs::read_to_string(&path).unwrap();
    let renamed = raw
        .replace(
            "\"type\":\"baseline_observed\"",
            "\"type\":\"baseline_observed_v99\"",
        )
        .replace(
            "\"type\":\"baseline_compared\"",
            "\"type\":\"baseline_compared_v99\"",
        );
    fs::write(&path, &renamed).unwrap();
    assert!(read_all(&run_dir(root, &res.run_id)).is_ok());
    assert_eq!(
        read_journal(&run_dir(root, &res.run_id))
            .unwrap()
            .unknown
            .len(),
        3
    );
    // The before snapshot alone, with no checkpoint after it, is refused.
    let cut: String = renamed
        .lines()
        .take_while(|l| {
            !l.contains("\"type\":\"node_started\"") || !l.contains("\"node\":\"work\"")
        })
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&path, cut).unwrap();
    assert!(read_all(&run_dir(root, &res.run_id)).is_err());
}

// --- connector sources --------------------------------------------------

const CONNECTOR: &str = r#"
name: crm
version: 0.1.0
account_fields:
  - name: base_url
    required: true
  - name: token
    secret: true
functions:
  - name: broken
    description: A read that fails
    read_only: true
    mock: { status: 500, body: { error: down } }
  - name: paged
    description: A read with more pages
    read_only: true
    mock: { status: 200, body: { totalCount: 0, pageInfo: { hasNextPage: true } } }
  - name: big
    description: A read whose body is over the cap
    read_only: true
    method: GET
    url: "{{account.base_url}}/big"
  - name: slow
    description: A read that never answers in time
    read_only: true
    method: GET
    url: "{{account.base_url}}/slow"
    timeout_sec: 60
"#;

/// Runs a playbook whose one baseline check calls `function` on the `crm`
/// fixture connector with `base_url`.
fn connector_run(function: &str, base_url: &str) -> Vec<Event> {
    connector_run_with(function, base_url, "", &[])
}

/// [`connector_run`] with `extra` lines in the check, and `env` set while
/// the run is driven.
fn connector_run_with(
    function: &str,
    base_url: &str,
    extra: &str,
    env: &[(&'static str, &str)],
) -> Vec<Event> {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let _env: Vec<common::EnvVar> = env.iter().map(|(k, v)| common::EnvVar::set(k, v)).collect();
    let cdir = cfg.path().join("connectors/crm");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(cdir.join("connector.yaml"), CONNECTOR).unwrap();
    let head = format!(
        "baseline_check:\n  checks:\n    - id: deals\n      connector: {{ name: crm, function: {function} }}\n      complete_when: {{ path: pageInfo.hasNextPage, equals: false }}\n{extra}      observe:\n        - {{ id: count, type: number, path: totalCount, expect: {{ equals: 0 }} }}"
    );
    let yaml = linear("con", &head);
    seed(root, "con", &yaml, &[("work.sh", "true\n")]);
    let acct = apb_core::connector::config::project_config_path(root, "crm");
    fs::create_dir_all(acct.parent().unwrap()).unwrap();
    fs::write(
        &acct,
        format!("accounts:\n  - name: main\n    base_url: {base_url}\n"),
    )
    .unwrap();
    let pb = apb_core::schema::Playbook::from_yaml(&yaml).unwrap();
    let resolved = apb_core::connector::resolve::resolve_playbook(root, &pb).unwrap();
    let mut opts = RunOptions::default();
    for (name, c) in &resolved.connectors {
        opts.expected_connectors
            .insert(name.clone(), c.loaded.digest.clone());
        for a in &c.accounts {
            opts.expected_connector_accounts.insert(
                format!("{name}/{}", a.name),
                apb_core::connector::config::account_digest(a),
            );
        }
    }
    let res = run(root, "con", None, opts).unwrap();
    events(root, &res.run_id)
}

fn assert_never_a_value(ev: &[Event], why: &str) {
    let obs = observed(ev);
    assert_eq!(obs[0].1, "unknown", "{obs:?}");
    assert!(obs[0].2.as_deref().unwrap_or("").contains(why), "{obs:?}");
    let c = &compared(ev)[0];
    assert_eq!((c.1.as_str(), c.2), ("unknown", None), "{c:?}");
}

#[test]
fn a_connector_read_that_fails_is_unknown() {
    let ev = connector_run("broken", "http://127.0.0.1:9");
    assert_never_a_value(&ev, "the call failed");
}

#[test]
fn a_connector_read_that_is_not_complete_is_unknown_never_zero() {
    let ev = connector_run("paged", "http://127.0.0.1:9");
    assert_never_a_value(&ev, "is not complete");
}

#[test]
fn a_connector_response_cut_at_the_cap_is_unknown() {
    let body = format!(
        "{{\"totalCount\": 0, \"pad\": \"{}\"}}",
        "x".repeat(1024 * 1024 + 16)
    );
    let server = common::spawn_http(200, "OK", &[], body);
    let ev = connector_run("big", &server.base_url);
    assert_never_a_value(&ev, "incomplete_response");
}

/// The check's `timeout_seconds` bounds a connector read as well as the
/// function's own `timeout_sec`: a service that never answers is unknown
/// after the check's bound, not the function's.
#[test]
fn a_connector_read_is_bounded_by_the_checks_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            std::thread::sleep(Duration::from_secs(30));
            drop(stream);
        }
    });
    let started = Instant::now();
    let ev = connector_run_with("slow", &base_url, "      timeout_seconds: 1\n", &[]);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert_never_a_value(&ev, "the call failed (timeout)");
}

// --- the working tree and the environment ---------------------------------

/// A playbook whose two checks run in the tree: a script that reports
/// whether `APB_TEST_CRM_TOKEN` reached it, and the project's `make test`.
fn tree_checks(root: &Path, id: &str, makefile: &str) {
    let head = "baseline_check:\n  checks:\n    - id: env\n      script: scripts/env.sh\n      observe:\n        - { id: leaked, type: number, path: n, expect: { equals: 0 } }\n    - id: tests\n      project_checks: { only: [test] }";
    seed(
        root,
        id,
        &linear(id, head),
        &[
            (
                "env.sh",
                "if [ -n \"$APB_TEST_CRM_TOKEN\" ]; then echo '{\"n\": 1}'; else echo '{\"n\": 0}'; fi\n",
            ),
            ("work.sh", "true\n"),
        ],
    );
    fs::write(root.join("Makefile"), makefile).unwrap();
}

/// The value an observation of the before snapshot of `check` read.
fn before_value(ev: &[Event], check: &str, observation: &str) -> apb_core::baseline::Observation {
    ev.iter()
        .find_map(|e| match &e.payload {
            EventPayload::BaselineObserved {
                check: c,
                phase,
                observations,
                ..
            } if c == check && phase == "before" => observations.get(observation).cloned(),
            _ => None,
        })
        .expect("the observation")
}

/// A connector secret exported in apb's environment never reaches a
/// baseline script or a project command, as it never reaches an agent.
#[test]
fn baseline_scripts_and_project_commands_never_see_a_connector_secret() {
    use apb_core::baseline::{Observation, ObservedValue};
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    let _token = common::EnvVar::set("APB_TEST_CRM_TOKEN", "x");
    let cdir = cfg.path().join("connectors/crm");
    fs::create_dir_all(&cdir).unwrap();
    fs::write(cdir.join("connector.yaml"), CONNECTOR).unwrap();
    tree_checks(root, "env", "test:\n\ttest -z \"$$APB_TEST_CRM_TOKEN\"\n");
    let acct = apb_core::connector::config::project_config_path(root, "crm");
    fs::create_dir_all(acct.parent().unwrap()).unwrap();
    fs::write(
        &acct,
        "accounts:\n  - name: main\n    base_url: http://127.0.0.1:9\n    token: \"{{env.APB_TEST_CRM_TOKEN}}\"\n",
    )
    .unwrap();
    let (_, ev) = run_ok(root, "env");
    assert_eq!(
        before_value(&ev, "env", "leaked"),
        Observation::value(ObservedValue::Number(0.0))
    );
    assert_eq!(
        before_value(&ev, "tests", "make_test.status"),
        Observation::value(ObservedValue::Text("passed".into()))
    );
}

/// A project command that writes into the tree reads unknown, as a script
/// that does.
#[test]
fn a_project_check_that_changes_the_tree_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tree_checks(root, "pt", "test:\n\ttouch made.txt\n");
    let (_, ev) = run_ok(root, "pt");
    let obs = observed(&ev);
    let tests: Vec<_> = obs.iter().filter(|o| o.2.is_some()).collect();
    assert!(
        tests.iter().any(
            |o| o.0 == "before" && o.2.as_deref() == Some("the check changed the working tree")
        ),
        "{obs:?}"
    );
    let c = compared(&ev);
    let row = c.iter().find(|c| c.0 == "make_test").unwrap();
    assert_eq!(row.1, "unknown", "{row:?}");
}

/// A tree that is not a git work tree cannot be checked for changes, so a
/// script or a project command run in it reads unknown.
#[test]
fn a_tree_that_cannot_be_checked_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    tree_checks(root, "ng", "test:\n\ttrue\n");
    fs::remove_dir_all(root.join(".git")).unwrap();
    let (_, ev) = run_ok(root, "ng");
    let obs = observed(&ev);
    assert_eq!(obs.len(), 4, "{obs:?}");
    for o in &obs {
        assert_eq!(
            (o.1.as_str(), o.2.as_deref()),
            ("unknown", Some("the working tree could not be checked")),
            "{obs:?}"
        );
    }
    assert!(compared(&ev).iter().all(|c| c.1 == "unknown"));
}

/// A playbook whose nodes never act reaches the finish without a before
/// snapshot: the finish takes it right before the after one, so the
/// comparison is real.
#[test]
fn a_run_whose_nodes_never_act_compares_two_real_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let yaml = format!(
        r#"
schema: 2
id: quiet
name: Quiet
version: 1.0.0
{}
nodes:
  - {{ id: start, type: start }}
  - {{ id: note, type: prompt, prompt: "look only" }}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: note }}
  - {{ from: note, to: done }}
"#,
        count_check(true)
    );
    seed(root, "quiet", &yaml, &[("count.sh", COUNT_SH)]);
    fs::write(root.join("state.json"), "{\"count\": 0}").unwrap();
    let (res, ev) = run_ok(root, "quiet");
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let phases: Vec<(String, String)> = observed(&ev).into_iter().map(|o| (o.0, o.1)).collect();
    assert_eq!(
        phases,
        [
            ("before".to_string(), "observed".to_string()),
            ("after".to_string(), "observed".to_string())
        ]
    );
    assert_eq!(
        compared(&ev),
        [("count".into(), "met".into(), Some(false), "0 -> 0".into())]
    );
    assert_eq!(reads(root, &res.run_id), ["before", "after"]);
}

/// A driver that died between two `baseline_compared` events of one check:
/// the resume journals the missing comparison and judges the whole set, so
/// an enforced run whose unjournaled observation is not met still fails.
#[test]
fn a_resume_completes_a_partial_set_of_comparisons_before_judging() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let head = "baseline_check:\n  enforce: true\n  checks:\n    - id: deals\n      script: scripts/count.sh\n      observe:\n        - { id: count, type: number, path: count, expect: { equals: 0 } }\n        - { id: strict, type: number, path: count, expect: { at_most: -1 } }";
    seed(
        root,
        "pc",
        &linear("pc", head),
        &[("count.sh", COUNT_SH), ("work.sh", WORK_SH)],
    );
    fs::write(root.join("state.json"), "{\"count\": 3}").unwrap();
    fs::write(root.join("next.json"), "{\"count\": 0}").unwrap();
    let (first, ev) = run_ok(root, "pc");
    assert_eq!(first.outcome, RunStatus::Failed);
    assert_eq!(compared(&ev).len(), 2);
    // Cut the journal right after the first comparison.
    let path = run_dir(root, &first.run_id).join("events.jsonl");
    let raw = fs::read_to_string(&path).unwrap();
    let mut kept = String::new();
    for line in raw.lines() {
        kept.push_str(line);
        kept.push('\n');
        if line.contains("\"type\":\"baseline_compared\"") {
            break;
        }
    }
    fs::write(&path, kept).unwrap();
    let res = resume(root, &first.run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Failed);
    let ev = events(root, &first.run_id);
    let verdicts: Vec<(String, String)> = compared(&ev).into_iter().map(|c| (c.0, c.1)).collect();
    assert_eq!(
        verdicts,
        [
            ("count".to_string(), "met".to_string()),
            ("strict".to_string(), "not_met".to_string())
        ]
    );
    assert!(
        run_errors(&ev)
            .iter()
            .any(|r| r.contains("observation `strict` not_met")),
        "{:?}",
        run_errors(&ev)
    );
    // The after snapshot was journaled before the cut: it is not read again.
    assert_eq!(reads(root, &first.run_id), ["before", "after"]);
}

/// A `--now` pause while the finish reads the after snapshot interrupts the
/// read like any node work: nothing of it is journaled, the run pauses, and
/// the resume takes the after snapshot again.
#[test]
fn a_pause_now_during_the_after_read_journals_nothing_and_the_resume_retakes_it() {
    let _lock = common::env_lock();
    let _grace = common::EnvVar::set("APB_PAUSE_GRACE_MS", "5000");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    seed(
        &root,
        "pa",
        &linear("pa", &count_check(true)),
        &[
            (
                "count.sh",
                "echo \"$APB_BASELINE_PHASE\" >> \"$APB_RUN_DIR/reads.log\"\nif [ \"$APB_BASELINE_PHASE\" = after ] && [ -f slow_after ]; then\n  touch \"$APB_RUN_DIR/after_running\"\n  while [ -f slow_after ]; do sleep 0.05; done\nfi\ncat state.json\n",
            ),
            ("work.sh", WORK_SH),
        ],
    );
    fs::write(root.join("state.json"), "{\"count\": 3}").unwrap();
    fs::write(root.join("next.json"), "{\"count\": 0}").unwrap();
    fs::write(root.join("slow_after"), "").unwrap();
    let (tx, rx) = mpsc::channel();
    let r = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "pa", None, RunOptions::default()));
    });
    let run_id = poll_until("the after read to start", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().join("after_running").is_file())
            .map(|e| e.file_name().to_string_lossy().to_string())
    });
    request_pause(
        &root,
        &run_id,
        &PauseRequest {
            by: Some(PauseBy::Operator),
            mode: PauseMode::Now,
            ..PauseRequest::default()
        },
    )
    .unwrap();
    let paused: Result<RunResult, EngineError> = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(paused.unwrap().outcome, RunStatus::Paused);
    let ev = events(&root, &run_id);
    assert_eq!(observed(&ev).iter().filter(|o| o.0 == "after").count(), 0);
    assert!(compared(&ev).is_empty());
    fs::remove_file(root.join("slow_after")).unwrap();
    let res = resume(&root, &run_id, None).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(&root, &run_id);
    assert_eq!(observed(&ev).iter().filter(|o| o.0 == "after").count(), 1);
    assert_eq!(compared(&ev)[0].1, "met");
    assert_eq!(reads(&root, &run_id), ["before", "after", "after"]);
}

/// A run with baseline scripts and no goal scripts pins its `scripts/`
/// copy too: a playbook snapshot rewritten mid-run (as a supervisor patch
/// that leaves the baseline alone rewrites it) does not make the after
/// snapshot read as tampered.
#[test]
fn a_rewritten_playbook_snapshot_does_not_make_a_baseline_script_tampered() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    seed_count(&root, "pin", false, 3, 0);
    fs::write(root.join("slow"), "").unwrap();
    let (tx, rx) = mpsc::channel();
    let r = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(run(&r, "pin", None, RunOptions::default()));
    });
    poll_until("the work node to start", || {
        root.join("work_running").is_file().then_some(())
    });
    let run_dir = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("pin-"))
        .unwrap()
        .path();
    let yaml = fs::read_to_string(run_dir.join("playbook.yaml")).unwrap();
    fs::write(
        run_dir.join("playbook.yaml"),
        format!("{yaml}\n# patched\n"),
    )
    .unwrap();
    fs::remove_file(root.join("slow")).unwrap();
    let res = rx.recv_timeout(DEADLINE).unwrap().unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let ev = events(&root, &res.run_id);
    let after = observed(&ev).into_iter().find(|o| o.0 == "after").unwrap();
    assert_eq!((after.1.as_str(), after.2), ("observed", None));
    assert_eq!(compared(&ev)[0].1, "met");
}

/// How the after read of a finish with a prompt is interrupted.
#[derive(Debug, Clone, Copy)]
enum Interrupt {
    PauseNow,
    Abort,
}

/// An interrupted after read of a finish node that has a prompt never
/// composes the answer again: the resume continues at the after read, the
/// answer agent runs exactly once and its answer is the finish output.
#[test]
fn an_interrupted_after_read_of_a_finish_with_a_prompt_never_reruns_the_answer_agent() {
    let _lock = common::env_lock();
    let _grace = common::EnvVar::set("APB_PAUSE_GRACE_MS", "5000");
    for how in [Interrupt::PauseNow, Interrupt::Abort] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        let home = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let calls = dir.path().join("answer_calls.log");
        let stub = dir.path().join("agent.sh");
        common::write_sync(
            &stub,
            &format!(
                "#!/bin/sh\necho composed >> '{}'\necho FINAL_ANSWER\n",
                calls.display()
            ),
        );
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let _agent = common::EnvVar::set("APB_AGENT_CMD", &stub);
        let _home = common::EnvVar::set("HOME", home.path());
        let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
        let yaml = linear("fa", &count_check(false))
            .replace(
                "version: 1.0.0\n",
                "version: 1.0.0\ndefaults:\n  profile: main\n",
            )
            .replace(
                "{ id: done, type: finish, outcome: success }",
                "{ id: done, type: finish, outcome: success, prompt: \"compose the answer\" }",
            );
        seed(
            &root,
            "fa",
            &yaml,
            &[
                (
                    "count.sh",
                    "if [ \"$APB_BASELINE_PHASE\" = after ] && [ -f slow_after ]; then\n  touch \"$APB_RUN_DIR/after_running\"\n  while [ -f slow_after ]; do sleep 0.05; done\nfi\ncat state.json\n",
                ),
                ("work.sh", WORK_SH),
            ],
        );
        common::seed_main(&root);
        fs::write(root.join("state.json"), "{\"count\": 3}").unwrap();
        fs::write(root.join("next.json"), "{\"count\": 0}").unwrap();
        fs::write(root.join("slow_after"), "").unwrap();
        let (tx, rx) = mpsc::channel();
        let r = root.clone();
        std::thread::spawn(move || {
            let _ = tx.send(run(&r, "fa", None, RunOptions::default()));
        });
        let run_id = poll_until("the after read to start", || {
            fs::read_dir(root.join(".apb/runs"))
                .ok()?
                .filter_map(|e| e.ok())
                .find(|e| e.path().join("after_running").is_file())
                .map(|e| e.file_name().to_string_lossy().to_string())
        });
        match how {
            Interrupt::PauseNow => {
                request_pause(
                    &root,
                    &run_id,
                    &PauseRequest {
                        by: Some(PauseBy::Operator),
                        mode: PauseMode::Now,
                        ..PauseRequest::default()
                    },
                )
                .unwrap();
            }
            Interrupt::Abort => {
                apb_engine::stop::stop_run(&root, &run_id).unwrap();
            }
        }
        let interrupted: Result<RunResult, EngineError> = rx.recv_timeout(DEADLINE).unwrap();
        let expected = match how {
            Interrupt::PauseNow => RunStatus::Paused,
            Interrupt::Abort => RunStatus::Aborted,
        };
        assert_eq!(interrupted.unwrap().outcome, expected, "{how:?}");
        fs::remove_file(root.join("slow_after")).unwrap();
        let res = resume(&root, &run_id, None).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded, "{how:?}");
        let ev = events(&root, &run_id);
        let composed = fs::read_to_string(&calls).unwrap();
        assert_eq!(composed.lines().count(), 1, "{how:?}: {composed}");
        let starts = ev
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "done"),
            )
            .count();
        assert_eq!(starts, 1, "{how:?}");
        let output = ev.iter().find_map(|e| match &e.payload {
            EventPayload::NodeFinished { node, output, .. } if node == "done" => {
                Some(output.clone())
            }
            _ => None,
        });
        assert_eq!(output.as_deref(), Some("FINAL_ANSWER"), "{how:?}");
        assert_eq!(compared(&ev)[0].1, "met", "{how:?}");
    }
}
