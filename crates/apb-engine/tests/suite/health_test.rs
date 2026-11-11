//! Routine health end to end (routine health D2): the rules over a
//! playbook's runs, one notice per trip claimed and settled in the signed
//! health file, the hold every start honours, a clear and a mute that
//! fence every claim made before them, and the archive counted after a
//! prune. The notify target is a stand-in chat service.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

use apb_core::connector::{config, store};
use apb_core::registry::init_project;
use apb_core::scope::Scope;
use apb_core::signed_store::{self, Unsigned};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::deadline_hooks::{self, Hook};
use apb_engine::event::{EventLog, EventPayload};
use apb_engine::health::state::{HealthFile, NoticeRecord, RuleState};
use apb_engine::scheduler::{RunOptions, run};

use crate::common::{self, EnvVar, Reply, ScriptedHttpServer, spawn_script};

const CONNECTOR: &str = "mock-tracker";
const TOKEN_VAR: &str = "APB_HEALTH_TEST_TOKEN";
const H: u64 = 3_600_000;
const MIN: u64 = 60_000;
const OK: &str = r#"{"id":"m-1"}"#;

/// `r`: one script node that fails while `<root>/fail` exists, with the
/// health block `health`.
fn routine(health: &str) -> String {
    format!(
        "schema: 2\nid: r\nname: r\nversion: 1.0.0\n{health}nodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: \"scripts/work.sh\", runner: sh }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: done }}\n"
    )
}

/// A project with playbook `r` and the stand-in target set as the
/// machine's notify target. Fields drop in declaration order: the env lock
/// goes last.
struct Env {
    _cfg_var: EnvVar,
    cfg: tempfile::TempDir,
    proj: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new(health: &str, server: &ScriptedHttpServer) -> Self {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let cfg_var = EnvVar::set("APB_CONFIG_DIR", cfg.path());
        apb_core::run_origin::ensure_key().unwrap();
        let proj = tempfile::tempdir().unwrap();
        init_project(proj.path()).unwrap();
        let env = Env {
            _cfg_var: cfg_var,
            cfg,
            proj,
            _lock: lock,
        };
        env.add_playbook("r", &routine(health));
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/connectors")
            .join(CONNECTOR);
        let dst = env.cfg.path().join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::copy(src.join("connector.yaml"), dst.join("connector.yaml")).unwrap();
        env.write_account(&server.base_url, TOKEN_VAR);
        std::fs::write(
            env.root().join(".apb/secrets.env"),
            format!("{TOKEN_VAR}=health-token-value\nAPB_HEALTH_OTHER=health-token-value\n"),
        )
        .unwrap();
        std::fs::write(
            env.cfg.path().join("config.yaml"),
            format!(
                "notify:\n  target: {{ connector: {CONNECTOR}, account: me, function: notify_person, to: \"42\" }}\n"
            ),
        )
        .unwrap();
        env.approve();
        common::seed_main(env.root());
        env
    }

    fn write_account(&self, base_url: &str, token_var: &str) {
        let accounts = config::project_config_path(self.root(), CONNECTOR);
        std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
        std::fs::write(
            accounts,
            format!(
                "accounts:\n  - name: me\n    default: true\n    base_url: {base_url}\n    token: \"{{{{env.{token_var}}}}}\"\n"
            ),
        )
        .unwrap();
    }

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
        let accounts = config::load_merged(self.root(), CONNECTOR).unwrap();
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

    fn add_playbook(&self, id: &str, yaml: &str) {
        let dir = self.root().join(".apb/playbooks").join(id);
        let vdir = dir.join("1.0.0");
        std::fs::create_dir_all(vdir.join("scripts")).unwrap();
        std::fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
        std::fs::write(dir.join("current"), "1.0.0").unwrap();
        let fail = self.root().join("fail");
        std::fs::write(
            vdir.join("scripts/work.sh"),
            format!(
                "#!/bin/sh\nif [ -f '{}' ]; then exit 1; fi\necho ok\n",
                fail.display()
            ),
        )
        .unwrap();
    }

    fn policy(&self, yaml: &str) {
        std::fs::write(self.cfg.path().join("policy.yaml"), yaml).unwrap();
    }
}

fn now() -> u64 {
    apb_core::clock::now_ms_u64()
}

/// The terminal event of a seeded run.
enum End<'a> {
    Ok,
    Failed,
    Aborted(&'a str, &'a str),
    Open,
}

