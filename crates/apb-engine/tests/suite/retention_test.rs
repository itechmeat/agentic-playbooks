//! Run retention end to end (routine health D3): what `apb runs prune`
//! keeps and why, families removed whole or kept whole, the signed
//! summary written before the directories leave, the recovery of an
//! interrupted removal, one prune at a time, and readers that count every
//! run once at every moment.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::policy_file::RetentionSettings;
use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_engine::deadline_hooks::{self, Hook};
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::retention::archive::{self, ArchivedFamily, ArchivedRun};
use apb_engine::retention::{self, KeepReason, PruneAction, PruneRow};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common;

const DAY_MS: u64 = 86_400_000;

/// `job`: one script node that fails while `<root>/fail-job` exists and
/// waits while `<root>/hold-job` exists.
fn job_yaml(id: &str, root: &Path) -> String {
    let _ = root;
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nparams:\n  - {{ name: account, type: text, required: false }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

/// A parent whose one node runs `child` as a sub-playbook.
fn parent_yaml(id: &str, child: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: sub, type: playbook, playbook: {child} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: sub }}\n  - {{ from: sub, to: done }}\n"
    )
}

/// A playbook with a follow-up check one minute after its successful end.
fn follow_up_yaml(id: &str) -> String {
    format!(
        "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nfollow_up:\n  after: 1m\n  check:\n    id: deal_won\n    description: the deal moved\n    script: scripts/check.sh\n    observe:\n      - {{ id: stage, type: field, path: stage, expect: {{ equals: won }} }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

fn seed_playbook(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    let r = root.display();
    fs::write(
        vdir.join("scripts/work.sh"),
        format!(
            "#!/bin/sh\nwhile [ -f \"{r}/hold-{id}\" ]; do sleep 0.05; done\nif [ -f \"{r}/fail-{id}\" ]; then exit 1; fi\necho ok\n"
        ),
    )
    .unwrap();
    fs::write(
        vdir.join("scripts/check.sh"),
        "#!/bin/sh\necho '{\"stage\": \"won\"}'\n",
    )
    .unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
}

/// A project with the playbooks every test here uses.
/// The project, with the env lock held for the test: the installation key
/// and every trust read come from the config dir, which the tests that set
/// `APB_CONFIG_DIR` change under that lock.
fn project(root: &Path) -> std::sync::MutexGuard<'static, ()> {
    let env = common::env_lock();
    apb_core::run_origin::ensure_key().unwrap();
    init_project(root).unwrap();
    for id in ["job", "kid", "other"] {
        seed_playbook(root, id, &job_yaml(id, root));
    }
    seed_playbook(root, "parent", &parent_yaml("parent", "kid"));
    seed_playbook(root, "mid", &parent_yaml("mid", "kid"));
    seed_playbook(root, "top", &parent_yaml("top", "mid"));
    seed_playbook(root, "fu", &follow_up_yaml("fu"));
    seed_playbook(root, "fuparent", &parent_yaml("fuparent", "fu"));
    common::seed_main(root);
    env
}

fn settings() -> RetentionSettings {
    RetentionSettings {
        keep_days: 1,
        keep_last: 1,
        keep_failed_days: 1,
    }
}

/// Two days from now: every run made by a test is old, and a start key
/// bound now is still inside its default seven days.
fn later() -> u64 {
    apb_core::clock::now_ms_u64() + 2 * DAY_MS
}

fn start(root: &Path, id: &str, opts: RunOptions) -> String {
    run(root, id, None, opts).unwrap().run_id
}

fn ok_run(root: &Path, id: &str) -> String {
    let r = run(root, id, None, RunOptions::default()).unwrap();
    assert_eq!(r.outcome, RunStatus::Succeeded, "{id} {}", r.run_id);
    r.run_id
}

fn failed_run(root: &Path, id: &str) -> String {
    fs::write(root.join(format!("fail-{id}")), "").unwrap();
    let r = run(root, id, None, RunOptions::default()).unwrap();
    fs::remove_file(root.join(format!("fail-{id}"))).unwrap();
    assert_eq!(r.outcome, RunStatus::Failed, "{id} {}", r.run_id);
    r.run_id
}

fn run_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".apb/runs").join(id)
}

/// The children a run's journal names, in order.
fn children(root: &Path, id: &str) -> Vec<String> {
    read_all(&run_dir(root, id))
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ChildRunStarted { run_id, .. } => Some(run_id),
            _ => None,
        })
        .collect()
}

fn row<'a>(rows: &'a [PruneRow], id: &str) -> &'a PruneRow {
    rows.iter()
        .find(|r| r.run_id == id)
        .unwrap_or_else(|| panic!("no row for {id}: {rows:?}"))
}

fn kept(rows: &[PruneRow], id: &str) -> Option<KeepReason> {
    match &row(rows, id).action {
        PruneAction::Keep(r) => Some(*r),
        PruneAction::Remove => None,
    }
}

fn plan(root: &Path) -> Vec<PruneRow> {
    retention::plan(root, &settings(), None, later()).unwrap()
}

