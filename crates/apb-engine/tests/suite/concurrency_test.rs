//! The concurrency key end to end (routine health D1): one active and one
//! waiting run per key, the newest start replacing a waiting one, a keyed
//! start never holding the tree while it waits, refusals that leave
//! nothing behind, and the window of a queued routine read after its turn.

use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, RunResult, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

/// `start -> work -> done`, `work` a script that writes `started` under
/// the root and waits while `<root>/hold` exists.
fn keyed_project(root: &Path, id: &str, on_busy: &str, key: Option<&str>) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    let r = root.display();
    let concurrency = match key {
        Some(k) => format!("concurrency:\n  key: \"{k}\"\n  on_busy: {on_busy}\n"),
        None => format!("concurrency:\n  on_busy: {on_busy}\n"),
    };
    fs::write(
        vdir.join("playbook.yaml"),
        format!(
            "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nparams:\n  - {{ name: account, type: text }}\n{concurrency}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
        ),
    )
    .unwrap();
    fs::write(
        vdir.join("scripts/work.sh"),
        format!(
            "#!/bin/sh\ntouch \"{r}/started\"\nwhile [ -f \"{r}/hold\" ]; do sleep 0.05; done\necho ok\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
}

/// A keyed start with `account` set.
fn keyed(account: &str) -> RunOptions {
    RunOptions {
        params: [(("account").to_string(), account.to_string())]
            .into_iter()
            .collect(),
        ..RunOptions::default()
    }
}

fn cfg() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    (dir, lock)
}

fn wait_started(root: &Path, what: &str) {
    let start = Instant::now();
    while !root.join("started").is_file() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the run `run_id` reaches a terminal event (a
/// `run_finished`, or a `run_aborted` a supersede or a deadline wrote).
fn wait_run(root: &Path, run_id: &str, what: &str) -> RunStatus {
    let start = Instant::now();
    loop {
        if let Ok(events) = read_all(&root.join(".apb/runs").join(run_id)) {
            let status = events.iter().find_map(|e| match &e.payload {
                EventPayload::RunFinished { outcome } => match outcome.as_str() {
                    "succeeded" => Some(RunStatus::Succeeded),
                    "failed" => Some(RunStatus::Failed),
                    _ => Some(RunStatus::Aborted),
                },
                EventPayload::RunAborted { .. } => Some(RunStatus::Aborted),
                _ => None,
            });
            if let Some(status) = status {
                return status;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The run directories under `prefix` right now.
fn run_ids(root: &Path, prefix: &str) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(prefix))
                .collect()
        })
        .unwrap_or_default()
}

/// Waits until the newest run under `prefix` reaches a terminal event (a
/// `run_finished`, or a `run_aborted` a supersede or a deadline wrote).
fn wait_terminal(root: &Path, prefix: &str, what: &str) -> (String, RunStatus) {
    let start = Instant::now();
    loop {
        if let Ok(rd) = fs::read_dir(root.join(".apb/runs")) {
            for e in rd.filter_map(Result::ok) {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(prefix)
                    && let Ok(events) = read_all(&e.path())
                {
                    let status = events.iter().find_map(|e| match &e.payload {
                        EventPayload::RunFinished { outcome } => match outcome.as_str() {
                            "succeeded" => Some(RunStatus::Succeeded),
                            "failed" => Some(RunStatus::Failed),
                            _ => Some(RunStatus::Aborted),
                        },
                        EventPayload::RunAborted { .. } => Some(RunStatus::Aborted),
                        _ => None,
                    });
                    if let Some(status) = status {
                        return (name, status);
                    }
                }
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn aborted_reason(root: &Path, run_id: &str) -> Option<String> {
    read_all(&root.join(".apb/runs").join(run_id))
        .ok()?
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RunAborted { reason, .. } => Some(reason.clone()),
            _ => None,
        })
}

/// The concurrency record of the run `run_id`'s provenance.
fn record_of(root: &Path, run_id: &str) -> Option<apb_engine::event::ConcurrencyRecord> {
    read_all(&root.join(".apb/runs").join(run_id))
        .ok()?
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::RunProvenance {
                concurrency: Some(record),
                ..
            } => Some(record),
            _ => None,
        })
}

/// Whether the run `run_id` started a node.
fn started_a_node(root: &Path, run_id: &str) -> bool {
    read_all(&root.join(".apb/runs").join(run_id))
        .map(|events| {
            events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::NodeStarted { .. }))
        })
        .unwrap_or(false)
}

