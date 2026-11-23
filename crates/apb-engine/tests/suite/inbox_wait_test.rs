//! The inbox wait end to end (inbox-waits D4): authorization, the visit
//! over the journal position, the declared output values, the retention
//! gap and the crash recovery. Built like `connector_run` and
//! `wait_test`: the `echo-hooks` fixture connector installed under a temp
//! `APB_CONFIG_DIR`, the account approved in the trust store, real runs
//! driven by `scheduler::run` with the connector permit maps the gate
//! would hand over, and events stored with `Inbox::append_*`.
//!
//! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` and the trust
//! store are process-wide.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use apb_core::connector::inbox::{
    Appended, DEDUPE_MAX_ENTRIES, DEDUPE_RETENTION_MS, Inbox, Retention,
};
use apb_core::connector::resolve::resolve_playbook;
use apb_core::registry::init_project;
use apb_core::schema::Playbook;
use apb_engine::event::{Event, EventPayload, read_all};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::RunStatus;

use crate::common::{self, EnvVar};

const CONNECTOR: &str = "echo-hooks";
const ACCOUNT: &str = "main";
const NODE: &str = "wait";
/// The account's two secret fields reference these; the workspace redactor
/// knows their values from `<root>/.apb/secrets.env`. The long one is over
/// `MIN_SECRET_LEN` (6), the short one under it.
const LONG_VAR: &str = "APB_IW_LONG";
const SHORT_VAR: &str = "APB_IW_SHORT";
const LONG_SECRET: &str = "abcd1234";
const SHORT_SECRET: &str = "ab12";

const POLL_DEADLINE: Duration = Duration::from_secs(20);
const POLL_STEP: Duration = Duration::from_millis(10);

fn poll_until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if started.elapsed() > POLL_DEADLINE {
            panic!("timed out after {POLL_DEADLINE:?} waiting for {what}");
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// The inbox of the test's one account, under the current `APB_CONFIG_DIR`.
fn inbox() -> Inbox {
    let cfg = std::env::var_os("APB_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap();
    Inbox::at(&cfg.join("connector-inbox"), CONNECTOR, ACCOUNT).unwrap()
}

/// An event appended with the wall clock under the store's lock.
fn store(body: serde_json::Value) -> u64 {
    store_typed(None, body)
}

fn store_typed(event_type: Option<&str>, body: serde_json::Value) -> u64 {
    match inbox().append_typed(
        &format!("m-{}", apb_core::connector::marker::new_nonce()),
        event_type,
        &body,
    ) {
        Ok(Appended::Stored(seq)) => seq,
        other => panic!("the event was not stored: {other:?}"),
    }
}

/// An event appended with an explicit receipt stamp (the test seam of
/// `append_bounded`): `received_at` is `max(now, last.received_at)`, so a
/// past stamp reads as a delivery that arrived then.
fn store_at(now: u64, body: serde_json::Value) -> u64 {
    match inbox().append_bounded(
        &format!("m-{}", apb_core::connector::marker::new_nonce()),
        &body,
        None,
        &Retention::default(),
        DEDUPE_RETENTION_MS,
        DEDUPE_MAX_ENTRIES,
        now,
    ) {
        Ok(Appended::Stored(seq)) => seq,
        other => panic!("the event was not stored: {other:?}"),
    }
}

/// An event appended to another account's log, under the store's lock.
fn store_in(account: &str, body: serde_json::Value) -> u64 {
    let cfg = std::env::var_os("APB_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap();
    let inbox = Inbox::at(&cfg.join("connector-inbox"), CONNECTOR, account).unwrap();
    match inbox.append_typed(
        &format!("m-{}", apb_core::connector::marker::new_nonce()),
        None,
        &body,
    ) {
        Ok(Appended::Stored(seq)) => seq,
        other => panic!("the event was not stored: {other:?}"),
    }
}

/// One project with the connector installed, the account configured, the
/// secrets set and both digests approved.
struct Env {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
    _cfg_var: EnvVar,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new() -> Env {
        Self::with_secrets(&[(LONG_VAR, LONG_SECRET), (SHORT_VAR, SHORT_SECRET)])
    }

    /// The same, with nothing approved: the trust store stays empty.
    fn unapproved() -> Env {
        Self::with_secrets_unapproved(&[(LONG_VAR, LONG_SECRET), (SHORT_VAR, SHORT_SECRET)])
    }

    /// An environment whose account's secret fields source `{{cmd:...}}`
    /// shell commands instead of env vars. The storage redactor never runs
    /// a command, so such a secret is always unresolved for it.
    fn with_cmd_secrets(first: &str, second: &str) -> Env {
        let env = Self::build();
        let account = format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{cmd:{first}}}}}\"\n    app_secret: \"{{{{cmd:{second}}}}}\"\n"
        );
        write_account_config(env.cfg.path(), &account);
        env.approve();
        env
    }

    fn with_secrets(secrets: &[(&str, &str)]) -> Env {
        let env = Self::with_secrets_unapproved(secrets);
        env.approve();
        env
    }

    fn with_secrets_unapproved(secrets: &[(&str, &str)]) -> Env {
        let env = Self::build();
        let account = format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"{{{{env.{SHORT_VAR}}}}}\"\n"
        );
        write_account_config(env.cfg.path(), &account);
        let dotenv = secrets
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect::<String>();
        fs::write(env.root.path().join(".apb/secrets.env"), dotenv).unwrap();
        env
    }

    fn build() -> Env {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let cfg_var = EnvVar::set("APB_CONFIG_DIR", cfg.path());
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/connectors")
            .join(CONNECTOR)
            .join("connector.yaml");
        let installed = cfg.path().join("connectors").join(CONNECTOR);
        fs::create_dir_all(&installed).unwrap();
        fs::write(
            installed.join("connector.yaml"),
            fs::read_to_string(fixture).unwrap(),
        )
        .unwrap();
        init_project(root.path()).unwrap();
        Env {
            root,
            cfg,
            _cfg_var: cfg_var,
            _lock: lock,
        }
    }

    /// Approves the connector's tree digest and the account's digest, the
    /// two checks `inbox_wait::authorize` re-reads.
    fn approve(&self) {
        self.approve_connector();
        self.approve_accounts();
    }

    fn approve_connector(&self) {
        use apb_core::trust::{OriginKind, TrustStore};
        let loaded = apb_core::connector::store::load(CONNECTOR).unwrap();
        let mut trust = TrustStore::load();
        trust
            .approve(&loaded.digest, CONNECTOR, OriginKind::LocallyApproved)
            .unwrap();
    }

    fn approve_accounts(&self) {
        use apb_core::trust::{OriginKind, TrustStore};
        let accounts =
            apb_core::connector::config::load_merged(self.root.path(), CONNECTOR).unwrap();
        let mut trust = TrustStore::load();
        for a in &accounts {
            trust
                .approve(
                    &apb_core::connector::config::account_digest(a),
                    &format!("{CONNECTOR}/{}", a.name),
                    OriginKind::LocallyApproved,
                )
                .unwrap();
        }
    }

    /// Rewrites the account config so the live account digest no longer
    /// matches the manifest's.
    fn change_account(&self) {
        let account = format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"https://changed.example.com\"\n"
        );
        write_account_config(self.cfg.path(), &account);
    }

    /// Restores the account config the manifest recorded, so the live
    /// digest matches again.
    fn restore_account(&self) {
        let account = format!(
            "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"{{{{env.{SHORT_VAR}}}}}\"\n"
        );
        write_account_config(self.cfg.path(), &account);
    }

    fn seed(&self, yaml: &str) {
        let dir = self.root.path().join(".apb/playbooks/w/1.0.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playbook.yaml"), yaml).unwrap();
        fs::write(self.root.path().join(".apb/playbooks/w/current"), "1.0.0").unwrap();
    }

    /// The two permit maps the gate would hand the engine for `yaml`.
    fn maps(&self, yaml: &str) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        let pb = Playbook::from_yaml(yaml).unwrap();
        let out = resolve_playbook(self.root.path(), &pb).unwrap();
        let mut connectors = BTreeMap::new();
        let mut accounts = BTreeMap::new();
        for (name, resolved) in &out.connectors {
            connectors.insert(name.clone(), resolved.loaded.digest.clone());
            for a in &resolved.accounts {
                accounts.insert(
                    format!("{name}/{}", a.name),
                    apb_core::connector::config::account_digest(a),
                );
            }
        }
        (connectors, accounts)
    }

    /// Starts the playbook `w` on a thread, with the connector permit maps.
    fn spawn(
        &self,
        yaml: &str,
    ) -> mpsc::Receiver<Result<apb_engine::RunResult, apb_engine::error::EngineError>> {
        let (connectors, accounts) = self.maps(yaml);
        self.spawn_opts(RunOptions {
            expected_connectors: connectors,
            expected_connector_accounts: accounts,
            ..RunOptions::default()
        })
    }

    fn spawn_opts(
        &self,
        opts: RunOptions,
    ) -> mpsc::Receiver<Result<apb_engine::RunResult, apb_engine::error::EngineError>> {
        let (tx, rx) = mpsc::channel();
        let root = self.root.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(run(&root, "w", None, opts));
        });
        rx
    }

    /// The run directory of the latest started run.
    fn latest_run(&self) -> PathBuf {
        poll_until("run dir", || {
            let mut dirs: Vec<_> = fs::read_dir(self.root.path().join(".apb/runs"))
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.join("events.jsonl").is_file())
                .collect();
            dirs.sort_by_key(|p| fs::metadata(p).and_then(|m| m.created()).ok());
            dirs.pop()
        })
    }

    fn run_id_of(&self, run_dir: &Path) -> String {
        run_dir.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// The wall instant of the open visit's deadline (`wait_started` plus
    /// the timeout on the paused-interval clock), the same arithmetic
    /// `in_time` reads, with the wait's `wait_started` stamp.
    fn wait_deadline_ms(&self, run_dir: &Path, timeout_seconds: u64) -> (u128, u128) {
        let events = read_all(run_dir).unwrap();
        let ws = events
            .iter()
            .rev()
            .find(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE))
            .unwrap()
            .ts;
        let now = apb_core::clock::now_ms();
        let active = apb_engine::pause::active_elapsed_ms(&events, ws, now, now);
        // Not clamped at `now`: once the deadline passed it stays where it
        // was (now less the active time past the timeout; parks count as
        // active time, and no person pauses a test run after its deadline).
        let timeout = u128::from(timeout_seconds) * 1000;
        (ws, (now + timeout).saturating_sub(active))
    }
}

fn write_account_config(_cfg: &Path, yaml: &str) {
    let path = apb_core::connector::config::global_config_path(CONNECTOR).unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, yaml).unwrap();
}

/// The plain wait playbook: the inbox wait, success on a match, failure on
/// the timeout.
fn plain_yaml(timeout: u64) -> String {
    format!(
        r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main
    timeout_seconds: {timeout}
  - {{ id: ok, type: finish, outcome: success }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: wait }}
  - {{ from: wait, to: ok, condition: {{ type: node_status, node: wait, equals: success }} }}
  - {{ from: wait, to: no, fallback: true }}
"#
    )
}

/// A playbook with `match` conditions and `take` entries.
fn take_yaml(timeout: u64, conditions: &str, take: &str) -> String {
    format!(
        r#"
schema: 1
id: w
name: W
version: 1.0.0
params:
  - {{ name: customer, type: text }}
nodes:
  - {{ id: start, type: start }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main
      match: {conditions}
      take: {take}
    timeout_seconds: {timeout}
  - {{ id: ok, type: finish, outcome: success }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: wait }}
  - {{ from: wait, to: ok, condition: {{ type: node_status, node: wait, equals: success }} }}
  - {{ from: wait, to: no, fallback: true }}
"#
    )
}

/// The run result of a drive that must finish within the poll deadline.
fn wait_result(
    rx: &mpsc::Receiver<Result<apb_engine::RunResult, apb_engine::error::EngineError>>,
) -> apb_engine::RunResult {
    rx.recv_timeout(POLL_DEADLINE)
        .unwrap_or_else(|_| panic!("the drive did not return in time"))
        .expect("the drive must not fail")
}

/// The `inbox_wait_read` lines of the run journal, in order.
fn wait_lines<'a>(events: &'a [Event], node: &str) -> Vec<&'a Event> {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node: n, .. } if n == node))
        .collect()
}

/// The node's latest `node_finished`, if any.
fn node_finished<'a>(events: &'a [Event], node: &str) -> Option<(&'a str, &'a str)> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::NodeFinished {
            node: n,
            status,
            output,
            ..
        } if n == node => Some((status.as_str(), output.as_str())),
        _ => None,
    })
}

/// The raw journal file of `run_dir`.
fn raw_journal(run_dir: &Path) -> String {
    fs::read_to_string(run_dir.join("events.jsonl")).unwrap()
}

/// The account directory's `cursors.yaml` bytes, and its sorted listing.
fn inbox_dir_state() -> (Option<Vec<u8>>, Vec<String>) {
    let dir = inbox().dir().to_path_buf();
    let cursors = fs::read(dir.join("cursors.yaml")).ok();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    (cursors, names)
}

/// The log's bytes right after a test stored the one event `seq` while a
/// visit is open: they must be the bytes before the store plus that one
/// event's line, so a wait that rewrote the log in between is caught even
/// though it may run before the snapshot.
fn bytes_after_one_store(before: &[u8], seq: u64) -> Vec<u8> {
    let after = fs::read(inbox().dir().join("events.jsonl")).unwrap();
    assert!(after.starts_with(before), "the log was rewritten");
    let added = std::str::from_utf8(&after[before.len()..]).unwrap();
    let lines: Vec<&str> = added.lines().collect();
    assert_eq!(lines.len(), 1, "exactly the stored line was added: {added}");
    let event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(event["seq"], seq);
    after
}