fn apply(root: &Path) -> retention::PruneReport {
    retention::apply(root, &settings(), None, later()).unwrap()
}

fn append(root: &Path, id: &str, payload: EventPayload) {
    EventLog::open(&run_dir(root, id))
        .unwrap()
        .append(payload)
        .unwrap();
}

fn grade(root: &Path, id: &str) {
    let dir = run_dir(root, id).join("grades");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("g-1.json"), "{}").unwrap();
}

/// A hash of every path and file byte under `dir`, links not followed.
fn tree_hash(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            let m = fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                stack.push(p.clone());
            }
            paths.push(p);
        }
    }
    paths.sort();
    for p in paths {
        h.update(p.to_string_lossy().as_bytes());
        let m = fs::symlink_metadata(&p).unwrap();
        if m.is_file() {
            h.update(fs::read(&p).unwrap());
        } else if m.file_type().is_symlink() {
            h.update(fs::read_link(&p).unwrap().to_string_lossy().as_bytes());
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn summary_path(root: &Path, id: &str) -> PathBuf {
    root.join(".apb/runs-archive").join(format!("{id}.json"))
}

/// The archived runs of the combined view.
fn archived(root: &Path) -> Vec<ArchivedRun> {
    archive::runs_view(root).unwrap().archived
}

fn family(id: &str, origin: &str) -> ArchivedFamily {
    ArchivedFamily {
        format: 1,
        root: id.into(),
        runs: vec![ArchivedRun {
            run_id: id.into(),
            playbook: "job".into(),
            scope: "project".into(),
            version: "1.0.0".into(),
            origin: origin.into(),
            parent: None,
            started_ms: 1,
            ended_ms: Some(2),
            outcome: "succeeded".into(),
            class: "succeeded".into(),
            aborted_by: None,
            abort_code: None,
            active_ms: None,
            tokens: None,
            cost_usd: None,
            execution: None,
        }],
        mac: String::new(),
    }
}

/// Starts a run of `id` that waits on `hold-<id>`; returns once its node
/// started, with the channel its answer lands on.
fn held_run(root: &Path, id: &str, opts: RunOptions) -> (String, mpsc::Receiver<RunStatus>) {
    fs::write(root.join(format!("hold-{id}")), "").unwrap();
    let before: Vec<String> = fs::read_dir(root.join(".apb/runs"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let (tx, rx) = mpsc::channel();
    let r = root.to_path_buf();
    let pid = id.to_string();
    std::thread::spawn(move || {
        let res = run(&r, &pid, None, opts).unwrap();
        let _ = tx.send(res.outcome);
    });
    let t = Instant::now();
    loop {
        let now: Vec<String> = fs::read_dir(root.join(".apb/runs"))
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !before.contains(n) && n.starts_with(&format!("{id}-")))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(new) = now.into_iter().next()
            && read_all(&run_dir(root, &new)).is_ok_and(|ev| {
                ev.iter()
                    .any(|e| matches!(&e.payload, EventPayload::NodeStarted { node, .. } if node == "work"))
            })
        {
            return (new, rx);
        }
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "timed out waiting for the held run of {id}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An aborted run of `id`, stopped by `by` with `reason`.
fn aborted_run(
    root: &Path,
    id: &str,
    by: apb_engine::pause::kind::PauseBy,
    reason: &str,
) -> String {
    let (rid, rx) = held_run(root, id, RunOptions::default());
    apb_engine::stop::stop_run_with(
        root,
        &rid,
        &apb_engine::stop::StopRequest {
            by,
            reason,
            graceful: false,
            due: None,
        },
    )
    .unwrap();
    fs::remove_file(root.join(format!("hold-{id}"))).unwrap();
    let _ = rx.recv_timeout(Duration::from_secs(15));
    rid
}

// 1. The latest production success stays, and an eval success never
// takes its place.
#[test]
fn the_latest_production_success_stays_over_an_eval_success() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let latest = ok_run(root, "job");
    let eval = start(
        root,
        "job",
        RunOptions {
            eval: Some(apb_engine::run_config::EvalRunSettings::default()),
            ..RunOptions::default()
        },
    );
    let _newest_failure = failed_run(root, "job");
    let rows = plan(root);
    assert_eq!(kept(&rows, &latest), Some(KeepReason::LatestSuccess));
    assert_eq!(kept(&rows, &old), None, "an older success goes");
    assert_eq!(
        kept(&rows, &eval),
        None,
        "an eval success is not the latest"
    );
}

// 2. A child's evidence keeps its whole family.
#[test]
fn a_graded_child_keeps_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "parent");
    let _p2 = ok_run(root, "parent");
    let _p3 = ok_run(root, "parent");
    let kid = children(root, &p1).remove(0);
    grade(root, &kid);
    let rows = plan(root);
    assert_eq!(kept(&rows, &kid), Some(KeepReason::Graded));
    assert_eq!(kept(&rows, &p1), Some(KeepReason::KeptChild));
    apply(root);
    assert!(run_dir(root, &p1).is_dir() && run_dir(root, &kid).is_dir());
}

#[test]
fn a_pending_follow_up_child_keeps_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "fuparent");
    let _p2 = ok_run(root, "fuparent");
    let _p3 = ok_run(root, "fuparent");
    let kid = children(root, &p1).remove(0);
    let rows = plan(root);
    assert_eq!(kept(&rows, &kid), Some(KeepReason::FollowUpPending));
    assert_eq!(kept(&rows, &p1), Some(KeepReason::KeptChild));
}