/// A run of `playbook` started at `started_ms`, stamped as created here,
/// with `run_provenance` and the end `end`.
fn seed_run(root: &Path, id: &str, playbook: &str, started_ms: u64, end: End<'_>) -> PathBuf {
    let dir = root.join(".apb/runs").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    apb_core::run_origin::stamp(&dir, id).unwrap();
    let mut lines = vec![
        serde_json::json!({"seq": 1, "ts": started_ms, "type": "run_started", "playbook": playbook, "version": "1.0.0"}),
        serde_json::json!({"seq": 2, "ts": started_ms, "type": "run_provenance", "origin": "project"}),
    ];
    match end {
        End::Ok => lines.push(serde_json::json!({"seq": 3, "ts": started_ms + 10, "type": "run_finished", "outcome": "succeeded"})),
        End::Failed => lines.push(serde_json::json!({"seq": 3, "ts": started_ms + 10, "type": "run_finished", "outcome": "failed"})),
        End::Aborted(by, reason) => lines.push(serde_json::json!({"seq": 3, "ts": started_ms + 10, "type": "run_aborted", "reason": reason, "by": by})),
        End::Open => {}
    }
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("events.jsonl"), text).unwrap();
    dir
}

/// `n` failed runs of `r`, the newest started `ago` ms before now.
fn failures(root: &Path, n: usize, ago: u64) {
    for i in 0..n {
        let t = now() - ago - (n - i) as u64 * MIN;
        seed_run(root, &format!("r-{t}"), "r", t, End::Failed);
    }
}

fn state(root: &Path) -> HealthFile {
    signed_store::read::<HealthFile>(root, Scope::Project, "r")
        .unwrap()
        .file
}

fn has_state(root: &Path) -> bool {
    signed_store::file::<HealthFile>(root, Scope::Project, "r").exists()
}

fn rule(root: &Path, name: &str) -> Option<RuleState> {
    state(root).rules.get(name).cloned()
}

fn tripped(root: &Path, name: &str) -> bool {
    rule(root, name).is_some_and(|r| r.tripped)
}

fn eval(root: &Path, at: u64) -> apb_engine::health::HealthView {
    apb_engine::health::evaluate(root, Scope::Project, "r", &|| at).unwrap()
}

fn change(root: &Path, f: impl FnOnce(&mut HealthFile)) {
    signed_store::transact(
        root,
        Scope::Project,
        "r",
        Unsigned::Refuse,
        |h: &mut HealthFile| {
            f(h);
            Ok::<_, signed_store::StoreError>(((), true))
        },
    )
    .unwrap();
}

fn notices(root: &Path) -> Vec<NoticeRecord> {
    state(root).notices
}

fn record(id: &str, state: &str, tries: u32) -> NoticeRecord {
    NoticeRecord {
        id: id.into(),
        rule: "consecutive_failures".into(),
        kind: "trip".into(),
        recorded_ms: now(),
        subject: "r: consecutive_failures tripped".into(),
        text: "t".into(),
        target: apb_engine::notify::record::NotifyTargetRecord {
            connector: CONNECTOR.into(),
            account: "me".into(),
            function: "notify_person".into(),
            to: "42".into(),
            connector_digest: String::new(),
            account_digest: String::new(),
        },
        state: state.into(),
        claimant: None,
        claimed_ms: None,
        claim_generation: None,
        tries,
        detail: None,
    }
}

const TWO: &str = "health:\n  consecutive_failures: 2\n";
const THREE: &str = "health:\n  consecutive_failures: 3\n";

// 1. A notice per evaluation.
#[test]
fn one_notice_per_trip() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 3, H);
    for _ in 0..3 {
        eval(env.root(), now());
    }
    assert_eq!(server.requests().len(), 1);
    assert!(tripped(env.root(), "consecutive_failures"));
    let n = notices(env.root());
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].state, "sent");
}

