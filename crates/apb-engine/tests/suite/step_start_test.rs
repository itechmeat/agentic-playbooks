//! Runs started from a run step (routine operations, D3), through the run
//! gate and the engine as the CLI and MCP start surfaces call them: the
//! new run's launch record names its starter, a child is never marked, the
//! token is never stored, a forged starter is not verified, the chain is
//! bounded across projects and not on resumes, a flagged starter flags the
//! start, and a starter in another workspace is named there.
//!
//! The starting runs are journals seeded and stamped as this installation
//! creates them; the step is this test process with the variables apb
//! gives a step (`APB_RUN_ID`, `APB_RUN_DIR`, `APB_NODE_ID`, and an
//! attempt's token).

use std::fs;
use std::path::{Path, PathBuf};

use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_engine::gate::launch::{Initiator, LaunchContext, LaunchKind};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;
use serde_json::{Value, json};

use crate::common::{self, EnvVar};

const TOKEN: &str = "tok-5f1c9e2a7b";

/// The env lock, a config dir with an installation key, and every step
/// variable cleared; dropped in reverse.
struct Step {
    vars: Vec<EnvVar>,
    _cfg: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Step {
    fn new() -> Self {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let mut vars = vec![EnvVar::set("APB_CONFIG_DIR", cfg.path())];
        for k in [
            "APB_RUN_ID",
            "APB_RUN_DIR",
            "APB_NODE_ID",
            "APB_ATTEMPT",
            "APB_ATTEMPT_TOKEN",
            "APB_HOST_TASK",
            "APB_ITEM_ID",
        ] {
            vars.push(EnvVar::remove(k));
        }
        apb_core::run_origin::ensure_key().unwrap();
        Step {
            vars,
            _cfg: cfg,
            _lock: lock,
        }
    }

    /// This process becomes a step of the run at `dir`, node `node`, with
    /// `token` when it is an agent's attempt.
    fn enter(&mut self, dir: &Path, node: &str, token: Option<&str>) {
        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
        self.vars.push(EnvVar::set("APB_RUN_ID", id));
        self.vars.push(EnvVar::set("APB_RUN_DIR", dir));
        self.vars.push(EnvVar::set("APB_NODE_ID", node));
        match token {
            Some(t) => self.vars.push(EnvVar::set("APB_ATTEMPT_TOKEN", t)),
            None => self.vars.push(EnvVar::remove("APB_ATTEMPT_TOKEN")),
        }
    }
}

const FIX: &str = "schema: 2\nid: fix\nname: fix\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: s, type: script, script: \"scripts/s.sh\", runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: s }\n  - { from: s, to: done }\n";

const PARENT: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: fix }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: done }\n";

/// A project with the playbooks `fix` and `parent`, registered.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    for (id, yaml) in [("fix", FIX), ("parent", PARENT)] {
        let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
        fs::create_dir_all(vdir.join("scripts")).unwrap();
        fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        fs::write(vdir.join("scripts/s.sh"), "echo ok\n").unwrap();
        fs::write(
            root.join(".apb/playbooks").join(id).join("current"),
            "1.0.0",
        )
        .unwrap();
    }
    // `bind_id`, not `touch`: `touch` registers nothing under `CI` or
    // `APB_NO_REGISTRY`, and the starter must be registered to be verified.
    apb_core::projects::bind_id(root).unwrap();
    dir
}

/// A starting run `id` under `root`: an open attempt of `triage` with the
/// token, stamped as created here, optionally flagged and at `chain`.
fn starter(root: &Path, id: &str, chain: Option<u32>, flagged: bool) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    let lines = [
        json!({"type": "run_started", "playbook": "triage", "version": "1.0.0", "external_data": flagged}),
        json!({"type": "node_started", "node": "triage", "attempt": 1}),
        json!({"type": "attempt_started", "node": "triage", "attempt": 2, "agent": "claude", "token": TOKEN}),
    ];
    let text: String = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut l = l.clone();
            l["seq"] = json!(i + 1);
            l["ts"] = json!(i + 1);
            format!("{l}\n")
        })
        .collect();
    fs::write(dir.join("events.jsonl"), text).unwrap();
    if let Some(chain) = chain {
        let manifest = json!({
            "profiles": [], "node_bindings": {},
            "launch": {
                "kind": "start", "initiator": "run_step:cli", "policy": "allow", "sources": [],
                "started_by": { "run_id": "earlier", "verified": true, "chain": chain }
            }
        });
        fs::write(dir.join("manifest.yaml"), manifest.to_string()).unwrap();
    }
    apb_core::run_origin::stamp(&dir, id).unwrap();
    dir
}

