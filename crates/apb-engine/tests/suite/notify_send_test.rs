//! "Notify me" end to end against a stub chat service: a subscribed run
//! sends each transition once through the send ledger, a repeat of the same
//! wait sends nothing, a target whose connector changed since the person
//! subscribed is refused before anything leaves, and the run's outcome
//! never counts a notification as one of its own sends.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apb_core::connector::{config, store};
use apb_core::notify::{NotifyRequest, NotifyTarget, Transition};
use apb_core::registry::init_project;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::notify::{ChangedBy, RunNotify};
use apb_engine::review::{ReviewCommand, post_review};
use apb_engine::run_outcome::{NeedsLook, outcome_intents};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar, Reply, ScriptedHttpServer, request_json, spawn_script};

const CONNECTOR: &str = "mock-tracker";
const TOKEN_VAR: &str = "APB_NOTIFY_TEST_TOKEN";
const DEADLINE: Duration = Duration::from_secs(20);

const FINISH_ONLY: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: done }\n";

const WITH_GATE: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, title: Ship it }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: done }\n";

/// A project with the stub-backed `mock-tracker` installed and approved.
/// Fields drop in declaration order: the env lock goes last, so every
/// variable is restored while it is still held.
struct Env {
    _cfg_var: EnvVar,
    cfg: tempfile::TempDir,
    proj: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new(playbook: &str, server: &ScriptedHttpServer) -> Self {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let cfg_var = EnvVar::set("APB_CONFIG_DIR", cfg.path());
        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let vdir = proj.path().join(".apb/playbooks/p/1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("playbook.yaml"), playbook).unwrap();
        std::fs::write(proj.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/connectors")
            .join(CONNECTOR);
        let dst = cfg.path().join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::copy(src.join("connector.yaml"), dst.join("connector.yaml")).unwrap();
        let accounts = config::project_config_path(proj.path(), CONNECTOR);
        std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
        std::fs::write(
            accounts,
            format!(
                "accounts:\n  - name: me\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
                server.base_url
            ),
        )
        .unwrap();
        std::fs::write(
            proj.path().join(".apb/secrets.env"),
            format!("{TOKEN_VAR}=notify-token-value\n"),
        )
        .unwrap();
        let env = Env {
            _cfg_var: cfg_var,
            cfg,
            proj,
            _lock: lock,
        };
        env.approve();
        env
    }

    /// Approves the connector and the account as they are now.
    fn approve(&self) {
        let mut trust = TrustStore::load();
        let loaded = store::load(CONNECTOR).unwrap();
        trust
            .approve_kind(
                &loaded.digest,
                CONNECTOR,
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        let accounts = config::load_merged(self.proj.path(), CONNECTOR).unwrap();
        let digest = config::account_digest(&accounts[0]);
        trust
            .approve_kind(
                &digest,
                &account_trust_id(CONNECTOR, "me"),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
    }

    fn root(&self) -> &Path {
        self.proj.path()
    }

    /// Writes playbook `id` 1.0.0 with `scripts` beside it.
    fn add_playbook(&self, id: &str, yaml: &str, scripts: &[(&str, String)]) {
        let dir = self.root().join(".apb/playbooks").join(id);
        let vdir = dir.join("1.0.0");
        std::fs::create_dir_all(vdir.join("scripts")).unwrap();
        std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        std::fs::write(dir.join("current"), "1.0.0").unwrap();
        for (name, body) in scripts {
            std::fs::write(vdir.join("scripts").join(name), body).unwrap();
        }
    }

    /// A script that waits (bounded) until the file `go` appears in the
    /// project, then succeeds.
    fn hold_script(&self) -> String {
        let go = self.root().join("go");
        format!(
            "i=0\nwhile [ ! -f '{}' ]; do i=$((i+1)); [ \"$i\" -gt 400 ] && exit 1; sleep 0.05; done\necho held\n",
            go.display()
        )
    }

    fn release_hold(&self) {
        std::fs::write(self.root().join("go"), "").unwrap();
    }

    /// Installs a new version of the connector and approves it: trusted,
    /// but not the one a subscription pinned before.
    fn change_connector(&self) {
        let manifest = self
            .cfg
            .path()
            .join("connectors")
            .join(CONNECTOR)
            .join("connector.yaml");
        let changed = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("version: 0.1.0", "version: 0.1.1");
        std::fs::write(&manifest, changed).unwrap();
        self.approve();
    }

    fn options(on: &[Transition]) -> RunOptions {
        RunOptions {
            notify: Some(NotifyRequest::On {
                target: Some(NotifyTarget {
                    connector: CONNECTOR.into(),
                    account: "me".into(),
                    function: "notify_person".into(),
                    to: "42".into(),
                }),
                on: Some(on.to_vec()),
            }),
            ..RunOptions::default()
        }
    }

    /// Runs `p` on a thread; the caller drives the gate.
    fn run_in_background(&self, on: &[Transition]) -> Background {
        let (tx, rx) = mpsc::channel();
        let root: PathBuf = self.root().to_path_buf();
        let opts = Self::options(on);
        let handle = std::thread::spawn({
            let root = root.clone();
            move || {
                let status = run(&root, "p", None, opts).map_or(RunStatus::Failed, |r| r.outcome);
                let _ = tx.send(status);
            }
        });
        Background {
            root,
            done: rx,
            handle: Some(handle),
        }
    }

    /// The directory of the run of playbook `p` (not of a child it starts).
    fn run_dir(&self) -> PathBuf {
        let runs = self.root().join(".apb/runs");
        let poll = Instant::now();
        loop {
            if let Some(dir) = std::fs::read_dir(&runs)
                .into_iter()
                .flatten()
                .flatten()
                .find(|d| d.file_name().to_string_lossy().starts_with("p-"))
            {
                return dir.path();
            }
            assert!(poll.elapsed() < DEADLINE, "no run directory appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A run driven on its own thread. Dropped before the [`Env`] it runs in
/// (declare it after), it stops every run of the project and joins the
/// thread, so a failed assertion never leaves a drive running against
/// deleted directories and a restored environment.
struct Background {
    root: PathBuf,
    done: mpsc::Receiver<RunStatus>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Background {
    /// The run's outcome, once its thread is done.
    fn finish(&mut self) -> RunStatus {
        let status = self
            .done
            .recv_timeout(DEADLINE)
            .expect("the run finishes in time");
        if let Some(h) = self.handle.take() {
            h.join().expect("the drive thread ends cleanly");
        }
        status
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        let Some(h) = self.handle.take() else {
            return;
        };
        let runs = self.root.join(".apb/runs");
        for entry in std::fs::read_dir(runs).into_iter().flatten().flatten() {
            if let Some(id) = entry.file_name().to_str() {
                let _ = apb_engine::stop_run(&self.root, id);
            }
        }
        let _ = h.join();
    }
}

fn poll<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The notification intents recorded: their ids and labels.
fn notify_intents(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SendIntentRecorded {
                node,
                intent,
                label,
                ..
            } if node == "@notify" => Some((intent.clone(), label.clone())),
            _ => None,
        })
        .collect()
}

fn settlement(events: &[Event], intent: &str) -> Option<(String, String)> {
    settled(events, intent).map(|(outcome, source, _)| (outcome, source))
}

/// The settlement of `intent`: its outcome, source and detail.
fn settled(events: &[Event], intent: &str) -> Option<(String, String, String)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::SendIntentSettled {
            intent: i,
            outcome,
            source,
            detail,
            ..
        } if i == intent => Some((
            outcome.clone(),
            source.clone(),
            detail.clone().unwrap_or_default(),
        )),
        _ => None,
    })
}

fn wait_for_review(run_dir: &Path) {
    poll("the review gate", || {
        read_all(run_dir)
            .ok()?
            .iter()
            .find_map(|e| matches!(&e.payload, EventPayload::ReviewRequested { .. }).then_some(()))
    });
}

fn decide(run_dir: &Path) {
    wait_for_review(run_dir);
    post_review(
        run_dir,
        ReviewCommand {
            node: "gate".into(),
            decision: "approve".into(),
            note: String::new(),
        },
    )
    .unwrap();
}

#[test]
fn the_end_of_a_run_is_sent_once_as_apbs_own_call() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(FINISH_ONLY, &server);
    let res = run(env.root(), "p", None, Env::options(&[Transition::Finished])).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = env.root().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();

    let intents = notify_intents(&events);
    assert_eq!(intents.len(), 1, "one notification");
    let (intent, label) = &intents[0];
    assert!(label.starts_with("notify:finished."), "{label}");
    assert_eq!(
        settlement(&events, intent),
        Some(("confirmed".to_string(), "response".to_string()))
    );
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ConnectorCall { node_id, origin: Some(o), .. }
            if node_id == "@notify" && o == "notify"
    )));