// 2. Two senders of one notice.
#[test]
fn three_simultaneous_evaluations_send_one_notice() {
    let server = spawn_script(vec![
        Reply::Respond(400, r#"{"error":"bad"}"#.into()),
        Reply::Respond(200, OK.into()),
    ]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    eval(env.root(), now());
    assert_eq!(notices(env.root())[0].state, "failed");
    // A run of `r` still open: its terminal event is the third evaluator.
    let open = seed_run(env.root(), "r-open", "r", now() - MIN, End::Open);
    let barrier = Arc::new(Barrier::new(3));
    let root = env.root().to_path_buf();
    let threads: Vec<_> = (0..3)
        .map(|i| {
            let (b, root, open) = (barrier.clone(), root.clone(), open.clone());
            std::thread::spawn(move || {
                b.wait();
                match i {
                    0 => {
                        EventLog::open(&open)
                            .unwrap()
                            .append(EventPayload::RunFinished {
                                outcome: "failed".into(),
                            })
                            .unwrap();
                    }
                    1 => {
                        let _ = apb_engine::health::evaluate(&root, Scope::Project, "r", &now);
                    }
                    _ => {
                        let _ = apb_engine::health::sweep(&root, &now);
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(server.requests().len(), 2, "one failed call and one retry");
    let n = notices(env.root());
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].state, "sent");
    assert_eq!(n[0].tries, 1);
}

// 3. A resend after a crash mid-send.
#[test]
fn a_stale_claim_becomes_unknown() {
    let server = spawn_script(vec![]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    let t = now();
    change(env.root(), |h| {
        h.first_seen_ms = t - H;
        let mut n = record("h-old", "claimed", 0);
        n.claimant = Some("another-process".into());
        n.claimed_ms = Some(t - 61_000);
        n.claim_generation = Some(0);
        h.notices.push(n);
        h.rules.insert(
            "consecutive_failures".into(),
            RuleState {
                tripped: true,
                since_ms: t - H,
                detail: "2 failed in a row".into(),
                trip_notice: Some("h-old".into()),
                delivered_ms: None,
            },
        );
    });
    eval(env.root(), t);
    let n = notices(env.root());
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0].state, "unknown");
    assert!(server.requests().is_empty());
}

// 4. A missed first run.
#[test]
fn the_third_failure_of_a_new_routine_trips_at_once() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(THREE, &server);
    std::fs::write(env.root().join("fail"), "").unwrap();
    for _ in 0..3 {
        let r = run(env.root(), "r", None, RunOptions::default()).unwrap();
        assert_eq!(r.outcome, apb_engine::state::RunStatus::Failed);
    }
    assert!(has_state(env.root()), "the run ends evaluated the routine");
    assert!(tripped(env.root(), "consecutive_failures"));
}

// 5. A person's stop counted.
#[test]
fn a_person_stop_is_not_a_failure() {
    let server = spawn_script(vec![]);
    let env = Env::new(THREE, &server);
    failures(env.root(), 2, 2 * H);
    seed_run(
        env.root(),
        "r-stop",
        "r",
        now() - H,
        End::Aborted("operator", "stop requested"),
    );
    let v = eval(env.root(), now());
    assert_ne!(v.status, "tripped");
    assert!(!tripped(env.root(), "consecutive_failures") || !has_state(env.root()));
    assert!(server.requests().is_empty());
}

// 6. `overdue` tripping a new routine.
#[test]
fn overdue_gives_a_new_routine_its_grace() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new("health:\n  overdue: 26h\n", &server);
    let t = now();
    assert_eq!(eval(env.root(), t).status, "ok");
    assert!(!tripped(env.root(), "overdue"));
    assert_eq!(eval(env.root(), t + 26 * H + MIN).status, "tripped");
    assert!(tripped(env.root(), "overdue"));
}

// 7. A hold that the gate alone enforces.
#[test]
fn a_hold_set_after_the_gate_refuses_the_start() {
    let server = spawn_script(vec![]);
    let env = Env::new(
        "health:\n  consecutive_failures: 2\n  on_trip: hold\n",
        &server,
    );
    let t = now();
    change(env.root(), |h| {
        h.first_seen_ms = t;
        h.hold = Some(apb_engine::health::state::Hold {
            since_ms: t,
            rule: "consecutive_failures".into(),
            detail: "2 failed in a row".into(),
        });
    });
    let e = run(env.root(), "r", None, RunOptions::default()).unwrap_err();
    assert!(e.to_string().contains("playbook_on_hold"), "{e}");
    let runs = std::fs::read_dir(env.root().join(".apb/runs"))
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(runs, 0, "a refused start leaves no run");
}

// 8. A clear that holds again.
#[test]
fn a_clear_starts_over_overdue_included() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(
        "health:\n  overdue: 26h\n  consecutive_failures: 3\n  on_trip: hold\n",
        &server,
    );
    failures(env.root(), 5, H);
    let t = now();
    eval(env.root(), t + 27 * H);
    assert_eq!(
        apb_engine::health::view(env.root(), Scope::Project, "r", t + 27 * H)
            .unwrap()
            .unwrap()
            .status,
        "held"
    );
    apb_engine::health::clear(env.root(), Scope::Project, "r", t + 27 * H).unwrap();
    let v = eval(env.root(), t + 27 * H + MIN);
    assert_eq!(v.status, "ok", "{v:?}");
    assert!(state(env.root()).hold.is_none());
    assert!(apb_engine::health::hold_refusal(env.root(), Scope::Project, "r").is_ok());
}

// 9. A mute that swallows a trip.
#[test]
fn a_trip_while_muted_is_notified_after_unmute() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    let t = now();
    apb_engine::health::mute(env.root(), Scope::Project, "r", Some(t + 100 * H), t).unwrap();
    failures(env.root(), 2, H);
    eval(env.root(), t);
    assert!(tripped(env.root(), "consecutive_failures"));
    assert!(server.requests().is_empty(), "muted: nothing goes out");
    apb_engine::health::mute(env.root(), Scope::Project, "r", None, t).unwrap();
    eval(env.root(), t + MIN);
    assert_eq!(server.requests().len(), 1);
}

// 10. A recovery for an undelivered trip.
#[test]
fn no_recovery_for_an_undelivered_trip() {
    let bad = || Reply::Respond(400, r#"{"error":"bad"}"#.into());
    let server = spawn_script(vec![bad(), bad(), bad(), Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    for _ in 0..4 {
        eval(env.root(), now());
    }
    assert_eq!(server.requests().len(), 3);
    let n = notices(env.root());
    assert_eq!((n[0].state.as_str(), n[0].tries), ("failed", 3));
    seed_run(env.root(), "r-ok", "r", now() - 1000, End::Ok);
    eval(env.root(), now());
    assert!(!tripped(env.root(), "consecutive_failures"));
    assert_eq!(server.requests().len(), 3, "no recovery notice");
    assert!(notices(env.root()).iter().all(|n| n.kind == "trip"));
}

// 11. A drifted target.
#[test]
fn a_drifted_target_is_not_sent() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    let base_url = server.base_url.clone();
    let root = env.root().to_path_buf();
    deadline_hooks::set(Hook::HealthBeforeDispatch, env.root(), move |_| {
        // The account changes (another token variable) and is approved
        // again: trusted, but not the account the notice pinned.
        let accounts = config::project_config_path(&root, CONNECTOR);
        std::fs::write(
            accounts,
            format!(
                "accounts:\n  - name: me\n    default: true\n    base_url: {base_url}\n    token: \"{{{{env.APB_HEALTH_OTHER}}}}\"\n"
            ),
        )
        .unwrap();
        let accounts = config::load_merged(&root, CONNECTOR).unwrap();
        TrustStore::load()
            .approve_kind(
                &config::account_digest(&accounts[0]),
                &account_trust_id(CONNECTOR, "me"),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
    });
    eval(env.root(), now());
    deadline_hooks::clear(env.root());
    assert!(
        server.requests().is_empty(),
        "nothing goes to a drifted target"
    );
    let n = notices(env.root());
    assert_eq!(n[0].state, "failed", "{n:?}");
}

// 12. Outcome classes.
#[test]
fn outcome_classes_settle_the_ledger() {
    for (reply, want) in [
        (Reply::Respond(500, r#"{"error":"down"}"#.into()), "unknown"),
        (Reply::Respond(400, r#"{"error":"bad"}"#.into()), "failed"),
        (Reply::Cut, "unknown"),
    ] {
        let server = spawn_script(vec![reply]);
        let env = Env::new(TWO, &server);
        failures(env.root(), 2, H);
        eval(env.root(), now());
        assert_eq!(server.requests().len(), 1);
        assert_eq!(notices(env.root())[0].state, want);
    }
}

// 13. A forged state.
#[test]
fn a_forged_state_reads_as_held() {
    let server = spawn_script(vec![]);
    let env = Env::new(TWO, &server);
    let path = signed_store::file::<HealthFile>(env.root(), Scope::Project, "r");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"schema":1,"scope":"project","playbook":"r","notices":[]}"#,
    )
    .unwrap();
    let v = apb_engine::health::view(env.root(), Scope::Project, "r", now())
        .unwrap()
        .unwrap();
    assert_eq!(v.status, "held");
    assert_eq!(v.hold.as_ref().map(|h| h.rule.as_str()), Some("unreadable"));
    let refusal = apb_engine::health::hold_refusal(env.root(), Scope::Project, "r").unwrap_err();
    assert_eq!(refusal["policy"], "playbook_on_hold");
    assert_eq!(eval(env.root(), now()).status, "held");
}

// 14. An unwatched machine notifying.
#[test]
fn an_unwatched_machine_sends_nothing() {
    let server = spawn_script(vec![]);
    let env = Env::new(TWO, &server);
    env.policy("health:\n  playbooks: []\n");
    failures(env.root(), 3, H);
    let _ = apb_engine::health::evaluate(env.root(), Scope::Project, "r", &now);
    let _ = apb_engine::health::sweep(env.root(), &now);
    assert!(server.requests().is_empty());
    if let Some(v) = apb_engine::health::view(env.root(), Scope::Project, "r", now()).unwrap() {
        assert_eq!(v.status, "unwatched");
    }
}

// 15. A child's end evaluated.
#[test]
fn a_child_run_end_is_not_evaluated() {
    let server = spawn_script(vec![]);
    let env = Env::new(THREE, &server);
    env.add_playbook(
        "parent",
        "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: sub, type: playbook, playbook: r }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: sub }\n  - { from: sub, to: done }\n",
    );
    std::fs::write(env.root().join("fail"), "").unwrap();
    let _ = run(env.root(), "parent", None, RunOptions::default()).unwrap();
    assert!(!has_state(env.root()), "a child's end evaluates nothing");
    assert!(server.requests().is_empty());
}

// 16. Pruning resets health.
#[test]
fn health_counts_archived_failures() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new("health:\n  consecutive_failures: 5\n", &server);
    failures(env.root(), 5, 48 * H);
    eval(env.root(), now());
    assert!(tripped(env.root(), "consecutive_failures"));
    let report = apb_engine::retention::apply(
        env.root(),
        &apb_core::policy_file::RetentionSettings {
            keep_days: 1,
            keep_last: 2,
            keep_failed_days: 1,
        },
        None,
        now(),
    )
    .unwrap();
    assert_eq!(report.removed, 3, "{:?}", report.rows);
    assert_eq!(
        apb_engine::health::facts::facts(env.root(), Scope::Project, "r")
            .unwrap()
            .len(),
        5
    );
    eval(env.root(), now());
    assert!(tripped(env.root(), "consecutive_failures"));
}

// 17. A deadline abort.
#[test]
fn a_deadline_abort_is_a_failure() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(THREE, &server);
    for i in 0..3 {
        seed_run(
            env.root(),
            &format!("r-d{i}"),
            "r",
            now() - (3 - i) * H,
            End::Aborted("engine", "deadline_exceeded: the run took longer than 2h"),
        );
    }
    eval(env.root(), now());
    assert!(tripped(env.root(), "consecutive_failures"));
}

// 18. A superseded run.
fn superseded_seed(root: &Path) {
    let t = now();
    seed_run(root, "r-1", "r", t - 4 * H, End::Failed);
    seed_run(
        root,
        "r-2",
        "r",
        t - 3 * H,
        End::Aborted("engine", "superseded: run r-3 started for key acme"),
    );
    seed_run(root, "r-3", "r", t - 2 * H, End::Failed);
    seed_run(
        root,
        "r-4",
        "r",
        t - H,
        End::Aborted("engine", "superseded: run r-5 started for key acme"),
    );
}

#[test]
fn superseded_runs_are_not_failures() {
    let server = spawn_script(vec![]);
    let env = Env::new(THREE, &server);
    superseded_seed(env.root());
    let _ = apb_engine::health::evaluate(env.root(), Scope::Project, "r", &now);
    assert!(!has_state(env.root()) || !tripped(env.root(), "consecutive_failures"));
    assert!(server.requests().is_empty());
}

#[test]
fn superseded_archived_runs_are_not_failures() {
    let server = spawn_script(vec![]);
    let env = Env::new(THREE, &server);
    superseded_seed(env.root());
    // A newer success keeps the latest success and the newest run alive;
    // the four old runs go.
    seed_run(env.root(), "r-9", "r", now() - 30 * MIN, End::Ok);
    let report = apb_engine::retention::apply(
        env.root(),
        &apb_core::policy_file::RetentionSettings {
            keep_days: 1,
            keep_last: 1,
            keep_failed_days: 1,
        },
        None,
        now() + 72 * H,
    )
    .unwrap();
    assert_eq!(report.removed, 4, "{:?}", report.rows);
    // Without the success the failures and the superseded runs remain:
    // two failures, no streak of three.
    std::fs::remove_dir_all(env.root().join(".apb/runs/r-9")).unwrap();
    eval(env.root(), now());
    assert!(!tripped(env.root(), "consecutive_failures"));
    assert!(server.requests().is_empty());
}

// 19. External data.
#[test]
fn external_data_runs_count() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(THREE, &server);
    std::fs::write(env.root().join("fail"), "").unwrap();
    for _ in 0..3 {
        let _ = run(
            env.root(),
            "r",
            None,
            RunOptions {
                external_data: true,
                ..RunOptions::default()
            },
        )
        .unwrap();
    }
    eval(env.root(), now());
    assert!(tripped(env.root(), "consecutive_failures"));
}

// 20. A terminal writer that skips health.
#[test]
fn every_terminal_event_is_observed() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(THREE, &server);
    let open = seed_run(env.root(), "r-a", "r", now() - H, End::Open);
    EventLog::open(&open)
        .unwrap()
        .append(EventPayload::RunFinished {
            outcome: "failed".into(),
        })
        .unwrap();
    assert!(has_state(env.root()), "a bare append evaluates the routine");
    let first = state(env.root()).evaluated_at_ms;
    std::thread::sleep(Duration::from_millis(5));
    seed_run(env.root(), "r-b", "r", now() - MIN, End::Open);
    apb_engine::stop_run(env.root(), "r-b").unwrap();
    assert!(
        state(env.root()).evaluated_at_ms > first,
        "a stop of a dead run evaluates it"
    );
}

#[test]
fn a_preparation_failure_before_provenance_is_not_a_fact() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new("health:\n  overdue: 26h\n", &server);
    env.add_playbook(
        "r",
        "schema: 2\nid: r\nname: r\nversion: 1.0.0\nhealth:\n  overdue: 26h\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: agent_task, prompt: go, profile: missing }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n",
    );
    let t = now();
    assert!(run(env.root(), "r", None, RunOptions::default()).is_err());
    assert!(
        apb_engine::health::facts::facts(env.root(), Scope::Project, "r")
            .unwrap()
            .is_empty()
    );
    eval(env.root(), t);
    assert_eq!(eval(env.root(), t + 26 * H + MIN).status, "tripped");
}

// 21. A clear racing a sender.
#[test]
fn a_clear_before_dispatch_sends_nothing() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    let root = env.root().to_path_buf();
    deadline_hooks::set(Hook::HealthBeforeDispatch, env.root(), move |_| {
        apb_engine::health::clear(&root, Scope::Project, "r", now()).unwrap();
    });
    eval(env.root(), now());
    deadline_hooks::clear(env.root());
    assert!(server.requests().is_empty());
    let n = notices(env.root());
    assert_eq!(n[0].state, "retired", "{n:?}");
}