fn wref(id: &str) -> PlaybookRef {
    PlaybookRef {
        origin: Origin::Project { workspace_id: None },
        id: id.into(),
        version: None,
    }
}

fn step_ctx(kind: LaunchKind) -> LaunchContext {
    LaunchContext::new(Initiator::cli(false), kind)
}

/// Starts `id` in `root` the way a step's `apb run` does: the flag through
/// the surface rule, the run gate, then the engine.
fn start(root: &Path, id: &str, external_data: Option<bool>) -> Result<String, Value> {
    let flag = apb_engine::external_data::for_surface_start(root, external_data, None)
        .map_err(|r| r.body())?;
    let permit = apb_engine::gate::check_run(
        root,
        &wref(id),
        true,
        false,
        &step_ctx(LaunchKind::Start).with_external_data(flag),
    )?;
    let mut opts = RunOptions {
        external_data: flag,
        ..RunOptions::default()
    };
    permit.apply(&mut opts);
    let res = run(root, id, None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    Ok(res.run_id)
}

fn run_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".apb/runs").join(id)
}

fn by_of(root: &Path, id: &str) -> Option<apb_core::launch::StartedBy> {
    apb_engine::run_view::started_by(&run_dir(root, id))
}

/// The new run names its starter (verified, node, attempt, chain 1), the
/// starter's journal is unchanged, and the starter's reverse list has it.
#[test]
fn a_step_start_links_both_runs() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let t = starter(root, "t-1", None, false);
    let before = fs::read(t.join("events.jsonl")).unwrap();
    step.enter(&t, "triage", Some(TOKEN));
    let f = start(root, "fix", None).unwrap();
    let by = by_of(root, &f).expect("started_by");
    assert_eq!(by.run_id, "t-1");
    assert_eq!(by.node.as_deref(), Some("triage"));
    assert_eq!((by.attempt, by.verified, by.chain), (Some(2), true, 1));
    assert_eq!(
        by.workspace_id,
        apb_core::projects::registered_id(root),
        "the starter's workspace"
    );
    assert_eq!(fs::read(t.join("events.jsonl")).unwrap(), before);
    let list = apb_engine::run_view::started_runs(root, "t-1");
    assert_eq!(list.runs.len(), 1);
    assert_eq!(list.runs[0].run_id, f);
    assert_eq!(list.runs[0].playbook, "fix");
    // Both directions on the report: the new run names its starter, the
    // starter lists the new run.
    let html = apb_engine::report_html::render(root, &run_dir(root, &f)).unwrap();
    assert!(
        html.contains("started by: run t-1 (node triage, attempt 2)"),
        "{html}"
    );
    let html = apb_engine::report_html::render(root, &t).unwrap();
    assert!(
        html.contains(&format!("started runs in this project: {f} (fix)")),
        "{html}"
    );
}

/// A sub-playbook child of a step-started run is not marked, though the
/// drive runs with the step's variables in its environment.
#[test]
fn a_child_of_a_step_started_run_is_not_marked() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let t = starter(root, "t-1", None, false);
    step.enter(&t, "triage", Some(TOKEN));
    let parent = start(root, "parent", None).unwrap();
    assert!(by_of(root, &parent).is_some());
    let child = fs::read_dir(root.join(".apb/runs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("fix-"))
        .expect("the child run");
    assert_eq!(by_of(root, &child), None);
}

/// The token the step carried is in no file of the new run.
#[test]
fn the_token_is_not_stored() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let t = starter(root, "t-1", None, false);
    step.enter(&t, "triage", Some(TOKEN));
    let f = start(root, "fix", None).unwrap();
    for entry in fs::read_dir(run_dir(root, &f)).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let body = fs::read(&path).unwrap();
            assert!(
                !String::from_utf8_lossy(&body).contains(TOKEN),
                "{} holds the token",
                path.display()
            );
        }
    }
}