    let sent = server.requests();
    assert_eq!(sent.len(), 1);
    let body = request_json(&sent[0]);
    assert_eq!(body["to"], "42");
    let text = body["text"].as_str().unwrap();
    assert!(
        text.contains("Run ") && text.contains(" of p has ended: succeeded"),
        "{text}"
    );
    assert!(body["subject"].as_str().unwrap().contains("p succeeded"));

    let view = RunNotify::from_events(&events, apb_core::clock::now_ms_u64()).unwrap();
    assert!(view.active);
    assert_eq!(view.sent.len(), 1);
    assert_eq!(view.sent[0].state, "confirmed");
    // The run's own outcome does not count the notification.
    assert!(outcome_intents(&events, apb_core::clock::now_ms_u64()).is_empty());
}

#[test]
fn a_notification_nobody_knows_went_out_settles_unknown_and_needs_no_look() {
    // The service takes the request and drops the connection.
    let server = spawn_script(vec![Reply::Cut]);
    let env = Env::new(FINISH_ONLY, &server);
    let res = run(env.root(), "p", None, Env::options(&[Transition::Finished])).unwrap();
    let run_dir = env.root().join(".apb/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    let intents = notify_intents(&events);
    assert_eq!(intents.len(), 1);
    assert_eq!(settlement(&events, &intents[0].0).unwrap().0, "unknown");
    assert_eq!(NeedsLook::from_events(None, &events), None);
    assert_eq!(server.requests().len(), 1);
}

/// A sent notification is never sent again whatever time passes; that rule
/// is pinned with old timestamps in `notify_transitions_test`. Here the
/// drive keeps sweeping while the gate waits, and none of those sweeps
/// sends again.
#[test]
fn a_waiting_review_is_sent_once_while_the_drive_keeps_sweeping() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(WITH_GATE, &server);
    let mut bg = env.run_in_background(&[Transition::Review]);
    let run_dir = env.run_dir();
    poll("the review notification to settle", || {
        let events = read_all(&run_dir).ok()?;
        let (intent, _) = notify_intents(&events).into_iter().next()?;
        settlement(&events, &intent)
    });
    // Many drive turns, each with its sweep, while the gate waits.
    std::thread::sleep(Duration::from_millis(500));
    decide(&run_dir);
    assert_eq!(bg.finish(), RunStatus::Succeeded);

    let events = read_all(&run_dir).unwrap();
    let intents = notify_intents(&events);
    assert_eq!(intents.len(), 1, "{intents:?}");
    assert!(intents[0].1.starts_with("notify:review."));
    let sent = server.requests();
    assert_eq!(sent.len(), 1);
    let text = request_json(&sent[0])["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("\"Ship it\"") && text.contains("apb review"),
        "{text}"
    );
}

