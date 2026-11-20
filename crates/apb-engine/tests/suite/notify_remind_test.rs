//! Reminders for open waits (design D9): an open review, question or host
//! task covered by the run's subscription gets one reminder after each
//! listed duration of active run time, through the send ledger under its
//! own label. The journals are hand-written, so the ten-minute
//! thresholds sit in the journal's times and nothing is waited for; the
//! connector, the ledger and the sweep are the real ones, against a stub
//! chat service.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::connector::sites::NOTIFY_SITE;
use apb_core::connector::{config, store};
use apb_core::registry::init_project;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::event::{Event, EventLog, read_all};
use apb_engine::notify::Notifier;

use crate::common::{self, EnvVar, Reply, ScriptedHttpServer, spawn_script};

const CONNECTOR: &str = "mock-tracker";
const TOKEN_VAR: &str = "APB_NOTIFY_REMIND_TOKEN";
const DEADLINE: Duration = Duration::from_secs(20);
/// The shortest `policy.yaml` allows: the tests put ten minutes into the
/// journal's times.
const REMIND_MS: u128 = 10 * 60 * 1000;
const HOUR_MS: u128 = 60 * 60 * 1000;

const PLAYBOOK: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, title: Ship it }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: done }\n";

fn now() -> u128 {
    apb_core::clock::now_ms()
}

/// A project with the stub-backed connector installed and approved, and
/// `notify.remind` in the machine's policy.yaml.
struct Env {
    /// The notify retry gap, set under the env lock and put back before it
    /// is released (fields drop in declaration order).
    _gap: EnvVar,
    _cfg_var: EnvVar,
    /// Held for its lifetime: the config dir the env var names.
    _cfg: tempfile::TempDir,
    proj: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new(
        server: &ScriptedHttpServer,
        extra_account: Option<&ScriptedHttpServer>,
        retry_gap_ms: &str,
    ) -> Self {
        let lock = common::env_lock();
        let gap = EnvVar::set(apb_engine::notify::transitions::RETRY_GAP_ENV, retry_gap_ms);
        let cfg = tempfile::tempdir().unwrap();
        let cfg_var = EnvVar::set("APB_CONFIG_DIR", cfg.path());
        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let vdir = proj.path().join(".apb/playbooks/p/1.0.0");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join("playbook.yaml"), PLAYBOOK).unwrap();
        std::fs::write(proj.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/connectors")
            .join(CONNECTOR);
        let dst = cfg.path().join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::copy(src.join("connector.yaml"), dst.join("connector.yaml")).unwrap();
        let accounts = config::project_config_path(proj.path(), CONNECTOR);
        std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
        let mut yaml = format!(
            "accounts:\n  - name: me\n    default: true\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
            server.base_url
        );
        if let Some(extra) = extra_account {
            yaml.push_str(&format!(
                "  - name: her\n    base_url: {}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n",
                extra.base_url
            ));
        }
        std::fs::write(&accounts, yaml).unwrap();
        std::fs::write(
            proj.path().join(".apb/secrets.env"),
            format!("{TOKEN_VAR}=notify-token-value\n"),
        )
        .unwrap();
        // The machine's reminder cadence.
        std::fs::write(cfg.path().join("policy.yaml"), "notify:\n  remind: [10m]\n").unwrap();
        let env = Env {
            _gap: gap,
            _cfg_var: cfg_var,
            _cfg: cfg,
            proj,
            _lock: lock,
        };
        env.approve("me");
        if extra_account.is_some() {
            env.approve("her");
        }
        env
    }

    fn approve(&self, account: &str) {
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
        let account = accounts.iter().find(|a| a.name == account).unwrap();
        let digest = config::account_digest(account);
        trust
            .approve_kind(
                &digest,
                &account_trust_id(CONNECTOR, &account.name),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
    }

    fn root(&self) -> &Path {
        self.proj.path()
    }
}

/// The pinned digest pair of the installed connector and its account.
fn pins(env: &Env, account: &str) -> (String, String) {
    let loaded = store::load(CONNECTOR).unwrap();
    let accounts = config::load_merged(env.root(), CONNECTOR).unwrap();
    let account = accounts.iter().find(|a| a.name == account).unwrap();
    (loaded.digest.clone(), config::account_digest(account))
}

/// A hand-written run directory: the journal is `lines`, each a
/// `(ts, payload)` pair, with seqs in order and a playbook snapshot.
fn seed_run(env: &Env, run_id: &str, lines: &[(u128, serde_json::Value)]) -> PathBuf {
    let run_dir = env.root().join(".apb/runs").join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(run_dir.join("playbook.yaml"), PLAYBOOK).unwrap();
    let mut out = String::new();
    for (i, (ts, payload)) in lines.iter().enumerate() {
        let mut v = payload.clone();
        v["seq"] = serde_json::json!(i as u64);
        v["ts"] = serde_json::json!(ts);
        out.push_str(&serde_json::to_string(&v).unwrap());
        out.push('\n');
    }
    std::fs::write(run_dir.join("events.jsonl"), out).unwrap();
    run_dir
}

/// The target record of an account, with its pinned digests.
fn target(env: &Env, account: &str, to: &str) -> serde_json::Value {
    let (connector_digest, account_digest) = pins(env, account);
    serde_json::json!({
        "connector": CONNECTOR,
        "account": account,
        "function": "notify_person",
        "to": to,
        "connector_digest": connector_digest,
        "account_digest": account_digest,
    })
}

fn notify_set(target: serde_json::Value, remind_ms: &[u128]) -> serde_json::Value {
    serde_json::json!({
        "type": "notify_set",
        "target": target,
        "on": ["review", "question", "host_task"],
        "source": "run",
        "remind_ms": remind_ms,
    })
}

fn review_requested(_ts: u128, node: &str) -> serde_json::Value {
    serde_json::json!({"type": "review_requested", "node": node, "options": ["approved", "rejected"]})
}

fn confirmed_notice(seq_hint: &str, label: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "send_intent_recorded",
            "intent": format!("i-{seq_hint}"),
            "nonce": format!("n-{seq_hint}"),
            "node": NOTIFY_SITE,
            "connector": CONNECTOR,
            "function": "notify_person",
            "account": "me",
            "mode": "none",
            "label": label,
            "pid": std::process::id(),
            "deadline_ms": 0,
        }),
        serde_json::json!({
            "type": "send_intent_settled",
            "intent": format!("i-{seq_hint}"),
            "outcome": "confirmed",
            "source": "response",
        }),
    ]
}