/// A run id of a finished run, a wrong token and a run directory reached
/// through a link are not verified, and their chain is 1.
#[test]
fn a_forged_run_id_is_not_verified() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let t = starter(root, "t-1", Some(4), false);
    step.enter(&t, "triage", Some("forged"));
    let f = start(root, "fix", None).unwrap();
    let by = by_of(root, &f).unwrap();
    assert_eq!((by.verified, by.chain, by.attempt), (false, 1, None));

    let elsewhere = tempfile::tempdir().unwrap();
    let link = elsewhere.path().join("t-1");
    std::os::unix::fs::symlink(&t, &link).unwrap();
    step.enter(&link, "triage", Some(TOKEN));
    let f = start(root, "fix", None).unwrap();
    let by = by_of(root, &f).unwrap();
    assert_eq!((by.verified, by.chain), (false, 1));

    fs::write(
        t.join("events.jsonl"),
        format!(
            "{}{}\n",
            fs::read_to_string(t.join("events.jsonl")).unwrap(),
            json!({"seq": 4, "ts": 4, "type": "run_finished", "outcome": "succeeded"})
        ),
    )
    .unwrap();
    step.enter(&t, "triage", Some(TOKEN));
    let f = start(root, "fix", None).unwrap();
    let by = by_of(root, &f).unwrap();
    assert_eq!((by.verified, by.chain), (false, 1));
}

/// A chain of runs started from steps alternating between two registered
/// projects keeps counting: the sixth start is refused
/// `start_chain_too_deep` on the run gate.
#[test]
fn the_sixth_chained_start_is_refused_across_projects() {
    let mut step = Step::new();
    let a = project();
    let b = project();
    let mut from = starter(a.path(), "t-0", None, false);
    for n in 1..=5u32 {
        let target = if n % 2 == 1 { b.path() } else { a.path() };
        step.enter(&from, "triage", Some(TOKEN));
        let id = start(target, "fix", None).unwrap();
        let by = by_of(target, &id).unwrap();
        assert!(by.verified, "start {n}: {by:?}");
        assert_eq!(by.chain, n);
        // The new run is the next starter: an open attempt, its record kept.
        from = run_dir(target, &id);
        reopen(&from);
    }
    step.enter(&from, "triage", Some(TOKEN));
    let target = b.path();
    let runs_before = fs::read_dir(target.join(".apb/runs")).unwrap().count();
    let refused = start(target, "fix", None).unwrap_err();
    assert_eq!(refused["policy"], "start_chain_too_deep", "{refused}");
    assert_eq!(refused["chain"], 6);
    assert_eq!(
        fs::read_dir(target.join(".apb/runs")).unwrap().count(),
        runs_before,
        "a refused start left a run"
    );
}

/// Turns a finished run back into one with an open attempt of `triage`
/// (its manifest kept), stamped again.
fn reopen(dir: &Path) {
    let id = dir.file_name().unwrap().to_string_lossy().into_owned();
    let lines = [
        json!({"seq": 1, "ts": 1, "type": "run_started", "playbook": "fix", "version": "1.0.0"}),
        json!({"seq": 2, "ts": 2, "type": "node_started", "node": "triage", "attempt": 1}),
        json!({"seq": 3, "ts": 3, "type": "attempt_started", "node": "triage", "attempt": 1, "agent": "claude", "token": TOKEN}),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("events.jsonl"), text).unwrap();
    apb_core::run_origin::stamp(dir, &id).unwrap();
}

/// A resume of a run at chain 5, from a run step, is never refused for its
/// chain: only a new start names a starter.
#[test]
fn a_resume_at_chain_five_is_not_refused() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let t = starter(root, "t-1", Some(4), false);
    step.enter(&t, "triage", Some(TOKEN));
    let fifth = start(root, "fix", None).unwrap();
    assert_eq!(by_of(root, &fifth).unwrap().chain, 5);
    let kind = LaunchKind::Resume {
        run_id: fifth.clone(),
    };
    let grant = apb_engine::resume_launch::check_resume_launch(root, &fifth, &step_ctx(kind));
    assert!(grant.is_ok(), "{:?}", grant.err());
}

