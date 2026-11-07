//! End to end: what `run.previous_success.*` renders across the runs of
//! one routine, driven by a stand-in agent that appends its prompt to a
//! file.

use apb_core::registry::init_project;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, list_runs, resume, run};
use apb_engine::state::RunStatus;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::common;

const ROUTINE: &str = r#"schema: 2
id: routine
name: Routine
version: 1.0.0
defaults: { profile: main }
previous_success:
  when_none:
    started_at: "2026-01-01T00:00:00Z"
nodes:
  - { id: start, type: start }
  - { id: collect, type: agent_task, prompt: "window {{run.previous_success.started_at}} id [{{run.previous_success.id}}] items {{run.previous_success.nodes.collect.output.items}}", cache: auto, accept_untrusted: "the window is data the prompt quotes" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: collect }
  - { from: collect, to: done }
"#;

const PLAIN: &str = r#"schema: 2
id: plain
name: Plain
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "work" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

const PARENT: &str = r#"schema: 2
id: parent
name: Parent
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: sub, type: playbook, playbook: routine }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: sub }
  - { from: sub, to: done }
"#;

fn seed(root: &Path, id: &str, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(format!(".apb/playbooks/{id}/1.0.0"));
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(format!(".apb/playbooks/{id}/current")), "1.0.0").unwrap();
    common::seed_main(root);
}

fn write_agent(root: &Path, name: &str, body: &str) -> String {
    let path = root.join(name);
    common::write_sync(&path, body);
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

/// A stand-in agent that appends its prompt between `===PROMPT===`
/// markers and answers with one item.
fn ok_agent(root: &Path, prompts: &Path) -> String {
    write_agent(
        root,
        "ok.sh",
        &format!(
            "#!/bin/sh\nfor last; do :; done\nprintf '%s\\n' '===PROMPT===' >> '{p}'\nprintf '%s\\n' \"$last\" >> '{p}'\necho '{{\"items\": [1]}}'\n",
            p = prompts.display()
        ),
    )
}

/// The same, but it fails while `marker` is absent and creates it, so a
/// run with a fresh marker fails and a later one succeeds.
fn flaky_agent(root: &Path, prompts: &Path, marker: &Path) -> String {
    write_agent(
        root,
        "flaky.sh",
        &format!(
            "#!/bin/sh\nfor last; do :; done\nprintf '%s\\n' '===PROMPT===' >> '{p}'\nprintf '%s\\n' \"$last\" >> '{p}'\nif [ -f '{m}' ]; then echo '{{\"items\": [2]}}'; exit 0; fi\ntouch '{m}'\necho bad 1>&2\nexit 1\n",
            p = prompts.display(),
            m = marker.display()
        ),
    )
}

/// The prompts the agent appended, in order.
fn prompts(prompts: &Path) -> Vec<String> {
    fs::read_to_string(prompts)
        .unwrap()
        .split("===PROMPT===")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn started_at_of(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| matches!(&e.payload, EventPayload::RunStarted { .. }).then_some(e.ts as u64))
        .map(apb_core::clock::iso_utc)
        .unwrap()
}

/// Drives one routine run under a config dir that has an installation
/// key, so the run directories carry a verifiable origin stamp.
fn drive(root: &Path, cfg: &Path, agent: &str, opts: RunOptions) -> (String, RunStatus) {
    let _guard = common::env_lock();
    // SAFETY: the env lock keeps every other env-touching test out.
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg);
    apb_core::run_origin::ensure_key().unwrap();
    unsafe { std::env::set_var("APB_AGENT_CMD", agent) };
    let res = run(root, "routine", None, opts);
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    let res = res.unwrap();
    (res.run_id, res.outcome)
}