#[test]
fn a_clear_during_a_send_changes_no_rule_state() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    let root = env.root().to_path_buf();
    deadline_hooks::set(Hook::HealthBeforeSettle, env.root(), move |_| {
        apb_engine::health::clear(&root, Scope::Project, "r", now()).unwrap();
    });
    eval(env.root(), now());
    deadline_hooks::clear(env.root());
    assert_eq!(server.requests().len(), 1);
    let s = state(env.root());
    assert_eq!(s.notices[0].state, "retired");
    assert!(
        s.notices[0]
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("sent")),
        "{:?}",
        s.notices[0]
    );
    assert!(s.rules.is_empty(), "{:?}", s.rules);
}

#[test]
fn a_mute_during_a_send_is_honoured() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    let root = env.root().to_path_buf();
    deadline_hooks::set(Hook::HealthBeforeSettle, env.root(), move |_| {
        apb_engine::health::mute(&root, Scope::Project, "r", Some(now() + 10 * H), now()).unwrap();
    });
    eval(env.root(), now());
    deadline_hooks::clear(env.root());
    let s = state(env.root());
    assert_eq!(s.notices[0].state, "retired");
    assert!(
        s.rules
            .get("consecutive_failures")
            .is_some_and(|r| r.delivered_ms.is_none()),
        "{:?}",
        s.rules
    );
}