/// 1: a queued key run holds no tree lock, and runs after the holder.
#[test]
fn a_queued_run_holds_no_tree_lock() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "queue_latest", Some("{{params.account}}"));
    // A holds its node from the start, so B cannot miss it.
    fs::write(root.join("hold"), "").unwrap();

    let first = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "the holder's node");
    let a_id = run_ids(root, "ck-")
        .pop()
        .expect("the holder's run directory exists");

    let second = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    // B is admitted as A's waiter and runs nothing while A holds the key.
    let start = Instant::now();
    let b_id = loop {
        let queued = run_ids(root, "ck-")
            .into_iter()
            .filter(|id| id != &a_id)
            .find(|id| record_of(root, id).is_some_and(|r| r.queued));
        if let Some(id) = queued {
            break id;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "B was never recorded as queued"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    std::thread::sleep(Duration::from_millis(300));
    assert!(!started_a_node(root, &b_id), "B ran while A held the key");
    assert!(root.join("hold").exists());

    // While B waits, A's tree stays A's: the next node of A takes it.
    fs::remove_file(root.join("hold")).unwrap();
    let first_status = wait_run(root, &a_id, "the holder to finish");
    assert_eq!(first_status, RunStatus::Succeeded);
    let second = second.join().unwrap().unwrap();
    assert_eq!(second.run_id, b_id);
    assert_eq!(second.outcome, RunStatus::Succeeded);
    let _ = first.join().unwrap();
}

/// A keyed start that may share the tree runs beside the run that holds
/// it, as an unkeyed one does, instead of waiting for the tree.
#[test]
fn a_keyed_start_that_shares_the_tree_does_not_wait_for_it() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ha", "queue_latest", Some("{{params.account}}"));
    keyed_project(root, "kb", "queue_latest", Some("{{params.account}}"));
    // B's own node does not wait for the hold.
    fs::write(
        root.join(".apb/playbooks/kb/1.0.0/scripts/work.sh"),
        "#!/bin/sh\necho ok\n",
    )
    .unwrap();
    fs::write(root.join("hold"), "").unwrap();
    let holder = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ha", None, keyed("acme"))
    });
    wait_started(root, "the holder's node");
    let (tx, rx) = mpsc::channel();
    let b = std::thread::spawn({
        let root = root.to_path_buf();
        move || {
            let r = run(
                &root,
                "kb",
                None,
                RunOptions {
                    allow_shared_workdir: true,
                    workdir_queue_wait: Some(Duration::from_secs(1)),
                    ..keyed("other")
                },
            );
            let _ = tx.send(());
            r
        }
    });
    let finished = rx.recv_timeout(Duration::from_secs(10)).is_ok();
    fs::remove_file(root.join("hold")).unwrap();
    let res = b.join().unwrap();
    let _ = holder.join().unwrap();
    assert!(finished, "B waited for the tree it may share");
    assert_eq!(res.unwrap().outcome, RunStatus::Succeeded);
}

/// A detached start that did not wait for its key keeps the window its
/// preparation read: a success of another key before its driver starts
/// does not move it.
#[test]
fn a_detached_start_that_did_not_queue_keeps_its_window() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "cw", "queue_latest", Some("{{params.account}}"));
    keyed_project(root, "cwk", "queue_latest", None);
    // `cw` reads its window in the instruction it hands its sub-playbook.
    let yaml_path = root.join(".apb/playbooks/cw/1.0.0/playbook.yaml");
    let yaml = fs::read_to_string(&yaml_path).unwrap().replace(
        "{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }",
        "{ id: work, type: playbook, playbook: cwk, instruction: \"since {{run.previous_success.id}}\" }",
    );
    fs::write(&yaml_path, yaml).unwrap();
    let s = run(root, "cw", None, keyed("s")).unwrap();
    assert_eq!(s.outcome, RunStatus::Succeeded);
    let b_id = {
        let prepared =
            apb_engine::scheduler::prepare_supervised_background(root, "cw", None, keyed("b"))
                .unwrap();
        prepared.run_id().to_string()
    };
    assert!(!record_of(root, &b_id).unwrap().queued, "B did not queue");
    // C succeeds on another key before B's driver starts.
    let c = run(root, "cw", None, keyed("c")).unwrap();
    assert_eq!(c.outcome, RunStatus::Succeeded);
    let res = apb_engine::scheduler::drive_run_from_dir(root, &b_id).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&b_id)).unwrap();
    let window = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::PreviousSuccessSet { previous_success } => Some(previous_success.clone()),
            EventPayload::RunProvenance {
                previous_success, ..
            } => Some(previous_success.clone()),
            _ => None,
        })
        .flatten()
        .and_then(|p| p.run_id);
    assert_eq!(window.as_deref(), Some(s.run_id.as_str()), "{events:?}");
}

