//! Decision channels: a person-only gate takes a decision only from a
//! person's channel, and a run step never decides its own run.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::registry::init_project;
use apb_core::step_env::StepCaller;
use apb_engine::decision_channel::{CallerRefusal, DecisionChannel};
use apb_engine::error::EngineError;
use apb_engine::event::{Event, EventLog, EventPayload, read_all};
use apb_engine::review::channel::refuse_own_run;
use apb_engine::review::{ReviewCommand, post_decision_via, post_review};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

const DEADLINE: Duration = Duration::from_secs(60);

const PERSON_ONLY: &str = r#"schema: 2
id: po
name: po
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: g, type: human_review, options: [approve, reject], person_only: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: g }
  - { from: g, to: done }
"#;

/// A review gate that offers an option named `publish`: an ordinary
/// `human_review`, never a publish node.
const REVIEW_PUBLISH_OPTION: &str = r#"schema: 2
id: po
name: po
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: g, type: human_review, options: [publish, reject] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: g }
  - { from: g, to: done }
"#;

fn seed(root: &Path, yaml: &str) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/po/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks/po/current"), "1.0.0").unwrap();
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts a run of `yaml` and waits for the request on gate `node`: the
/// run dir, the request seq, and the receiver of the outcome.
fn started_on(
    root: &Path,
    yaml: &str,
    node: &str,
    person_only: bool,
) -> (PathBuf, u64, mpsc::Receiver<(RunStatus, Vec<Event>)>) {
    seed(root, yaml);
    let (tx, rx) = mpsc::channel();
    let owned = root.to_path_buf();
    std::thread::spawn(move || {
        let r = run(&owned, "po", None, RunOptions::default()).unwrap();
        let rd = owned.join(".apb/runs").join(&r.run_id);
        let _ = tx.send((r.outcome, read_all(&rd).unwrap()));
    });
    let rd = poll("the run dir", || {
        fs::read_dir(root.join(".apb/runs"))
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
    });
    let seq = poll("the request", || {
        read_all(&rd).ok()?.iter().find_map(|e| match &e.payload {
            EventPayload::ReviewRequested {
                node: gate,
                person_only: po,
                ..
            } if gate == node => {
                assert_eq!(*po, person_only, "the request's person_only mark");
                Some(e.seq)
            }
            _ => None,
        })
    });
    (rd, seq, rx)
}

/// Starts the person-only run and waits for its request: the run dir, the
/// request seq, and the receiver of the outcome.
fn started(root: &Path) -> (PathBuf, u64, mpsc::Receiver<(RunStatus, Vec<Event>)>) {
    started_on(root, PERSON_ONLY, "g", true)
}

fn approve() -> ReviewCommand {
    ReviewCommand {
        node: "g".into(),
        decision: "approve".into(),
        note: String::new(),
    }
}

fn reviews(rd: &Path) -> String {
    fs::read_to_string(rd.join("reviews.jsonl")).unwrap_or_default()
}

/// Every channel apb does not count as a person's is refused with its own
/// reason and writes nothing; a signed-in dashboard session decides, and
/// the decision is journaled with its channel.
#[test]
fn a_person_only_gate_refuses_every_channel_but_a_persons() {
    let dir = tempfile::tempdir().unwrap();
    let (rd, _, rx) = started(dir.path());
    let refused = [
        (DecisionChannel::Mcp, CallerRefusal::AgentChannel),
        (DecisionChannel::RunStep, CallerRefusal::RunStep),
        (DecisionChannel::CliUnattended, CallerRefusal::NoTerminal),
        (
            DecisionChannel::DashboardKeyless,
            CallerRefusal::KeylessDashboard,
        ),
        (DecisionChannel::HttpBearer, CallerRefusal::BearerKey),
        (DecisionChannel::Engine, CallerRefusal::NotThisCaller),
        (DecisionChannel::Scenario, CallerRefusal::NotThisCaller),
    ];
    for (channel, want) in refused {
        match post_decision_via(&rd, approve(), None, None, channel) {
            Err(EngineError::ChannelRefused { refusal, message }) => {
                assert_eq!(refusal, want, "{channel:?}");
                assert!(message.contains("apb review"), "{message}");
            }
            other => panic!("{channel:?}: {other:?}"),
        }
    }
    match post_review(&rd, approve()) {
        Err(EngineError::ChannelRefused { refusal, .. }) => {
            assert_eq!(refusal, CallerRefusal::NotThisCaller)
        }
        other => panic!("no channel: {other:?}"),
    }
    assert_eq!(reviews(&rd), "");

    post_decision_via(
        &rd,
        approve(),
        None,
        None,
        DecisionChannel::DashboardSession,
    )
    .unwrap();
    let (status, events) = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(status, RunStatus::Succeeded);
    let via = events.iter().find_map(|e| match &e.payload {
        EventPayload::ReviewDecided { via, .. } => Some(*via),
        _ => None,
    });
    assert_eq!(via, Some(Some(DecisionChannel::DashboardSession)));
}