/// The windows of four straight runs: the first renders the `when_none`
/// time and an empty id; the next renders the previous success; a failed
/// run does not move the window.
#[test]
fn windows_move_and_a_failure_keeps_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "routine", ROUTINE);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);

    let (r1, s1) = drive(root, cfg.path(), &ok, RunOptions::default());
    assert_eq!(s1, RunStatus::Succeeded);
    let ev1 = read_all(&root.join(".apb/runs").join(&r1)).unwrap();
    let t1 = started_at_of(&ev1);
    let got = &prompts(&pf)[0];
    assert!(
        got.contains("window 2026-01-01T00:00:00Z id [] items"),
        "run 1 renders the when_none window: {got}"
    );

    let (r2, s2) = drive(root, cfg.path(), &ok, RunOptions::default());
    assert_eq!(s2, RunStatus::Succeeded);
    let got = &prompts(&pf)[1];
    assert!(
        got.contains(&format!("id [{r1}]")) && got.contains(&format!("window {t1}")),
        "run 2 renders run 1: {got}"
    );
    assert!(!got.contains("2026-01-01T00:00:00Z"), "{got}");
    assert!(
        got.contains("items [1]"),
        "run 2 renders run 1's items: {got}"
    );

    // A failed run is not a success: the window stays on run 2.
    let marker = root.join("fail.marker");
    let flaky = flaky_agent(root, &pf, &marker);
    let (r3, s3) = drive(root, cfg.path(), &flaky, RunOptions::default());
    assert_eq!(s3, RunStatus::Failed);

    let (_r4, s4) = drive(root, cfg.path(), &ok, RunOptions::default());
    assert_eq!(s4, RunStatus::Succeeded);
    let got = &prompts(&pf)[3];
    assert!(
        got.contains(&format!("id [{r2}]")),
        "run 4 keeps run 2 as its window: {got}"
    );
    assert!(!got.contains(&format!("id [{r3}]")), "{got}");
}

/// A resumed run renders the window its start recorded, even though a
/// later run of the playbook succeeded in the meantime.
#[test]
fn a_resume_renders_the_window_its_start_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "routine", ROUTINE);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);
    let marker = root.join("fail.marker");
    let flaky = flaky_agent(root, &pf, &marker);

    let (r1, _) = drive(root, cfg.path(), &ok, RunOptions::default());
    // A run that fails after its first node succeeded, so its journal can
    // be cut to the crash state.
    let (r2, s2) = drive(root, cfg.path(), &flaky, RunOptions::default());
    assert_eq!(s2, RunStatus::Failed);
    // A newer success that a resume must not pick up.
    let (r3, _) = drive(root, cfg.path(), &ok, RunOptions::default());

    // Cut run 2's journal to just after its first node: the crash state.
    let run2_dir = root.join(".apb/runs").join(&r2);
    let events = read_all(&run2_dir).unwrap();
    let keep = events
        .iter()
        .position(
            |e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == "start"),
        )
        .unwrap();
    let cut: String = events[..=keep]
        .iter()
        .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
        .collect();
    fs::write(run2_dir.join("events.jsonl"), cut).unwrap();

    let _guard = common::env_lock();
    // SAFETY: the env lock keeps every other env-touching test out.
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    unsafe { std::env::set_var("APB_AGENT_CMD", &ok) };
    let resumed = resume(root, &r2, None).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    assert_eq!(resumed.outcome, RunStatus::Succeeded);

    let got = prompts(&pf).last().unwrap().clone();
    assert!(
        got.contains(&format!("id [{r1}]")),
        "the resumed run renders run 1, its start's window: {got}"
    );
    assert!(!got.contains(&format!("id [{r3}]")), "{got}");
}

/// A run continued from a failed run keeps the predecessor's record.
#[test]
fn a_continued_run_keeps_the_predecessors_record() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "routine", ROUTINE);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);
    let marker = root.join("fail.marker");
    let flaky = flaky_agent(root, &pf, &marker);

    let (r1, _) = drive(root, cfg.path(), &ok, RunOptions::default());
    let (_r2, s2) = drive(root, cfg.path(), &flaky, RunOptions::default());
    assert_eq!(s2, RunStatus::Failed);

    let (_r3, s3) = drive(
        root,
        cfg.path(),
        &ok,
        RunOptions {
            continued_from: Some(_r2.clone()),
            ..Default::default()
        },
    );
    assert_eq!(s3, RunStatus::Succeeded);
    let got = prompts(&pf).last().unwrap().clone();
    assert!(
        got.contains(&format!("id [{r1}]")),
        "the continued run keeps its predecessor's window: {got}"
    );
}

/// A child run started by a parent playbook renders the none window,
/// although a top-level run of the same playbook succeeded.
#[test]
fn a_child_run_renders_the_none_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "routine", ROUTINE);
    seed(root, "parent", PARENT);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);

    let (_r1, s1) = drive(root, cfg.path(), &ok, RunOptions::default());
    assert_eq!(s1, RunStatus::Succeeded);

    let _guard = common::env_lock();
    // SAFETY: the env lock keeps every other env-touching test out.
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    unsafe { std::env::set_var("APB_AGENT_CMD", &ok) };
    let res = run(root, "parent", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let got = prompts(&pf).last().unwrap().clone();
    assert!(
        got.contains("window 2026-01-01T00:00:00Z id [] items"),
        "the child renders the none window: {got}"
    );
}