/// A step of a flagged run starts a flagged run; an explicit `false` is
/// refused `external_data_immutable`; a step of an unflagged run starts an
/// unflagged one. A grant naming a flagged starter flags the run in
/// preparation too, when the surface's value was lost.
#[test]
fn a_step_of_a_flagged_run_starts_a_flagged_run() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let flagged = starter(root, "t-ext", None, true);
    step.enter(&flagged, "triage", Some(TOKEN));
    let f = start(root, "fix", None).unwrap();
    assert!(apb_engine::external_data::is_flagged(&run_dir(root, &f)));
    let refused = start(root, "fix", Some(false)).unwrap_err();
    assert_eq!(refused["error"], "external_data_immutable", "{refused}");
    // The preparation backstop: the grant names the flagged starter, the
    // options say false.
    let permit = apb_engine::gate::check_run(
        root,
        &wref("fix"),
        true,
        false,
        &step_ctx(LaunchKind::Start),
    )
    .unwrap();
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let res = run(root, "fix", None, opts).unwrap();
    assert!(apb_engine::external_data::is_flagged(&run_dir(
        root,
        &res.run_id
    )));

    let plain = starter(root, "t-plain", None, false);
    step.enter(&plain, "triage", Some(TOKEN));
    let f = start(root, "fix", None).unwrap();
    assert!(!apb_engine::external_data::is_flagged(&run_dir(root, &f)));
}

/// A host task a resumed drive adopted keeps the chain of its run: a step
/// of a run at chain 5 whose subagent carries the first attempt's token is
/// verified and refused `start_chain_too_deep`.
#[test]
fn an_adopted_host_task_keeps_its_chain() {
    let mut step = Step::new();
    let p = project();
    let root = p.path();
    let t = starter(root, "t-h", None, false);
    let requested = json!({"type": "host_task_requested", "task_id": "task-1", "node": "triage", "attempt": 2, "prompt_ref": "p"});
    let more = [
        requested.clone(),
        json!({"type": "run_paused", "reason": "later", "by": "operator"}),
        json!({"type": "run_resumed", "from_node": "triage", "by": "operator"}),
        json!({"type": "attempt_started", "node": "triage", "attempt": 3, "agent": "host", "token": "tok-later"}),
        requested,
    ];
    let mut text = fs::read_to_string(t.join("events.jsonl")).unwrap();
    for (i, l) in more.iter().enumerate() {
        let mut l = l.clone();
        l["seq"] = json!(4 + i);
        l["ts"] = json!(4 + i);
        text.push_str(&format!("{l}\n"));
    }
    fs::write(t.join("events.jsonl"), text).unwrap();
    let manifest = json!({
        "profiles": [], "node_bindings": {},
        "launch": {
            "kind": "start", "initiator": "run_step:cli", "policy": "allow", "sources": [],
            "started_by": { "run_id": "earlier", "verified": true, "chain": 5 }
        }
    });
    fs::write(t.join("manifest.yaml"), manifest.to_string()).unwrap();
    apb_core::run_origin::stamp(&t, "t-h").unwrap();
    step.enter(&t, "triage", Some(TOKEN));
    step.vars.push(EnvVar::set("APB_HOST_TASK", "task-1"));
    let refused = start(root, "fix", None).unwrap_err();
    assert_eq!(refused["policy"], "start_chain_too_deep", "{refused}");
}

/// A run in project B started from a step of a run in project A names A's
/// run and A's workspace; B's reverse list of that id is empty.
#[test]
fn a_starter_in_another_workspace_is_named_there() {
    let mut step = Step::new();
    let a = project();
    let b = project();
    let t = starter(a.path(), "t-a", None, false);
    step.enter(&t, "triage", Some(TOKEN));
    let f = start(b.path(), "fix", None).unwrap();
    let by = by_of(b.path(), &f).unwrap();
    assert!(by.verified);
    assert_eq!(by.run_id, "t-a");
    let ws_a = apb_core::projects::registered_id(a.path()).unwrap();
    assert_eq!(by.workspace_id.as_deref(), Some(ws_a.as_str()));
    let line = apb_engine::run_view::started_by_line(
        &by,
        apb_core::projects::registered_id(b.path()).as_deref(),
    );
    assert!(line.contains(&format!("in workspace {ws_a}")), "{line}");
    assert!(
        apb_engine::run_view::started_runs(b.path(), "t-a")
            .runs
            .is_empty()
    );
    assert_eq!(
        apb_engine::run_view::started_runs(a.path(), "t-a")
            .runs
            .len(),
        0,
        "A's list covers A's runs only"
    );
}