/// A line appended by hand without stamps and one stamped with an agent's
/// channel are never consumed and never stand in the way of a person's
/// decision; `apb doctor --run` lists them.
#[test]
fn forged_entries_are_never_consumed_and_doctor_lists_them() {
    let dir = tempfile::tempdir().unwrap();
    let (rd, seq, rx) = started(dir.path());
    let forged = format!(
        "{{\"seq\":0,\"node\":\"g\",\"decision\":\"approve\",\"note\":\"\"}}\n{{\"seq\":1,\"node\":\"g\",\"decision\":\"approve\",\"note\":\"\",\"request\":{seq},\"channel\":\"mcp\"}}\n"
    );
    fs::write(rd.join("reviews.jsonl"), forged).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let events = read_all(&rd).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::ReviewDecided { .. })),
        "a forged entry was consumed"
    );
    let run_id = rd.file_name().unwrap().to_str().unwrap();
    let checks = apb_engine::run_doctor::diagnose_run(dir.path(), run_id).unwrap();
    let row = checks
        .iter()
        .find(|c| c.subject == "reviews")
        .expect("a reviews row");
    assert!(row.detail.starts_with("2 decision(s)"), "{}", row.detail);
    assert!(row.detail.contains("no request stamp"), "{}", row.detail);
    assert!(
        row.detail.contains("not a person's channel"),
        "{}",
        row.detail
    );

    post_decision_via(&rd, approve(), None, Some(seq), DecisionChannel::Terminal).unwrap();
    let (status, _) = rx.recv_timeout(DEADLINE).unwrap();
    assert_eq!(status, RunStatus::Succeeded);
    // Exactly one decision was taken, and it is the person's: every entry
    // says `approve`, so the outcome alone cannot tell them apart.
    let decided: Vec<_> = read_all(&rd)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ReviewDecided { via, .. } => Some(via),
            _ => None,
        })
        .collect();
    assert_eq!(decided, [Some(DecisionChannel::Terminal)]);
}

/// A step of the run, of its parent, of its child or of a sibling (a child
/// of the same parent, whose outcome routes that parent) is refused for
/// every gate and question; a step of an unrelated run and a person pass.
/// A tree that does not read whole refuses every step.
#[test]
fn a_step_of_the_run_tree_never_decides_it() {
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path().join("runs");
    for id in ["parent", "child", "grandchild", "sibling", "other"] {
        fs::create_dir_all(runs.join(id)).unwrap();
    }
    let with_parent = |id: &str, parent: &str| {
        apb_engine::run_config::write_run_config(
            &runs.join(id),
            &apb_engine::run_config::RunConfig {
                parent_run: Some(parent.into()),
                ..Default::default()
            },
        )
        .unwrap();
    };
    with_parent("child", "parent");
    with_parent("grandchild", "child");
    with_parent("sibling", "parent");
    let mut log = EventLog::create(&runs.join("child")).unwrap();
    log.append(EventPayload::ChildRunStarted {
        node_id: "sub".into(),
        run_id: "grandchild".into(),
    })
    .unwrap();
    let mut log = EventLog::create(&runs.join("parent")).unwrap();
    for (node_id, run_id) in [("a", "child"), ("b", "sibling")] {
        log.append(EventPayload::ChildRunStarted {
            node_id: node_id.into(),
            run_id: run_id.into(),
        })
        .unwrap();
    }
    let step = |run: &str| StepCaller {
        run_id: Some(run.into()),
        host_task: None,
    };
    let target = runs.join("child");
    let refused = |own: &str| match refuse_own_run(&target, &step(own)) {
        Err(EngineError::ChannelRefused { refusal, message }) => {
            assert_eq!(refusal, CallerRefusal::RunStepOwnRun, "{own}");
            message
        }
        other => panic!("{own}: {other:?}"),
    };
    for own in ["child", "parent", "grandchild", "sibling"] {
        refused(own);
    }
    assert!(refuse_own_run(&target, &step("other")).is_ok());
    assert!(refuse_own_run(&target, &StepCaller::default()).is_ok());

    // A journal of the tree with a malformed line before its end: the tree
    // is not known, so even an unrelated step is refused, and a person
    // still passes.
    let journal = runs.join("sibling/events.jsonl");
    fs::write(&journal, "{not json\n{\"seq\":1}\n").unwrap();
    let message = refused("other");
    assert!(message.contains("cannot be read whole"), "{message}");
    assert!(refuse_own_run(&target, &StepCaller::default()).is_ok());
}