#[test]
fn a_graded_grandchild_keeps_the_whole_family() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let t1 = ok_run(root, "top");
    let _t2 = ok_run(root, "top");
    let _t3 = ok_run(root, "top");
    let mid = children(root, &t1).remove(0);
    let grand = children(root, &mid).remove(0);
    grade(root, &grand);
    let rows = plan(root);
    assert_eq!(kept(&rows, &grand), Some(KeepReason::Graded));
    assert_eq!(kept(&rows, &mid), Some(KeepReason::KeptChild));
    assert_eq!(kept(&rows, &t1), Some(KeepReason::KeptChild));
    apply(root);
    for id in [&t1, &mid, &grand] {
        assert!(run_dir(root, id).is_dir(), "{id} must stay");
    }
}

// 3. Following a planted path.
#[test]
fn nothing_outside_the_anchors_changes() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(outside.path().join("job-1")).unwrap();
    fs::write(outside.path().join("job-1/events.jsonl"), "precious").unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "parent");
    let _p2 = ok_run(root, "parent");
    let _p3 = ok_run(root, "parent");
    append(
        root,
        &p1,
        EventPayload::ChildRunStarted {
            node_id: "sub".into(),
            run_id: "../../etc".into(),
        },
    );
    std::os::unix::fs::symlink(outside.path().join("job-1"), run_dir(root, "job-1")).unwrap();
    fs::write(run_dir(root, "job-2"), "a file").unwrap();
    let before = tree_hash(outside.path());
    let rows = plan(root);
    assert_eq!(kept(&rows, "job-1"), Some(KeepReason::Unsafe));
    assert_eq!(kept(&rows, "job-2"), Some(KeepReason::Unsafe));
    assert_eq!(kept(&rows, &p1), Some(KeepReason::Unsafe));
    apply(root);
    assert_eq!(tree_hash(outside.path()), before);
    assert!(run_dir(root, &p1).is_dir());
    assert!(fs::symlink_metadata(run_dir(root, "job-1")).is_ok());
    assert!(run_dir(root, "job-2").is_file());
}

#[test]
fn a_linked_archive_refuses_the_prune() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    std::os::unix::fs::symlink(outside.path(), root.join(".apb/runs-archive")).unwrap();
    let rows = plan(root);
    assert!(
        rows.iter()
            .all(|r| r.action == PruneAction::Keep(KeepReason::Unsafe)),
        "{rows:?}"
    );
    let e = retention::apply(root, &settings(), None, later()).unwrap_err();
    assert!(e.to_string().contains("runs_archive_unsafe"), "{e}");
    assert!(run_dir(root, &old).is_dir());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

// 4. An unknown send stays.
#[test]
fn an_unknown_send_stays() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let gone = ok_run(root, "job");
    let _new = ok_run(root, "job");
    append(
        root,
        &old,
        EventPayload::SendIntentRecorded {
            item: None,
            intent: "si-1".into(),
            nonce: "n".into(),
            node: "work".into(),
            attempt: Some(1),
            attempt_seq: None,
            connector: "crm".into(),
            function: "send".into(),
            account: "main".into(),
            mode: "key".into(),
            label: "v1".into(),
            pid: 0,
            deadline_ms: 0,
            replay_of: None,
            resend_of: None,
        },
    );
    append(
        root,
        &old,
        EventPayload::SendIntentSettled {
            intent: "si-1".into(),
            outcome: "unknown".into(),
            source: "response".into(),
            detail: Some("timeout".into()),
            effect_ref: None,
            by: None,
        },
    );
    let rows = plan(root);
    assert_eq!(kept(&rows, &old), Some(KeepReason::UnknownSend));
    apply(root);
    assert!(run_dir(root, &old).is_dir());
    assert!(!run_dir(root, &gone).exists());
}

// 5. A dry run writes nothing.
#[test]
fn a_plan_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let _a = ok_run(root, "job");
    let _b = ok_run(root, "job");
    let _c = ok_run(root, "parent");
    let before = tree_hash(&root.join(".apb"));
    let _ = plan(root);
    assert_eq!(tree_hash(&root.join(".apb")), before);
    assert!(!root.join(".apb/runs-archive").exists());
}

// 6. A resume racing the prune.
#[test]
fn a_resumed_run_is_rechecked_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    let rows = plan(root);
    assert_eq!(kept(&rows, &old), None);
    append(root, &old, EventPayload::resumed("work", None, false));
    let report = apply(root);
    assert_eq!(report.removed, 0, "{:?}", report.rows);
    assert!(run_dir(root, &old).is_dir());
}

