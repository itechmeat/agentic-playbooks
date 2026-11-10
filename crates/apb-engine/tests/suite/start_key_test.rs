//! The start key (routine operations, D1), through the engine: a repeat
//! answers the run already started, another request is a conflict, a
//! failed run holds its key until a person reuses it, the store keeps no
//! key and refuses a file that does not verify, and the pending entry's
//! owner, never its age, decides while preparation runs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::signed_store::{self, Unsigned};
use apb_core::start_key::{
    EntryState, REQUEST_V, StartKey, StartKeyEntry, StartKeyRequest, StartKeysFile, StartRequest,
    StartRequestFields,
};
use apb_engine::error::EngineError;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::start_key::{Lookup, lookup};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

/// The env lock and a config dir with an installation key.
struct Cfg {
    _vars: Vec<EnvVar>,
    dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn cfg() -> Cfg {
    let lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let vars = vec![
        EnvVar::set("APB_CONFIG_DIR", dir.path()),
        EnvVar::remove("APB_RUN_ID"),
        EnvVar::remove("APB_HOST_TASK"),
    ];
    apb_core::run_origin::ensure_key().unwrap();
    Cfg {
        _vars: vars,
        dir,
        _lock: lock,
    }
}

/// `start -> s -> done`, `s` a script that fails while `<root>/fail`
/// exists and waits while `<root>/hold` exists.
fn project(id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!(
            "schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: s, type: script, script: \"scripts/s.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: s }}\n  - {{ from: s, to: done }}\n"
        ),
    )
    .unwrap();
    let d = root.display();
    fs::write(
        vdir.join("scripts/s.sh"),
        format!(
            "touch \"{d}/s_running\"\nwhile [ -f \"{d}/hold\" ]; do sleep 0.05; done\nif [ -f \"{d}/fail\" ]; then exit 1; fi\necho ok\n"
        ),
    )
    .unwrap();
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .unwrap();
    common::seed_main(root);
    dir
}

fn request(key: &str, f: StartRequestFields, reuse: bool) -> StartKeyRequest {
    StartKeyRequest {
        key: StartKey::parse(key).unwrap(),
        reuse,
        request: StartRequest::capture(&f).unwrap(),
    }
}

fn fields() -> StartRequestFields {
    StartRequestFields {
        v: REQUEST_V,
        supervision: "autonomous".into(),
        cache: "auto".into(),
        ..Default::default()
    }
}

fn keyed(key: &str) -> RunOptions {
    RunOptions {
        start_key: Some(request(key, fields(), false)),
        ..RunOptions::default()
    }
}

fn runs_of(root: &Path, id: &str) -> Vec<String> {
    fs::read_dir(root.join(".apb/runs"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&format!("{id}-")))
                .collect()
        })
        .unwrap_or_default()
}

fn already(r: Result<apb_engine::scheduler::RunResult, EngineError>) -> String {
    match r {
        Err(EngineError::AlreadyStarted(id)) => id,
        other => panic!("not already started: {other:?}"),
    }
}

fn store_path(root: &Path, id: &str) -> PathBuf {
    signed_store::file::<StartKeysFile>(root, Scope::Project, id)
}

#[test]
fn a_repeat_answers_the_existing_run() {
    let _c = cfg();
    let p = project("pk");
    let root = p.path();
    let first = run(root, "pk", None, keyed("event-1")).unwrap();
    assert_eq!(first.outcome, RunStatus::Succeeded);
    assert_eq!(
        already(run(root, "pk", None, keyed("event-1"))),
        first.run_id
    );
    let req = request("event-1", fields(), false);
    assert_eq!(
        lookup(root, "project", "pk", &req).unwrap(),
        Lookup::Started {
            run_id: first.run_id.clone(),
            status: RunStatus::Succeeded
        }
    );
    assert_eq!(runs_of(root, "pk").len(), 1);
    // Another key is another event.
    run(root, "pk", None, keyed("event-2")).unwrap();
    assert_eq!(runs_of(root, "pk").len(), 2);
    // The provenance shows a prefix of the key's MAC, never the key.
    let ev = apb_engine::event::read_all(&root.join(".apb/runs").join(&first.run_id)).unwrap();
    let shown = ev.iter().find_map(|e| match &e.payload {
        apb_engine::event::EventPayload::RunProvenance { start_key, .. } => start_key.clone(),
        _ => None,
    });
    assert_eq!(shown.map(|s| s.len()), Some(12));
}