// ----------------------------------------------------------------- 2.6

fn gate_refusal(env: &Env) -> serde_json::Value {
    apb_engine::gate::check_run(
        env.root.path(),
        &apb_core::scope::PlaybookRef {
            origin: apb_core::scope::Origin::Project { workspace_id: None },
            id: "w".into(),
            version: None,
        },
        // The playbook's own digest is unapproved in a fresh project; the
        // acknowledge waives only that. Connector and account trust is
        // deliberately never bypassable (gate/mod.rs check_connectors).
        true,
        false,
        &apb_engine::gate::launch::LaunchContext::new(
            apb_engine::gate::launch::Initiator::CliUnattested { terminal: true },
            apb_engine::gate::launch::LaunchKind::Start,
        ),
    )
    .expect_err("the start is refused")
}

/// The gate refuses a start whose inbox wait names an unapproved account,
/// before any run directory exists.
#[test]
fn an_unapproved_account_refuses_the_start() {
    let env = Env::unapproved();
    // The connector digest is approved, the account's is not: the account
    // check is the one that fires.
    env.approve_connector();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let refusal = gate_refusal(&env);
    assert_eq!(
        refusal["policy"], "unapproved_connector_account",
        "was: {refusal}"
    );
    assert!(
        fs::read_dir(env.root.path().join(".apb/runs"))
            .map(|d| d.count())
            .unwrap_or(0)
            == 0,
        "no run directory was created"
    );
}

/// The gate refuses a start whose inbox wait names a connector that is not
/// installed.
#[test]
fn a_connector_that_is_not_installed_refuses_the_start() {
    let env = Env::new();
    fs::remove_dir_all(env.cfg.path().join("connectors").join(CONNECTOR)).unwrap();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let refusal = gate_refusal(&env);
    assert_eq!(refusal["policy"], "connector_unresolved", "was: {refusal}");
}

/// The manifest records the wait's binding site with the one account and
/// no function.
#[test]
fn the_manifest_records_the_inbox_site() {
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = env.latest_run();
    poll_until("wait_started", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE))
            .then_some(())
    });
    store(serde_json::json!({ "from": "x" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);

    let m = apb_engine::manifest::read(&run_dir)
        .unwrap()
        .expect("the manifest exists");
    let site = apb_core::inbox_wait::inbox_site(NODE);
    let grants = m.grants_for(&site);
    assert_eq!(grants.len(), 1, "one grant for the site: {:?}", grants);
    assert_eq!(grants[0].connector, CONNECTOR);
    assert_eq!(grants[0].accounts, vec![ACCOUNT.to_string()]);
    assert!(grants[0].functions.is_empty(), "no function is granted");
    let c = m
        .connector(CONNECTOR)
        .expect("the connector is snapshotted");
    assert!(c.accounts.iter().any(|a| a.name == ACCOUNT));
}

/// The account edited while the run was paused: the resumed drive's first
/// poll refuses and the node fails `inbox_wait_unauthorized` with nothing
/// journaled for the wait.
#[test]
fn a_changed_account_fails_the_wait_on_resume() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    use apb_engine::scheduler::resume;
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = poll_until("wait_started", || {
        let rd = env.latest_run();
        read_all(&rd)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE))
            .then_some(rd)
    });
    let run_id = env.run_id_of(&run_dir);

    // A person's pause stops the waiting drive.
    let req = PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(env.root.path(), &run_id, &req).unwrap();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Paused);

    // The account changes while nobody drives the run.
    env.change_account();

    let resumed = resume(env.root.path(), &run_id, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Failed, "the wait refuses");
    let events = read_all(&run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "failed");
    assert!(
        output.starts_with("inbox_wait_unauthorized:"),
        "was: {output}"
    );
    // Nothing was journaled for the wait beyond the start pair: no second
    // `wait_started`, no read line, no end event.
    assert_eq!(
        events
            .iter()
            .filter(
                |e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE)
            )
            .count(),
        1,
        "no second wait_started"
    );
    // The only read line is the first drive's start line.
    assert_eq!(wait_lines(&events, NODE).len(), 1, "no further read line");
    assert!(
        !events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::WaitSignalled { node } | EventPayload::WaitTimeout { node } if node == NODE
        )),
        "no end event"
    );
}

// ----------------------------------------------------------------- 2.7

/// Waits until the wait node of the latest run has journaled its
/// `wait_started` and returns the run directory.
fn await_wait_started(env: &Env) -> PathBuf {
    poll_until("wait_started", || {
        let rd = env.latest_run();
        read_all(&rd)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE))
            .then_some(rd)
    })
}

/// The node's output parsed as JSON.
fn output_json(events: &[Event], node: &str) -> serde_json::Value {
    let (_, output) = node_finished(events, node).expect("the node finished");
    serde_json::from_str(output).unwrap_or_else(|e| panic!("output `{output}` is not JSON: {e}"))
}

/// The journal's `wait_signalled` / `wait_timeout` events of the node.
fn end_events(events: &[Event], node: &str) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::WaitSignalled { node: n } if n == node => Some("signalled"),
            EventPayload::WaitTimeout { node: n } if n == node => Some("timeout"),
            _ => None,
        })
        .collect()
}

/// The end line of the node's visit: the last `inbox_wait_read` with `end`
/// set.
fn end_line(events: &[Event], node: &str) -> (Option<u64>, u32, String, Option<String>) {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::InboxWaitRead {
                node: n,
                through_seq,
                examined,
                end: Some(end),
                output,
            } if n == node => Some((Some(*through_seq), *examined, end.clone(), output.clone())),
            _ => None,
        })
        .expect("an end line")
}

/// A wait ends at its first matching event: the node succeeds with the
/// event's declared output, the end pair is one `inbox_wait_read` with
/// `matched` and a `wait_signalled`.
#[test]
fn a_wait_ends_at_its_first_matching_event() {
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "from": "x" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (status, _) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "succeeded");
    assert_eq!(end_events(&events, NODE), vec!["signalled"]);
    let (through, examined, end, output) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(through, Some(1), "the position is the matching event's seq");
    assert_eq!(examined, 0, "nothing was examined before the match");
    let output = output.expect("the end line carries the output");
    let v: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(v["seq"], 1);
}

/// The output is the declared values: a `take` entry per declared field,
/// read by the baseline reader; a field no entry names never appears.
#[test]
fn the_output_is_the_declared_values() {
    let env = Env::new();
    let yaml = take_yaml(
        60,
        r#"[{ path: from, equals: "{{params.customer}}" }]"#,
        r#"[{ name: text, path: text }]"#,
    );
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "from": "acme", "text": "hello", "secret_marker": "undeclared" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let v = output_json(&events, NODE);
    assert_eq!(v["text"], "hello");
    assert!(v.get("secret_marker").is_none(), "undeclared: {v}");
    assert!(
        v.get("seq").is_some() && v.get("received_at").is_some(),
        "{v}"
    );
    // The marker is nowhere in the journal file either (2.9 pins the
    // redactor; this pins the declared-values shape).
    assert!(
        !raw_journal(&run_dir).contains("secret_marker"),
        "an undeclared field reached the journal"
    );
}

/// The output keeps the event's type, its seq and its receive time.
#[test]
fn the_output_keeps_the_event_type() {
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let seq = store_typed(Some("push"), serde_json::json!({ "n": 1 }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let v = output_json(&events, NODE);
    assert_eq!(v["type"], "push");
    assert_eq!(v["seq"], seq);
    assert!(v["received_at"].as_u64().is_some_and(|t| t > 0), "{v}");
}

/// Nothing matches: the visit times out at the deadline, the end pair
/// carries `timeout` and a `wait_timeout`, and the node fails.
#[test]
fn a_timeout_without_a_match_times_out() {
    let env = Env::new();
    let yaml = plain_yaml(1);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "timed_out");
    assert_eq!(output, "wait timeout");
    assert_eq!(end_events(&events, NODE), vec!["timeout"]);
    let (_, _, end, output) = end_line(&events, NODE);
    assert_eq!(end, "timeout");
    assert_eq!(output, None, "a timeout line carries no output");
}

/// Receipt decides: an event stored with a receipt before the deadline
/// while no drive runs still ends the visit when the first read of it
/// comes after the deadline. The run is parked, and a park does not move
/// the deadline, so the resumed drive reads it late.
#[test]
fn an_event_received_before_the_deadline_matches_when_polled_late() {
    use apb_engine::scheduler::resume_pause;
    let env = Env::new();
    // The origin key exists before the run is stamped: the resume
    // verifies it.
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = plain_yaml(2);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let parks = run_parks(&read_all(&run_dir).unwrap());
    let seq = parks[0].0;
    let until = parks[0].3.expect("the park carries a wake time");
    let (ws, deadline) = env.wait_deadline_ms(&run_dir, 2);
    // A park is active wait time: the deadline is the wait's own timeout
    // and the park wakes at it.
    assert_eq!(deadline, ws + 2_000);
    assert_eq!(until, u64::try_from(ws + 2_000).unwrap());

    // The event is stored with a receipt 1 s before the deadline while no
    // drive runs.
    store_at(
        u64::try_from(deadline - 1_000).unwrap(),
        serde_json::json!({ "from": "x" }),
    );

    // The run resumes after the deadline: the first read of the event
    // comes late, and the receipt decides.
    while apb_core::clock::now_ms() < u128::from(until) {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(50));
    let resumed =
        resume_pause(env.root.path(), &run_id, seq, None).expect("the resumed run drives");
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (status, _) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "succeeded");
    let (_, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
}

/// Two events: the first one is read by the visit, the second one is stored
/// inside the read interval before the deadline. The visit ends with the
/// second one: a timeout never ends a visit on a stale read.
#[test]
fn a_second_event_stored_inside_the_read_interval_is_not_lost() {
    let env = Env::new();
    let yaml = take_yaml(
        2,
        r#"[{ path: from, equals: "{{params.customer}}" }]"#,
        "[]",
    );
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    let (ws, deadline) = env.wait_deadline_ms(&run_dir, 2);
    // The first event does not match. It is stored 1.1 s into the wait, so
    // the visit reads it and the next read is due 1 s later, past the
    // deadline. The second event is stored right after that read, which
    // leaves the whole second of the read interval for it.
    let at = ws + 1_100;
    while apb_core::clock::now_ms() < at {
        std::thread::sleep(Duration::from_millis(10));
    }
    store(serde_json::json!({ "from": "other" }));
    poll_until("the read of the first event", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 2).then_some(())
    });
    // The second event matches and is stored inside the read interval
    // before the deadline.
    store(serde_json::json!({ "from": "acme" }));
    assert!(
        apb_core::clock::now_ms() < deadline,
        "the second event is stored in time"
    );
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched", "the visit ends with the second event");
    assert_eq!(through, Some(2));
}

/// An event stamped after the deadline is left above the position: the
/// visit times out with `through_seq` below it and the loop's next visit
/// ends with it.
#[test]
fn an_event_received_after_the_deadline_is_left_for_the_next_visit() {
    let env = Env::new();
    let yaml = loop2_yaml(2);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    // While visit 1 is open, an event is stored with a receipt half a
    // second after its deadline: visit 1 reads it (before and after its
    // deadline, its final read included) and must leave it unexamined,
    // since it arrived late for that visit. The second visit's receipt
    // rule takes it.
    let (_, deadline) = env.wait_deadline_ms(&run_dir, 2);
    let seq = store_at(
        u64::try_from(deadline + 500).unwrap(),
        serde_json::json!({ "from": "x" }),
    );
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed, "the loop is exhausted");
    let events = read_all(&run_dir).unwrap();
    // The second visit examined the event and ended with it.
    let ends: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::InboxWaitRead {
                node: n,
                through_seq,
                end: Some(end),
                output,
                ..
            } if n == NODE => Some((Some(*through_seq), end.clone(), output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2, "two visit ends: {ends:?}");
    assert_eq!(ends[0].0, Some(0), "the first visit stays below the event");
    assert_eq!(ends[0].1, "timeout");
    assert_eq!(ends[1].1, "matched");
    let out: serde_json::Value =
        serde_json::from_str(ends[1].2.as_deref().expect("the second output")).unwrap();
    assert_eq!(out["seq"], seq, "the next visit ends with the stored event");
}

/// A delivery that would have arrived before the deadline but is stamped
/// after it (it held the store's lock across it) is late: the visit times
/// out with `through_seq` below it.
#[test]
fn a_delivery_that_waited_on_the_lock_across_the_deadline_is_late() {
    let env = Env::new();
    let yaml = plain_yaml(2);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let (ws, deadline) = env.wait_deadline_ms(&run_dir, 2);
    assert_eq!(
        deadline - ws,
        2_000,
        "no pause: the deadline is the start plus the timeout"
    );
    // The store is written before the deadline, with a receipt 1 ms past
    // it: what a delivery that held the lock past the deadline is.
    let sleep = (deadline - apb_core::clock::now_ms()).saturating_sub(800);
    std::thread::sleep(Duration::from_millis(u64::try_from(sleep).unwrap()));
    store_at(
        u64::try_from(deadline + 1).unwrap(),
        serde_json::json!({ "from": "x" }),
    );
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "timeout");
    assert_eq!(through, Some(0), "the late event stays above the position");
}