// 22. Retry, mute and cooldown.
#[test]
fn a_failed_notice_is_not_retried_while_muted() {
    let server = spawn_script(vec![
        Reply::Respond(400, r#"{"error":"bad"}"#.into()),
        Reply::Respond(200, OK.into()),
    ]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, H);
    let t = now();
    eval(env.root(), t);
    assert_eq!(notices(env.root())[0].state, "failed");
    apb_engine::health::mute(env.root(), Scope::Project, "r", Some(t + 100 * H), t).unwrap();
    eval(env.root(), t + MIN);
    eval(env.root(), t + 2 * MIN);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(notices(env.root())[0].state, "retired");
    apb_engine::health::mute(env.root(), Scope::Project, "r", None, t + 3 * MIN).unwrap();
    eval(env.root(), t + 4 * MIN);
    assert_eq!(server.requests().len(), 2);
    let n = notices(env.root());
    assert_eq!(n.len(), 2, "{n:?}");
    assert_eq!(n[1].state, "sent");
}

#[test]
fn a_retrip_within_the_cooldown_waits() {
    let server = spawn_script(vec![
        Reply::Respond(200, OK.into()),
        Reply::Respond(200, OK.into()),
        Reply::Respond(200, OK.into()),
    ]);
    let env = Env::new(
        "health:\n  consecutive_failures: 1\n  cooldown: 24h\n",
        &server,
    );
    let t = now();
    seed_run(env.root(), "r-1", "r", t - 3 * H, End::Failed);
    eval(env.root(), t);
    assert_eq!(server.requests().len(), 1, "the trip notice");
    seed_run(env.root(), "r-2", "r", t - 2 * H, End::Ok);
    eval(env.root(), t + MIN);
    assert_eq!(server.requests().len(), 2, "the recovery notice");
    seed_run(env.root(), "r-3", "r", t - H, End::Failed);
    eval(env.root(), t + H);
    assert!(tripped(env.root(), "consecutive_failures"));
    assert_eq!(server.requests().len(), 2, "within the cooldown");
    eval(env.root(), t + 24 * H + MIN);
    assert_eq!(server.requests().len(), 3, "after the cooldown");
}

#[test]
fn a_recovery_while_muted_sends_nothing() {
    let server = spawn_script(vec![Reply::Respond(200, OK.into())]);
    let env = Env::new(TWO, &server);
    failures(env.root(), 2, 2 * H);
    let t = now();
    eval(env.root(), t);
    assert_eq!(server.requests().len(), 1);
    apb_engine::health::mute(env.root(), Scope::Project, "r", Some(t + 100 * H), t).unwrap();
    seed_run(env.root(), "r-ok", "r", t - H, End::Ok);
    eval(env.root(), t + MIN);
    assert!(!tripped(env.root(), "consecutive_failures"));
    assert_eq!(server.requests().len(), 1);
    assert!(notices(env.root()).iter().all(|n| n.kind == "trip"));
}

// 23. The engine refuses a held playbook.
#[test]
fn a_held_playbook_is_refused_by_the_engine() {
    let server = spawn_script(vec![]);
    let env = Env::new(
        "health:\n  consecutive_failures: 2\n  on_trip: hold\n",
        &server,
    );
    let t = now();
    change(env.root(), |h| {
        h.first_seen_ms = t;
        h.hold = Some(apb_engine::health::state::Hold {
            since_ms: t,
            rule: "consecutive_failures".into(),
            detail: "2 failed in a row".into(),
        });
    });
    let e = apb_engine::scheduler::start_detached(env.root(), "r", None, RunOptions::default())
        .unwrap_err();
    assert!(e.to_string().contains("playbook_on_hold"), "{e}");
}

// D1 test 11. A notifier blocking the slot.
#[test]
fn a_blocked_notifier_never_holds_the_slot() {
    use apb_engine::concurrency::{ConcurrencyFile, Holder, Slot};
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = spawn_script(vec![Reply::Held {
        body: OK.into(),
        arrived: arrived_tx,
        release: release_rx,
    }]);
    let env = Env::new("health:\n  consecutive_failures: 1\n", &server);
    // A failed run trips the rule at the first evaluation, which the stop
    // of a waiter whose owner and driver are gone runs: its run is open,
    // so the heal promises its stop, and the stop's observation sends.
    seed_run(env.root(), "r-f", "r", now() - 2 * H, End::Failed);
    let w = seed_run(env.root(), "r-w", "r", now() - H, End::Open);
    let t = now() - H;
    std::fs::write(
        w.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"seq": 1, "ts": t, "type": "run_started", "playbook": "r", "version": "1.0.0"}),
            serde_json::json!({"seq": 2, "ts": t, "type": "run_provenance", "origin": "project",
                "concurrency": {"key_digest": "k1", "display": "acme", "on_busy": "queue_latest", "queued": true}}),
        ),
    )
    .unwrap();
    signed_store::transact(
        env.root(),
        Scope::Project,
        "r",
        Unsigned::Refuse,
        |f: &mut ConcurrencyFile| {
            f.slots.push(Slot {
                key_digest: "k1".into(),
                display: "acme".into(),
                active: None,
                waiting: Some(Holder {
                    run_id: "r-w".into(),
                    reservation: Some("gone".into()),
                }),
                stops: Vec::new(),
            });
            Ok::<_, signed_store::StoreError>(((), true))
        },
    )
    .unwrap();
    let record = |key: &str| apb_engine::event::ConcurrencyRecord {
        key_digest: key.into(),
        display: key.into(),
        on_busy: "queue_latest".into(),
        queued: false,
    };
    let root = env.root().to_path_buf();
    let first = std::thread::spawn(move || {
        apb_engine::concurrency::admit(
            &root,
            Scope::Project,
            "r",
            &record("k1"),
            apb_core::concurrency::OnBusy::QueueLatest,
            "r-new1",
            &Default::default(),
        )
        .map(|_| ())
    });
    arrived_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the stop's health notice reached the stand-in");
    let started = Instant::now();
    let other = apb_engine::concurrency::admit(
        env.root(),
        Scope::Project,
        "r",
        &record("k2"),
        apb_core::concurrency::OnBusy::QueueLatest,
        "r-new2",
        &Default::default(),
    );
    assert!(other.is_ok(), "{other:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "admission waited {:?} behind a send",
        started.elapsed()
    );
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
}