#[test]
fn a_connector_that_changed_after_subscribing_is_refused_before_anything_leaves() {
    let server = spawn_script(vec![]);
    let env = Env::new(WITH_GATE, &server);
    let mut bg = env.run_in_background(&[Transition::Finished]);
    let run_dir = env.run_dir();
    wait_for_review(&run_dir);
    env.change_connector();
    decide(&run_dir);
    assert_eq!(bg.finish(), RunStatus::Succeeded);

    let events = read_all(&run_dir).unwrap();
    let intents = notify_intents(&events);
    assert_eq!(intents.len(), 1);
    // Refused by the pin comparison, not by the trust check: the new
    // version is approved.
    let (outcome, source, detail) = settled(&events, &intents[0].0).unwrap();
    assert_eq!((outcome.as_str(), source.as_str()), ("not_sent", "gate"));
    assert!(detail.contains("changed since"), "{detail}");
    assert!(!events.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ConnectorCall { node_id, .. } if node_id == "@notify"
    )));
    assert!(server.requests().is_empty());
}

#[test]
fn a_target_that_cannot_carry_notifications_refuses_the_start() {
    let server = spawn_script(vec![]);
    let env = Env::new(FINISH_ONLY, &server);
    let mut opts = Env::options(&[Transition::Finished]);
    if let Some(NotifyRequest::On {
        target: Some(t), ..
    }) = opts.notify.as_mut()
    {
        t.function = "create_item".into();
    }
    let check = apb_engine::notify::check(env.root(), opts.notify.as_ref());
    let why = check.refusal.expect("refused");
    assert!(why.contains("cannot carry notifications"), "{why}");
    let err = run(env.root(), "p", None, opts).unwrap_err().to_string();
    assert!(err.contains("cannot carry notifications"), "{err}");
    // Refused before anything is written: no run is left behind.
    let runs = std::fs::read_dir(env.root().join(".apb/runs"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(runs, 0, "a refused start leaves no run");
    assert!(server.requests().is_empty());
}

fn ev(seq: u64, payload: EventPayload) -> Event {
    Event {
        seq,
        ts: 1,
        payload,
    }
}

fn started() -> Event {
    ev(
        0,
        EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
        },
    )
}

#[test]
fn a_notice_says_what_waits_and_how_to_answer_but_never_what_was_asked() {
    use apb_engine::notify::notice::Notice;
    use apb_engine::notify::transitions::TransitionId;

    let server = spawn_script(vec![]);
    let env = Env::new(FINISH_ONLY, &server);
    std::fs::write(env.cfg.path().join("config.yaml"), "port: 7400\n").unwrap();
    let ws = apb_core::workspace::ensure_id(env.root()).unwrap();
    let run_dir = env.root().join(".apb/runs/r1");
    let events = vec![
        started(),
        ev(
            1,
            EventPayload::QuestionAsked {
                node: "ask".into(),
                question: "Which secret plan do we follow?".into(),
                options: vec![],
            },
        ),
        ev(
            2,
            EventPayload::ReviewRequested {
                node: "gate".into(),
                options: vec![],
                title: Some("Ship notify-token-value now".into()),
                instruction: "Check the diff line by line".into(),
                prompt: Some("Check the diff line by line".into()),
                recommendation: None,
                plan_digest: None,
            },
        ),
    ];
    let question = Notice::build(
        env.root(),
        &run_dir,
        "r1",
        &events,
        &TransitionId {
            kind: Transition::Question,
            seq: 1,
        },
    );
    assert!(question.subject.contains("p asked you a question"));
    assert!(
        question
            .text
            .contains("apb answer r1 --node ask \"<answer>\"")
    );
    assert!(
        question
            .text
            .contains(&format!("http://127.0.0.1:7400/#/run/{ws}/r1"))
    );
    assert!(!question.text.contains("secret plan"));

    let review = Notice::build(
        env.root(),
        &run_dir,
        "r1",
        &events,
        &TransitionId {
            kind: Transition::Review,
            seq: 2,
        },
    );
    assert!(review.text.contains("apb review r1 gate --decision"));
    assert!(!review.text.contains("line by line"));
    assert!(
        !review.text.contains("notify-token-value")
            && !review.subject.contains("notify-token-value"),
        "a secret in the gate title must not reach the notice"
    );

    let line = review.one_line();
    assert!(!line.contains('\n'));
    assert!(line.starts_with(&review.subject));
}

const HOLD_THEN_GATE: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: hold, type: script, script: \"scripts/hold.sh\", runner: sh }\n  - { id: gate, type: human_review, title: Ship it }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: hold }\n  - { from: hold, to: gate }\n  - { from: gate, to: done }\n";

#[test]
fn setting_the_subscription_up_again_sends_a_review_the_gate_refused() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(HOLD_THEN_GATE, &server);
    env.add_playbook("p", HOLD_THEN_GATE, &[("hold.sh", env.hold_script())]);
    let mut bg = env.run_in_background(&[Transition::Review]);
    let run_dir = env.run_dir();
    // The connector changes while the run holds, before the review opens.
    poll("the subscription", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .find_map(|e| matches!(&e.payload, EventPayload::NotifySet { .. }).then_some(()))
    });
    env.change_connector();
    env.release_hold();
    let refused = poll("the refused review notification", || {
        let events = read_all(&run_dir).ok()?;
        let (intent, _) = notify_intents(&events).into_iter().next()?;
        settled(&events, &intent)
    });
    assert_eq!(
        (refused.0.as_str(), refused.1.as_str()),
        ("not_sent", "gate")
    );
    assert!(server.requests().is_empty());

    // The person sets it up again: the run's target and transitions, with
    // the new version pinned.
    let run_id = run_dir.file_name().unwrap().to_str().unwrap().to_string();
    let again = apb_engine::notify::set(
        env.root(),
        &run_id,
        &NotifyRequest::On {
            target: None,
            on: None,
        },
        ChangedBy::Person,
    )
    .unwrap();
    assert_eq!(again.refusal, None);
    assert_eq!(
        again.plan.as_ref().map(|p| p.on.clone()),
        Some(vec![Transition::Review])
    );
    poll("the review notification through the new version", || {
        (server.requests().len() == 1).then_some(())
    });
    decide(&run_dir);
    assert_eq!(bg.finish(), RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let intents = notify_intents(&events);
    assert_eq!(intents.len(), 2, "{intents:?}");
    assert_eq!(
        settlement(&events, &intents[1].0),
        Some(("confirmed".to_string(), "response".to_string()))
    );
}