#[test]
fn a_repeat_while_the_tree_is_held_answers_already_started() {
    let _c = cfg();
    let p = project("pt");
    let root = p.path().to_path_buf();
    fs::write(root.join("hold"), "").unwrap();
    let (r2, root2) = (std::sync::mpsc::channel(), root.clone());
    std::thread::spawn(move || {
        let _ = r2.0.send(run(&root2, "pt", None, keyed("k")));
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !root.join("s_running").is_file() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let first = runs_of(&root, "pt").pop().unwrap();
    assert_eq!(already(run(&root, "pt", None, keyed("k"))), first);
    fs::remove_file(root.join("hold")).unwrap();
    r2.1.recv().unwrap().unwrap();
}

#[test]
fn eight_concurrent_starts_make_one_run() {
    let _c = cfg();
    let p = project("pc");
    let root = Arc::new(p.path().to_path_buf());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                loop {
                    match run(&root, "pc", None, keyed("k")) {
                        Err(EngineError::Conflict(m)) if m.starts_with("start_key_busy") => {
                            continue;
                        }
                        other => return other.map(|r| r.run_id),
                    }
                }
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let started: Vec<&String> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(started.len(), 1, "{results:?}");
    for r in &results {
        if let Err(e) = r {
            assert!(
                matches!(e, EngineError::AlreadyStarted(id) if id == started[0]),
                "{e:?}"
            );
        }
    }
    assert_eq!(runs_of(&root, "pc").len(), 1);
}

/// Another request under the key is a conflict, whatever field differs.
#[test]
fn another_request_is_a_conflict() {
    let _c = cfg();
    let p = project("pr");
    let root = p.path();
    let first = run(root, "pr", None, keyed("k")).unwrap();
    for (what, f) in [
        (
            "params",
            StartRequestFields {
                params: BTreeMap::from([("a".to_string(), "1".to_string())]),
                ..fields()
            },
        ),
        (
            "version",
            StartRequestFields {
                version: Some("1.0.0".into()),
                ..fields()
            },
        ),
        (
            "external_data",
            StartRequestFields {
                external_data: true,
                ..fields()
            },
        ),
    ] {
        let opts = RunOptions {
            start_key: Some(request("k", f.clone(), false)),
            ..RunOptions::default()
        };
        match run(root, "pr", None, opts) {
            Err(EngineError::Conflict(m)) => {
                assert!(m.starts_with("start_key_conflict"), "{what}: {m}");
                assert!(m.contains(&first.run_id), "{what}: {m}");
            }
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(
            lookup(root, "project", "pr", &request("k", f, false)).unwrap(),
            Lookup::Conflict {
                run_id: first.run_id.clone()
            }
        );
    }
    assert_eq!(runs_of(root, "pr").len(), 1);
}

#[test]
fn a_failed_run_holds_its_key_until_reuse() {
    let _c = cfg();
    let p = project("pf");
    let root = p.path();
    fs::write(root.join("fail"), "").unwrap();
    let first = run(root, "pf", None, keyed("k")).unwrap();
    assert_eq!(first.outcome, RunStatus::Failed);
    assert_eq!(
        lookup(root, "project", "pf", &request("k", fields(), false)).unwrap(),
        Lookup::Started {
            run_id: first.run_id.clone(),
            status: RunStatus::Failed
        }
    );
    assert_eq!(already(run(root, "pf", None, keyed("k"))), first.run_id);
    fs::remove_file(root.join("fail")).unwrap();
    let reuse = RunOptions {
        start_key: Some(request("k", fields(), true)),
        ..RunOptions::default()
    };
    let second = run(root, "pf", None, reuse).unwrap();
    assert_eq!(second.outcome, RunStatus::Succeeded);
    assert_ne!(second.run_id, first.run_id);
    let f = signed_store::read::<StartKeysFile>(root, Scope::Project, "pf").unwrap();
    assert_eq!(f.file.entries.len(), 1);
    assert_eq!(f.file.entries[0].reused, 1);
    assert_eq!(already(run(root, "pf", None, keyed("k"))), second.run_id);
}

#[test]
fn reuse_of_a_succeeded_run_is_refused() {
    let _c = cfg();
    let p = project("ps");
    let root = p.path();
    let first = run(root, "ps", None, keyed("k")).unwrap();
    let reuse = RunOptions {
        start_key: Some(request("k", fields(), true)),
        ..RunOptions::default()
    };
    match run(root, "ps", None, reuse) {
        Err(EngineError::Conflict(m)) => {
            assert!(m.starts_with("start_key_reuse_refused"), "{m}");
            assert!(m.contains(&first.run_id) && m.contains("succeeded"), "{m}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn reuse_from_a_run_step_is_refused() {
    let _c = cfg();
    let p = project("pu");
    let root = p.path();
    fs::write(root.join("fail"), "").unwrap();
    run(root, "pu", None, keyed("k")).unwrap();
    let _step = EnvVar::set("APB_RUN_ID", "some-step-run");
    let reuse = RunOptions {
        start_key: Some(request("k", fields(), true)),
        ..RunOptions::default()
    };
    match run(root, "pu", None, reuse) {
        Err(EngineError::Conflict(m)) => assert!(m.starts_with("start_key_reuse_refused"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_deleted_run_frees_its_key() {
    let _c = cfg();
    let p = project("pd");
    let root = p.path();
    let first = run(root, "pd", None, keyed("k")).unwrap();
    fs::remove_dir_all(root.join(".apb/runs").join(&first.run_id)).unwrap();
    let second = run(root, "pd", None, keyed("k")).unwrap();
    assert_ne!(second.run_id, first.run_id);
}

#[test]
fn the_store_holds_no_key() {
    let _c = cfg();
    let p = project("pn");
    let root = p.path();
    let key = "order-77812-mail";
    run(root, "pn", None, keyed(key)).unwrap();
    let plain = {
        use sha2::Digest;
        apb_core::content::hex_lower(&sha2::Sha256::digest(key.as_bytes()))
    };
    let mut stack = vec![root.join(".apb/start-keys")];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().filter_map(Result::ok) {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            seen += 1;
            let body = String::from_utf8_lossy(&fs::read(&path).unwrap()).to_string();
            assert!(!body.contains(key), "{} holds the key", path.display());
            assert!(
                !body.contains(&plain),
                "{} holds its sha256",
                path.display()
            );
        }
    }
    assert!(seen > 0);
    for f in ["manifest.yaml", "events.jsonl", "run.yaml"] {
        let run = runs_of(root, "pn").pop().unwrap();
        let body = fs::read_to_string(root.join(".apb/runs").join(run).join(f)).unwrap_or_default();
        assert!(!body.contains(key), "{f} holds the key");
    }
}

#[test]
fn a_tampered_store_refuses_keyed_starts() {
    let _c = cfg();
    let p = project("pz");
    let root = p.path();
    let path = store_path(root, "pz");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for bytes in [
        b"not json at all".to_vec(),
        br#"{"schema":1,"scope":"project","playbook":"pz","entries":[]}"#.to_vec(),
    ] {
        fs::write(&path, &bytes).unwrap();
        match run(root, "pz", None, keyed("k")) {
            Err(EngineError::Conflict(m)) => {
                assert!(m.starts_with("start_key_state_unreadable"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        assert!(lookup(root, "project", "pz", &request("k", fields(), false)).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes, "the file was replaced");
    }
    assert!(runs_of(root, "pz").is_empty());
    let unkeyed = run(root, "pz", None, RunOptions::default()).unwrap();
    assert_eq!(unkeyed.outcome, RunStatus::Succeeded);
    assert!(apb_engine::start_key::doctor_line(root).is_some());
}

/// Writes `entry` into the signed file of `id` (as this installation).
fn put(root: &Path, id: &str, entry: StartKeyEntry) {
    signed_store::transact(
        root,
        Scope::Project,
        id,
        Unsigned::Refuse,
        |f: &mut StartKeysFile| -> Result<((), bool), signed_store::StoreError> {
            f.entries.push(entry);
            Ok(((), true))
        },
    )
    .unwrap();
}

fn entry_for(
    key: &str,
    run_id: &str,
    state: EntryState,
    reservation: Option<&str>,
) -> StartKeyEntry {
    let req = request(key, fields(), false);
    StartKeyEntry {
        key: apb_core::start_key::key_tag(&req.key).unwrap(),
        request: req.request.tag().to_string(),
        state,
        reservation: reservation.map(str::to_string),
        run_id: Some(run_id.to_string()),
        at_ms: apb_core::clock::now_ms_u64(),
        reused: 0,
    }
}

#[test]
fn a_forged_entry_is_void() {
    let _c = cfg();
    let p = project("pg");
    let other = project("other");
    let root = p.path();
    // A run of another playbook, and a directory without an origin stamp.
    let foreign = run(other.path(), "other", None, RunOptions::default()).unwrap();
    let moved = root.join(".apb/runs").join(&foreign.run_id);
    fs::create_dir_all(root.join(".apb/runs")).unwrap();
    fs::rename(other.path().join(".apb/runs").join(&foreign.run_id), &moved).unwrap();
    put(
        root,
        "pg",
        entry_for("k1", &foreign.run_id, EntryState::Bound, None),
    );
    fs::create_dir_all(root.join(".apb/runs/pg-forged")).unwrap();
    fs::write(
        root.join(".apb/runs/pg-forged/events.jsonl"),
        "{\"seq\":1,\"ts\":1,\"type\":\"run_started\",\"playbook\":\"pg\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();
    put(
        root,
        "pg",
        entry_for("k2", "pg-forged", EntryState::Bound, None),
    );
    for key in ["k1", "k2"] {
        let res = run(root, "pg", None, keyed(key)).unwrap();
        assert_eq!(res.outcome, RunStatus::Succeeded, "{key}");
    }
}

/// A pending entry whose owner is gone and whose run directory has an
/// empty journal is void: the keyed start runs.
#[test]
fn a_dead_owner_without_run_started_frees_its_key() {
    let _c = cfg();
    let p = project("po");
    let root = p.path();
    let dir = root.join(".apb/runs/po-dead");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), "").unwrap();
    apb_core::run_origin::stamp(&dir, "po-dead").unwrap();
    put(
        root,
        "po",
        entry_for("k", "po-dead", EntryState::Pending, Some("gone")),
    );
    let res = run(root, "po", None, keyed("k")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    assert_ne!(res.run_id, "po-dead");
}

/// Holds a keyed start of `id` right after its claim's `attach` in a
/// thread; returns the hook and the start's result channel.
fn held_start(
    root: &Path,
    id: &str,
) -> (
    Arc<std::sync::Barrier>,
    std::sync::mpsc::Receiver<Result<apb_engine::scheduler::RunResult, EngineError>>,
) {
    let hook = apb_engine::start_key::test_hooks::hold_after_attach(root);
    let (tx, rx) = std::sync::mpsc::channel();
    let (root2, id2) = (root.to_path_buf(), id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run(&root2, &id2, None, keyed("k")));
    });
    hook.wait();
    (hook, rx)
}

/// While preparation is held after `attach` (before the origin stamp), a
/// lookup and a keyed start both answer the preparing run; once released
/// exactly one run exists.
#[test]
fn a_lookup_during_preparation_answers_the_preparing_run() {
    let _c = cfg();
    let p = project("pl");
    let root = p.path();
    let (hook, rx) = held_start(root, "pl");
    let preparing = runs_of(root, "pl").pop().expect("the allocated run");
    assert_eq!(
        lookup(root, "project", "pl", &request("k", fields(), false)).unwrap(),
        Lookup::Started {
            run_id: preparing.clone(),
            status: RunStatus::Running
        }
    );
    assert_eq!(already(run(root, "pl", None, keyed("k"))), preparing);
    apb_engine::start_key::test_hooks::clear(root);
    hook.wait();
    let res = rx.recv().unwrap().unwrap();
    assert_eq!(res.run_id, preparing);
    assert_eq!(runs_of(root, "pl").len(), 1);
}

/// A slow preparation keeps its key: its pending entry is two minutes old,
/// its owner alive, and a second keyed start answers its run.
#[test]
fn a_slow_preparation_keeps_its_key() {
    let _c = cfg();
    let p = project("pw");
    let root = p.path();
    let (hook, rx) = held_start(root, "pw");
    signed_store::transact(
        root,
        Scope::Project,
        "pw",
        Unsigned::Refuse,
        |f: &mut StartKeysFile| -> Result<((), bool), signed_store::StoreError> {
            for e in &mut f.entries {
                e.at_ms = e.at_ms.saturating_sub(120_000);
            }
            Ok(((), true))
        },
    )
    .unwrap();
    let preparing = runs_of(root, "pw").pop().unwrap();
    assert_eq!(already(run(root, "pw", None, keyed("k"))), preparing);
    apb_engine::start_key::test_hooks::clear(root);
    hook.wait();
    rx.recv().unwrap().unwrap();
    assert_eq!(runs_of(root, "pw").len(), 1);
}

/// A preparation that fails after `allocate_run` and before provenance
/// (the launch check of its commit refuses) holds the key as a failed
/// run: the repeat answers it.
#[test]
fn a_failure_before_provenance_holds_the_key() {
    let c = cfg();
    let p = project("pp");
    let root = p.path();
    let (hook, rx) = held_start(root, "pp");
    let policy = c.dir.path().join("policy.yaml");
    fs::write(&policy, "launch:\n  agent: deny\n").unwrap();
    apb_engine::start_key::test_hooks::clear(root);
    hook.wait();
    let failed = rx.recv().unwrap();
    assert!(failed.is_err(), "{failed:?}");
    fs::remove_file(&policy).unwrap();
    let first = runs_of(root, "pp").pop().expect("the failed run");
    assert_eq!(
        lookup(root, "project", "pp", &request("k", fields(), false)).unwrap(),
        Lookup::Started {
            run_id: first.clone(),
            status: RunStatus::Failed
        }
    );
    assert_eq!(already(run(root, "pp", None, keyed("k"))), first);
}

/// A start refused before `allocate_run` (the tree busy, no queue) leaves
/// no entry: the repeat after the tree is free starts.
#[test]
fn a_refusal_before_allocation_leaves_the_key_free() {
    let _c = cfg();
    let p = project("pb");
    let root = p.path();
    let holder = apb_engine::workdir::acquire(root, false).unwrap().unwrap();
    match run(root, "pb", None, keyed("k")) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("{other:?}"),
    }
    drop(holder);
    let f = signed_store::read::<StartKeysFile>(root, Scope::Project, "pb").unwrap();
    assert!(f.file.entries.is_empty(), "{:?}", f.file.entries);
    let res = run(root, "pb", None, keyed("k")).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
}