/// One read fails (an injected failure, armed before the matching event
/// is stored, so the read that would find it is the one that fails) and
/// the log is not written again: a failed read is never cached, so the
/// next poll reads it again and the visit ends with the stored event.
#[test]
fn a_failed_read_is_retried_without_a_new_write() {
    let env = Env::new();
    let yaml = take_yaml(30, r#"[{ path: from, equals: "acme" }]"#, "[]");
    env.seed(&yaml);
    // An event that matches no condition is stored before the run starts:
    // the first read of the visit examines it and waits on.
    store(serde_json::json!({ "from": "other" }));
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    // The first read is observed, so the log's stamp is known and no read
    // is due until the log changes.
    poll_until("the first read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 1).then_some(())
    });
    // The failure is armed before the matching event is stored: the read
    // the store makes due is the one that fails, whatever the timing.
    apb_engine::inbox_wait::fail_next_reads(&run_dir, 1);
    store(serde_json::json!({ "from": "acme" }));
    let events_path = inbox().dir().join("events.jsonl");
    let bytes_before = fs::read(&events_path).unwrap();
    let mtime_before = fs::metadata(&events_path).unwrap().modified().unwrap();
    poll_until("the failed read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).failures >= 1).then_some(())
    });
    // Nothing touches the log after the failure: the retry finds the same
    // bytes and the same modification time, so only a failed read that is
    // not cached reads it again.
    assert_eq!(
        fs::read(&events_path).unwrap(),
        bytes_before,
        "the log was written to between the failure and the retry"
    );
    assert_eq!(
        fs::metadata(&events_path).unwrap().modified().unwrap(),
        mtime_before,
        "the log's stamp changed between the failure and the retry"
    );
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded, "the retry reads");
    let events = read_all(&run_dir).unwrap();
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(through, Some(2), "the visit ends with the stored event");
    // The wait wrote nothing: the file is exactly the test's bytes.
    assert_eq!(
        fs::read(&events_path).unwrap(),
        bytes_before,
        "the log was written to by the wait"
    );
    assert!(
        !inbox().dir().join("cursors.yaml").exists(),
        "the wait acknowledged nothing"
    );
}

/// An idle wait reads the account's log once: a successful read of an
/// absent log is cached like any other, so the visit does not read again
/// on every poll.
#[test]
fn an_idle_wait_reads_the_absent_log_once() {
    let env = Env::new();
    let yaml = plain_yaml(10);
    env.seed(&yaml);
    assert!(
        !inbox().dir().join("events.jsonl").exists(),
        "the account has no log yet"
    );
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    // The first read of the absent log is observed.
    poll_until("the first read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 1).then_some(())
    });
    // The visit keeps polling for a second: the absent log is not read
    // again.
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(
        apb_engine::inbox_wait::read_counts(&run_dir).reads,
        1,
        "the absent log is read once, not on every poll"
    );
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed, "the wait times out");
}

/// The first head read fails and the second succeeds: the start pair is
/// written with the head the retry read. With every read of the schedule
/// failing, the node fails `inbox_wait_unreadable` and nothing is
/// journaled for the wait.
#[test]
fn a_transient_head_read_error_is_retried() {
    use std::io::Write as _;
    // Run A: the torn line is repaired while the drive sits in its first
    // backoff, so the second head read succeeds.
    let torn_repair = {
        let env = Env::new();
        let yaml = plain_yaml(3);
        env.seed(&yaml);
        let events_path = inbox().dir().join("events.jsonl");
        fs::create_dir_all(inbox().dir()).unwrap();
        {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&events_path)
                .unwrap();
            f.write_all(br#"{"seq":1,"received_at":1,"provider_id":"t","body":{}"#)
                .unwrap();
        }
        let _backoff = EnvVar::set("APB_INFRA_BACKOFF_MS", "1500");
        let rx = env.spawn(&yaml);
        // The repair lands only once the first head read was seen to fail,
        // inside the 1500 ms backoff before the second read.
        let started = env.latest_run();
        poll_until("the first head read failed", || {
            (apb_engine::inbox_wait::head_read_counts(&started).failures >= 1).then_some(())
        });
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&events_path)
                .unwrap();
            f.write_all(b"}\n").unwrap();
        }
        let result = wait_result(&rx);
        // The event of the repaired line sits below the start line, so the
        // visit waits and times out at 3 s: the run fails, but the start
        // pair was written with the head the retry read.
        assert_eq!(result.outcome, RunStatus::Failed);
        let run_dir = env.latest_run();
        let events = read_all(&run_dir).unwrap();
        // One start line (the timeout's end line is the only other read
        // line): the head was read again after the failed read.
        let starts: Vec<u64> = wait_lines(&events, NODE)
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::InboxWaitRead {
                    through_seq,
                    end: None,
                    ..
                } => Some(*through_seq),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            vec![1],
            "the head was read again after the failed read"
        );
        let heads = apb_engine::inbox_wait::head_read_counts(&run_dir);
        assert_eq!(
            (heads.reads, heads.failures),
            (2, 1),
            "one failed head read, then the one that succeeded"
        );
        true
    };
    assert!(torn_repair);

    // Run B: every read of the schedule fails; the node fails
    // `inbox_wait_unreadable` with nothing journaled for the wait.
    let env = Env::new();
    let yaml = plain_yaml(3);
    env.seed(&yaml);
    let events_path = inbox().dir().join("events.jsonl");
    fs::create_dir_all(inbox().dir()).unwrap();
    fs::write(&events_path, "{\"seq\":1,\"received_at\":1,\"pro\n").unwrap();
    let _backoff = EnvVar::set("APB_INFRA_BACKOFF_MS", "30,30");
    let rx = env.spawn(&yaml);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let run_dir = env.latest_run();
    let events = read_all(&run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "failed");
    assert!(
        output.starts_with("inbox_wait_unreadable:"),
        "was: {output}"
    );
    assert!(
        !events.iter().any(|e| {
            matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE)
        }),
        "no visit was declared before the head was read"
    );
    assert!(wait_lines(&events, NODE).is_empty(), "no start line");
}

/// A person's pause between `wait_started` and the deadline moves the
/// deadline by its length: an event stamped after the wall time of the
/// unpaused deadline but before the paused-interval deadline matches; one
/// after the moved deadline does not. What the test pins is the move of the
/// deadline, not the lateness of the read that follows it.
#[test]
fn a_pause_moves_the_deadline_by_its_length() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    use apb_engine::scheduler::resume;
    let run_case = |stamp_after_moved: bool, pause_ms: u64| -> RunStatus {
        let env = Env::new();
        let yaml = plain_yaml(4);
        env.seed(&yaml);
        let rx = env.spawn(&yaml);
        let run_dir = await_wait_started(&env);
        let run_id = env.run_id_of(&run_dir);
        let req = PauseRequest {
            reason: Some("looking".into()),
            by: Some(PauseBy::Operator),
            mode: PauseMode::Drain,
            ..PauseRequest::default()
        };
        request_pause(env.root.path(), &run_id, &req).unwrap();
        assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
        // Stay paused well past the stamp's offset below: the pause must
        // be longer than it for the event to sit between the two
        // deadlines.
        std::thread::sleep(Duration::from_millis(pause_ms));
        let (ws, moved) = env.wait_deadline_ms(&run_dir, 4);
        let unpaused = ws + 4_000;
        assert!(
            moved > unpaused,
            "the pause moved the deadline: {unpaused} vs {moved}"
        );
        let stamp = if stamp_after_moved {
            // Well past the moved deadline: the run stays paused until the
            // resume below, which moves the deadline further by however
            // long this thread takes to get there, so a near stamp could
            // become timely on a slow machine.
            moved + 20_000
        } else {
            // After the wall time of the unpaused deadline, before the
            // moved one: only the pause makes this receipt in time.
            unpaused + 1_000
        };
        assert!(stamp < moved || stamp_after_moved, "{stamp} vs {moved}");
        if !stamp_after_moved {
            // The accepting case stays paused past the unpaused deadline, so
            // the read that finds the event below comes late.
            assert!(
                apb_core::clock::now_ms() > unpaused,
                "the read comes after the unpaused deadline"
            );
        }
        store_at(
            u64::try_from(stamp).unwrap(),
            serde_json::json!({ "from": "x" }),
        );
        let resumed = resume(env.root.path(), &run_id, None).unwrap();
        if stamp_after_moved {
            assert_eq!(resumed.outcome, RunStatus::Failed);
        } else {
            assert_eq!(resumed.outcome, RunStatus::Succeeded);
        }
        let events = read_all(&run_dir).unwrap();
        let (_, _, end, _) = end_line(&events, NODE);
        if stamp_after_moved {
            assert_eq!(end, "timeout", "an event past the moved deadline is late");
        } else {
            assert_eq!(end, "matched", "the moved deadline accepts the event");
        }
        resumed.outcome
    };
    // The accepting case resumes after the unpaused deadline: the read that
    // finds the event comes late, and the receipt still decides.
    assert_eq!(run_case(false, 4_200), RunStatus::Succeeded);
    // An event stamped after the moved deadline is late.
    assert_eq!(run_case(true, 1_200), RunStatus::Failed);
}