const PARENT: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: done }\n";

#[test]
fn a_sub_playbook_stops_notifying_once_its_parent_was_turned_off() {
    let server = spawn_script(vec![]);
    let env = Env::new(PARENT, &server);
    env.add_playbook(
        "child",
        &HOLD_THEN_GATE.replace("id: p\nname: P", "id: child\nname: Child"),
        &[("hold.sh", env.hold_script())],
    );
    let mut bg = env.run_in_background(&[Transition::Review]);
    let parent_dir = env.run_dir();
    let parent_id = parent_dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let child_id = poll("the child run", || {
        read_all(&parent_dir)
            .ok()?
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
                _ => None,
            })
    });
    let child_dir = env.root().join(".apb/runs").join(&child_id);
    poll("the child's inherited subscription", || {
        read_all(&child_dir)
            .ok()?
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::NotifySet { source, .. } if source == "parent" => Some(()),
                _ => None,
            })
    });
    // The person turns the parent off while the child still holds.
    apb_engine::notify::set(
        env.root(),
        &parent_id,
        &NotifyRequest::Off,
        ChangedBy::Person,
    )
    .unwrap();
    env.release_hold();
    wait_for_review(&child_dir);
    // Many child drive turns, each with its sweep.
    std::thread::sleep(Duration::from_millis(500));
    assert!(notify_intents(&read_all(&child_dir).unwrap()).is_empty());
    assert!(server.requests().is_empty());
    decide(&child_dir);
    assert_eq!(bg.finish(), RunStatus::Succeeded);
    assert!(server.requests().is_empty());
}