fn raced_family(on_lock: impl Fn(&Path, &str) + Send + Sync + 'static) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "parent");
    let _p2 = ok_run(root, "parent");
    let _p3 = ok_run(root, "parent");
    let kid = children(root, &p1).remove(0);
    assert_eq!(
        kept(&plan(root), &p1),
        None,
        "the family is planned for removal"
    );
    let parent_before = tree_hash(&run_dir(root, &p1));
    let k = kid.clone();
    let r = root.to_path_buf();
    deadline_hooks::set(Hook::PruneFamilyLocked, root, move |_| on_lock(&r, &k));
    let report = apply(root);
    deadline_hooks::clear(root);
    assert!(
        !report
            .rows
            .iter()
            .any(|r| r.run_id == p1 && r.action == PruneAction::Remove),
        "{:?}",
        report.rows
    );
    assert!(run_dir(root, &kid).is_dir());
    assert!(run_dir(root, &p1).is_dir());
    let _ = parent_before;
    assert!(!summary_path(root, &p1).exists());
}

#[test]
fn a_child_resumed_between_plan_and_commit_keeps_the_family() {
    raced_family(|root, kid| {
        // The resume that won the lock race journals before the recheck;
        // the prune holds the event lock, so the line is written raw.
        let path = root.join(".apb/runs").join(kid).join("events.jsonl");
        let events = read_all(&root.join(".apb/runs").join(kid)).unwrap();
        let seq = events.last().map_or(1, |e| e.seq + 1);
        let line = serde_json::json!({
            "seq": seq,
            "ts": apb_core::clock::now_ms(),
            "type": "run_resumed",
            "from_node": "work",
        });
        use std::io::Write;
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{line}").unwrap();
    });
}

#[test]
fn a_child_graded_between_plan_and_commit_keeps_the_family() {
    raced_family(grade);
}

// 7. A summary counted twice, missing, or stale.
#[test]
fn a_crash_after_the_summary_is_undone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    let mut f = family(&old, "production");
    f.runs[0].playbook = "job".into();
    archive::write_summary(root, &f).unwrap();
    assert!(summary_path(root, &old).is_file());
    // Keep the run by planting a grade, so the next apply only recovers.
    grade(root, &old);
    apply(root);
    assert!(!summary_path(root, &old).exists(), "the stale summary goes");
    assert!(run_dir(root, &old).is_dir(), "the run stays");
}

#[test]
fn a_crash_mid_stage_is_undone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "parent");
    let _p2 = ok_run(root, "parent");
    let _p3 = ok_run(root, "parent");
    let kid = children(root, &p1).remove(0);
    let mut f = family(&p1, "production");
    let mut child = f.runs[0].clone();
    child.run_id = kid.clone();
    child.parent = Some(p1.clone());
    f.runs.push(child);
    archive::write_summary(root, &f).unwrap();
    let staging = root
        .join(".apb/runs-archive")
        .join(format!(".removing-{p1}"));
    fs::create_dir_all(&staging).unwrap();
    fs::rename(run_dir(root, &kid), staging.join(&kid)).unwrap();
    // Grade the child once it is back so the replanned family is kept
    // and the recovery can be seen alone.
    let report = retention::apply(root, &settings(), None, later()).unwrap();
    assert!(!staging.exists(), "the staging directory is gone");
    // The recovery moved the child back, then the family was planned
    // again and removed whole: its summary names both.
    let removed = report
        .rows
        .iter()
        .filter(|r| r.action == PruneAction::Remove)
        .count();
    assert!(removed >= 2, "{:?}", report.rows);
    let view = archived(root);
    assert!(view.iter().any(|a| a.run_id == p1));
    assert!(view.iter().any(|a| a.run_id == kid));
    assert_eq!(view.iter().filter(|a| a.run_id == kid).count(), 1);
}

#[test]
fn a_committed_removal_is_finished() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let _a = ok_run(root, "job");
    let removed = root.join(".apb/runs-archive/.removed-job-1");
    fs::create_dir_all(removed.join("job-1")).unwrap();
    fs::write(removed.join("job-1/events.jsonl"), "x").unwrap();
    apply(root);
    assert!(!removed.exists());
}

#[test]
fn a_reader_counts_a_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    archive::write_summary(root, &family(&old, "production")).unwrap();
    let view = archive::runs_view(root).unwrap();
    assert!(view.dirs.contains(&old));
    assert!(
        view.archived.iter().all(|a| a.run_id != old),
        "a run with a directory is counted from it"
    );
    // A summary of a run with no directory is counted from the summary.
    archive::write_summary(root, &family("job-9", "production")).unwrap();
    let view = archive::runs_view(root).unwrap();
    assert_eq!(
        view.archived.iter().filter(|a| a.run_id == "job-9").count(),
        1
    );
}