/// One sweep over the run's journal as it reads now.
fn sweep(root: &Path, run_dir: &Path, run_id: &str) {
    let events = read_all(run_dir).unwrap();
    Notifier::new(root, run_dir, run_id).sweep(&events);
}

/// Waits until `pred` holds over the journal.
fn wait_journal(run_dir: &Path, what: &str, pred: impl Fn(&[Event]) -> bool) -> Vec<Event> {
    let start = Instant::now();
    loop {
        let events = read_all(run_dir).unwrap();
        if pred(&events) {
            return events;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "timed out waiting for {what}: {}",
            events
                .iter()
                .map(|e| format!("{} {}", e.seq, serde_json::to_string(&e.payload).unwrap()))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether the label's intent is recorded and confirmed.
fn confirmed_under(events: &[Event], label: &str) -> bool {
    let mut confirmed = Vec::new();
    let mut recorded = Vec::new();
    for e in events {
        match &e.payload {
            EventPayload::SendIntentRecorded {
                intent, label: l, ..
            } if l == label => {
                recorded.push(intent.clone());
            }
            EventPayload::SendIntentSettled {
                intent, outcome, ..
            } if recorded.contains(intent) && outcome == "confirmed" => {
                confirmed.push(intent.clone())
            }
            _ => {}
        }
    }
    !confirmed.is_empty()
}

/// The journal intents recorded under `label`.
fn intents(run_dir: &Path, label: &str) -> Vec<String> {
    read_all(run_dir)
        .unwrap()
        .into_iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SendIntentRecorded { label: l, .. } if l == label => Some(l.clone()),
            _ => None,
        })
        .collect()
}

use apb_engine::event::EventPayload;

/// The reminder test's journal: subscribed to target A under `remind_ms`,
/// review `gate` open since `opened_ago_ms` ago, its notice confirmed.
fn base_journal(env: &Env, run_id: &str, remind_ms: &[u128], opened_ago_ms: u128) -> PathBuf {
    let t0 = now() - opened_ago_ms;
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(target(env, "me", "42"), remind_ms)),
        (t0, review_requested(t0, "gate")),
    ];
    for v in confirmed_notice("n1", "notify:review.2") {
        lines.push((t0 + 1_000, v));
    }
    seed_run(env, run_id, &lines)
}

#[test]
fn a_reminder_is_sent_through_the_ledger_once() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-once", &[REMIND_MS], REMIND_MS + 60_000);

    // The sweep sends the reminder, once: a second sweep sends nothing.
    sweep(env.root(), &run_dir, "r-once");
    wait_journal(&run_dir, "the reminder", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    assert_eq!(intents(&run_dir, "notify:review.2.r1").len(), 1);
    assert_eq!(server.requests().len(), 1, "one request to the target");
    sweep(env.root(), &run_dir, "r-once");
    assert_eq!(server.requests().len(), 1, "never a second reminder");
    assert_eq!(intents(&run_dir, "notify:review.2.r1").len(), 1);
}

#[test]
fn a_pause_in_the_middle_moves_the_reminder() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    // A 1h reminder over a gate open two wall hours, of which the run was
    // paused 90 minutes: 30 active minutes, not due.
    let t0 = now() - 2 * HOUR_MS;
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(target(&env, "me", "42"), &[HOUR_MS])),
        (t0, review_requested(t0, "gate")),
    ];
    for v in confirmed_notice("n1", "notify:review.2") {
        lines.push((t0 + 1_000, v));
    }
    // The person paused the run 30 minutes in, for 90 minutes.
    lines.push((
        t0 + 30 * 60_000,
        serde_json::json!({"type": "run_paused", "reason": "looking", "by": "operator", "mode": "drain"}),
    ));
    lines.push((
        t0 + 120 * 60_000,
        serde_json::json!({"type": "run_resumed", "from_node": "gate", "by": "operator"}),
    ));
    let run_dir = seed_run(&env, "r-pause", &lines);

    sweep(env.root(), &run_dir, "r-pause");
    assert!(
        intents(&run_dir, "notify:review.2.r1").is_empty(),
        "a 1h reminder over a 2h gate paused 90m is not due: {}",
        server.requests().len()
    );
    assert_eq!(server.requests().len(), 0);

    // The positive control: the same journal one wall hour earlier has
    // 90 active minutes behind it, and the same gate is reminded.
    shift_all_back(&run_dir, HOUR_MS);
    sweep(env.root(), &run_dir, "r-pause");
    wait_journal(&run_dir, "the reminder", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    assert_eq!(server.requests().len(), 1);
}