/// A start after the latest success's journal was corrupted refuses with
/// `previous_success_unreadable` and leaves no run directory behind.
#[test]
fn an_unreadable_latest_success_refuses_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "routine", ROUTINE);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);

    let (_r1, s1) = drive(root, cfg.path(), &ok, RunOptions::default());
    assert_eq!(s1, RunStatus::Succeeded);
    // A line that is not JSON, in the middle: the journal does not read.
    let path = root.join(".apb/runs").join(&_r1).join("events.jsonl");
    let content = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    lines.insert(2, "not json".into());
    fs::write(&path, lines.join("\n") + "\n").unwrap();

    let runs_before = list_runs(root).unwrap().len();
    let err = {
        let _guard = common::env_lock();
        // SAFETY: the env lock keeps every other env-touching test out.
        let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
        apb_core::run_origin::ensure_key().unwrap();
        unsafe { std::env::set_var("APB_AGENT_CMD", &ok) };
        let res = run(root, "routine", None, RunOptions::default());
        unsafe { std::env::remove_var("APB_AGENT_CMD") };
        res.unwrap_err()
    };
    assert!(
        err.to_string().contains("previous_success_unreadable"),
        "{err}"
    );
    assert_eq!(list_runs(root).unwrap().len(), runs_before);
}

/// A definition that does not read the namespace records nothing: no
/// `previous_success` key on its provenance and no new guard.
#[test]
fn a_definition_that_does_not_read_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "plain", PLAIN);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);
    let _guard = common::env_lock();
    // SAFETY: the env lock keeps every other env-touching test out.
    let _cfg = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    apb_core::run_origin::ensure_key().unwrap();
    unsafe { std::env::set_var("APB_AGENT_CMD", &ok) };
    let res = run(root, "plain", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("APB_AGENT_CMD") };
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let events = read_all(&root.join(".apb/runs").join(&res.run_id)).unwrap();
    for e in &events {
        if let EventPayload::RunProvenance {
            previous_success, ..
        } = &e.payload
        {
            assert!(previous_success.is_none());
        }
        if let EventPayload::RunGuard { requires } = &e.payload {
            assert!(
                !requires.iter().any(|f| f == "previous_success"),
                "{requires:?}"
            );
        }
    }
}

/// With `cache: auto` the window is part of the key: run 2's key differs
/// from run 1's, so a cache hit across routine runs never replays a stale
/// window.
#[test]
fn the_cache_key_moves_with_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed(root, "routine", ROUTINE);
    // The cache fingerprint reads the tree through git, like the cache
    // tests seed it.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git runs")
    };
    std::fs::write(root.join(".gitignore"), ".apb/\nprompts\nfail.marker\n").unwrap();
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "c1"]);
    let cfg = tempfile::tempdir().unwrap();
    let pf = root.join("prompts");
    let ok = ok_agent(root, &pf);

    let cache = RunOptions {
        cache: apb_engine::run_config::CacheRunMode::Auto,
        ..Default::default()
    };
    let (_r1, s1) = drive(root, cfg.path(), &ok, cache);
    assert_eq!(s1, RunStatus::Succeeded);
    let ev1 = read_all(&root.join(".apb/runs").join(&_r1)).unwrap();
    let (_r2, s2) = drive(
        root,
        cfg.path(),
        &ok,
        RunOptions {
            cache: apb_engine::run_config::CacheRunMode::Auto,
            ..Default::default()
        },
    );
    assert_eq!(s2, RunStatus::Succeeded);
    let ev2 = read_all(&root.join(".apb/runs").join(&_r2)).unwrap();
    let key_of = |events: &[Event], take: fn(&EventPayload) -> Option<String>| {
        events.iter().find_map(|e| take(&e.payload))
    };
    // Run 1 records its key in some cache event; run 2 misses under a new
    // one. The exact event shape (Miss, Stored, Rejected with the reason
    // naming the mode) belongs to the cache tests; what matters here is
    // that the keys move with the window.
    let any_key = |p: &EventPayload| match p {
        EventPayload::NodeCacheMiss { node, key }
        | EventPayload::NodeCacheStored { node, key }
        | EventPayload::NodeCacheHit { node, key, .. }
            if node == "collect" =>
        {
            Some(key.clone())
        }
        EventPayload::NodeCacheRejected { node, .. } if node == "collect" => Some(String::new()),
        _ => None,
    };
    let k1 = key_of(&ev1, any_key).expect("run 1 records its cache key");
    let k2 = key_of(&ev2, any_key).expect("run 2 records its cache key");
    assert_ne!(k1, k2, "the window moves the key: {k1} == {k2}");
    assert!(
        !ev2.iter().any(
            |e| matches!(&e.payload, EventPayload::NodeCacheHit { node, .. } if node == "collect")
        ),
        "run 2 must not replay run 1's window"
    );
}