const LIVE_QUESTION: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: ask, type: agent_task, prompt: \"ask something\", profile: arch, interactive: true }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: ask }\n  - { from: ask, to: done }\n";

/// A stub claude that asks through the live channel, as the `ask_user`
/// sidecar would, then blocks until the answer lands (bounded).
const ASKING_AGENT: &str = "#!/bin/sh\nrd=\"$APB_RUN_DIR\"\nprintf '%s\\n' '{\"seq\":0,\"node\":\"ask\",\"attempt\":1,\"question\":\"Which DB?\",\"options\":[]}' >> \"$rd/questions.jsonl\"\ni=0\nwhile ! grep -q '\"node\":\"ask\"' \"$rd/answers.jsonl\" 2>/dev/null; do i=$((i+1)); [ \"$i\" -gt 400 ] && break; sleep 0.05; done\necho answered\nexit 0\n";

#[test]
fn a_live_question_is_sent_while_the_agent_still_waits_for_the_answer() {
    use std::os::unix::fs::PermissionsExt as _;

    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(LIVE_QUESTION, &server);
    let profile = env.root().join(".apb/profiles/arch");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("profile.yaml"),
        "name: arch\ndescription: d\nexecutor:\n  agent: claude\n  model: haiku\n",
    )
    .unwrap();
    std::fs::write(profile.join("SOUL.md"), "role").unwrap();
    let bin = tempfile::tempdir().unwrap();
    let agent = bin.path().join("agent.sh");
    common::write_sync(&agent, ASKING_AGENT);
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = tempfile::tempdir().unwrap();
    let _agent_var = EnvVar::set("APB_AGENT_CMD", &agent);
    let _home_var = EnvVar::set("HOME", home.path());

    let mut bg = env.run_in_background(&[Transition::Question]);
    let run_dir = env.run_dir();
    poll("the question notification", || {
        (server.requests().len() == 1).then_some(())
    });
    // The agent still waits: nothing answered the question yet.
    let events = read_all(&run_dir).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::QuestionAnswered { .. }))
    );
    let text = request_json(&server.requests()[0])["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("apb answer"), "{text}");
    assert!(!text.contains("Which DB?"), "the question stays in the run");
    apb_engine::question::post_answer(&run_dir, Some("ask"), "pg", "human").unwrap();
    assert_eq!(bg.finish(), RunStatus::Succeeded);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_review_title_reaches_a_notice_as_plain_text() {
    use apb_engine::notify::notice::Notice;
    use apb_engine::notify::transitions::TransitionId;

    let server = spawn_script(vec![]);
    let env = Env::new(FINISH_ONLY, &server);
    let events = vec![
        started(),
        ev(
            1,
            EventPayload::ReviewRequested {
                node: "gate".into(),
                options: vec![],
                title: Some("*Ship* `now` [here](https://example.com) <!channel>".into()),
                instruction: String::new(),
                prompt: None,
                recommendation: None,
                plan_digest: None,
            },
        ),
    ];
    let notice = Notice::build(
        env.root(),
        &env.root().join(".apb/runs/r1"),
        "r1",
        &events,
        &TransitionId {
            kind: Transition::Review,
            seq: 1,
        },
    );
    assert!(
        notice
            .text
            .contains("\"Ship now here https://example.com channel\""),
        "{}",
        notice.text
    );
}

#[test]
fn a_request_the_project_narrows_to_nothing_is_refused_not_dropped() {
    let server = spawn_script(vec![]);
    let env = Env::new(FINISH_ONLY, &server);
    std::fs::write(
        env.cfg.path().join("config.yaml"),
        "notify:\n  on: [review]\n",
    )
    .unwrap();
    std::fs::write(
        env.root().join(".apb/config.yaml"),
        "notify:\n  on: [finished]\n",
    )
    .unwrap();
    let mut opts = Env::options(&[]);
    if let Some(NotifyRequest::On { on, .. }) = opts.notify.as_mut() {
        *on = None;
    }
    let check = apb_engine::notify::check(env.root(), opts.notify.as_ref());
    let why = check.refusal.expect("an explicit request is refused");
    assert!(why.contains("no transition is left"), "{why}");
}

#[test]
fn a_machine_default_that_cannot_be_read_is_journaled_as_a_problem() {
    let server = spawn_script(vec![]);
    let env = Env::new(FINISH_ONLY, &server);
    std::fs::write(
        env.cfg.path().join("config.yaml"),
        "notify:\n  default: sometimes\n",
    )
    .unwrap();
    let check = apb_engine::notify::check(env.root(), None);
    assert_eq!(check.refusal, None);
    assert_eq!(check.warnings.len(), 1);
    assert!(matches!(
        check.event(),
        Some(EventPayload::NotifySet {
            target: None,
            source,
            problem: Some(_),
            ..
        }) if source == "machine"
    ));
}

#[test]
fn a_sub_playbook_follows_its_parent_set_up_again_after_a_trust_refusal() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(PARENT, &server);
    env.add_playbook(
        "child",
        &HOLD_THEN_GATE.replace("id: p\nname: P", "id: child\nname: Child"),
        &[("hold.sh", env.hold_script())],
    );
    let mut bg = env.run_in_background(&[Transition::Review]);
    let parent_dir = env.run_dir();
    let parent_id = parent_dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let child_id = poll("the child run", || {
        read_all(&parent_dir)
            .ok()?
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::ChildRunStarted { run_id, .. } => Some(run_id.clone()),
                _ => None,
            })
    });
    let child_dir = env.root().join(".apb/runs").join(&child_id);
    poll("the child's inherited subscription", || {
        read_all(&child_dir)
            .ok()?
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::NotifySet { source, .. } if source == "parent" => Some(()),
                _ => None,
            })
    });
    // The connector changes while the child holds: its review is refused.
    env.change_connector();
    env.release_hold();
    let refused = poll("the child's refused review notification", || {
        let events = read_all(&child_dir).ok()?;
        let (intent, _) = notify_intents(&events).into_iter().next()?;
        settled(&events, &intent)
    });
    assert_eq!(refused.1, "gate");

    // The person sets the parent up again, to another recipient.
    let mut target = match Env::options(&[]).notify {
        Some(NotifyRequest::On {
            target: Some(t), ..
        }) => t,
        _ => unreachable!("the options name a target"),
    };
    target.to = "77".into();
    apb_engine::notify::set(
        env.root(),
        &parent_id,
        &NotifyRequest::On {
            target: Some(target),
            on: None,
        },
        ChangedBy::Person,
    )
    .unwrap();
    poll("the child's review through the parent's new target", || {
        (server.requests().len() == 1).then_some(())
    });
    assert_eq!(request_json(&server.requests()[0])["to"], "77");
    // The child's own view shows where its notices go now.
    let events = read_all(&child_dir).unwrap();
    let view = RunNotify::for_run(&child_dir, &events, apb_core::clock::now_ms_u64()).unwrap();
    assert_eq!(view.target.map(|t| t.to), Some("77".to_string()));
    decide(&child_dir);
    assert_eq!(bg.finish(), RunStatus::Succeeded);
}

#[test]
fn settings_that_do_not_read_are_journaled_as_their_own_source_and_off_needs_none() {
    let server = spawn_script(vec![]);
    let env = Env::new(FINISH_ONLY, &server);
    std::fs::write(
        env.root().join(".apb/config.yaml"),
        "notify:\n  enabled: maybe\n",
    )
    .unwrap();
    let check = apb_engine::notify::check(env.root(), None);
    assert!(matches!(
        check.event(),
        Some(EventPayload::NotifySet { source, problem: Some(_), .. }) if source == "project"
    ));
    // A run that asked for none has nothing to warn about.
    let off = apb_engine::notify::check(env.root(), Some(&NotifyRequest::Off));
    assert!(off.warnings.is_empty());
    assert!(off.event().is_none());
}