/// Moves every event of the journal back by `ms`, in the file: the
/// journal is the test's, and the reminder clock reads these times.
fn shift_all_back(run_dir: &Path, ms: u128) {
    let path = run_dir.join("events.jsonl");
    let content = std::fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in content.lines() {
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        v["ts"] = serde_json::json!(v["ts"].as_u64().unwrap() - ms as u64);
        out.push_str(&serde_json::to_string(&v).unwrap());
        out.push('\n');
    }
    std::fs::write(path, out).unwrap();
}

#[test]
fn a_decided_gate_sends_no_reminder() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let t0 = now() - HOUR_MS;
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(target(&env, "me", "42"), &[HOUR_MS])),
        (t0, review_requested(t0, "gate")),
    ];
    for v in confirmed_notice("n1", "notify:review.2") {
        lines.push((t0 + 1_000, v));
    }
    // The positive control: the same gate left open is reminded at 60
    // minutes.
    let open_dir = seed_run(&env, "r-open", &lines);
    // Decided at 59 minutes: the wait closed before the reminder.
    lines.push((
        t0 + 59 * 60_000,
        serde_json::json!({"type": "review_decided", "node": "gate", "decision": "approved", "note": ""}),
    ));
    let run_dir = seed_run(&env, "r-decided", &lines);

    sweep(env.root(), &run_dir, "r-decided");
    assert!(intents(&run_dir, "notify:review.2.r1").is_empty());
    assert_eq!(server.requests().len(), 0, "nothing due at 60 minutes");
    sweep(env.root(), &open_dir, "r-open");
    wait_journal(&open_dir, "the reminder of the open gate", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_reminder_reads_still_waiting() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-still", &[REMIND_MS], REMIND_MS + 60_000);
    sweep(env.root(), &run_dir, "r-still");
    wait_journal(&run_dir, "the reminder", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    let body = server.requests().last().cloned().unwrap();
    assert!(
        body.contains("Still waiting after 10m"),
        "the reminder text: {body}"
    );
    assert!(body.contains("Reminder"), "the subject prefix: {body}");
}

#[test]
fn a_reask_restarts_the_reminders() {
    let server = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"m-1"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-2"}"#.into()),
    ]);
    let env = Env::new(&server, None, "100");
    // The first request open 11 minutes, its notice confirmed; withdrawn
    // `scope_changed`, the gate asked again at once.
    let t0 = now() - REMIND_MS - 60_000;
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(target(&env, "me", "42"), &[REMIND_MS])),
        (t0, review_requested(t0, "gate")),
    ];
    for v in confirmed_notice("n1", "notify:review.2") {
        lines.push((t0 + 1_000, v));
    }
    let reask = t0 + REMIND_MS - 30_000;
    lines.push((
        reask,
        serde_json::json!({"type": "review_withdrawn", "node": "gate", "reason": "scope_changed: the scope changed while the request waited"}),
    ));
    lines.push((reask, review_requested(reask, "gate")));
    let run_dir = seed_run(&env, "r-reask", &lines);
    // Only a form gate is withdrawn `scope_changed`: the gate here limits
    // its scope, so its decision must name the request it answers.
    std::fs::write(run_dir.join("playbook.yaml"), FORM_PLAYBOOK).unwrap();

    // The withdrawn request sends no reminder; the new request's notice
    // goes out.
    let new_seq = read_all(&run_dir)
        .unwrap()
        .iter()
        .rev()
        .find(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "gate"),
        )
        .unwrap()
        .seq;
    sweep(env.root(), &run_dir, "r-reask");
    wait_journal(&run_dir, "the new notice", |ev| {
        ev.iter().any(|e| matches!(&e.payload,
            EventPayload::SendIntentRecorded { label, .. } if *label == format!("notify:review.{new_seq}")))
    });
    assert!(
        intents(&run_dir, "notify:review.2.r1").is_empty(),
        "no reminder of the withdrawn request"
    );
    // Once the new request's notice is confirmed, its reminder is due
    // after its own threshold: the test writes the new request's opening
    // time back past the threshold (the journal is this test's).
    shift_ts_back(&run_dir, new_seq, REMIND_MS + 60_000);
    sweep(env.root(), &run_dir, "r-reask");
    let reminder_label = format!("notify:review.{new_seq}.r1");
    wait_journal(&run_dir, "the reminder of the new request", |ev| {
        confirmed_under(ev, &reminder_label)
    });
    let reminder_body = server
        .requests()
        .into_iter()
        .find(|r| r.contains("Still waiting after"))
        .expect("the reminder names Still waiting");
    assert!(reminder_body.contains("Reminder"), "{reminder_body}");
    assert!(
        reminder_body.contains(&format!("--request=review:{new_seq} r-reask gate")),
        "the reminder's command names the re-asked request: {reminder_body}"
    );
}

/// [`PLAYBOOK`] with a form gate (`max_scope`).
const FORM_PLAYBOOK: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: gate, type: human_review, title: Ship it, options: [approved, rejected], max_scope: { files: 100 } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: gate }\n  - { from: gate, to: done }\n";