/// Events stored before the wait began are below its start line: neither
/// examined nor reported.
#[test]
fn events_before_the_wait_are_below_its_start_line() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    for i in 0..3 {
        store(serde_json::json!({ "n": i }));
    }
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    // The start line holds the head the visit starts at.
    poll_until("the start line", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, .. } if node == NODE))
            .then_some(())
    });
    let events_now = read_all(&run_dir).unwrap();
    let lines = wait_lines(&events_now, NODE);
    let start_through = match &lines[0].payload {
        EventPayload::InboxWaitRead { through_seq, .. } => *through_seq,
        _ => unreachable!(),
    };
    assert_eq!(start_through, 3, "the start line holds the log's head");
    let seq = store(serde_json::json!({ "n": 3 }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (through, examined, end, output) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(through, Some(seq));
    assert_eq!(examined, 0, "the earlier events were never examined");
    let out: serde_json::Value = serde_json::from_str(&output.expect("the output")).unwrap();
    assert_eq!(out["seq"], seq, "only event 4 ends the visit");
}

/// Events above the matching one stay for the next visit: the visit ends
/// with the first match, `through_seq` is its seq, and the loop's next
/// visit ends with the second.
#[test]
fn events_after_the_match_stay_for_the_next_visit() {
    let env = Env::new();
    let yaml = loop2_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let first = store(serde_json::json!({ "n": 1 }));
    let second = store(serde_json::json!({ "n": 2 }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed, "the loop is exhausted");
    let events = read_all(&run_dir).unwrap();
    let ends: Vec<(Option<u64>, String, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::InboxWaitRead {
                node: n,
                through_seq,
                end: Some(end),
                output,
                ..
            } if n == NODE => Some((Some(*through_seq), end.clone(), output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2, "two visit ends: {ends:?}");
    assert_eq!(
        ends[0].0,
        Some(first),
        "the visit ends with the first match"
    );
    assert_eq!(ends[0].1, "matched");
    let first_out = ends[0].2.clone().unwrap();
    let out: serde_json::Value = serde_json::from_str(&first_out).unwrap();
    assert_eq!(out["seq"], first);
    assert_eq!(
        ends[1].0,
        Some(second),
        "the next visit ends with the second event"
    );
    // The second visit wrote no start line: it starts after the first
    // visit's end line.
    let starts = events
        .iter()
        .filter(|e| {
            matches!(&e.payload,
            EventPayload::InboxWaitRead { node, end: None, .. } if node == NODE)
        })
        .count();
    assert_eq!(starts, 1, "one start line only");
}

/// A second visit counts only newer events: the first visit's event is
/// never examined again.
#[test]
fn a_second_visit_counts_only_newer_events() {
    let env = Env::new();
    let yaml = loop2_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let first = store(serde_json::json!({ "n": 1 }));
    // Visit 1 ends with the stored event; the loop comes back.
    poll_until("the first visit ended", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == NODE))
            .then_some(())
    });
    let second = store(serde_json::json!({ "n": 2 }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed, "the loop is exhausted");
    let events = read_all(&run_dir).unwrap();
    let ends: Vec<(Option<u64>, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::InboxWaitRead {
                node: n,
                through_seq,
                end: Some(end),
                ..
            } if n == NODE => Some((Some(*through_seq), end.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2, "{ends:?}");
    assert_eq!(ends[0], (Some(first), "matched".into()));
    assert_eq!(
        ends[1],
        (Some(second), "matched".into()),
        "only the newer event"
    );
}

/// Two runs wait on one account independently: each keeps its position in
/// its own journal and both end with the one event.
#[test]
fn two_runs_wait_on_one_account_independently() {
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx1 = env.spawn(&yaml);
    let rx2 = env.spawn(&yaml);
    let mut dirs = poll_until("two run dirs", || {
        let mut all: Vec<PathBuf> = fs::read_dir(env.root.path().join(".apb/runs"))
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("events.jsonl").is_file())
            .collect();
        all.sort_by_key(|p| fs::metadata(p).and_then(|m| m.created()).ok());
        (all.len() == 2).then_some(all)
    });
    dirs.sort();
    for d in &dirs {
        poll_until("wait_started", || {
            read_all(d)
                .ok()?
                .iter()
                .any(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE))
                .then_some(())
        });
    }
    let log_before = fs::read(inbox().dir().join("events.jsonl")).unwrap_or_default();
    let seq = store(serde_json::json!({ "from": "x" }));
    let bytes = bytes_after_one_store(&log_before, seq);
    let (before_cursors, before_names) = inbox_dir_state();
    let r1 = wait_result(&rx1);
    let r2 = wait_result(&rx2);
    assert_eq!(r1.outcome, RunStatus::Succeeded);
    assert_eq!(r2.outcome, RunStatus::Succeeded);
    for d in &dirs {
        let events = read_all(d).unwrap();
        let (through, _, end, output) = end_line(&events, NODE);
        assert_eq!(end, "matched");
        assert_eq!(through, Some(seq));
        let out: serde_json::Value = serde_json::from_str(&output.expect("the output")).unwrap();
        assert_eq!(out["seq"], seq);
        // Each run's start line sits at the same head: nothing outside the
        // run holds a position.
        let lines = wait_lines(&events, NODE);
        let start_through = match &lines[0].payload {
            EventPayload::InboxWaitRead { through_seq, .. } => *through_seq,
            _ => unreachable!(),
        };
        assert_eq!(start_through, seq - 1, "each started at the same head");
    }
    let (after_cursors, after_names) = inbox_dir_state();
    assert_eq!(before_cursors, after_cursors, "no cursors were written");
    assert!(after_cursors.is_none(), "no cursor exists");
    assert_eq!(before_names, after_names, "the directory is unchanged");
    assert_eq!(
        fs::read(inbox().dir().join("events.jsonl")).unwrap(),
        bytes,
        "the log holds exactly what was stored"
    );
}

/// A wait leaves the inbox directory unchanged: no cursors file, no new
/// files, and the log's bytes are exactly what was stored.
#[test]
fn a_wait_leaves_the_inbox_directory_unchanged() {
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    await_wait_started(&env);
    let events_path = inbox().dir().join("events.jsonl");
    let log_before = fs::read(&events_path).unwrap_or_default();
    let seq = store(serde_json::json!({ "from": "x" }));
    // The bytes right after the store are checked against the bytes
    // before it plus the stored line, so a write the visit made before
    // this snapshot is caught too; reads write nothing, so the bytes and
    // the listing must come back exactly.
    let before_bytes = bytes_after_one_store(&log_before, seq);
    let (before_cursors, before_names) = inbox_dir_state();
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    assert_eq!(fs::read(&events_path).unwrap(), before_bytes);
    let (after_cursors, after_names) = inbox_dir_state();
    assert_eq!(before_cursors, after_cursors);
    assert!(after_cursors.is_none(), "no cursor exists");
    assert_eq!(before_names, after_names);
}

/// A loop playbook whose wait comes back to itself once: every visit ends
/// (matched or timed out), the bounce timer routes back for the first
/// visit, and the exhausted loop fails the run.
fn loop2_yaml(timeout: u64) -> String {
    format!(
        r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - {{ id: start, type: start }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main
    timeout_seconds: {timeout}
  - {{ id: bounce, type: wait, wait_for: {{ type: timer, seconds: 0 }}, timeout_seconds: 60 }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: wait }}
  - {{ from: wait, to: bounce, condition: {{ type: node_status, node: wait, equals: success }} }}
  - {{ from: wait, to: bounce, fallback: true }}
  - {{ id: again, from: bounce, to: wait, max_traversals: 1 }}
  - {{ from: bounce, to: no, fallback: true }}
"#
    )
}

/// A loop playbook whose wait matches a customer and comes back to itself
/// once: every visit ends (matched or timed out), the bounce timer routes
/// back for the first visit, and the exhausted loop fails the run.
fn loop2_take_yaml(timeout: u64) -> String {
    format!(
        r#"
schema: 1
id: w
name: W
version: 1.0.0
params:
  - {{ name: customer, type: text }}
nodes:
  - {{ id: start, type: start }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main
      match: [{{ path: from, equals: "acme" }}]
      take: []
    timeout_seconds: {timeout}
  - {{ id: bounce, type: wait, wait_for: {{ type: timer, seconds: 0 }}, timeout_seconds: 60 }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: wait }}
  - {{ from: wait, to: bounce, condition: {{ type: node_status, node: wait, equals: success }} }}
  - {{ from: wait, to: bounce, fallback: true }}
  - {{ id: again, from: bounce, to: wait, max_traversals: 1 }}
  - {{ from: bounce, to: no, fallback: true }}
"#
    )
}

/// A scenario run scripts the wait: `elapsed` ends it with the empty
/// object, no inbox is read and no read line is journaled.
#[test]
fn a_scripted_inbox_wait_ends_with_an_empty_object() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;
    let env = Env::new();
    // No connector installed here: a scenario never resolves one, and the
    // validator's connector rules stay silent without a store.
    fs::remove_dir_all(env.cfg.path().join("connectors").join(CONNECTOR)).unwrap();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let scenario = r#"
schema: 1
id: w
answers:
  wait:
    - { visit: 1, wait: { elapsed: true } }
"#;
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:iw-scripted".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = run_scenario(env.root.path(), "w", None, &prep).unwrap();
    assert_eq!(quick.outcome, RunStatus::Succeeded);
    let events = read_all(&quick.run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "succeeded");
    assert_eq!(output, "{}", "the scripted wait ends with the empty object");
    assert_eq!(end_events(&events, NODE), vec!["signalled"]);
    assert!(
        wait_lines(&events, NODE).is_empty(),
        "the scripted wait reads nothing and journals no read line"
    );
}

/// A scenario run scripts the wait's timeout: the visit ends with a
/// timeout, the node times out and no inbox is read.
#[test]
fn a_scripted_inbox_wait_that_times_out_ends_with_a_timeout() {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;
    let env = Env::new();
    // No connector installed here: a scenario never resolves one, and the
    // validator's connector rules stay silent without a store.
    fs::remove_dir_all(env.cfg.path().join("connectors").join(CONNECTOR)).unwrap();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let scenario = r#"
schema: 1
id: w
answers:
  wait:
    - { visit: 1, wait: { timed_out: true } }
"#;
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:iw-scripted-timeout".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let quick = run_scenario(env.root.path(), "w", None, &prep).unwrap();
    assert_eq!(quick.outcome, RunStatus::Failed);
    let events = read_all(&quick.run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "timed_out");
    assert_eq!(output, "wait timeout");
    assert_eq!(end_events(&events, NODE), vec!["timeout"]);
    assert!(
        wait_lines(&events, NODE).is_empty(),
        "the scripted wait reads nothing and journals no read line"
    );
}

/// The next action of a run with an open inbox wait names the wait and its
/// connector: the one line a surface shows for it.
#[test]
fn the_inbox_next_action_names_the_wait_and_its_connector() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let obs = apb_engine::next_action::Observation::load(&run_dir, &run_id).unwrap();
    let actions = apb_engine::next_action::next_actions(&obs);
    let reasons: Vec<&str> = actions.actions.iter().map(|a| a.reason.as_str()).collect();
    assert!(
        reasons.contains(&"node `wait` waits for an inbox event of `echo-hooks`"),
        "{reasons:?}"
    );
}

/// The journal's sequence of event `type`s.
fn journal_types(run_dir: &Path) -> Vec<String> {
    fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            value["type"].as_str().map(|t| t.to_string())
        })
        .collect()
}

/// A scenario run's timer wait with no answer stops the run at once, and
/// the journal is the one main writes: the visit is declared (the
/// `node_started` and the `wait_started`) before the run stops.
#[test]
fn a_scenario_timer_wait_without_an_answer_declares_the_visit_first() {
    assert_eq!(
        scenario_wait_journal(TIMER_WAIT_YAML),
        vec![
            "run_started",
            "run_provenance",
            "run_guard",
            "node_started",
            "node_finished",
            "run_guard",
            "edge_traversed",
            "node_started",
            "wait_started",
            "run_error",
            "run_finished",
            "run_guard",
        ]
    );
}

/// A scenario run's webhook wait with no answer stops the run at once, and
/// the journal is the one main writes: the visit is declared before the run
/// stops.
#[test]
fn a_scenario_webhook_wait_without_an_answer_declares_the_visit_first() {
    assert_eq!(
        scenario_wait_journal(WEBHOOK_WAIT_YAML),
        vec![
            "run_started",
            "run_provenance",
            "node_started",
            "node_finished",
            "edge_traversed",
            "node_started",
            "wait_started",
            "run_error",
            "run_finished",
        ]
    );
}

/// A playbook whose wait is a timer, in a scenario with no answer.
const TIMER_WAIT_YAML: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: timer, seconds: 1 }, timeout_seconds: 60 }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: ok }
"#;

/// A playbook whose wait is a webhook, in a scenario with no answer.
const WEBHOOK_WAIT_YAML: &str = r#"
schema: 1
id: w
name: W
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: wait, type: wait, wait_for: { type: webhook, key: inbox-waits }, timeout_seconds: 60 }
  - { id: ok, type: finish, outcome: success }
edges:
  - { from: start, to: wait }
  - { from: wait, to: ok }
"#;

/// The journal a scenario run of `yaml` writes when its wait has no answer:
/// the sequence of event `type`s.
fn scenario_wait_journal(yaml: &str) -> Vec<String> {
    use apb_core::scenario::{Scenario, ScenarioLevel};
    use apb_engine::scenario::session::ScenarioPrep;
    use apb_engine::scheduler::run_scenario;
    let env = Env::new();
    env.seed(yaml);
    let scenario = "schema: 1\nid: w\non_missing: error\nanswers: {}\n";
    let prep = ScenarioPrep {
        scenario: yaml_serde::from_str::<Scenario>(scenario).unwrap(),
        level: ScenarioLevel::Quick,
        scenario_digest: "sha256:iw-timer".into(),
        max_requests: 20,
        sent: Default::default(),
        external_data: false,
    };
    let result = run_scenario(env.root.path(), "w", None, &prep).unwrap();
    assert_eq!(result.outcome, RunStatus::Failed);
    journal_types(&result.run_dir)
}

// ----------------------------------------------------------------- 2.8

/// The journal's `inbox_gap` lines of the run, in order.
fn gap_lines(events: &[Event]) -> Vec<(String, u64, u64)> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::InboxGap {
                node_id,
                from,
                through,
                ..
            } => Some((node_id.clone(), *from, *through)),
            _ => None,
        })
        .collect()
}

/// Writes `events.jsonl` lines directly, with explicit seqs: the shape a
/// log has after retention dropped an unread prefix.
fn write_events_raw(from: u64, through: u64, body: serde_json::Value) {
    use std::io::Write as _;
    fs::create_dir_all(inbox().dir()).unwrap();
    let path = inbox().dir().join("events.jsonl");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    for seq in from..=through {
        let line = serde_json::json!({
            "seq": seq,
            "received_at": apb_core::clock::now_ms_u64(),
            "provider_id": format!("raw-{seq}"),
            "body": body,
        });
        writeln!(f, "{line}").unwrap();
    }
}

/// The gap of the events retention dropped is journaled for the wait: the
/// start line sits at 0, the log then holds 5 to 9 with 1 to 4 dropped, and
/// the visit journals one `inbox_gap` 1 to 4 before it examines event 5.
#[test]
fn a_gap_after_an_empty_start_is_journaled() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    // The start line sits at 0: the log was empty.
    poll_until("the start line", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, .. } if node == NODE))
            .then_some(())
    });
    // Retention's outcome, written as it leaves the log: seqs 5 to 9.
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        gap_lines(&events),
        vec![("wait".to_string(), 1, 4)],
        "one gap line under the wait node's own id"
    );
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(
        through,
        Some(5),
        "the visit ends with the first stored event"
    );
}

/// The gap line is in the journal before the end pair: nothing of the end
/// happens before the loss is recorded.
#[test]
fn a_gap_is_journaled_before_the_end_pair() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    poll_until("the start line", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, .. } if node == NODE))
            .then_some(())
    });
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let gap_seq = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::InboxGap { .. }))
        .expect("a gap line")
        .seq;
    // The first read line that moves the position past the missing range
    // (any start, progress or end line above 0) must come after the gap:
    // the end event alone would be too late to prove it.
    let moved_seq = first_move_past(&events, 0).expect("a read line moved the position");
    assert!(
        gap_seq < moved_seq,
        "the gap line ({gap_seq}) precedes the first line that moves the position ({moved_seq})"
    );
}

/// The journal seq of the first `inbox_wait_read` of the wait node whose
/// `through_seq` is above `position`: the first line that moves the
/// position past it.
fn first_move_past(events: &[Event], position: u64) -> Option<u64> {
    events
        .iter()
        .find(|e| {
            matches!(
                &e.payload,
                EventPayload::InboxWaitRead { node, through_seq, .. }
                    if node == NODE && *through_seq > position
            )
        })
        .map(|e| e.seq)
}

/// The gap is journaled once: a resume of the open visit reads the same
/// gap again, and the journal still holds one line, under the wait node's
/// own id.
#[test]
fn a_retention_gap_is_journaled_once() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    use apb_engine::scheduler::resume;
    let env = Env::new();
    // A condition no stored event meets, so the visit keeps waiting across
    // the resume and reads the same gap again.
    let yaml = take_yaml(3, r#" [{ path: from, equals: nobody }]"#, "[]");
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    poll_until("the start line", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, .. } if node == NODE))
            .then_some(())
    });
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    poll_until("the gap line", || {
        (!gap_lines(&read_all(&run_dir).unwrap()).is_empty()).then_some(())
    });
    // The visit is still open: a person pauses and resumes it, so a second
    // drive reads the same gap.
    let req = PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(env.root.path(), &run_id, &req).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let resumed = resume(env.root.path(), &run_id, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Failed, "the timeout ends it");
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        gap_lines(&events),
        vec![("wait".to_string(), 1, 4)],
        "one gap line for the range, across the resume, under the wait's id"
    );
    // The timeout end keeps the position at the start line's through_seq
    // when nothing matched: the resume re-read from there.
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "timeout");
    assert!(through.is_some());
}