#[test]
fn a_reader_between_stage_and_commit_counts_every_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "parent");
    let _p2 = ok_run(root, "parent");
    let _p3 = ok_run(root, "parent");
    let kid = children(root, &p1).remove(0);
    let (tx, rx) = mpsc::channel::<()>();
    let tx = std::sync::Mutex::new(tx);
    deadline_hooks::set(Hook::PruneAfterStage, root, move |_| {
        let _ = tx.lock().unwrap().send(());
        std::thread::sleep(Duration::from_secs(1));
    });
    let r = root.to_path_buf();
    let reader = std::thread::spawn(move || {
        rx.recv_timeout(Duration::from_secs(30))
            .expect("the prune reached its stage");
        let facts = apb_engine::health::facts::facts(&r, Scope::Project, "parent").unwrap();
        (archive::runs_view(&r).unwrap(), facts)
    });
    apply(root);
    deadline_hooks::clear(root);
    let (view, facts) = reader.join().unwrap();
    assert_eq!(
        facts.iter().filter(|f| f.run_id == p1).count(),
        1,
        "health counts the top-level run once: {facts:?}"
    );
    for id in [&p1, &kid] {
        let n = view.dirs.iter().filter(|d| *d == id).count()
            + view.archived.iter().filter(|a| &a.run_id == id).count();
        assert_eq!(
            n, 1,
            "{id} counted {n} times: {:?} {:?}",
            view.dirs, view.archived
        );
    }
}

// 8. Two prunes at once.
#[test]
fn a_second_prune_is_refused_while_one_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    let (tx, rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let tx = std::sync::Mutex::new(tx);
    let go_rx = std::sync::Mutex::new(go_rx);
    deadline_hooks::set(Hook::PruneFamilyLocked, root, move |_| {
        let _ = tx.lock().unwrap().send(());
        let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
    });
    let r = root.to_path_buf();
    let first = std::thread::spawn(move || retention::apply(&r, &settings(), None, later()));
    rx.recv_timeout(Duration::from_secs(30))
        .expect("the first prune holds its family");
    let before = tree_hash(&root.join(".apb/runs"));
    let e = retention::apply(root, &settings(), None, later()).unwrap_err();
    assert!(e.to_string().contains("prune_in_progress"), "{e}");
    assert_eq!(tree_hash(&root.join(".apb/runs")), before);
    go_tx.send(()).unwrap();
    let report = first.join().unwrap().unwrap();
    deadline_hooks::clear(root);
    assert_eq!(report.removed, 1);
    assert!(!run_dir(root, &old).exists());
}

// 9. A forged or eval summary counted.
#[test]
fn a_copied_summary_is_not_counted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    archive::write_summary(root, &family("job-9", "production")).unwrap();
    let path = summary_path(root, "job-9");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("succeeded", "failed");
    fs::write(&path, text).unwrap();
    assert!(archived(root).is_empty());
}

#[test]
fn an_eval_summary_is_not_production() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let eval = start(
        root,
        "job",
        RunOptions {
            eval: Some(apb_engine::run_config::EvalRunSettings::default()),
            ..RunOptions::default()
        },
    );
    let _a = ok_run(root, "job");
    let _b = ok_run(root, "job");
    apply(root);
    let view = archived(root);
    let a = view
        .iter()
        .find(|a| a.run_id == eval)
        .expect("the eval run is archived");
    assert_eq!(a.origin, "eval");
}

// 10. A secret in a summary.
#[test]
fn the_archive_holds_no_param_value_or_reason() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let rid = aborted_run(
        root,
        "job",
        apb_engine::pause::kind::PauseBy::Engine,
        "giving up on acct-7781 with token sk-live-QQQ999",
    );
    let _a = ok_run(root, "job");
    apply(root);
    let bytes = fs::read_to_string(summary_path(root, &rid)).expect("the aborted run is archived");
    assert!(!bytes.contains("acct-7781"), "{bytes}");
    assert!(!bytes.contains("sk-live-QQQ999"), "{bytes}");
    let a = archived(root)
        .into_iter()
        .find(|a| a.run_id == rid)
        .unwrap();
    assert_eq!(a.abort_code.as_deref(), Some("other"));
}

// 11. A class that differs once archived.
#[test]
fn an_archived_run_has_the_class_of_the_live_run() {
    use apb_engine::pause::kind::PauseBy;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let mut ids = vec![ok_run(root, "job"), failed_run(root, "job")];
    ids.push(aborted_run(
        root,
        "job",
        PauseBy::Engine,
        "deadline_exceeded: the run took longer than 1h",
    ));
    ids.push(aborted_run(
        root,
        "job",
        PauseBy::Engine,
        "superseded: run job-9 started for key acme",
    ));
    let (stopped, rx) = held_run(root, "job", RunOptions::default());
    apb_engine::stop_run(root, &stopped).unwrap();
    fs::remove_file(root.join("hold-job")).unwrap();
    let _ = rx.recv_timeout(Duration::from_secs(15));
    ids.push(stopped);
    ids.push(aborted_run(
        root,
        "job",
        PauseBy::Engine,
        apb_engine::stop::PARENT_ABORTED_REASON,
    ));
    fs::write(root.join("fail-job"), "").unwrap();
    ids.push(start(
        root,
        "job",
        RunOptions {
            external_data: true,
            ..RunOptions::default()
        },
    ));
    fs::remove_file(root.join("fail-job")).unwrap();
    // The newest run is a success that stays, so every run above is old
    // and goes.
    let _keep = ok_run(root, "job");
    let class_of = |root: &Path| -> std::collections::BTreeMap<String, String> {
        apb_engine::health::facts::facts(root, Scope::Project, "job")
            .unwrap()
            .into_iter()
            .map(|f| (f.run_id, format!("{:?}", f.class)))
            .collect()
    };
    let before = class_of(root);
    let report = apply(root);
    for id in &ids {
        assert!(
            !run_dir(root, id).exists(),
            "{id} was pruned: {:?}",
            report.rows
        );
    }
    let after = class_of(root);
    for id in &ids {
        assert_eq!(before.get(id), after.get(id), "{id}");
        assert!(before.contains_key(id), "{id} is a fact");
    }
    assert_eq!(before[&ids[3]], "Ignored", "superseded");
    assert_eq!(before[&ids[2]], "Failed", "deadline");
}