/// Moves the `ts` of the event at `seq` back by `ms`, in the file: the
/// journal is the test's, and the reminder clock reads these times.
fn shift_ts_back(run_dir: &Path, seq: u64, ms: u128) {
    let path = run_dir.join("events.jsonl");
    let content = std::fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in content.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v["seq"] == serde_json::json!(seq) {
            let mut v = v.clone();
            v["ts"] = serde_json::json!(v["ts"].as_u64().unwrap() - ms as u64);
            out.push_str(&serde_json::to_string(&v).unwrap());
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    std::fs::write(path, out).unwrap();
}

#[test]
fn a_reminder_admitted_before_the_decision_completes() {
    let (arrived_tx, arrived) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let server = spawn_script(vec![Reply::Held {
        body: r#"{"id":"m-1"}"#.into(),
        arrived: arrived_tx,
        release: release_rx,
    }]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-inflight", &[REMIND_MS], REMIND_MS + 60_000);

    // The reminder's request reaches the service (its intent was admitted
    // under the journal lock before it left); the wait closes while the
    // answer is still on its way; then the answer arrives.
    let sweeper = {
        let (root, dir) = (env.root().to_path_buf(), run_dir.clone());
        std::thread::spawn(move || sweep(&root, &dir, "r-inflight"))
    };
    arrived
        .recv_timeout(DEADLINE)
        .expect("the reminder's request reached the service");
    let mut log = EventLog::open(&run_dir).unwrap();
    let decided = log
        .append(
            serde_json::from_value::<EventPayload>(serde_json::json!({
                "type": "review_decided", "node": "gate", "decision": "approved", "note": ""
            }))
            .unwrap(),
        )
        .unwrap()
        .seq;
    drop(log);
    release.send(()).unwrap();
    sweeper.join().unwrap();

    // The reminder settles confirmed under its label, after the decision:
    // an admitted send completes even though its wait closed meanwhile.
    let events = wait_journal(&run_dir, "the settlement", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    let intent = events
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::SendIntentRecorded { intent, label, .. }
                if label == "notify:review.2.r1" =>
            {
                Some(intent.clone())
            }
            _ => None,
        })
        .unwrap();
    let settled = events
        .iter()
        .find(|e| {
            matches!(&e.payload, EventPayload::SendIntentSettled { intent: i, outcome, .. }
                if *i == intent && outcome == "confirmed")
        })
        .unwrap()
        .seq;
    assert!(
        settled > decided,
        "the confirmation ({settled}) follows the decision ({decided})"
    );
    assert_eq!(server.requests().len(), 1);
    sweep(env.root(), &run_dir, "r-inflight");
    assert_eq!(
        server.requests().len(),
        1,
        "the closed wait is not reminded again"
    );
}

/// The cut reminder intent of [`a_crash_after_the_intent_does_not_resend`]:
/// recorded by a sender that already exited, with no answer, written back
/// a minute (far past the retry gap); `settled` adds a `not_sent`
/// settlement of a passing fault, a try that may be retried.
fn cut_reminder_intent(run_dir: &Path, settled: bool) {
    let mut log = EventLog::open(run_dir).unwrap();
    let recorded = log
        .append(
            serde_json::from_value::<EventPayload>(serde_json::json!({
                "type": "send_intent_recorded",
                "intent": "i-cut",
                "nonce": "n-cut",
                "node": NOTIFY_SITE,
                "connector": CONNECTOR,
                "function": "notify_person",
                "account": "me",
                "mode": "none",
                "label": "notify:review.2.r1",
                // The sender that crashed: a process that already exited.
                "pid": dead_pid(),
                "deadline_ms": 0
            }))
            .unwrap(),
        )
        .unwrap()
        .seq;
    let settlement = settled.then(|| {
        log.append(
            serde_json::from_value::<EventPayload>(serde_json::json!({
                "type": "send_intent_settled",
                "intent": "i-cut",
                "outcome": "not_sent",
                "source": "refused",
            }))
            .unwrap(),
        )
        .unwrap()
        .seq
    });
    drop(log);
    for seq in std::iter::once(recorded).chain(settlement) {
        shift_ts_back(run_dir, seq, 60_000);
    }
}

#[test]
fn a_crash_after_the_intent_does_not_resend() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    // A journal cut after the reminder's intent was recorded, before its
    // settlement: the intent with no answer, older than the retry gap.
    let run_dir = base_journal(&env, "r-crash", &[REMIND_MS], REMIND_MS + 60_000);
    cut_reminder_intent(&run_dir, false);

    // The next sweep reconciles or refuses it through the ledger and never
    // sends a second request for the label.
    sweep(env.root(), &run_dir, "r-crash");
    assert_eq!(
        server.requests().len(),
        0,
        "no second request for the cut label"
    );
    assert_eq!(intents(&run_dir, "notify:review.2.r1").len(), 1);

    // The control: an equally old try known not to have gone out is tried
    // again, so the retry gap is not what held the cut one back.
    let control = base_journal(&env, "r-crash-control", &[REMIND_MS], REMIND_MS + 60_000);
    cut_reminder_intent(&control, true);
    sweep(env.root(), &control, "r-crash-control");
    wait_journal(&control, "the retried reminder", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    assert_eq!(server.requests().len(), 1, "the not-sent try is retried");
}

#[test]
fn a_question_and_a_host_task_are_reminded() {
    let server = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"m-1"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-2"}"#.into()),
    ]);
    let env = Env::new(&server, None, "100");
    let t0 = now() - REMIND_MS - 60_000;
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(target(&env, "me", "42"), &[REMIND_MS])),
        (
            t0,
            serde_json::json!({"type": "question_asked", "node": "ask", "question": "go on?"}),
        ),
        (
            t0,
            serde_json::json!({"type": "host_task_requested", "task_id": "t-1", "node": "work", "attempt": 1}),
        ),
    ];
    for v in confirmed_notice("n1", "notify:question.2") {
        lines.push((t0 + 1_000, v));
    }
    for v in confirmed_notice("n2", "notify:host_task.3") {
        lines.push((t0 + 1_000, v));
    }
    let run_dir = seed_run(&env, "r-both", &lines);

    sweep(env.root(), &run_dir, "r-both");
    wait_journal(&run_dir, "both reminders", |ev| {
        confirmed_under(ev, "notify:question.2.r1") && confirmed_under(ev, "notify:host_task.3.r1")
    });
    assert_eq!(server.requests().len(), 2, "one reminder each");
}