/// 2: B, C and D start in turn behind A; B and C end aborted
/// `superseded:`, D runs after A.
#[test]
fn the_newest_start_replaces_the_waiting_one() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "queue_latest", Some("{{params.account}}"));
    fs::write(root.join("hold"), "").unwrap();

    let a = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "A's node");
    let a_id = run_ids(root, "ck-")
        .pop()
        .expect("the holder's run directory exists");
    let mut waiters = Vec::new();
    for _ in 0..3 {
        waiters.push(std::thread::spawn({
            let root = root.to_path_buf();
            move || run(&root, "ck", None, keyed("acme"))
        }));
        std::thread::sleep(Duration::from_millis(400));
    }
    fs::remove_file(root.join("hold")).unwrap();
    let a_status = wait_run(root, &a_id, "A to finish");
    assert_eq!(a_status, RunStatus::Succeeded);
    let mut superseded = 0;
    let mut ran = 0;
    for w in waiters {
        match w.join().unwrap() {
            // A replaced waiter answers Superseded with its journal
            // holding the abort.
            Err(apb_engine::error::EngineError::Superseded(_)) => superseded += 1,
            Ok(res) => {
                if let Some(reason) = aborted_reason(root, &res.run_id) {
                    assert!(reason.starts_with("superseded:"), "{reason}");
                    superseded += 1;
                } else {
                    assert_eq!(res.outcome, RunStatus::Succeeded);
                    ran += 1;
                }
            }
            other => panic!("unexpected waiter outcome: {other:?}"),
        }
    }
    assert_eq!(superseded, 2, "two waiters must be replaced");
    assert_eq!(ran, 1, "the newest waiter must run");
    let _ = a.join().unwrap();
}

/// 4: `supersede` stops the holder gracefully and runs after it.
#[test]
fn supersede_stops_the_holder_gracefully() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "supersede", Some("{{params.account}}"));
    fs::write(root.join("hold"), "").unwrap();

    let a = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "A's node");
    let b = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    let (a_id, a_status) = wait_terminal(root, "ck-", "A to be stopped");
    assert_eq!(a_status, RunStatus::Aborted, "A must end aborted");
    let reason = aborted_reason(root, &a_id).unwrap_or_default();
    assert!(reason.starts_with("superseded:"), "{reason}");
    match a.join().unwrap() {
        Ok(_) | Err(apb_engine::error::EngineError::Superseded(_)) => {}
        other => panic!("unexpected holder outcome: {other:?}"),
    }
    // B's node waits on the hold: free it so B can finish.
    fs::remove_file(root.join("hold")).unwrap();
    let b = b.join().unwrap().unwrap();
    assert_eq!(b.outcome, RunStatus::Succeeded);
}

/// 5: eight racing starts with one key and `refuse`: exactly one is
/// admitted, seven are refused `concurrency_busy`.
#[test]
fn refuse_admits_one_of_eight() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "refuse", Some("{{params.account}}"));
    fs::write(root.join("hold"), "").unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let (tx, rx) = mpsc::channel::<Result<RunResult, apb_engine::error::EngineError>>();
    for _ in 0..8 {
        let barrier = std::sync::Arc::clone(&barrier);
        let tx = tx.clone();
        std::thread::spawn({
            let root = root.to_path_buf();
            move || {
                barrier.wait();
                let _ = tx.send(run(&root, "ck", None, keyed("acme")));
            }
        });
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut busy = 0;
    let mut left = 8;
    while left > 1 {
        let msg = rx.recv_timeout(deadline - Instant::now());
        match msg {
            Ok(Err(e)) if e.to_string().contains("concurrency_busy") => {
                busy += 1;
                left -= 1;
            }
            Ok(other) => panic!("unexpected early outcome: {other:?}"),
            Err(_) => panic!("timed out waiting for the seven refusals"),
        }
    }
    assert_eq!(busy, 7, "exactly seven must be refused");
    // The admitted run is the one thread still going: free the hold and
    // read its answer when it lands.
    fs::remove_file(root.join("hold")).unwrap();
    let admitted = rx.recv().unwrap();
    let (id, status) = wait_terminal(root, "ck-", "the admitted run to finish");
    assert_eq!(status, RunStatus::Succeeded);
    assert!(
        admitted.is_ok(),
        "the admitted run answers Ok: {admitted:?}"
    );
    assert_eq!(id, admitted.unwrap().run_id);
}