// 12. Unreadable journals.
fn unknown_line(root: &Path, id: &str) -> String {
    let events = read_all(&run_dir(root, id)).unwrap();
    let seq = events.last().map_or(1, |e| e.seq + 1);
    serde_json::json!({"seq": seq, "ts": 1, "type": "from_a_newer_apb"}).to_string()
}

fn push_line(root: &Path, id: &str, line: &str) {
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(run_dir(root, id).join("events.jsonl"))
        .unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

#[test]
fn an_unknown_event_after_the_last_checkpoint_keeps_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    let line = unknown_line(root, &old);
    push_line(root, &old, &format!("{line}\n"));
    assert_eq!(kept(&plan(root), &old), Some(KeepReason::Unreadable));
    apply(root);
    assert!(run_dir(root, &old).is_dir());
}

#[test]
fn an_unknown_event_before_a_checkpoint_does_not_keep_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    // Rewrite the journal with an unknown event before its last
    // checkpoint (the terminal event).
    let path = run_dir(root, &old).join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let last = lines.pop().unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&last).unwrap();
    let seq = v["seq"].as_u64().unwrap();
    lines.push(serde_json::json!({"seq": seq, "ts": 1, "type": "from_a_newer_apb"}).to_string());
    v["seq"] = serde_json::json!(seq + 1);
    lines.push(v.to_string());
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    assert!(read_all(&run_dir(root, &old)).is_ok(), "the journal reads");
    assert_eq!(kept(&plan(root), &old), None);
    apply(root);
    assert!(!run_dir(root, &old).exists());
}

#[test]
fn a_torn_tail_keeps_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    push_line(root, &old, "{\"seq\": 99, \"ts\": 1, \"type\": \"node_sta");
    assert_eq!(kept(&plan(root), &old), Some(KeepReason::Unreadable));
    apply(root);
    assert!(run_dir(root, &old).is_dir());
}

// 13. The playbook trash is never used.
#[test]
fn the_trash_is_not_used() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let _a = ok_run(root, "job");
    let _b = ok_run(root, "job");
    let _c = ok_run(root, "job");
    let before = apb_core::versioning::list_trash(root).unwrap().len();
    apply(root);
    assert_eq!(
        apb_core::versioning::list_trash(root).unwrap().len(),
        before
    );
}

// 15. Keyed runs.
#[test]
fn start_key_and_concurrency_runs_stay() {
    use apb_core::start_key::{
        REQUEST_V, StartKey, StartKeyRequest, StartRequest, StartRequestFields,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    fs::write(root.join("fail-job"), "").unwrap();
    let keyed = run(
        root,
        "job",
        None,
        RunOptions {
            start_key: Some(StartKeyRequest {
                key: StartKey::parse("e1").unwrap(),
                reuse: false,
                request: StartRequest::capture(&StartRequestFields {
                    v: REQUEST_V,
                    supervision: "autonomous".into(),
                    cache: "auto".into(),
                    ..Default::default()
                })
                .unwrap(),
            }),
            ..RunOptions::default()
        },
    )
    .unwrap()
    .run_id;
    fs::remove_file(root.join("fail-job")).unwrap();
    let slotted = failed_run(root, "other");
    let _o2 = ok_run(root, "other");
    let _j2 = ok_run(root, "job");
    let _j3 = ok_run(root, "job");
    let s = slotted.clone();
    apb_core::signed_store::transact(
        root,
        Scope::Project,
        "other",
        apb_core::signed_store::Unsigned::Refuse,
        move |f: &mut apb_engine::concurrency::ConcurrencyFile| {
            f.slots.push(apb_engine::concurrency::Slot {
                key_digest: "d1".into(),
                display: "acme".into(),
                active: Some(apb_engine::concurrency::Holder {
                    run_id: s,
                    reservation: None,
                }),
                waiting: None,
                stops: Vec::new(),
            });
            Ok::<_, apb_core::signed_store::StoreError>(((), true))
        },
    )
    .unwrap();
    let rows = plan(root);
    assert_eq!(kept(&rows, &keyed), Some(KeepReason::StartKey));
    assert_eq!(kept(&rows, &slotted), Some(KeepReason::ConcurrencyKey));
    apply(root);
    assert!(run_dir(root, &keyed).is_dir());
    assert!(run_dir(root, &slotted).is_dir());
}

#[test]
fn an_unverified_start_key_file_keeps_the_playbooks_runs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    let path = root.join(".apb/start-keys/project/job.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{\"schema\": 1, \"entries\": []}").unwrap();
    assert_eq!(kept(&plan(root), &old), Some(KeepReason::StartKey));
}