/// A scenario run's scripted decision is the person of a person-only gate
/// that binds results: it is stamped with the open request and the binding
/// nonce, journaled through the scenario channel, and satisfies the node
/// that requires the approval.
#[test]
fn a_scenario_run_decides_a_person_only_bound_gate_by_its_script() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    init_project(root).unwrap();
    let yaml = "schema: 2\nid: sc\nname: sc\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [] }\n  - { id: g, type: human_review, options: [approve, reject], person_only: true, approves: { results: [w] } }\n  - { id: d, type: script, script: scripts/d.sh, runner: sh, requires_approval: { gate: g } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: g }\n  - { from: g, to: d, condition: { type: review_status, equals: approve } }\n  - { from: d, to: done }\n";
    let pdir = root.join(".apb/playbooks/sc");
    fs::create_dir_all(pdir.join("1.0.0/scripts")).unwrap();
    fs::write(pdir.join("1.0.0/playbook.yaml"), yaml).unwrap();
    for s in ["w.sh", "d.sh"] {
        fs::write(pdir.join("1.0.0/scripts").join(s), "exit 99\n").unwrap();
    }
    fs::write(pdir.join("current"), "1.0.0").unwrap();
    let scenarios = apb_core::scenario::scenarios_dir(&pdir);
    fs::create_dir_all(&scenarios).unwrap();
    fs::write(
        scenarios.join("ok.yaml"),
        "schema: 1\nid: ok\nanswers:\n  w:\n    - { visit: 1, script: { exit: 0, stdout: report } }\n  g:\n    - { visit: 1, review: { decision: approve } }\n  d:\n    - { visit: 1, script: { exit: 0, stdout: shipped } }\n",
    )
    .unwrap();
    let loaded = apb_core::scenario::load(&pdir, "ok").unwrap();
    let prep = apb_engine::scenario::session::ScenarioPrep {
        scenario: loaded.scenario,
        level: apb_core::scenario::ScenarioLevel::Quick,
        scenario_digest: loaded.digest,
        max_requests: 20,
        sent: Default::default(),
    };
    let r = apb_engine::scheduler::run_scenario(root, "sc", None, &prep).unwrap();
    let events = read_all(&r.run_dir).unwrap();
    assert_eq!(r.outcome, RunStatus::Succeeded, "{events:#?}");
    let nonce = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ReviewRequested {
                binding: Some(b), ..
            } => Some(b.nonce.clone()),
            _ => None,
        })
        .expect("the request is bound");
    let decided: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ReviewDecided { binding, via, .. } => Some((binding.clone(), *via)),
            _ => None,
        })
        .collect();
    assert_eq!(decided, [(Some(nonce), Some(DecisionChannel::Scenario))]);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::NodeFinished { node, status, .. } if node == "d" && status == "succeeded"
    )));
}

/// The keyless-dashboard rule protects consent to a publish node's push
/// plan only: a `human_review` gate that offers an option named `publish`
/// takes that decision from a dashboard without keys like any other
/// review decision, while a publish node's consent is still refused, also
/// when the run's playbook does not read (fail closed).
#[test]
fn a_keyless_dashboard_decides_a_publish_named_option_on_a_review_gate() {
    let dir = tempfile::tempdir().unwrap();
    let (rd, _seq, rx) = started_on(dir.path(), REVIEW_PUBLISH_OPTION, "g", false);
    let publish = ReviewCommand {
        node: "g".into(),
        decision: "publish".into(),
        note: String::new(),
    };
    match post_decision_via(
        &rd,
        publish.clone(),
        None,
        None,
        DecisionChannel::DashboardKeyless,
    ) {
        Ok(_) => {}
        other => panic!("a review option named `publish` was refused: {other:?}"),
    }
    assert!(reviews(&rd).contains("publish"), "{}", reviews(&rd));

    // A publish node's consent is still the rule's, and a playbook that
    // does not read fails closed into it.
    let bare = dir.path().join(".apb/runs").join("bare");
    fs::create_dir_all(&bare).unwrap();
    match post_decision_via(
        &bare,
        publish,
        None,
        None,
        DecisionChannel::DashboardKeyless,
    ) {
        Err(EngineError::ChannelRefused {
            refusal: CallerRefusal::KeylessDashboard,
            message,
        }) => assert!(message.contains("publish approve"), "{message}"),
        other => panic!("a keyless publish consent went through: {other:?}"),
    }
    rx.recv().unwrap();
}