#[test]
fn a_target_change_moves_reminders_to_the_next_request() {
    let server_a = spawn_script(vec![Reply::Respond(200, r#"{"id":"a-1"}"#.into())]);
    let server_b = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"b-1"}"#.into()),
        Reply::Respond(200, r#"{"id":"b-2"}"#.into()),
    ]);
    let env = Env::new(&server_a, Some(&server_b), "100");
    // Review 1 noticed at A, eleven minutes open; then the target changed
    // to B.
    let run_dir = base_journal(&env, "r-move", &[REMIND_MS], REMIND_MS + 60_000);
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(
        serde_json::from_value::<EventPayload>(serde_json::json!({
            "type": "notify_set",
            "target": target(&env, "her", "7"),
            "on": ["review"],
            "source": "person",
            "by": "operator",
            "remind_ms": [600000]
        }))
        .unwrap(),
    )
    .unwrap();
    drop(log);

    // Eleven minutes of active time, the notice confirmed at A before the
    // change: no reminder at A or B.
    sweep(env.root(), &run_dir, "r-move");
    assert!(
        intents(&run_dir, "notify:review.2.r1").is_empty(),
        "an open request gets no reminder after its target changed"
    );
    assert_eq!(server_a.requests().len(), 0);
    assert_eq!(server_b.requests().len(), 0);

    // Review 1 decided; review 2 requested: its notice goes to B and, at
    // its threshold, its reminder goes to B.
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(
        serde_json::from_value::<EventPayload>(serde_json::json!({
            "type": "review_decided", "node": "gate", "decision": "approved", "note": ""
        }))
        .unwrap(),
    )
    .unwrap();
    drop(log);
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(
        serde_json::from_value::<EventPayload>(serde_json::json!({
            "type": "review_requested", "node": "gate", "options": ["approved", "rejected"]
        }))
        .unwrap(),
    )
    .unwrap();
    drop(log);
    sweep(env.root(), &run_dir, "r-move");
    wait_journal(&run_dir, "the notice at B", |ev| {
        ev.iter().any(|e| {
            matches!(&e.payload,
            EventPayload::SendIntentRecorded { label, .. } if label == "notify:review.7")
        })
    });
    // The notice's intent records the whole target it went to: B, with
    // its recipient and pins.
    let b: apb_engine::notify::record::NotifyTargetRecord =
        serde_json::from_value(target(&env, "her", "7")).unwrap();
    let recorded = read_all(&run_dir)
        .unwrap()
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::SendIntentRecorded {
                label,
                target_digest,
                ..
            } if label == "notify:review.7" => Some(target_digest),
            _ => None,
        });
    assert_eq!(recorded, Some(Some(b.digest())));
    // Its notice settled confirmed to B; the next sweep reminds at B.
    // The second request's reminder is due after its own threshold: the
    // test writes its opening time back past it.
    let second = read_all(&run_dir)
        .unwrap()
        .iter()
        .rev()
        .find(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "gate"),
        )
        .unwrap()
        .seq;
    shift_ts_back(&run_dir, second, REMIND_MS + 60_000);
    sweep(env.root(), &run_dir, "r-move");
    wait_journal(&run_dir, "the reminder at B", |ev| {
        confirmed_under(ev, &format!("notify:review.{second}.r1"))
    });
    assert_eq!(server_a.requests().len(), 0, "A saw nothing of the change");
    assert!(
        server_b.requests().len() >= 2,
        "B saw the notice and the reminder"
    );
}

/// A list node with a pilot (`list.pilot`): its pilot gate is a review on
/// the list node, so the reminders cover it like any gate.
const PILOT_PLAYBOOK: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - id: work\n    type: script\n    script: scripts/item.sh\n    runner: sh\n    list:\n      items: { inline: [ { id: i1 }, { id: i2 }, { id: i3 } ] }\n      batch: { size: 1, parallel: 1, retries: 0 }\n      pilot: { items: 1 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: s, to: work }\n  - { from: work, to: done }\n";

fn pilot_requested(node: &str) -> serde_json::Value {
    serde_json::json!({"type": "review_requested", "node": node, "options": ["continue", "revise", "stop"]})
}