// 16. External data.
#[test]
fn an_external_data_summary_keeps_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let flagged = start(
        root,
        "job",
        RunOptions {
            external_data: true,
            ..RunOptions::default()
        },
    );
    let _a = ok_run(root, "job");
    let _b = ok_run(root, "job");
    apply(root);
    let a = archived(root)
        .into_iter()
        .find(|a| a.run_id == flagged)
        .expect("the flagged run is archived");
    assert_eq!(a.origin, "external_data");
    let _ = row;
}

// Review fixes.

/// A recovery with `.apb/runs` a link to a directory outside the project
/// moves nothing there: it refuses and leaves the staged entry in place.
#[test]
fn recovery_never_writes_through_a_linked_runs_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let outside = tempfile::tempdir().unwrap();
    let runs = root.join(".apb/runs");
    let _ = fs::remove_dir_all(&runs);
    std::os::unix::fs::symlink(outside.path(), &runs).unwrap();
    let staging = root.join(".apb/runs-archive/.removing-x");
    fs::create_dir_all(staging.join("job-1")).unwrap();
    fs::write(staging.join("injected.txt"), "x").unwrap();
    let r = retention::apply(root, &settings(), None, later());
    assert!(
        fs::read_dir(outside.path()).unwrap().next().is_none(),
        "nothing lands outside the project"
    );
    assert!(r.is_err(), "the recovery refuses: {r:?}");
    assert!(staging.join("injected.txt").is_file());
    assert!(staging.join("job-1").is_dir());
}

/// A staged entry that is not a real directory (a file, a link) is never
/// moved under `.apb/runs`: it stays and is reported.
#[test]
fn recovery_moves_back_only_real_run_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    fs::create_dir_all(root.join(".apb/runs")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let staging = root.join(".apb/runs-archive/.removing-x");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("injected.txt"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path(), staging.join("job-9")).unwrap();
    let e = retention::apply(root, &settings(), None, later()).unwrap_err();
    assert!(e.to_string().contains("runs_archive_conflict"), "{e}");
    assert!(!root.join(".apb/runs/injected.txt").exists());
    assert!(fs::symlink_metadata(root.join(".apb/runs/job-9")).is_err());
}

/// A seeded run of `job` that the live health facts never count: no
/// provenance (a preparation failure), or no origin stamp of this
/// installation (a journal that arrived from elsewhere).
fn seed_uncounted(root: &Path, id: &str, stamped: bool, provenance: bool) {
    let dir = run_dir(root, id);
    fs::create_dir_all(&dir).unwrap();
    if stamped {
        apb_core::run_origin::stamp(&dir, id).unwrap();
    }
    let mut lines = vec![
        serde_json::json!({"seq": 1, "ts": 1_000, "type": "run_started", "playbook": "job", "version": "1.0.0"}),
    ];
    if provenance {
        lines.push(
            serde_json::json!({"seq": 2, "ts": 1_000, "type": "run_provenance", "origin": "project"}),
        );
    }
    let n = lines.len() as u64 + 1;
    lines.push(
        serde_json::json!({"seq": n, "ts": 1_010, "type": "run_finished", "outcome": "failed"}),
    );
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

/// A run the live health facts do not count is archived as a run health
/// still does not count.
#[test]
fn an_archived_run_counts_for_health_only_when_it_did_live() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    seed_uncounted(root, "job-prep", true, false);
    seed_uncounted(root, "job-foreign", false, true);
    let good = ok_run(root, "job");
    let live: Vec<String> = apb_engine::health::facts::facts(root, Scope::Project, "job")
        .unwrap()
        .into_iter()
        .filter(|f| f.class != apb_core::health::RunClass::Ignored)
        .map(|f| f.run_id)
        .collect();
    assert_eq!(live, vec![good.clone()]);
    let report = apply(root);
    assert!(!run_dir(root, "job-prep").exists(), "{:?}", report.rows);
    assert!(!run_dir(root, "job-foreign").exists(), "{:?}", report.rows);
    // The rules skip an ignored fact: what counts is what counted live.
    let after: Vec<String> = apb_engine::health::facts::facts(root, Scope::Project, "job")
        .unwrap()
        .into_iter()
        .filter(|f| f.class != apb_core::health::RunClass::Ignored)
        .map(|f| f.run_id)
        .collect();
    assert_eq!(after, vec![good], "the archive adds no counted fact");
}