/// When the journal refuses the gap append, the read ends at once:
/// nothing is examined and nothing ended moves the position, the visit
/// does not park, and the next poll journals the gap before any line that
/// moves the position.
#[test]
fn a_failed_gap_append_does_not_advance_the_position() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    // The gap append of this run answers `false`, as a journal that cannot
    // be written would. The guard disarms it when it is dropped.
    let armed = apb_engine::inbox_wait::fail_next_gap_appends(&run_dir, 100);
    poll_until("the start line", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, .. } if node == NODE))
            .then_some(())
    });
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    // The read that finds the gap fails to journal it: nothing moves.
    poll_until("the read that found the gap", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 2).then_some(())
    });
    std::thread::sleep(Duration::from_millis(1_500));
    let during = read_all(&run_dir).unwrap();
    assert!(
        gap_lines(&during).is_empty(),
        "the gap line is not in the journal: {:?}",
        gap_lines(&during)
    );
    for line in wait_lines(&during, NODE) {
        if let EventPayload::InboxWaitRead { through_seq, .. } = &line.payload {
            assert!(
                *through_seq < 5,
                "a line moved the position past the loss: {line:?}"
            );
        }
    }
    assert!(
        end_events(&during, NODE).is_empty(),
        "the visit did not end on an unjournaled loss"
    );
    // The journal takes the gap line again: the visit goes on and ends
    // with the event.
    drop(armed);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        gap_lines(&events),
        vec![("wait".to_string(), 1, 4)],
        "the next poll journals the gap"
    );
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(
        through,
        Some(5),
        "the visit goes on and ends with the event"
    );
    // The gap line is in the journal before any line that moves the
    // position past the loss.
    let gap_seq = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::InboxGap { .. }))
        .unwrap()
        .seq;
    let moved_seq = first_move_past(&events, 0).expect("a read line moved the position");
    assert!(gap_seq < moved_seq, "{gap_seq} vs {moved_seq}");
}

/// The gap append fails while the visit may park: the visit does not park,
/// because the loss is not journaled and the visit is not caught up.
#[test]
fn a_failed_gap_append_does_not_park() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    // The run may release, so the park is admissible.
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let seq = run_parks(&read_all(&run_dir).unwrap())[0].0;
    // The gap append of this run answers `false`, as a journal that cannot
    // be written would.
    let armed = apb_engine::inbox_wait::fail_next_gap_appends(&run_dir, 1000);
    // The events appear while the run is parked: the resumed drive reads
    // them, finds the gap and cannot journal it.
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    let resumed = resume_pause_in_background(env.root.path(), &run_id, seq);
    poll_until("the read that found the gap", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 2).then_some(())
    });
    // The visit does not park: the loss is not journaled, so the visit is
    // not caught up and the park is refused.
    std::thread::sleep(Duration::from_millis(1_500));
    let during = read_all(&run_dir).unwrap();
    assert_eq!(
        run_parks(&during).len(),
        1,
        "the visit parked with an unjournaled loss: {:?}",
        run_parks(&during)
    );
    for line in wait_lines(&during, NODE) {
        if let EventPayload::InboxWaitRead { through_seq, .. } = &line.payload {
            assert!(
                *through_seq < 5,
                "a line moved the position past the loss: {line:?}"
            );
        }
    }
    // The journal takes the gap line again: the visit goes on and ends with
    // the event.
    drop(armed);
    let result = resumed.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        gap_lines(&events),
        vec![("wait".to_string(), 1, 4)],
        "the next poll journals the gap"
    );
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(
        through,
        Some(5),
        "the visit goes on and ends with the event"
    );
}

/// The gap append keeps failing through the deadline: the visit ends
/// `timeout` with `through_seq` at the position it held before the read.
#[test]
fn a_failed_gap_append_held_through_the_deadline_times_out_at_the_held_position() {
    let env = Env::new();
    let yaml = plain_yaml(10);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let seq = run_parks(&read_all(&run_dir).unwrap())[0].0;
    // The gap append of this run answers `false` for every poll.
    let armed = apb_engine::inbox_wait::fail_next_gap_appends(&run_dir, 1000);
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    let resumed = resume_pause_in_background(env.root.path(), &run_id, seq);
    // The visit ends at the deadline with the position it held.
    let result = resumed.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert_eq!(
        result.outcome,
        RunStatus::Failed,
        "the timeout ends the visit"
    );
    let events = read_all(&run_dir).unwrap();
    assert!(
        gap_lines(&events).is_empty(),
        "the gap was never journaled: {:?}",
        gap_lines(&events)
    );
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "timeout");
    assert_eq!(
        through,
        Some(0),
        "the timeout ends at the position held before the read"
    );
    drop(armed);
}

/// The final read before the timeout fails once and its retry succeeds: the
/// in-time event still ends the visit.
#[test]
fn a_final_read_that_fails_once_still_finds_the_event() {
    let env = Env::new();
    let yaml = take_yaml(2, r#"[{ path: from, equals: "acme" }]"#, "[]");
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    // The first read of the visit is observed before the first event is
    // stored, so the read that finds the event is the one that follows it.
    poll_until("the first read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 1).then_some(())
    });
    let before = apb_engine::inbox_wait::read_counts(&run_dir).reads;
    // The first event does not match. The visit reads it and holds its
    // position.
    store(serde_json::json!({ "from": "other" }));
    poll_until("the read of the first event", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads > before).then_some(())
    });
    // The ordinary reads of the visit fail from here on, so the final read
    // is the only read that can find the second event: a delayed drive
    // cannot take the branch with an ordinary read.
    apb_engine::inbox_wait::hold_ordinary_reads_until_final(&run_dir);
    // The second event matches and is stored inside the read interval that
    // holds the deadline: only the final read finds it.
    store(serde_json::json!({ "from": "acme" }));
    // The final read fails once; its retry reads the log again. An
    // ordinary read never spends this failure.
    apb_engine::inbox_wait::fail_next_final_reads(&run_dir, 1);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(
        end, "matched",
        "the retry of the final read found the event"
    );
    assert_eq!(through, Some(2), "the visit ends with the second event");
    let finals = apb_engine::inbox_wait::final_read_counts(&run_dir);
    assert_eq!(
        finals.reads, 2,
        "the final read failed once and its retry read again"
    );
    assert_eq!(finals.failures, 1, "the first final read failed");
}

/// The final read before the timeout fails twice: the visit times out at the
/// position it held, and the next visit examines the event.
#[test]
fn a_final_read_that_fails_twice_times_out_at_the_held_position() {
    let env = Env::new();
    let yaml = loop2_take_yaml(2);
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    // The first read of the visit is observed before the first event is
    // stored, so the read that finds the event is the one that follows it.
    poll_until("the first read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 1).then_some(())
    });
    let before = apb_engine::inbox_wait::read_counts(&run_dir).reads;
    // The first event does not match. The visit reads it and holds its
    // position.
    store(serde_json::json!({ "from": "other" }));
    poll_until("the read of the first event", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads > before).then_some(())
    });
    // The ordinary reads of the visit fail from here on, so the final read
    // is the only read that can find the second event: a delayed drive
    // cannot take the branch with an ordinary read.
    apb_engine::inbox_wait::hold_ordinary_reads_until_final(&run_dir);
    store(serde_json::json!({ "from": "acme" }));
    // The final read fails twice: the visit ends at the position it held.
    // An ordinary read never spends these failures.
    apb_engine::inbox_wait::fail_next_final_reads(&run_dir, 2);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed, "the run ends");
    let events = read_all(&run_dir).unwrap();
    let ends: Vec<(Option<u64>, String)> = wait_lines(&events, NODE)
        .iter()
        .filter_map(|line| match &line.payload {
            EventPayload::InboxWaitRead {
                through_seq,
                end: Some(end),
                ..
            } => Some((Some(*through_seq), end.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2, "two visits ended: {ends:?}");
    assert_eq!(
        ends[0],
        (Some(1), "timeout".to_string()),
        "the first visit timed out at the position it held"
    );
    assert_eq!(
        ends[1],
        (Some(2), "matched".to_string()),
        "the next visit examined the event"
    );
    let finals = apb_engine::inbox_wait::final_read_counts(&run_dir);
    assert_eq!(finals.reads, 2, "the final read was tried twice");
    assert_eq!(finals.failures, 2, "both final reads failed");
}

/// A log that stays unreadable is tried once per read interval, not on
/// every poll.
#[test]
fn a_failed_read_waits_the_read_interval() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    let env = Env::new();
    let yaml = take_yaml(6, r#"[{ path: from, equals: "acme" }]"#, "[]");
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    // The run is paused with its visit open, so the resumed drive reads
    // the account's log with a fresh scan and every read of it fails.
    let req = PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(env.root.path(), &run_id, &req).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    apb_engine::inbox_wait::fail_next_reads(&run_dir, 1000);
    let resumed = resume_in_background(env.root.path(), &run_id);
    poll_until("the first failed read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).failures >= 1).then_some(())
    });
    let first = apb_engine::inbox_wait::read_counts(&run_dir).reads;
    // The window is three read intervals and half an interval, so the poll
    // tick of the drive does not decide the count.
    std::thread::sleep(Duration::from_millis(3_500));
    let reads = apb_engine::inbox_wait::read_counts(&run_dir).reads - first;
    assert!(
        reads >= 3,
        "the unreadable log was read {reads} times in three seconds and a half: the visit stopped retrying"
    );
    assert!(
        reads <= 4,
        "the unreadable log was read {reads} times in three seconds and a half: the visit read it more than once per interval"
    );
    // The visit is still open: a failed read does not end it.
    let _ = resumed.recv_timeout(Duration::from_secs(30));
}

/// A range another node recorded (a connector read of the same account
/// journals it under its node) is journaled again for the wait, under the
/// wait's own id, before the position advances.
#[test]
fn a_gap_another_node_recorded_is_journaled_for_the_wait() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    poll_until("the start line", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, .. } if node == NODE))
            .then_some(())
    });
    // What a connector read of the same account journals: the range under
    // its own node id, no wait involved.
    {
        let mut log = apb_engine::event::EventLog::open(&run_dir).unwrap();
        log.append(EventPayload::InboxGap {
            node_id: "reader".into(),
            connector: CONNECTOR.into(),
            account: ACCOUNT.into(),
            from: 1,
            through: 4,
        })
        .unwrap();
    }
    write_events_raw(5, 9, serde_json::json!({ "from": "x" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        gap_lines(&events),
        vec![("reader".to_string(), 1, 4), ("wait".to_string(), 1, 4),],
        "the wait journals the same range again under its own id"
    );
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(
        through,
        Some(5),
        "the position advanced past the recorded loss"
    );
}

/// Two connector reads of one gap still journal it once: the existing
/// `journal_gap` rule, unchanged (the same assertion
/// `connector_inbox::a_read_reports_unread_events_retention_dropped_once_per_run`
/// makes through real calls).
#[test]
fn a_connector_read_gap_rule_is_unchanged() {
    use apb_core::connector::inbox::Inbox;
    use apb_engine::connector::call::{CallRequest, execute};
    use apb_engine::manifest::{self, ManifestConnectorGrant, RunExecutionManifest};
    let env = Env::new();
    // A run whose node holds the connector's inbox_read function.
    let run = env.root.path().join(".apb/runs/r1");
    fs::create_dir_all(&run).unwrap();
    let connectors = apb_engine::connector::run::snapshot_connectors(
        env.root.path(),
        &run,
        &Playbook::from_yaml(&plain_yaml(30)).unwrap(),
        &env.maps(&plain_yaml(30)).0,
        &env.maps(&plain_yaml(30)).1,
        false,
    )
    .unwrap()
    .0;
    let mut m = RunExecutionManifest {
        connectors,
        ..Default::default()
    };
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.into(),
            accounts: vec![ACCOUNT.into()],
            functions: vec!["inbox_read".into()],
            max_calls: None,
            on_unknown: None,
        }],
    );
    manifest::write(&run, &m).unwrap();
    // Deliveries 1 to 3, then a size cap that keeps only the newest: seqs
    // 2 and 3 are gone before the consumer read them.
    let store = Inbox::at(&env.cfg.path().join("connector-inbox"), CONNECTOR, ACCOUNT).unwrap();
    for i in 1..=3 {
        store
            .append(&format!("m{i}"), &serde_json::json!({ "n": i }))
            .unwrap();
    }
    store.ack("worker", 1).unwrap();
    let tiny = Retention {
        max_bytes: 1,
        max_age_ms: u64::MAX,
    };
    store
        .append_with("m4", &serde_json::json!({ "n": 4 }), &tiny)
        .unwrap();

    let call = |consumer: &str| {
        execute(CallRequest {
            run_dir: &run,
            root: env.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function: "inbox_read",
            account: None,
            args: serde_json::json!({ "consumer": consumer }),
            dry_run: false,
            full: false,
        })
    };
    for _ in 0..2 {
        let (out, ok) = call("worker");
        assert_eq!(out["ok"], true, "the read succeeds: {out}");
        assert_eq!(out["gap"], serde_json::json!({ "from": 2, "through": 3 }));
        let _ = ok;
    }
    let events = read_all(&run).unwrap();
    assert_eq!(
        gap_lines(&events),
        vec![("wait".to_string(), 2, 3)],
        "two reads, one journal line"
    );
}

// ----------------------------------------------------------------- 2.9