/// A pilot gate is reminded, and a `revise` that runs a new slice and asks
/// again restarts the reminders: the answered request gets none, the new
/// one gets its own after its own threshold, and its reminder's command
/// names the new request (a pilot gate's decision must name it).
#[test]
fn a_pilot_gate_is_reminded_and_a_revise_restarts_the_reminders() {
    let server = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"m-1"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-2"}"#.into()),
    ]);
    let env = Env::new(&server, None, "100");
    let t0 = now() - REMIND_MS - 60_000;
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(target(&env, "me", "42"), &[REMIND_MS])),
        (
            t0,
            serde_json::json!({"type": "list_pilot", "node": "work", "round": 1, "items": ["i1"]}),
        ),
        (t0, pilot_requested("work")),
    ];
    for v in confirmed_notice("n1", "notify:review.3") {
        lines.push((t0 + 1_000, v));
    }
    let reask = t0 + REMIND_MS - 30_000;
    lines.push((
        reask,
        serde_json::json!({"type": "review_decided", "node": "work", "decision": "revise", "note": "smaller slice"}),
    ));
    lines.push((
        reask,
        serde_json::json!({"type": "list_pilot", "node": "work", "round": 2, "items": ["i2"]}),
    ));
    lines.push((reask, pilot_requested("work")));
    let run_dir = seed_run(&env, "r-pilot", &lines);
    std::fs::write(run_dir.join("playbook.yaml"), PILOT_PLAYBOOK).unwrap();

    let new_seq = read_all(&run_dir)
        .unwrap()
        .iter()
        .rev()
        .find(
            |e| matches!(&e.payload, EventPayload::ReviewRequested { node, .. } if node == "work"),
        )
        .unwrap()
        .seq;
    sweep(env.root(), &run_dir, "r-pilot");
    wait_journal(&run_dir, "the new pilot notice", |ev| {
        ev.iter().any(|e| matches!(&e.payload,
            EventPayload::SendIntentRecorded { label, .. } if *label == format!("notify:review.{new_seq}")))
    });
    assert!(
        intents(&run_dir, "notify:review.3.r1").is_empty(),
        "no reminder of the answered pilot request"
    );
    shift_ts_back(&run_dir, new_seq, REMIND_MS + 60_000);
    sweep(env.root(), &run_dir, "r-pilot");
    let reminder_label = format!("notify:review.{new_seq}.r1");
    wait_journal(&run_dir, "the reminder of the new pilot request", |ev| {
        confirmed_under(ev, &reminder_label)
    });
    let reminder_body = server
        .requests()
        .into_iter()
        .find(|r| r.contains("Still waiting after"))
        .expect("the reminder names Still waiting");
    assert!(reminder_body.contains("Reminder"), "{reminder_body}");
    assert!(
        reminder_body.contains(&format!("--request=review:{new_seq} r-pilot work")),
        "the reminder's command names the new request: {reminder_body}"
    );
}

/// The pid of a process that already exited.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    assert!(child.wait().unwrap().success());
    child.id()
}