// Review fixes.

/// An evaluation that read the facts before the third failure ended never
/// applies them over the trip a later evaluation recorded from all three.
#[test]
fn an_older_evaluation_never_undoes_a_newer_one() {
    let server = spawn_script(vec![
        Reply::Respond(200, OK.into()),
        Reply::Respond(200, OK.into()),
    ]);
    let env = Env::new(THREE, &server);
    failures(env.root(), 2, H);
    eval(env.root(), now());
    assert!(!tripped(env.root(), "consecutive_failures"));
    let (at_tx, at_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let at_tx = std::sync::Mutex::new(at_tx);
    let go_rx = std::sync::Mutex::new(go_rx);
    let first = std::sync::atomic::AtomicBool::new(true);
    deadline_hooks::set(Hook::HealthAfterFacts, env.root(), move |_| {
        if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = at_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
        }
    });
    let root = env.root().to_path_buf();
    let a = std::thread::spawn({
        let root = root.clone();
        move || apb_engine::health::evaluate(&root, Scope::Project, "r", &now)
    });
    at_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("A read its facts");
    // The third failure ends, and its evaluation goes as far as it can.
    seed_run(env.root(), "r-third", "r", now() - MIN / 2, End::Failed);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let b = std::thread::spawn({
        let root = root.clone();
        move || {
            let v = apb_engine::health::evaluate(&root, Scope::Project, "r", &now);
            let _ = done_tx.send(());
            v
        }
    });
    // A B that does not wait for A ends here; one that waits ends after A.
    let _ = done_rx.recv_timeout(Duration::from_secs(3));
    go_tx.send(()).unwrap();
    a.join().unwrap().unwrap();
    b.join().unwrap().unwrap();
    deadline_hooks::clear(env.root());
    assert!(
        tripped(env.root(), "consecutive_failures"),
        "{:?}",
        notices(env.root())
    );
    assert!(
        notices(env.root()).iter().all(|n| n.kind != "recovery"),
        "{:?}",
        notices(env.root())
    );
}