/// A marker in an undeclared body field, and one in the field a condition
/// reads, reach neither the journal file nor the node output: only the
/// declared values are stored.
#[test]
fn an_undeclared_field_never_reaches_the_journal() {
    let env = Env::new();
    let yaml = take_yaml(
        30,
        r#"[{ path: from, equals: "{{params.customer}}" }]"#,
        r#"[{ name: text, path: text }]"#,
    );
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "MARKER-in-from".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({
        "from": "MARKER-in-from",
        "text": "hello",
        "hidden": "MARKER-undeclared",
    }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let raw = raw_journal(&run_dir);
    assert!(
        !raw.contains("MARKER-undeclared"),
        "an undeclared field reached the journal"
    );
    assert!(
        !raw.contains("MARKER-in-from"),
        "the condition's field value reached the journal"
    );
    let events = read_all(&run_dir).unwrap();
    let v = output_json(&events, NODE);
    assert_eq!(v["text"], "hello");
    assert!(v.get("hidden").is_none() && v.get("from").is_none(), "{v}");
}

/// The stated limit of the redactor: a known secret of 8 characters in a
/// declared field is stored redacted; one of 4 characters, below
/// `MIN_SECRET_LEN`, is stored as it is.
#[test]
fn a_short_known_secret_in_a_declared_field_is_the_stated_limit() {
    // The long secret: a known value over MIN_SECRET_LEN is scrubbed.
    let env = Env::new();
    let yaml = take_yaml(30, "[]", r#"[{ name: text, path: text }]"#);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "text": LONG_SECRET }));
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    let raw = raw_journal(&run_dir);
    assert!(!raw.contains(LONG_SECRET), "the secret leaked: {raw}");
    assert!(raw.contains("[redacted]"), "the value was replaced: {raw}");
    drop(env);

    // The short secret: a known value below MIN_SECRET_LEN is stored as it
    // is, the limit the docs state.
    let env = Env::new();
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "text": SHORT_SECRET }));
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    let raw = raw_journal(&run_dir);
    assert!(
        raw.contains(SHORT_SECRET),
        "the short secret is stored as it is: {raw}"
    );
    drop(env);
}

/// A secret the workspace references that this process cannot resolve (a
/// command that fails): the visit journals nothing for the wait and fails
/// `inbox_wait_secrets_unresolved`.
#[test]
fn an_unresolved_secret_stores_nothing_and_fails() {
    let env = Env::with_cmd_secrets("false", "false");
    let yaml = take_yaml(30, "[]", r#"[{ name: text, path: text }]"#);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "text": "hello" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "failed");
    assert!(
        output.starts_with("inbox_wait_secrets_unresolved:"),
        "was: {output}"
    );
    // Nothing was journaled for the wait: the start line only.
    let read_lines = wait_lines(&events, NODE);
    assert_eq!(read_lines.len(), 1, "only the start line: {}", events.len());
    let start_through = match &read_lines[0].payload {
        EventPayload::InboxWaitRead {
            through_seq,
            end: None,
            ..
        } => *through_seq,
        _ => unreachable!(),
    };
    assert_eq!(
        start_through, 0,
        "the start line was written first, at the empty head"
    );
    assert!(
        !events.iter().any(|e| {
            matches!(&e.payload, EventPayload::WaitSignalled { node } | EventPayload::WaitTimeout { node } if node == NODE)
        }),
        "no end event"
    );
    assert!(
        !raw_journal(&run_dir).contains("hello"),
        "the event's value reached the journal"
    );
}

/// A command-sourced secret: the redactor never runs the command, so no
/// value of it is stored and the visit fails the same way.
#[test]
fn a_command_sourced_secret_stores_no_value() {
    // The marker lives in the test's own directory, quoted, so a path with
    // a space is still one operand.
    let marker_dir = tempfile::tempdir().unwrap();
    let marker = marker_dir.path().join("redactor ran");
    let cmd = format!("touch '{}'", marker.display());
    // Positive control: the exact command line creates the marker.
    assert!(
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .status()
            .unwrap()
            .success()
            && marker.exists(),
        "the command line creates the marker"
    );
    fs::remove_file(&marker).unwrap();
    let env = Env::with_cmd_secrets(&cmd, &cmd);
    let yaml = take_yaml(30, "[]", r#"[{ name: text, path: text }]"#);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "text": "hello" }));
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let (_, output) = node_finished(&events, NODE).expect("the node finished");
    assert!(
        output.starts_with("inbox_wait_secrets_unresolved:"),
        "was: {output}"
    );
    assert!(
        !marker.exists(),
        "the storage redactor ran the command that sources the secret"
    );
    assert_eq!(wait_lines(&events, NODE).len(), 1, "only the start line");
}

// ----------------------------------------------------------------- 2.10

/// Cuts the journal right after the last line whose payload matches
/// `pred`. Only while no drive is running (the run is paused or ended).
fn cut_journal_after(run_dir: &Path, pred: &dyn Fn(&EventPayload) -> bool) {
    let text = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    let events = read_all(run_dir).unwrap();
    let target = events
        .iter()
        .rev()
        .find(|e| pred(&e.payload))
        .unwrap_or_else(|| panic!("no line matches"))
        .seq;
    // The cut point is the newline of the target line (journal seqs are
    // line numbers).
    let mut offset = 0;
    for _ in 0..=target {
        let len = text[offset..].find('\n').unwrap() + 1;
        offset += len;
    }
    let f = fs::OpenOptions::new()
        .write(true)
        .open(run_dir.join("events.jsonl"))
        .unwrap();
    f.set_len(offset as u64).unwrap();
}

/// The end is one write: a journal cut after the end line (its second line
/// gone) still finishes with that line's output - the repair is made from
/// the journal alone (crash table row 8).
#[test]
fn the_end_is_one_write() {
    use apb_engine::scheduler::resume;
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "from": "x" }));
    assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let end_output = end_line(&events, NODE).3.expect("the end output");
    // The drive is gone: cut after the end line, before the end event.
    cut_journal_after(
        &run_dir,
        &|p| matches!(&p, EventPayload::InboxWaitRead { node, end: Some(_), .. } if node == NODE),
    );
    let events = read_all(&run_dir).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitSignalled { .. })),
        "the end event is gone"
    );
    let resumed = resume(env.root.path(), &env.run_id_of(&run_dir), None).unwrap();
    assert_eq!(
        resumed.outcome,
        RunStatus::Succeeded,
        "the repair finishes it"
    );
    let events = read_all(&run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "succeeded");
    assert_eq!(output, end_output, "the journaled line's output");
}

/// A crash between the end pair and `node_finished`: the resumed drive
/// appends `node_finished` with the journaled output and declares no new
/// visit (crash table row 10, the test barrier).
#[test]
fn an_inbox_wait_ended_before_node_finished_keeps_its_output() {
    use apb_engine::deadline_hooks;
    use apb_engine::scheduler::resume;
    let env = Env::new();
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&fired);
    deadline_hooks::set(
        deadline_hooks::Hook::WaitBeforeNodeFinished,
        env.root.path(),
        move |_| {
            if !flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
                panic!("the drive died after the wait's end event");
            }
        },
    );
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    store(serde_json::json!({ "from": "x" }));
    poll_until("the barrier fired", || {
        fired
            .load(std::sync::atomic::Ordering::SeqCst)
            .then_some(())
    });
    deadline_hooks::clear(env.root.path());
    let events = read_all(&run_dir).unwrap();
    assert!(
        !events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::NodeFinished { node, .. } if node == NODE
        )),
        "the drive stopped before the node_finished"
    );
    let end_output = end_line(&events, NODE).3.expect("the end output");
    // The drive thread panicked at the barrier: wait until it has unwound
    // (its sender is dropped), so its workdir lock is released first.
    match rx.recv_timeout(Duration::from_secs(30)) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        other => panic!("the drive thread did not end at the barrier: {other:?}"),
    }
    let resumed = resume(env.root.path(), &env.run_id_of(&run_dir), None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (status, output) = node_finished(&events, NODE).expect("the node finished");
    assert_eq!(status, "succeeded");
    assert_eq!(output, end_output);
    // No new visit: one start pair, one end pair.
    assert_eq!(
        wait_lines(&events, NODE).len(),
        2,
        "start line and end line only"
    );
    assert_eq!(end_events(&events, NODE), vec!["signalled"]);
}

/// A crash before the end (the in-memory scan lost on a person's pause):
/// the fresh scan reads from the position and examines the same events
/// again (crash table row 3).
#[test]
fn a_crash_before_the_end_reads_the_same_events_again() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    use apb_engine::scheduler::resume;
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    // The event arrives while the drive is stopped: the scan it had is
    // lost with it.
    let req = PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(env.root.path(), &run_id, &req).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let seq = store(serde_json::json!({ "from": "x" }));
    let resumed = resume(env.root.path(), &run_id, None).unwrap();
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (through, examined, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(through, Some(seq), "the fresh scan examined the same event");
    assert_eq!(examined, 0);
}

/// A start line without its `wait_started` is superseded: the resumed
/// drive writes a new start pair at the end of the journal with the
/// current head, and the visit never examines an event at or below it
/// (crash table row 2).
#[test]
fn a_start_line_without_wait_started_is_superseded() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    use apb_engine::scheduler::resume;
    let env = Env::new();
    let yaml = plain_yaml(2);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    // The drive stops with the start pair in the journal; the pair is cut
    // after its first line.
    let req = PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(env.root.path(), &run_id, &req).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    cut_journal_after(
        &run_dir,
        &|p| matches!(&p, EventPayload::InboxWaitRead { node, end: None, .. } if node == NODE),
    );
    // The journal ends on the start line: no `wait_started` follows it, so
    // no visit was declared and the line is superseded.
    let tail_torn = {
        let events = read_all(&run_dir).unwrap();
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WaitStarted { node, .. } if node == NODE))
    };
    assert!(tail_torn, "the torn start line declared no visit");
    // Events stored before the resume: the new start pair must sit at
    // their head.
    store(serde_json::json!({ "n": 1 }));
    store(serde_json::json!({ "n": 2 }));
    store(serde_json::json!({ "n": 3 }));
    let resumed = resume(env.root.path(), &run_id, None).unwrap();
    assert_eq!(
        resumed.outcome,
        RunStatus::Failed,
        "nothing matched in time"
    );
    let events = read_all(&run_dir).unwrap();
    // The new start pair is the LAST start line, at the current head.
    let starts: Vec<u64> = wait_lines(&events, NODE)
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::InboxWaitRead {
                through_seq,
                end: None,
                ..
            } => Some(*through_seq),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        vec![0, 3],
        "the torn line, then the new pair at the head"
    );
    let new_start_seq = events
        .iter()
        .rev()
        .find(|e| matches!(&e.payload, EventPayload::InboxWaitRead { node, end: None, .. } if node == NODE))
        .unwrap()
        .seq;
    let end_seq = events
        .iter()
        .find(|e| matches!(&e.payload, EventPayload::WaitTimeout { node } if node == NODE))
        .unwrap()
        .seq;
    assert!(new_start_seq < end_seq, "the new pair precedes the end");
    // The visit examined nothing: every event sits at or below the head.
    let (_, examined, end, _) = end_line(&events, NODE);
    assert_eq!(end, "timeout");
    assert_eq!(examined, 0, "nothing at or below the head was examined");
    let (through, _, _, _) = end_line(&events, NODE);
    assert_eq!(through, Some(3), "the position is the new head");
}

/// A journaled end is finished without authorization: a whole end pair and
/// a torn one, each with the account edited and the inbox log removed
/// before the resume - the node finishes from the journal and the inbox is
/// not read.
#[test]
fn a_journaled_end_is_finished_without_authorization() {
    use apb_engine::scheduler::resume;
    let run_case = |torn: bool| -> bool {
        let env = Env::new();
        let yaml = plain_yaml(60);
        env.seed(&yaml);
        let rx = env.spawn(&yaml);
        let run_dir = await_wait_started(&env);
        store(serde_json::json!({ "from": "x" }));
        assert_eq!(wait_result(&rx).outcome, RunStatus::Succeeded);
        let events = read_all(&run_dir).unwrap();
        let end_output = end_line(&events, NODE).3.expect("the end output");
        // The drive is gone. Cut the journal after the end pair, or after
        // the end line alone (the torn pair).
        if torn {
            cut_journal_after(
                &run_dir,
                &|p| matches!(&p, EventPayload::InboxWaitRead { node, end: Some(_), .. } if node == NODE),
            );
        } else {
            cut_journal_after(
                &run_dir,
                &|p| matches!(&p, EventPayload::WaitSignalled { node } if node == NODE),
            );
        }
        // The account changes and the inbox log is gone: a read or an
        // authorize would refuse.
        env.change_account();
        fs::remove_file(inbox().dir().join("events.jsonl")).unwrap();
        let resumed = resume(env.root.path(), &env.run_id_of(&run_dir), None).unwrap();
        let ok = resumed.outcome == RunStatus::Succeeded;
        let events = read_all(&run_dir).unwrap();
        let (status, output) = node_finished(&events, NODE).expect("the node finished");
        assert_eq!(status, "succeeded");
        assert_eq!(output, end_output, "the journaled output");
        ok
    };
    assert!(run_case(false), "the whole end pair");
    assert!(run_case(true), "the torn end pair");
}