/// A grade written after the recheck and before the move waits for the
/// run's event lock the prune holds and is refused: nothing a grader
/// stored is deleted, and no grade lands where the run was.
#[test]
fn a_grade_after_the_recheck_waits_for_the_prune() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    assert_eq!(kept(&plan(root), &old), None);
    let graded = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (o, g) = (old.clone(), graded.clone());
    deadline_hooks::set(Hook::PruneAfterRecheck, root, move |d| {
        if d.ends_with(&o) {
            *g.lock().unwrap() = Some(apb_engine::grade::grade_rules(d).map(|_| ()));
        }
    });
    let report = apply(root);
    deadline_hooks::clear(root);
    let graded = graded.lock().unwrap().take().expect("the grader ran");
    assert!(graded.is_err(), "{graded:?}");
    assert_eq!(report.removed, 1, "{:?}", report.rows);
    assert!(!run_dir(root, &old).exists());
}

/// A child a resumed parent started after the plan keeps the family: the
/// membership is read again under the locks.
#[test]
fn a_child_added_after_the_plan_keeps_the_family() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let p1 = ok_run(root, "parent");
    let _p2 = ok_run(root, "parent");
    let _p3 = ok_run(root, "parent");
    let late = ok_run(root, "kid");
    assert_eq!(kept(&plan(root), &p1), None);
    let (r, p, k) = (root.to_path_buf(), p1.clone(), late.clone());
    deadline_hooks::set(Hook::PruneFamilyLocked, root, move |d| {
        if !d.ends_with(&p) {
            return;
        }
        // The parent's resume ran its sub-playbook node again and ended:
        // its new child names it, and its journal names the child. The
        // prune holds the parent's event lock, so the line is raw.
        let kid_dir = r.join(".apb/runs").join(&k);
        let mut cfg = apb_engine::run_config::read_run_config(&kid_dir).unwrap();
        cfg.parent_run = Some(p.clone());
        apb_engine::run_config::write_run_config(&kid_dir, &cfg).unwrap();
        let parent_dir = r.join(".apb/runs").join(&p);
        let seq = read_all(&parent_dir)
            .unwrap()
            .last()
            .map_or(1, |e| e.seq + 1);
        let line = serde_json::json!({
            "seq": seq,
            "ts": apb_core::clock::now_ms(),
            "type": "child_run_started",
            "node_id": "sub",
            "run_id": k,
        });
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(parent_dir.join("events.jsonl"))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    });
    let report = apply(root);
    deadline_hooks::clear(root);
    assert!(run_dir(root, &p1).is_dir(), "{:?}", report.rows);
    assert!(run_dir(root, &late).is_dir());
    assert!(!summary_path(root, &p1).exists());
}

/// A newer success reopened after the plan makes the older one the latest
/// success again, and the prune keeps it.
#[test]
fn a_success_reopened_after_the_plan_keeps_the_older_success() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let older = ok_run(root, "job");
    let newer = ok_run(root, "job");
    assert_eq!(kept(&plan(root), &older), None);
    let (r, o, n) = (root.to_path_buf(), older.clone(), newer.clone());
    deadline_hooks::set(Hook::PruneFamilyLocked, root, move |d| {
        if d.ends_with(&o) {
            append(&r, &n, EventPayload::resumed("work", None, false));
        }
    });
    let report = apply(root);
    deadline_hooks::clear(root);
    assert!(run_dir(root, &older).is_dir(), "{:?}", report.rows);
    assert_eq!(kept(&report.rows, &older), Some(KeepReason::LatestSuccess));
}

/// A grade write that a prune overtakes between making the grades
/// directory and writing the file never leaves the grade in a recreated
/// directory without its journal: the write holds the run's event lock,
/// which the prune takes before it moves the run.
#[test]
fn a_grade_write_and_a_prune_never_split_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let _env = project(root);
    let old = ok_run(root, "job");
    let _new = ok_run(root, "job");
    assert_eq!(kept(&plan(root), &old), None);
    let (at_tx, at_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let at_tx = std::sync::Mutex::new(at_tx);
    let go_rx = std::sync::Mutex::new(go_rx);
    deadline_hooks::set(Hook::GradeBeforeWrite, root, move |_| {
        let _ = at_tx.lock().unwrap().send(());
        let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
    });
    let grader = std::thread::spawn({
        let d = run_dir(root, &old);
        move || apb_engine::grade::grade_rules(&d)
    });
    at_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the grader made its grades directory");
    let _ = retention::apply(root, &settings(), None, later());
    go_tx.send(()).unwrap();
    let graded = grader.join().unwrap();
    deadline_hooks::clear(root);
    let d = run_dir(root, &old);
    if d.exists() {
        assert!(
            d.join("events.jsonl").is_file(),
            "a run without its journal"
        );
        assert!(graded.is_ok(), "{graded:?}");
        assert_eq!(kept(&apply(root).rows, &old), Some(KeepReason::Graded));
        assert!(d.join("grades").read_dir().unwrap().next().is_some());
    } else {
        assert!(graded.is_err(), "a grade stored for a removed run");
    }
}