/// Facts that cannot be read leave the state as it was: no recovery is
/// made up from an empty history.
#[test]
fn facts_that_do_not_read_leave_the_state() {
    let server = spawn_script(vec![
        Reply::Respond(200, OK.into()),
        Reply::Respond(200, OK.into()),
    ]);
    let env = Env::new(THREE, &server);
    failures(env.root(), 3, H);
    eval(env.root(), now());
    assert!(tripped(env.root(), "consecutive_failures"));
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), env.root().join(".apb/runs-archive")).unwrap();
    let r = apb_engine::health::evaluate(env.root(), Scope::Project, "r", &now);
    assert!(r.is_err(), "{r:?}");
    assert!(tripped(env.root(), "consecutive_failures"));
    assert!(notices(env.root()).iter().all(|n| n.kind != "recovery"));
}

// Second review round.

/// An evaluation that stalled before the evaluation lock reads its time
/// once it holds the lock: it never measures `overdue` at a time older
/// than the evaluation that recorded the trip before it.
#[test]
fn an_evaluation_reads_its_time_under_the_lock() {
    let server = spawn_script(vec![
        Reply::Respond(200, OK.into()),
        Reply::Respond(200, OK.into()),
    ]);
    let env = Env::new("health:\n  overdue: 1h\n", &server);
    let base = now() - 10 * H;
    seed_run(env.root(), "r-ok", "r", base, End::Ok);
    let before = base + 10 + H - MIN;
    let after = base + 10 + H + MIN;
    eval(env.root(), before);
    assert!(!tripped(env.root(), "overdue"));
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(before));
    let (at_tx, at_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let at_tx = std::sync::Mutex::new(at_tx);
    let go_rx = std::sync::Mutex::new(go_rx);
    let first = std::sync::atomic::AtomicBool::new(true);
    deadline_hooks::set(Hook::HealthBeforeLock, env.root(), move |_| {
        if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let _ = at_tx.lock().unwrap().send(());
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
        }
    });
    let a = std::thread::spawn({
        let root = env.root().to_path_buf();
        let clock = clock.clone();
        move || {
            let now = move || clock.load(std::sync::atomic::Ordering::SeqCst);
            apb_engine::health::evaluate(&root, Scope::Project, "r", &now)
        }
    });
    at_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("A reached the evaluation lock");
    // The threshold passes; B evaluates and records the trip.
    clock.store(after, std::sync::atomic::Ordering::SeqCst);
    eval(env.root(), after);
    assert!(tripped(env.root(), "overdue"));
    go_tx.send(()).unwrap();
    a.join().unwrap().unwrap();
    deadline_hooks::clear(env.root());
    assert!(tripped(env.root(), "overdue"), "{:?}", notices(env.root()));
    assert!(
        notices(env.root()).iter().all(|n| n.kind != "recovery"),
        "{:?}",
        notices(env.root())
    );
}