/// The gate decided after the journal a sweep read: what the run's
/// journal gains between the sweep's read and its admission.
fn decide_gate(run_dir: &Path) {
    let mut log = EventLog::open(run_dir).unwrap();
    log.append(
        serde_json::from_value::<EventPayload>(serde_json::json!({
            "type": "review_decided", "node": "gate", "decision": "approved", "note": ""
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Every event recorded or settled for `label`.
fn ledger_lines(run_dir: &Path, label: &str) -> (usize, Vec<String>) {
    let events = read_all(run_dir).unwrap();
    let mut recorded = Vec::new();
    let mut settled = Vec::new();
    for e in &events {
        match &e.payload {
            EventPayload::SendIntentRecorded {
                intent, label: l, ..
            } if l == label => recorded.push(intent.clone()),
            EventPayload::SendIntentSettled {
                intent, outcome, ..
            } if recorded.contains(intent) => settled.push(outcome.clone()),
            _ => {}
        }
    }
    (recorded.len(), settled)
}

/// The ordering boundary on the send path (design D9 item 7): a sweep
/// over a slice that still shows the gate open and the reminder due, when
/// the journal on disk has since closed it, sends nothing to a target that
/// resolves and records no intent: the admission rechecks under the
/// journal lock.
#[test]
fn a_reminder_due_on_a_stale_journal_is_not_admitted() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-stale", &[REMIND_MS], REMIND_MS + 60_000);
    let stale = read_all(&run_dir).unwrap();
    decide_gate(&run_dir);
    Notifier::new(env.root(), &run_dir, "r-stale").sweep(&stale);
    assert_eq!(server.requests().len(), 0, "no request reaches the target");
    assert_eq!(
        ledger_lines(&run_dir, "notify:review.2.r1"),
        (0, Vec::new()),
        "no intent for the reminder whose wait closed"
    );
}

/// The not-sent path of a closed wait whose target's function no longer
/// names the args a notification fills (design D9 item 8, the args
/// variant): no intent and no settlement for the reminder.
#[test]
fn a_closed_wait_gets_no_unsent_reminder_when_the_args_are_gone() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    // The installed function no longer names who a notification goes to;
    // the run's pins name this connector, approved again.
    let cfg = std::path::PathBuf::from(std::env::var("APB_CONFIG_DIR").unwrap());
    let yaml_path = cfg
        .join("connectors")
        .join(CONNECTOR)
        .join("connector.yaml");
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    std::fs::write(
        &yaml_path,
        yaml.replace(
            "people: { to: to, subject: subject, text: text }",
            "people: { subject: subject, text: text }",
        ),
    )
    .unwrap();
    env.approve("me");
    let run_dir = base_journal(&env, "r-args", &[REMIND_MS], REMIND_MS + 60_000);
    let stale = read_all(&run_dir).unwrap();
    decide_gate(&run_dir);
    Notifier::new(env.root(), &run_dir, "r-args").sweep(&stale);
    assert_eq!(server.requests().len(), 0);
    assert_eq!(
        ledger_lines(&run_dir, "notify:review.2.r1"),
        (0, Vec::new()),
        "no intent and no settlement for the closed wait"
    );
}

/// A person's `notify_set` with the same target and `remind_ms`, appended
/// after the journal a sweep read.
fn set_after_read(env: &Env, run_dir: &Path, to: &str, remind_ms: &[u128]) {
    let mut log = EventLog::open(run_dir).unwrap();
    log.append(
        serde_json::from_value::<EventPayload>(serde_json::json!({
            "type": "notify_set",
            "target": target(env, "me", to),
            "on": ["review", "question", "host_task"],
            "source": "person",
            "by": "operator",
            "remind_ms": remind_ms,
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Two sweeps of one run at once (the drive's and `apb resume --all`'s)
/// over a journal that showed reminder `r1` due, when a person turned the
/// reminders off before either admitted it: the locked recheck finds no
/// reminder due, so no intent is recorded and nothing is sent. Without the
/// recheck the send ledger alone admits one of them.
#[test]
fn two_sweeps_send_one_reminder() {
    let server = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"m-1"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-2"}"#.into()),
    ]);
    let env = Env::new(&server, None, "600000");
    let run_dir = base_journal(&env, "r-two", &[REMIND_MS], REMIND_MS + 60_000);
    let events = read_all(&run_dir).unwrap();
    set_after_read(&env, &run_dir, "42", &[]);
    race_two_sweeps_over(env.root(), &run_dir, "r-two", &events);
    assert_eq!(server.requests().len(), 0, "no request reaches the target");
    assert_eq!(
        ledger_lines(&run_dir, "notify:review.2.r1"),
        (0, Vec::new()),
        "no intent once the reminders were turned off"
    );
}

/// The plain double sweep, nothing changed in between: one intent for the
/// reminder and one request; the other sweep is answered from the ledger
/// (design D9 item 9, variant a).
#[test]
fn two_sweeps_of_an_unchanged_journal_send_one_reminder() {
    let server = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"m-1"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-2"}"#.into()),
    ]);
    // A long retry gap: a second try of the label is never due here.
    let env = Env::new(&server, None, "600000");
    let run_dir = base_journal(&env, "r-two-plain", &[REMIND_MS], REMIND_MS + 60_000);
    race_two_sweeps(env.root(), &run_dir, "r-two-plain");
    assert_eq!(server.requests().len(), 1, "one request to the target");
    let (recorded, settled) = ledger_lines(&run_dir, "notify:review.2.r1");
    assert_eq!(recorded, 1, "one intent for the label");
    assert_eq!(settled, vec!["confirmed".to_string()]);
}

/// The admission recomputes the due reminder under the new cadence: a
/// cadence changed between the sweep's read and the admission, even one
/// under which `r1` would still be the latest due threshold, records no
/// intent; the next sweep selects under the cadence in force.
#[test]
fn a_cadence_changed_before_admission_records_no_intent() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-cadence", &[REMIND_MS], REMIND_MS + 60_000);
    let stale = read_all(&run_dir).unwrap();
    set_after_read(&env, &run_dir, "42", &[REMIND_MS / 2]);
    Notifier::new(env.root(), &run_dir, "r-cadence").sweep(&stale);
    assert_eq!(server.requests().len(), 0, "no request reaches the target");
    assert_eq!(
        ledger_lines(&run_dir, "notify:review.2.r1"),
        (0, Vec::new())
    );
}

/// A target changed between the sweep's read and the admission records no
/// reminder intent under the target the sweep selected.
#[test]
fn a_target_changed_before_admission_records_no_intent() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-retarget", &[REMIND_MS], REMIND_MS + 60_000);
    let stale = read_all(&run_dir).unwrap();
    set_after_read(&env, &run_dir, "43", &[REMIND_MS]);
    Notifier::new(env.root(), &run_dir, "r-retarget").sweep(&stale);
    assert_eq!(server.requests().len(), 0, "no request reaches the target");
    assert_eq!(
        ledger_lines(&run_dir, "notify:review.2.r1"),
        (0, Vec::new())
    );
}

/// A sub-playbook whose inherited subscription started with no reminder
/// cadence follows its parent's cadence in force: the parent turned
/// reminders on while the child's review stayed open, and the child's own
/// sweep sends the reminder.
#[test]
fn a_child_follows_the_cadence_its_parent_turned_on() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let t0 = now() - REMIND_MS - 60_000;
    // The parent: subscribed without reminders, then turned them on with
    // the same target a moment ago.
    seed_run(
        &env,
        "r-parent",
        &[
            (
                t0,
                serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            ),
            (t0, notify_set(target(&env, "me", "42"), &[])),
            (
                now() - 1_000,
                notify_set(target(&env, "me", "42"), &[REMIND_MS]),
            ),
        ],
    );
    // The child: inherited the subscription with no cadence, its review
    // open past the parent's threshold, its notice confirmed.
    let run_dir = inherited_child(&env, "r-child", "r-parent", t0, 1);
    sweep(env.root(), &run_dir, "r-child");
    wait_journal(&run_dir, "the child's reminder", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    assert_eq!(server.requests().len(), 1, "one reminder to the target");
}

/// A sub-playbook's notice went to its parent's target; the parent then
/// changed only the recipient, in the same millisecond as far as any clock
/// can tell. The child sends no reminder: the new recipient never got the
/// notice.
#[test]
fn a_child_sends_no_reminder_after_its_parent_changed_the_recipient() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let t0 = now() - REMIND_MS - 60_000;
    seed_run(
        &env,
        "r-parent",
        &[
            (
                t0,
                serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            ),
            (t0, notify_set(target(&env, "me", "42"), &[REMIND_MS])),
            (
                t0 + 1_000,
                notify_set(target(&env, "me", "43"), &[REMIND_MS]),
            ),
        ],
    );
    let run_dir = inherited_child(&env, "r-child", "r-parent", t0, 1);
    sweep(env.root(), &run_dir, "r-child");
    assert!(
        intents(&run_dir, "notify:review.2.r1").is_empty(),
        "no reminder to a recipient the notice did not reach"
    );
    assert_eq!(server.requests().len(), 0);
}

/// Variant b of [`two_sweeps_of_an_unchanged_journal_send_one_reminder`]: a live target that does
/// not resolve (its connector was removed after the subscription pinned
/// it). Both sweeps reach the not-sent path of a target that does not
/// load; exactly one intent and one `not_sent` settlement are recorded for
/// the label.
#[test]
fn two_sweeps_record_one_unsent_reminder() {
    let server = spawn_script(vec![]);
    let env = Env::new(&server, None, "600000");
    let t0 = now() - REMIND_MS - 60_000;
    let pinned = target(&env, "me", "42");
    // The connector is gone: the live target no longer loads.
    std::fs::remove_dir_all(env._cfg.path().join("connectors").join(CONNECTOR)).unwrap();
    let mut lines: Vec<(u128, serde_json::Value)> = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, notify_set(pinned, &[REMIND_MS])),
        (t0, review_requested(t0, "gate")),
    ];
    for v in confirmed_notice("n1", "notify:review.2") {
        lines.push((t0 + 1_000, v));
    }
    let run_dir = seed_run(&env, "r-two-unsent", &lines);
    race_two_sweeps(env.root(), &run_dir, "r-two-unsent");
    assert_eq!(server.requests().len(), 0);
    let (recorded, settled) = ledger_lines(&run_dir, "notify:review.2.r1");
    assert_eq!(recorded, 1, "one intent for the label, never two");
    assert_eq!(settled, vec!["not_sent".to_string()]);
}