/// A failed visit lets the loop's next visit start fresh: the first visit
/// refuses (the account changed under it), the failure routes on, the
/// account is fixed while the loop's bounce timer runs, and the second
/// visit starts new and ends with the event.
#[test]
fn a_failed_inbox_wait_lets_the_next_visit_start() {
    use apb_engine::pause::{PauseBy, PauseMode, PauseRequest, request_pause};
    use apb_engine::scheduler::resume;
    let env = Env::new();
    // The bounce timer gives the test time to fix the account between the
    // visits; the bounded edge ends the loop after the second visit.
    let yaml = loop2_yaml(30).replace(
        "wait_for: { type: timer, seconds: 0 }",
        "wait_for: { type: timer, seconds: 2 }",
    );
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    // The drive stops with visit 1 declared; the account changes under it.
    let req = PauseRequest {
        reason: Some("looking".into()),
        by: Some(PauseBy::Operator),
        mode: PauseMode::Drain,
        ..PauseRequest::default()
    };
    request_pause(env.root.path(), &run_id, &req).unwrap();
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    env.change_account();
    // The resume drives visit 1 to its refusal and into the bounce timer;
    // the fix lands while it waits there.
    let (tx, res) = mpsc::channel();
    {
        let root = env.root.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(resume(&root, &run_id, None));
        });
    }
    std::thread::sleep(Duration::from_millis(1_000));
    env.restore_account();
    let seq = store(serde_json::json!({ "from": "x" }));
    let resumed = res
        .recv_timeout(POLL_DEADLINE)
        .expect("the run ended")
        .unwrap();
    // The loop is exhausted after the second visit (the failure finish),
    // but the second visit itself started new and succeeded.
    let events = read_all(&run_dir).unwrap();
    let finishes: Vec<(&str, String)> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                output,
                ..
            } if node == NODE => Some((status.as_str(), output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(finishes.len(), 2, "two visits: {finishes:?}");
    assert_eq!(finishes[0].0, "failed");
    assert!(
        finishes[0].1.starts_with("inbox_wait_unauthorized:"),
        "was: {}",
        finishes[0].1
    );
    assert_eq!(finishes[1].0, "succeeded", "the next visit started fresh");
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(through, Some(seq));
    assert_eq!(
        resumed.outcome,
        RunStatus::Failed,
        "the loop's failure finish"
    );
}

// ----------------------------------------------------------------- 2.13

use apb_engine::driver::read_driver_pid;
use apb_engine::pause::PauseBy;
use apb_engine::pause::{WakeOutcome, run_waker};

/// A wait with a `types` list and one `match` condition on the params: the
/// waker wakes on the type, the visit's conditions may still not match.
fn park_yaml(timeout: u64, types: &str, equals: &str) -> String {
    let types = if types.is_empty() {
        String::new()
    } else {
        format!("\n      types: [{types}]")
    };
    let r#match = if equals.is_empty() {
        String::new()
    } else {
        format!("\n      match:\n        - {{ path: from, equals: \"{equals}\" }}")
    };
    format!(
        r#"
schema: 1
id: w
name: W
version: 1.0.0
params:
  - {{ name: customer, type: text }}
nodes:
  - {{ id: start, type: start }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main{types}{match}
    timeout_seconds: {timeout}
  - {{ id: ok, type: finish, outcome: success }}
  - {{ id: no, type: finish, outcome: failure }}
edges:
  - {{ from: start, to: wait }}
  - {{ from: wait, to: ok, condition: {{ type: node_status, node: wait, equals: success }} }}
  - {{ from: wait, to: no, fallback: true }}
"#
    )
}

/// The run's parks: seq, reason, by, until, keep_lock.
type ParkRow = (u64, String, Option<PauseBy>, Option<u64>, bool);

fn run_parks(events: &[Event]) -> Vec<ParkRow> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::RunPaused {
                reason,
                by,
                until,
                keep_lock,
                ..
            } => Some((e.seq, reason.clone(), *by, *until, *keep_lock)),
            _ => None,
        })
        .collect()
}

/// The resumed drive of a paused run, on a thread: the test reads the
/// journal while the run drives.
fn resume_in_background(
    root: &Path,
    run_id: &str,
) -> mpsc::Receiver<Result<apb_engine::RunResult, apb_engine::error::EngineError>> {
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::resume(&r, &id, None));
    });
    rx
}

/// The resumed drive of the pause at journal sequence `seq`, on a
/// thread: the test reads the journal while the run drives.
fn resume_pause_in_background(
    root: &Path,
    run_id: &str,
    seq: u64,
) -> mpsc::Receiver<Result<apb_engine::RunResult, apb_engine::error::EngineError>> {
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(apb_engine::scheduler::resume_pause(&r, &id, seq, None));
    });
    rx
}

/// The park waker, run in-process on a thread: the test then drives the
/// resumed run itself (`resume_pause`), the way a test binary cannot
/// re-exec `__drive-run`.
fn waker_outcome(
    root: &Path,
    run_id: &str,
    seq: u64,
) -> mpsc::Receiver<Result<WakeOutcome, apb_engine::error::EngineError>> {
    let (tx, rx) = mpsc::channel();
    let (r, id) = (root.to_path_buf(), run_id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(run_waker(&r, &id, seq));
    });
    rx
}

/// A long inbox wait over an empty inbox parks through the wait park with
/// the timer park's own reason: `until` at the timeout, `by` engine, the
/// `wait_park` guard before and after, the start line the only read line,
/// no driver.
#[test]
fn a_long_inbox_wait_parks_with_the_timer_reason() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let events = read_all(&run_dir).unwrap();
    let parks = run_parks(&events);
    assert_eq!(parks.len(), 1, "one park: {parks:?}");
    let (seq, reason, by, until, keep_lock) = parks[0].clone();
    assert_eq!(reason, "wait (wait)", "the timer park's own reason form");
    assert_eq!(by, Some(PauseBy::Engine));
    assert!(!keep_lock, "a run whose nodes write nothing keeps no lock");
    // The wake time is the wait's deadline on the paused-interval clock.
    let (ws, deadline) = env.wait_deadline_ms(&run_dir, 30);
    let _ = deadline;
    assert_eq!(until, Some(u64::try_from(ws + 30_000).unwrap()));
    // The guard names `wait_park` before and after the park (the park is a
    // checkpoint).
    let idx = events.iter().position(|e| e.seq == seq).unwrap();
    let names = |e: &Event| {
        matches!(&e.payload, EventPayload::RunGuard { requires }
            if requires.iter().any(|f| f == "wait_park"))
    };
    assert!(
        events[..idx]
            .iter()
            .rev()
            .find(|e| matches!(&e.payload, EventPayload::RunGuard { .. }))
            .is_some_and(names),
        "the guard stands before the park"
    );
    assert!(
        events.get(idx + 1).is_some_and(names),
        "the park is journaled with a guard"
    );
    // The visit journaled its start line only: an empty inbox examined
    // nothing, so no progress line and no end.
    let lines = wait_lines(&events, NODE);
    assert_eq!(lines.len(), 1, "one read line: {lines:?}");
    assert!(
        matches!(
            &lines[0].payload,
            EventPayload::InboxWaitRead {
                through_seq: 0,
                examined: 0,
                end: None,
                ..
            }
        ),
        "the start line: {:?}",
        lines[0].payload
    );
    assert!(end_events(&events, NODE).is_empty(), "no end");
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == NODE)),
        "no node end"
    );
    assert!(
        read_driver_pid(&run_dir).is_none(),
        "a parked run holds no driver"
    );
}

/// A patch of the wait's definition, posted while the visit is open: the
/// patch version and the resume of the run.
fn patch_the_wait(root: &Path, run_id: &str, yaml: &str) -> String {
    let version =
        apb_core::versioning::create_patch_version(root, "w", "1.0.0", yaml, run_id, "improvement")
            .unwrap();
    apb_engine::scheduler::post_supervisor_command(
        root,
        run_id,
        apb_engine::control::Control::Patch {
            version: version.clone(),
            classification: "improvement".into(),
            continue_from: NODE.into(),
        },
    )
    .unwrap();
    version
}

/// A patch that changes the wait's account to one the manifest does not
/// grant fails the node: the next poll of the open visit is a first poll,
/// and it authorizes against the new definition.
#[test]
fn a_patch_that_changes_the_account_fails_the_wait() {
    let env = Env::new();
    // A second account, configured but not granted and not approved.
    let account = format!(
        "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"{{{{env.{SHORT_VAR}}}}}\"\n  - name: other\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"{{{{env.{SHORT_VAR}}}}}\"\n"
    );
    write_account_config(env.cfg.path(), &account);
    let yaml = plain_yaml(60);
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    // The original scan is observed before the patch: account A was read
    // and authorized, so the patch has a scan to invalidate.
    poll_until("the read of account A", || {
        (apb_engine::inbox_wait::account_read_counts(&run_dir, ACCOUNT).reads >= 1).then_some(())
    });
    // The patch names the other account.
    let patched = plain_yaml(60).replace("      account: main\n", "      account: other\n");
    patch_the_wait(env.root.path(), &run_id, &patched);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let (_, output) = node_finished(&events, NODE).expect("the node failed");
    assert!(
        output.starts_with("inbox_wait_unauthorized:"),
        "was: {output}"
    );
    // The other account was never read: the read counter of the account
    // says so, not the absence of a file (reading an absent log creates
    // none).
    assert_eq!(
        apb_engine::inbox_wait::account_read_counts(&run_dir, "other").reads,
        0,
        "the other account was read"
    );
}

/// A supervised fork with two deferred agent retries and an open inbox wait
/// on account A: the retries fail as a batch, the supervisor's patch changes
/// the wait to account B, which the manifest does not grant, and the run
/// continues from the wait. The patch replaced the definition the drive
/// executes, so the wait's scan is dropped: the next poll authorizes against
/// the new definition and refuses, and the other account is never read.
#[test]
fn a_batch_supervisor_patch_drops_the_wait_scan() {
    use apb_engine::scheduler::RunMode;
    let env = Env::new();
    // A second account, configured but not granted and not approved.
    let account = format!(
        "accounts:\n  - name: {ACCOUNT}\n    default: true\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"{{{{env.{SHORT_VAR}}}}}\"\n  - name: other\n    verify_token: \"{{{{env.{LONG_VAR}}}}}\"\n    app_secret: \"{{{{env.{SHORT_VAR}}}}}\"\n"
    );
    write_account_config(env.cfg.path(), &account);
    // The agent of the two retry nodes fails every call.
    let agent = failing_agent(env.root.path());
    let _cmd = EnvVar::set("APB_AGENT_CMD", &agent);
    common::seed_main(env.root.path());
    let yaml = batch_wait_yaml(600);
    env.seed(&yaml);
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        mode: RunMode::Supervised,
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    // The wait's visit is open and its scan is populated: account A was
    // read and authorized.
    poll_until("the read of account A", || {
        (apb_engine::inbox_wait::account_read_counts(&run_dir, ACCOUNT).reads >= 1).then_some(())
    });
    // The retries fail and the supervisor is asked about the first member
    // that failed: `a` or `b`, whichever deferred retry ran first, which a
    // loaded machine does not order.
    let first = poll_until("the wake of the first member", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::WakeRaised { node, .. } if node == "a" || node == "b" => {
                    Some(node.clone())
                }
                _ => None,
            })
    });
    let second = if first == "a" { "b" } else { "a" };
    // The other account's log holds an event: a scan that survives the patch
    // reads it without authorizing.
    store_in("other", serde_json::json!({ "from": "x" }));
    // The supervisor's patch changes the wait's account to one the manifest
    // does not grant, and continues from the wait.
    let patched = batch_wait_yaml(600).replace("      account: main\n", "      account: other\n");
    patch_the_wait(env.root.path(), &run_id, &patched);
    // The second member's wake: the run continues from the wait.
    poll_until("the wake of the second member", || {
        read_all(&run_dir)
            .ok()?
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::WakeRaised { node, .. } if node == second))
            .then_some(())
    });
    apb_engine::scheduler::post_supervisor_command(
        env.root.path(),
        &run_id,
        apb_engine::control::Control::ContinueFrom { node: NODE.into() },
    )
    .unwrap();
    // The wait refuses: its scan was dropped, so the poll authorizes against
    // the patched definition and the other account is never read. A loaded
    // machine reaches the finish slowly; the read of the other account is
    // the failure this test waits for, so the deadline is long and checked
    // beside the finish, not against the wall clock alone.
    let refused = poll_until("the refusal of the patched wait", || {
        let events = read_all(&run_dir).ok()?;
        let finished = node_finished(&events, NODE).map(|(_, output)| output.to_string());
        let other = apb_engine::inbox_wait::account_read_counts(&run_dir, "other").reads;
        if other > 0 {
            Some(None)
        } else {
            finished.map(Some)
        }
    });
    assert!(
        refused.is_some(),
        "the other account was read: the wait's scan survived the patch"
    );
    let output = refused.expect("the wait did not refuse the patched definition");
    assert!(
        output.starts_with("inbox_wait_unauthorized:"),
        "was: {output}"
    );
    // The failed node waits for a supervisor that is gone: the test ends the
    // run.
    apb_engine::scheduler::post_supervisor_command(
        env.root.path(),
        &run_id,
        apb_engine::control::Control::Abort {
            reason: "the test ends the run".into(),
            by: None,
            graceful: false,
        },
    )
    .ok();
    let _ = rx.recv_timeout(AGENT_WAIT_DEADLINE);
}

/// The deadline of a step that waits on agent processes of this file's
/// supervised runs: a loaded runner schedules the stub's shell processes
/// slowly, so the bound is named and longer than a plain poll's.
const AGENT_WAIT_DEADLINE: Duration = Duration::from_secs(60);