/// 13: a param value that looks like a credential refuses the start,
/// naming the param, never the value, and no file under .apb holds it.
#[test]
fn a_sensitive_key_value_is_refused() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "refuse", Some("{{params.account}}"));
    let secret = format!("ghp_{}", "a".repeat(36));
    let e = run(root, "ck", None, keyed(&secret)).unwrap_err();
    assert!(e.to_string().contains("concurrency_key_sensitive"), "{e}");
    assert!(e.to_string().contains("account"), "{e}");
    assert!(!e.to_string().contains("ghp_"), "{e}");
    // No run directory was left behind, and the value is in no file.
    let runs = fs::read_dir(root.join(".apb/runs"))
        .map(|rd| rd.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(runs, 0);
    fn walk(dir: &Path, needle: &str) -> bool {
        let mut found = false;
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.filter_map(Result::ok) {
                let p = e.path();
                if p.is_dir() {
                    found |= walk(&p, needle);
                } else if let Ok(bytes) = fs::read(&p) {
                    found |= String::from_utf8_lossy(&bytes).contains(needle);
                }
            }
        }
        found
    }
    assert!(!walk(&root.join(".apb"), &secret), "the value leaked");
}

/// 17: a child ignores its own block: the child runs, the parent ends
/// `succeeded`.
#[test]
fn a_child_ignores_its_own_block() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "refuse", Some("{{params.account}}"));
    // The parent holds the key; the child (opts.parent_run set) starts
    // through the engine directly and takes no slot, so it runs while the
    // parent holds the key.
    fs::write(root.join("hold"), "").unwrap();
    let parent = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "the parent's node");
    let child = std::thread::spawn({
        let root = root.to_path_buf();
        move || {
            let opts = RunOptions {
                parent_run: Some("ck-parent".into()),
                params: [("account".to_string(), "acme".to_string())]
                    .into_iter()
                    .collect(),
                workdir_queue_wait: Some(Duration::from_secs(30)),
                ..RunOptions::default()
            };
            run(&root, "ck", None, opts)
        }
    });
    // The child is admitted (a run directory of its own) while the parent
    // holds the key: the child ignores the block.
    let start = Instant::now();
    loop {
        if run_ids(root, "ck-").len() >= 2 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out waiting for the child's admission"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    fs::remove_file(root.join("hold")).unwrap();
    let parent_id = run_ids(root, "ck-")
        .into_iter()
        .find(|id| {
            read_all(&root.join(".apb/runs").join(id))
                .map(|ev| {
                    ev.iter().any(|e| {
                        matches!(
                            &e.payload,
                            EventPayload::RunProvenance {
                                concurrency: Some(_),
                                ..
                            }
                        )
                    })
                })
                .unwrap_or(false)
        })
        .expect("the parent's run directory");
    let parent_status = wait_run(root, &parent_id, "the parent to finish");
    assert_eq!(parent_status, RunStatus::Succeeded);
    let _ = parent.join().unwrap();
    let child = child.join().unwrap().unwrap();
    assert_eq!(child.outcome, RunStatus::Succeeded);
    let _ = child;
}