/// A child journal that inherited the subscription from `parent` with no
/// cadence, its review open past the threshold and its notice confirmed.
fn inherited_child(env: &Env, run_id: &str, parent: &str, t0: u128, depth: usize) -> PathBuf {
    let mut inherited = notify_set(target(env, "me", "42"), &[]);
    inherited["source"] = serde_json::json!("parent");
    inherited["on"] = serde_json::json!(["review", "question"]);
    let mut lines = vec![
        (
            t0,
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
        ),
        (t0, inherited),
        (t0, review_requested(t0, "gate")),
    ];
    // The notice went to the parent's target as it was then, recorded
    // with that target's digest.
    let sent_to: apb_engine::notify::record::NotifyTargetRecord =
        serde_json::from_value(target(env, "me", "42")).unwrap();
    for mut v in confirmed_notice("n1", "notify:review.2") {
        if v["type"] == "send_intent_recorded" {
            v["target_digest"] = serde_json::json!(sent_to.digest());
        }
        lines.push((t0 + 1_000, v));
    }
    let run_dir = seed_run(env, run_id, &lines);
    apb_engine::run_config::write_run_config(
        &run_dir,
        &apb_engine::run_config::RunConfig {
            parent_run: Some(parent.into()),
            depth,
            ..Default::default()
        },
    )
    .unwrap();
    run_dir
}

/// A sub-playbook of a sub-playbook follows the subscription in force at
/// the top of its chain, not the copy its own parent journaled: the top
/// run turned reminders on, and the grandchild's sweep sends its reminder.
#[test]
fn a_grandchild_follows_the_cadence_at_the_top_of_its_chain() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let t0 = now() - REMIND_MS - 60_000;
    seed_run(
        &env,
        "r-top",
        &[
            (
                t0,
                serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            ),
            (t0, notify_set(target(&env, "me", "42"), &[])),
            (
                now() - 1_000,
                notify_set(target(&env, "me", "42"), &[REMIND_MS]),
            ),
        ],
    );
    inherited_child(&env, "r-mid", "r-top", t0, 1);
    let run_dir = inherited_child(&env, "r-leaf", "r-mid", t0, 2);
    sweep(env.root(), &run_dir, "r-leaf");
    wait_journal(&run_dir, "the grandchild's reminder", |ev| {
        confirmed_under(ev, "notify:review.2.r1")
    });
    assert_eq!(server.requests().len(), 1, "one reminder to the target");
}

/// Two notifiers of one run sweep the journal as it reads now, released
/// together.
fn race_two_sweeps(root: &Path, run_dir: &Path, run_id: &str) {
    let events = read_all(run_dir).unwrap();
    race_two_sweeps_over(root, run_dir, run_id, &events);
}

/// Two notifiers of one run sweep the slice `events`, released together.
fn race_two_sweeps_over(root: &Path, run_dir: &Path, run_id: &str, events: &[Event]) {
    let events = events.to_vec();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let (root, run_dir, run_id) = (
                root.to_path_buf(),
                run_dir.to_path_buf(),
                run_id.to_string(),
            );
            let (events, barrier) = (events.clone(), barrier.clone());
            std::thread::spawn(move || {
                let notifier = Notifier::new(&root, &run_dir, &run_id);
                barrier.wait();
                notifier.sweep(&events);
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

/// The admission recheck applies the filters `due` applies: a person's
/// change that drops `review` from the subscription between the sweep's
/// read and the admission keeps the reminder from going out.
#[test]
fn a_reminder_of_a_kind_no_longer_covered_is_not_admitted() {
    let server = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
    let env = Env::new(&server, None, "100");
    let run_dir = base_journal(&env, "r-narrowed", &[REMIND_MS], REMIND_MS + 60_000);
    let stale = read_all(&run_dir).unwrap();
    let mut log = EventLog::open(&run_dir).unwrap();
    log.append(
        serde_json::from_value::<EventPayload>(serde_json::json!({
            "type": "notify_set",
            // The same target, so its notice still counts.
            "target": target(&env, "me", "42"),
            "on": ["question"],
            "source": "person",
            "by": "operator",
            "remind_ms": [REMIND_MS]
        }))
        .unwrap(),
    )
    .unwrap();
    drop(log);
    Notifier::new(env.root(), &run_dir, "r-narrowed").sweep(&stale);
    assert_eq!(server.requests().len(), 0, "no request reaches the target");
    assert_eq!(
        ledger_lines(&run_dir, "notify:review.2.r1"),
        (0, Vec::new())
    );
}