/// A supervised fork: two agent nodes whose retry waits, the inbox wait
/// beside them, and the finish at the end.
fn batch_wait_yaml(timeout: u64) -> String {
    format!(
        r#"
schema: 1
id: w
name: W
version: 1.0.0
defaults: {{ profile: main, max_parallel: 4 }}
nodes:
  - {{ id: start, type: start }}
  - {{ id: a, type: agent_task, prompt: "FAILS", max_retries: 1, retry_delay: {{ initial: 1s }} }}
  - {{ id: b, type: agent_task, prompt: "FAILS", max_retries: 1, retry_delay: {{ initial: 1s }} }}
  - id: wait
    type: wait
    wait_for:
      type: inbox
      connector: echo-hooks
      account: main
    timeout_seconds: {timeout}
  - {{ id: done, type: finish, outcome: success }}
edges:
  - {{ from: start, to: a }}
  - {{ from: start, to: b }}
  - {{ from: start, to: wait }}
  - {{ from: a, to: done }}
  - {{ from: b, to: done }}
  - {{ from: wait, to: done }}
"#
    )
}

/// The stub agent of the supervised batch test: every call fails, as an
/// agent that made a mistake would.
fn failing_agent(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("failing-agent.sh");
    common::write_sync(
        &path,
        "#!/bin/sh\necho 'the agent made a mistake' 1>&2\nexit 1\n",
    );
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path.to_string_lossy().into_owned()
}

/// A patch that changes the wait's condition matches by the new condition:
/// the next poll of the open visit renders the new definition.
#[test]
fn a_patch_that_changes_the_condition_matches_by_the_new_condition() {
    let env = Env::new();
    let yaml = take_yaml(30, r#"[{ path: from, equals: "acme" }]"#, "[]");
    env.seed(&yaml);
    let rx = env.spawn(&yaml);
    let run_dir = await_wait_started(&env);
    let run_id = env.run_id_of(&run_dir);
    // The first read of the absent log is observed, so the event below is
    // stored after it and the visit reads it in a later read.
    poll_until("the first read", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 1).then_some(())
    });
    // The event does not match the condition in force.
    store(serde_json::json!({ "from": "other" }));
    poll_until("the read of the event", || {
        (apb_engine::inbox_wait::read_counts(&run_dir).reads >= 2).then_some(())
    });
    // The patch changes the condition: the event matches it.
    let patched = take_yaml(30, r#"[{ path: from, equals: "other" }]"#, "[]");
    patch_the_wait(env.root.path(), &run_id, &patched);
    let result = wait_result(&rx);
    assert_eq!(result.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (_, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched", "the new condition matched");
}

/// A delivery stored while the run is parked resumes it within one poll of
/// the waker, and the visit ends with the event.
#[test]
fn a_delivery_resumes_a_parked_wait() {
    let env = Env::new();
    // The origin key exists before the run is stamped: the waker's engine
    // resume verifies it (the stable config dir the env lock holds).
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let seq = run_parks(&read_all(&run_dir).unwrap())[0].0;
    let woke = waker_outcome(env.root.path(), &run_id, seq);
    // The delivery lands while the waker is already polling.
    let stored = store(serde_json::json!({ "from": "x" }));
    let woke = woke.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert!(
        matches!(woke, WakeOutcome::Resumed { .. }),
        "the delivery resumed the run: {woke:?}"
    );
    // The resumed drive ends the visit with the event.
    let resumed = apb_engine::scheduler::resume_pause(env.root.path(), &run_id, seq, None)
        .expect("the resumed run drives");
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (through, _, end, output) = end_line(&events, NODE);
    assert_eq!(end, "matched");
    assert_eq!(through, Some(stored));
    assert!(output.is_some(), "the visit ends with the event's output");
}

/// An event stored after the visit's last read but before the waker
/// started is found by the waker's first read: the first call reads
/// whatever the log holds, not only what changes from its own start. The
/// visit first examines an event that matches no condition (the churn the
/// docs state), so its park writes a progress line; the barrier stores the
/// matching event exactly there, after the line and before the park is
/// posted, with no waker running yet.
#[test]
fn an_event_stored_before_the_waker_started_resumes_the_run() {
    let env = Env::new();
    let yaml = park_yaml(30, "order", "{{params.customer}}");
    env.seed(&yaml);
    apb_core::run_origin::ensure_key().unwrap();
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let (connectors, accounts) = env.maps(&yaml);
    let rx = env.spawn_opts(RunOptions {
        params: BTreeMap::from([("customer".to_string(), "acme".to_string())]),
        expected_connectors: connectors,
        expected_connector_accounts: accounts,
        ..RunOptions::default()
    });
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let first = run_parks(&read_all(&run_dir).unwrap())[0].0;
    // The visit examines an event that passes the types and fails the
    // condition, so its next park writes a progress line; the barrier
    // stores the matching event exactly there.
    store_typed(Some("order"), serde_json::json!({ "from": "not-acme" }));
    let woke = waker_outcome(env.root.path(), &run_id, first);
    let woke = woke.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert!(matches!(woke, WakeOutcome::Resumed { .. }), "{woke:?}");
    apb_engine::deadline_hooks::set(
        apb_engine::deadline_hooks::Hook::InboxWaitBeforePark,
        env.root.path(),
        move |_| {
            store_typed(Some("order"), serde_json::json!({ "from": "acme" }));
        },
    );
    let resumed = apb_engine::scheduler::resume_pause(env.root.path(), &run_id, first, None)
        .expect("the resumed run drives");
    assert_eq!(resumed.outcome, RunStatus::Paused, "the visit parks again");
    apb_engine::deadline_hooks::clear(env.root.path());
    let seq = run_parks(&read_all(&run_dir).unwrap())[1].0;
    // The progress line stands in the journal, the matching event stored
    // after it and before the park, and no waker has run yet.
    let events = read_all(&run_dir).unwrap();
    let progress = events
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                EventPayload::InboxWaitRead { node, end: None, examined: 1.., .. } if node == NODE
            )
        })
        .count();
    assert_eq!(progress, 1, "one progress line before the park");
    // The waker starts now, after the event was stored: its first call
    // reads and finds it.
    let woke = waker_outcome(env.root.path(), &run_id, seq);
    let woke = woke.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert!(
        matches!(woke, WakeOutcome::Resumed { .. }),
        "the stored event resumed the run: {woke:?}"
    );
    let resumed = apb_engine::scheduler::resume_pause(env.root.path(), &run_id, seq, None)
        .expect("the resumed run drives");
    assert_eq!(resumed.outcome, RunStatus::Succeeded);
    let events = read_all(&run_dir).unwrap();
    let (through, _, end, _) = end_line(&events, NODE);
    assert_eq!(end, "matched", "the visit ends with the stored event");
    assert_eq!(
        through,
        Some(2),
        "the visit ends with the event stored after the progress line"
    );
}

/// The park barrier of the tests runs on every inbox park: an inbox wait
/// over an empty inbox parks without a progress line, and the barrier runs
/// there too.
#[test]
fn the_park_barrier_fires_on_an_inbox_park_without_a_progress_line() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    apb_engine::deadline_hooks::set(
        apb_engine::deadline_hooks::Hook::InboxWaitBeforePark,
        env.root.path(),
        move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        },
    );
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    apb_engine::deadline_hooks::clear(env.root.path());
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the barrier ran on the inbox park"
    );
    // The park wrote no progress line: the visit examined nothing, so the
    // start line is the only read line.
    let run_dir = env.latest_run();
    let events = read_all(&run_dir).unwrap();
    assert_eq!(
        wait_lines(&events, NODE).len(),
        1,
        "{:?}",
        wait_lines(&events, NODE)
    );
}

/// An event of a listed type that matches no condition wakes the parked
/// visit once: it writes its progress line and parks again (the churn the
/// docs state), with no end for the node.
#[test]
fn a_parked_wait_over_unmatched_events_stays_parked() {
    let env = Env::new();
    // The origin key exists before the run is stamped: the waker's engine
    // resume verifies it (the stable config dir the env lock holds).
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = park_yaml(30, "order", "{{params.customer}}");
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let first = run_parks(&read_all(&run_dir).unwrap())[0].0;
    // The event passes the types but fails the condition: the waker wakes
    // the run, the visit examines and parks again.
    store_typed(
        Some("order"),
        serde_json::json!({ "from": "not-the-customer" }),
    );
    let woke = waker_outcome(env.root.path(), &run_id, first);
    let woke = woke.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert!(matches!(woke, WakeOutcome::Resumed { .. }), "{woke:?}");
    let resumed = apb_engine::scheduler::resume_pause(env.root.path(), &run_id, first, None)
        .expect("the resumed run drives");
    assert_eq!(resumed.outcome, RunStatus::Paused, "the visit parks again");
    let events = read_all(&run_dir).unwrap();
    let parks = run_parks(&events);
    assert_eq!(parks.len(), 2, "a second park: {parks:?}");
    assert_eq!(parks[1].1, "wait (wait)", "the same reason form");
    // The visit left its progress line and ended nothing.
    // A progress line carries a visit's examined count; the start line's
    // is 0.
    let progress = events
        .iter()
        .filter(|e| {
            matches!(
                &e.payload,
                EventPayload::InboxWaitRead { node, end: None, examined: 1.., .. } if node == NODE
            )
        })
        .count();
    assert_eq!(progress, 1, "one progress line: the examined event");
    assert!(end_events(&events, NODE).is_empty(), "no end");
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::NodeFinished { node, .. } if node == NODE)),
        "no node end"
    );
}

/// The waker does not resume a parked run for a delivery its account may
/// not answer: with the account changed under the parked run the check
/// refuses, the run stays parked until its `until`, and the resumed drive
/// then fails the node `inbox_wait_unauthorized`.
#[test]
fn the_waker_does_not_resume_for_an_unauthorized_account() {
    let env = Env::new();
    // The origin key exists before the run is stamped: the waker's engine
    // resume verifies it (the stable config dir the env lock holds).
    apb_core::run_origin::ensure_key().unwrap();
    let yaml = plain_yaml(6);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let _poll = EnvVar::set("APB_WAKER_POLL_MS", "50");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let run_id = env.run_id_of(&run_dir);
    let parks = run_parks(&read_all(&run_dir).unwrap());
    let (seq, _, _, until, _) = parks[0].clone();
    let until = until.unwrap();
    // The account is edited while the run is parked: its live digest no
    // longer matches the manifest's.
    env.change_account();
    // A delivery arrives; the check refuses to read for it.
    store(serde_json::json!({ "from": "x" }));
    let woke = waker_outcome(env.root.path(), &run_id, seq);
    let woke = woke.recv_timeout(POLL_DEADLINE).unwrap().unwrap();
    assert!(
        matches!(woke, WakeOutcome::Resumed { .. }),
        "the waker still wakes the run at its `until`: {woke:?}"
    );
    assert!(
        apb_core::clock::now_ms() >= u128::from(until).saturating_sub(200),
        "the run stayed parked until its `until`, not for the delivery"
    );
    // The resumed drive authorizes first: the node fails, nothing is
    // journaled for the wait.
    let resumed = apb_engine::scheduler::resume_pause(env.root.path(), &run_id, seq, None)
        .expect("the resumed run drives");
    assert_eq!(resumed.outcome, RunStatus::Failed);
    let events = read_all(&run_dir).unwrap();
    let (_, output) = node_finished(&events, NODE).expect("the node failed");
    assert!(
        output.starts_with("inbox_wait_unauthorized:"),
        "was: {output}"
    );
    assert!(end_events(&events, NODE).is_empty(), "no visit end");
    let lines = wait_lines(&events, NODE);
    assert_eq!(lines.len(), 1, "only the start line: {lines:?}");
}

/// The park is the wait going on: it counts as no intervention, and the
/// paused-interval clock keeps counting its time, so the deadline is the
/// plain timeout.
#[test]
fn an_inbox_park_is_no_intervention_and_not_paused_time() {
    let env = Env::new();
    let yaml = plain_yaml(30);
    env.seed(&yaml);
    let _release = EnvVar::set("APB_RELEASE_AFTER_MS", "300");
    let rx = env.spawn(&yaml);
    assert_eq!(wait_result(&rx).outcome, RunStatus::Paused);
    let run_dir = env.latest_run();
    let events = read_all(&run_dir).unwrap();

    // No intervention: the park is the engine's own way to wait, its
    // engine resume adds nothing, and no person or supervisor acted.
    let out = apb_engine::run_stats::interventions::of_run(&events);
    assert_eq!(out.operator_total(), Some(0), "{out:?}");
    assert_eq!(out.supervisor_run_total(), Some(0), "{out:?}");
    assert_eq!(out.supervisor_node_total(), 0, "{out:?}");
    assert_eq!(out.question_total(), 0, "{out:?}");
    assert_eq!(out.applied_total(), 0, "{out:?}");
    assert!(
        out.engine.is_empty(),
        "the park is no engine action either: {:?}",
        out.engine
    );
    assert_eq!(out.reviews.visits, 0, "{out:?}");

    // Not paused time: the park's time runs on the deadline clock, so the
    // park's `until` is the plain timeout and the wait's active time keeps
    // growing while parked.
    let parks = run_parks(&events);
    let (ws, _) = env.wait_deadline_ms(&run_dir, 30);
    assert_eq!(
        parks[0].3,
        Some(u64::try_from(ws + 30_000).unwrap()),
        "the deadline counts the park"
    );
    std::thread::sleep(Duration::from_millis(1_200));
    let now = apb_core::clock::now_ms();
    let active = apb_engine::pause::active_elapsed_ms(&read_all(&run_dir).unwrap(), ws, now, now);
    assert!(
        active > 1_200,
        "the park's time keeps counting: {active} ms after parking"
    );
}