/// 20: a refused keyed start frees its start key: no run directory was
/// left, and the same key runs after the holder ends.
#[test]
fn a_refused_keyed_start_frees_its_start_key() {
    use apb_core::scope::Scope;
    use apb_core::signed_store;
    use apb_core::start_key::{
        REQUEST_V, StartKey, StartKeyEntry, StartKeyRequest, StartKeysFile, StartRequest,
        StartRequestFields,
    };
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "refuse", Some("{{params.account}}"));
    fs::write(root.join("hold"), "").unwrap();

    let holder = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "the holder's node");

    // A keyed start (start key e1) of the busy key: refused, no run left.
    let req = StartKeyRequest {
        key: StartKey::parse("e1").unwrap(),
        reuse: false,
        request: StartRequest::capture(&StartRequestFields {
            v: REQUEST_V,
            params: [("account".to_string(), "acme".to_string())]
                .into_iter()
                .collect(),
            supervision: "autonomous".into(),
            cache: "auto".into(),
            ..Default::default()
        })
        .unwrap(),
    };
    let opts = RunOptions {
        start_key: Some(req.clone()),
        params: [("account".to_string(), "acme".to_string())]
            .into_iter()
            .collect(),
        ..RunOptions::default()
    };
    let e = run(root, "ck", None, opts).unwrap_err();
    assert!(e.to_string().contains("concurrency_busy"), "{e}");
    let runs = fs::read_dir(root.join(".apb/runs"))
        .map(|rd| rd.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(runs, 1, "the refused start left no run directory");
    // The start key stays free: its entry was released with the run.
    let file = signed_store::read::<StartKeysFile>(root, Scope::Project, "ck").unwrap();
    assert!(
        file.file
            .entries
            .iter()
            .all(|e: &StartKeyEntry| e.state != apb_core::start_key::EntryState::Bound),
        "no entry may be bound to a refused start"
    );

    fs::remove_file(root.join("hold")).unwrap();
    let (_, holder_status) = wait_terminal(root, "ck-", "the holder to finish");
    assert_eq!(holder_status, RunStatus::Succeeded);
    let holder_id = holder.join().unwrap().unwrap().run_id;
    let _ = holder_id;

    // The same event with e1 now starts.
    let req2 = StartKeyRequest {
        key: StartKey::parse("e1").unwrap(),
        reuse: false,
        request: StartRequest::capture(&StartRequestFields {
            v: REQUEST_V,
            params: [("account".to_string(), "acme".to_string())]
                .into_iter()
                .collect(),
            supervision: "autonomous".into(),
            cache: "auto".into(),
            ..Default::default()
        })
        .unwrap(),
    };
    let opts = RunOptions {
        start_key: Some(req2),
        params: [("account".to_string(), "acme".to_string())]
            .into_iter()
            .collect(),
        ..RunOptions::default()
    };
    let res = run(root, "ck", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}

/// 21: a queued run ends at its deadline, and the slot no longer names it.
#[test]
fn a_queued_run_ends_at_its_deadline() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "queue_latest", Some("{{params.account}}"));
    fs::write(root.join("hold"), "").unwrap();

    let holder = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "the holder's node");
    let holder_id = run_ids(root, "ck-")
        .pop()
        .expect("the holder's run directory exists");

    let mut opts = keyed("acme");
    opts.deadline_ms = Some(1_500);
    // The start is admitted into the key queue and ends there: the caller
    // sees the deadline conflict, the journal holds the abort.
    let e = run(root, "ck", None, opts).unwrap_err();
    assert!(
        e.to_string().contains("deadline_exceeded"),
        "the queued run ends at its deadline: {e}"
    );
    // The slot no longer names the ended run once a next access heals it:
    // a third start queues, becomes the waiter, and the ended one is gone.
    let before: Vec<String> = fs::read_dir(root.join(".apb/runs"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let third = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    let start = Instant::now();
    loop {
        let slots = apb_engine::concurrency::read_slots_for_test(root);
        let replaced = slots.len() == 1
            && slots[0]
                .waiting
                .as_ref()
                .is_some_and(|h| !before.contains(&h.run_id));
        if replaced {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out waiting for the third start to replace the ended one"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fs::remove_file(root.join("hold")).unwrap();
    let holder_status = wait_run(root, &holder_id, "the holder to finish");
    assert_eq!(holder_status, RunStatus::Succeeded);
    let _ = holder.join().unwrap();
    let third = third.join().unwrap().unwrap();
    assert_eq!(third.outcome, RunStatus::Succeeded);
}

/// 22: a detached queued run runs after the holder.
#[test]
fn a_detached_queued_run_runs_after_the_holder() {
    let (dir, _lock) = cfg();
    let root = dir.path();
    let _cfg_var = EnvVar::set("APB_CONFIG_DIR", root.join("cfg"));
    keyed_project(root, "ck", "queue_latest", Some("{{params.account}}"));
    fs::write(root.join("hold"), "").unwrap();

    let holder = std::thread::spawn({
        let root = root.to_path_buf();
        move || run(&root, "ck", None, keyed("acme"))
    });
    wait_started(root, "the holder's node");

    // The engine-side half of a detached start: preparation returns a run
    // that holds a place in the slot, and its driver waits in
    // drive_run_from_dir (the real `apb __drive-run` child is covered
    // from the CLI crate, the only one that can re-exec the binary).
    let b_id = {
        let prepared =
            apb_engine::scheduler::prepare_supervised_background(root, "ck", None, keyed("acme"))
                .unwrap();
        prepared.run_id().to_string()
    };
    let driver = std::thread::spawn({
        let root = root.to_path_buf();
        let b = b_id.clone();
        move || apb_engine::scheduler::drive_run_from_dir(&root, &b)
    });
    fs::remove_file(root.join("hold")).unwrap();
    let (_, holder_status) = wait_terminal(root, "ck-", "the holder to finish");
    assert_eq!(holder_status, RunStatus::Succeeded);
    // B's driver waits in drive_run_from_dir and runs after A.
    let res = driver.join().unwrap().unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let _ = holder.join().unwrap();
}
