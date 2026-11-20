//! Connector doubles of an eval case (eval-doubles D1): every connector
//! call of a doubled run is answered from the case's recorded answers and
//! captured, none reaches a service. Built like `connector_e2e`: the
//! `mock-tracker` fixture (with a few functions added) installed into a
//! temp `APB_CONFIG_DIR`, a doubled snapshot into a run directory whose
//! `run.yaml` carries the doubles, then `connector::call::execute`.
//!
//! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` is process-wide.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use apb_core::connector::config::{self, account_digest};
use apb_core::connector::resolve::resolve_playbook;
use apb_core::eval_doubles::{Doubles, args_contain, args_hold};
use apb_core::schema::Playbook;
use apb_engine::connector::call::{AttemptClaim, CallContext, CallRequest, execute_with};
use apb_engine::connector::run::snapshot_connectors;
use apb_engine::deadline_hooks::{self, Hook};
use apb_engine::eval::checks::{CheckInput, CheckStatus, evaluate};
use apb_engine::eval::doubles::{ArgsKept, CALLS_FILE, CapturedCall, read_calls};
use apb_engine::event::{EventLog, EventPayload, read_all_lossy_tail};
use apb_engine::manifest::{self, RunExecutionManifest};
use apb_engine::run_config::{EvalRunSettings, RunConfig, write_run_config};
use serde_json::{Value, json};

use crate::common::{self, Reply, ScriptedHttpServer, spawn_script};

const CONNECTOR: &str = "mock-tracker";
const NODE: &str = "a";
const TOKEN_VAR: &str = "APB_DOUBLES_TOKEN";

/// The fixture's functions plus a function with its own `mock`, a
/// read-back listing and a `marker` send with a text mode.
const EXTRA_FUNCTIONS: &str = r#"  - name: whoami
    description: The account's own user
    read_only: true
    method: GET
    url: "{{account.base_url}}/me"
  - name: mocked
    description: Answers from its own mock
    mock: { status: 200, body: { from: mock } }
  - name: list_notes
    description: List notes
    read_only: true
    method: GET
    url: "{{account.base_url}}/notes"
    args_schema: { type: object, properties: { since: { type: string } } }
  - name: post_note
    description: Post a note
    method: POST
    url: "{{account.base_url}}/notes"
    body: "{{args}}"
    text_args: { title: literal, text: markdown_no_commands }
    args_schema: { type: object, properties: { title: { type: string }, text: { type: string } }, required: [text] }
    idempotency:
      mode: marker
      arg: text
      format: html_comment
      reconcile:
        function: list_notes
        since_arg: since
        items: ""
        text: text
        author: user
"#;

/// Restores one process-wide env var on drop.
struct VarGuard {
    var: &'static str,
    prior: Option<OsString>,
}

impl Drop for VarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }
}

fn set_var(var: &'static str, value: impl AsRef<std::ffi::OsStr>) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe { std::env::set_var(var, value) };
    VarGuard { var, prior }
}

/// How the account's token is sourced.
enum Token {
    /// A variable the project's `secrets.env` sets: every redactor can
    /// scrub it.
    Env,
    /// A command that leaves a marker file when it runs: no redactor runs
    /// it, so the storage redactor withholds.
    Command,
}

/// One doubled run under a fresh config dir and project.
struct Env {
    root: tempfile::TempDir,
    cfg: tempfile::TempDir,
    run: PathBuf,
    server: ScriptedHttpServer,
    claim: AttemptClaim,
    // Dropped last: the env var and the lock.
    _cfg_var: VarGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn connector_yaml() -> String {
    let base = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/connectors")
            .join(CONNECTOR)
            .join("connector.yaml"),
    )
    .unwrap();
    let base = base.replacen(
        "healthcheck: ping\n",
        "healthcheck: ping\nidentity:\n  function: whoami\n  path: login\n",
        1,
    );
    format!("{}\n{EXTRA_FUNCTIONS}", base.trim_end())
}

const PLAYBOOK: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - name: secret_note\n    type: text\n    secret: true\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, profile: x, connectors: [{ name: mock-tracker }] }\nedges: []\n";

impl Env {
    /// A doubled run with `doubles` (YAML), its snapshot taken by
    /// `snapshot_connectors(.., doubled)`, its token sourced as `token`.
    fn new(doubles: &str, token: Token) -> Env {
        Env::with(doubles, token, true)
    }

    fn with(doubles: &str, token: Token, doubled: bool) -> Env {
        let lock = common::env_lock();
        let cfg = tempfile::tempdir().unwrap();
        let cfg_var = set_var("APB_CONFIG_DIR", cfg.path());
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        let server = spawn_script(vec![Reply::Respond(200, "{}".into())]);
        let installed = cfg.path().join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("connector.yaml"), connector_yaml()).unwrap();
        let token_ref = match token {
            Token::Env => format!("{{{{env.{TOKEN_VAR}}}}}"),
            Token::Command => format!(
                "{{{{cmd:touch {}}}}}",
                root.path().join("helper-ran").display()
            ),
        };
        let accounts = config::global_config_path(CONNECTOR).unwrap();
        std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
        std::fs::write(
            &accounts,
            format!(
                "accounts:\n  - name: main\n    default: true\n    base_url: {}\n    token: \"{token_ref}\"\n",
                server.base_url
            ),
        )
        .unwrap();
        let secrets = root.path().join(".apb/secrets.env");
        std::fs::write(&secrets, format!("{TOKEN_VAR}=token-value-0123456789\n")).unwrap();

        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let out = resolve_playbook(root.path(), &pb).unwrap();
        let mut conns = BTreeMap::new();
        let mut accts = BTreeMap::new();
        for (name, r) in &out.connectors {
            conns.insert(name.clone(), r.loaded.digest.clone());
            for a in &r.accounts {
                accts.insert(format!("{name}/{}", a.name), account_digest(a));
            }
        }
        let (connectors, grants) =
            snapshot_connectors(root.path(), &run, &pb, &conns, &accts, doubled).unwrap();
        manifest::write(
            &run,
            &RunExecutionManifest {
                connectors,
                connector_grants: grants,
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::write(run.join("playbook.yaml"), PLAYBOOK).unwrap();
        let doubles: Doubles = yaml_serde::from_str(doubles).unwrap();
        write_run_config(
            &run,
            &RunConfig {
                params: BTreeMap::from([(
                    "secret_note".to_string(),
                    "param-secret-value-42".to_string(),
                )]),
                eval: doubled.then(|| EvalRunSettings {
                    doubles: Some(doubles),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let mut env = Env {
            root,
            cfg,
            run,
            server,
            claim: AttemptClaim::default(),
            _cfg_var: cfg_var,
            _lock: lock,
        };
        if doubled {
            env.journal(provenance(true));
            // As preparation does for a doubled run.
            apb_engine::eval::doubles::create_capture(&env.run).unwrap();
        }
        env.journal(EventPayload::NodeStarted {
            node: NODE.into(),
            attempt: 1,
        });
        let token = apb_core::connector::marker::new_nonce();
        env.journal(EventPayload::AttemptStarted {
            item: None,
            node: NODE.into(),
            attempt: 1,
            agent: "claude".into(),
            soul_delivery: None,
            skills_mode: None,
            pid: Some(std::process::id()),
            spawn_ms: None,
            model: None,
            workdir: None,
            transcript: None,
            token: Some(token.clone()),
            isolation: None,
            exec: None,
            cloud: None,
        });
        env.claim = AttemptClaim {
            item: None,
            attempt: Some(1),
            token: Some(token),
            host_task: None,
        };
        env
    }

    fn journal(&self, payload: EventPayload) {
        EventLog::open(&self.run)
            .or_else(|_| EventLog::create(&self.run))
            .unwrap()
            .append(payload)
            .unwrap();
    }

    fn call_full(
        &self,
        function: &str,
        args: Value,
        intent: Option<&str>,
        dry_run: bool,
    ) -> (Value, bool) {
        execute_with(
            CallRequest {
                run_dir: &self.run,
                root: self.root.path(),
                node_id: NODE,
                connector: CONNECTOR,
                function,
                account: None,
                args,
                dry_run,
                full: false,
            },
            CallContext::node(intent, self.claim.clone()),
        )
    }

    fn call(&self, function: &str, args: Value) -> (Value, bool) {
        self.call_full(function, args, None, false)
    }

    fn send(&self, function: &str, args: Value, intent: &str) -> (Value, bool) {
        self.call_full(function, args, Some(intent), false)
    }

    fn calls(&self) -> Vec<CapturedCall> {
        read_calls(&self.run).unwrap()
    }

    /// The journal's `connector_call` outcomes, in order.
    fn call_outcomes(&self) -> Vec<String> {
        read_all_lossy_tail(&self.run)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::ConnectorCall { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect()
    }

    /// The journal's send settlements, in order.
    fn settlements(&self) -> Vec<String> {
        read_all_lossy_tail(&self.run)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::SendIntentSettled { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect()
    }

    /// The connector check rows (`connector_doubles` and every
    /// `connector_calls[..]`) of a case with `doubles` (the YAML the run
    /// got) and `entries` (`checks.connector_calls` list lines), run by
    /// `checks::evaluate` over this run's journal and its real capture.
    fn check_rows(
        &self,
        doubles: &str,
        entries: &str,
    ) -> Vec<(String, CheckStatus, Option<String>)> {
        let indented: String = doubles.lines().map(|l| format!("  {l}\n")).collect();
        let case: apb_core::eval::EvalCase = yaml_serde::from_str(&format!(
            "schema: 1\nid: c\nfixture: {{ dir: fixtures/x }}\nconnectors:\n{indented}checks:\n  connector_calls:\n{entries}"
        ))
        .unwrap();
        let pb = Playbook::from_yaml(PLAYBOOK).unwrap();
        let events = read_all_lossy_tail(&self.run).unwrap();
        let raw_types = apb_engine::eval::checks::raw_event_types(&self.run);
        let calls = read_calls(&self.run);
        let input = CheckInput {
            case: &case,
            playbook: &pb,
            events: &events,
            raw_types: &raw_types,
            tree: self.root.path(),
            fixture_commit: "HEAD",
            suite_dir: self.root.path(),
            stopped: None,
            script_env: Vec::new(),
            hooks_dir: self.root.path(),
            calls: calls.as_deref(),
        };
        evaluate(&input)
            .0
            .into_iter()
            .filter(|c| c.kind.starts_with("connector_"))
            .map(|c| (c.kind, c.status, c.detail))
            .collect()
    }

    fn capture_text(&self) -> String {
        std::fs::read_to_string(self.run.join(CALLS_FILE)).unwrap_or_default()
    }

    fn assert_nothing_sent(&self) {
        assert!(
            self.server.requests().is_empty(),
            "a request reached the service: {:?}",
            self.server.requests()
        );
        assert!(
            !self.root.path().join("helper-ran").exists(),
            "the account's secret helper ran"
        );
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        deadline_hooks::clear(&self.run);
        // An unwritable capture must not outlive the test's temp dir.
        set_mode(&self.run.join(CALLS_FILE), 0o600);
    }
}

fn provenance(eval_doubles: bool) -> EventPayload {
    serde_json::from_value(json!({ "type": "run_provenance", "eval_doubles": eval_doubles }))
        .unwrap()
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

fn row<'a>(
    rows: &'a [(String, CheckStatus, Option<String>)],
    kind: &str,
) -> (CheckStatus, Option<&'a str>) {
    let r = rows
        .iter()
        .find(|r| r.0 == kind)
        .unwrap_or_else(|| panic!("{kind} in {rows:?}"));
    (r.1, r.2.as_deref())
}

const LIST_TWICE: &str = "mock-tracker:\n  list_items:\n    - answer: { status: 200, body: { items: [1] } }\n      times: 1\n    - answer: { status: 200, body: { items: [2] } }\n";

#[test]
fn a_doubled_call_is_answered_and_captured() {
    let env = Env::new(LIST_TWICE, Token::Env);
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    assert_eq!(v["status"], 200, "{v}");
    assert_eq!(v["body"]["items"], json!([1]), "{v}");
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    assert_eq!(v["body"]["items"], json!([2]), "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|c| c.delivered && c.site == NODE));
    assert_eq!(calls[0].answer, Some(0));
    assert_eq!(calls[1].answer, Some(1));
    assert_eq!(calls[0].args, json!({ "q": "bug" }));
    assert_eq!(calls[0].args_kept, ArgsKept::Whole);
    assert_eq!(calls[0].account, "main");
    assert!(!calls[0].call_key.is_empty());
    assert_eq!(env.call_outcomes(), ["ok", "ok"]);
    env.assert_nothing_sent();
}

#[test]
fn an_unmatched_call_fails_closed_without_a_socket() {
    let env = Env::new(
        "mock-tracker:\n  list_items:\n    - match: { args: { q: only } }\n      answer: { status: 200, body: {} }\n",
        Token::Env,
    );
    let (v, ok) = env.call("list_items", json!({ "q": "other" }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "double_unmatched", "{v}");
    assert_eq!(env.call_outcomes(), ["double_unmatched"]);
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].answer, None);
    assert!(calls[0].delivered);
    env.assert_nothing_sent();
}

#[test]
fn a_doubled_run_resolves_no_secret() {
    let env = Env::new(LIST_TWICE, Token::Command);
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    env.assert_nothing_sent();
    // The storage redactor cannot account for the command-sourced secret.
    let calls = env.calls();
    assert_eq!(calls[0].args_kept, ArgsKept::Withheld);
    assert_eq!(calls[0].args, json!({ "withheld": "secrets_unresolved" }));
}

#[test]
fn captured_args_go_through_the_run_redactor() {
    let env = Env::new(LIST_TWICE, Token::Env);
    let (v, ok) = env.call(
        "list_items",
        json!({ "q": "param-secret-value-42 and token-value-0123456789" }),
    );
    assert!(ok, "{v}");
    let text = env.capture_text();
    assert!(!text.contains("param-secret-value-42"), "{text}");
    assert!(!text.contains("token-value-0123456789"), "{text}");
    assert_eq!(env.calls()[0].args_kept, ArgsKept::Whole);
}

#[test]
fn a_captured_item_id_goes_through_the_run_redactor() {
    let mut env = Env::new(LIST_TWICE, Token::Env);
    let item = "item-token-value-0123456789";
    let token = apb_core::connector::marker::new_nonce();
    env.journal(EventPayload::AttemptStarted {
        item: Some(item.into()),
        node: NODE.into(),
        attempt: 1,
        agent: "claude".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(std::process::id()),
        spawn_ms: None,
        model: None,
        workdir: None,
        transcript: None,
        token: Some(token.clone()),
        isolation: None,
        exec: None,
        cloud: None,
    });
    env.claim = AttemptClaim {
        item: Some(item.into()),
        attempt: Some(1),
        token: Some(token),
        host_task: None,
    };
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    let text = env.capture_text();
    assert!(!text.contains("token-value-0123456789"), "{text}");
    let calls = env.calls();
    let kept = calls[0].item.as_deref().expect("the item is kept");
    assert!(kept.starts_with("item-"), "{kept}");
}

#[test]
fn a_command_sourced_secret_withholds_captured_args() {
    let env = Env::new(LIST_TWICE, Token::Command);
    let (v, ok) = env.call("list_items", json!({ "q": "visible text" }));
    assert!(ok, "{v}");
    let calls = env.calls();
    assert_eq!(calls[0].args_kept, ArgsKept::Withheld);
    assert!(!env.capture_text().contains("visible text"));
    // An args assertion over it cannot decide: the args are not whole.
    assert!(!args_contain(&calls[0].args, "visible text"));
    env.assert_nothing_sent();
}

/// Panics the call at `hook` the first time it runs for this run.
fn crash_once(env: &Env, hook: Hook) {
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    deadline_hooks::set(hook, &env.run, move |_| {
        if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            panic!("the call process died here");
        }
    });
}

fn crashes(f: impl FnOnce() -> (Value, bool)) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err()
}

#[test]
fn a_crash_between_reservation_and_delivery_replays_the_answer() {
    let env = Env::new(LIST_TWICE, Token::Env);
    crash_once(&env, Hook::DoubleBeforeDeliver);
    assert!(crashes(|| env.call("list_items", json!({ "q": "bug" }))));
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].delivered);
    // Before the retry the delivery is truly missing: the checks fail.
    let rows = env.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 1 }\n",
    );
    assert_eq!(row(&rows, "connector_doubles").0, CheckStatus::Failed);
    // The retry of the same call gets the same answer, no `times` used.
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    assert_eq!(v["body"]["items"], json!([1]), "{v}");
    // The next call gets the next answer.
    let (v, _) = env.call("list_items", json!({ "q": "bug" }));
    assert_eq!(v["body"]["items"], json!([2]), "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 2, "one reservation per call: {calls:?}");
    assert!(calls.iter().all(|c| c.delivered));
    assert_eq!(
        calls.iter().filter(|c| c.answer == Some(0)).count(),
        1,
        "the crashed call counts once"
    );
    // Three calls journaled, two reservations and one replay captured: the
    // recovered run passes its checks.
    assert_eq!(env.call_outcomes().len(), 3);
    assert_eq!(calls[0].replays, 1);
    let rows = env.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 2 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles").0,
        CheckStatus::Passed,
        "{rows:?}"
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]").0,
        CheckStatus::Passed,
        "{rows:?}"
    );
}

#[test]
fn an_identical_live_call_is_not_replayed() {
    let env = Env::new(LIST_TWICE, Token::Env);
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(Some(release_rx));
    deadline_hooks::set(Hook::DoubleBeforeDeliver, &env.run, move |_| {
        // Taken in its own statement: the guard is dropped before the wait.
        let rx = release_rx.lock().unwrap().take();
        if let Some(rx) = rx {
            arrived_tx.send(()).unwrap();
            // A dropped sender (the test body failed) releases it too.
            let _ = rx.recv();
        }
    });
    std::thread::scope(|s| {
        // Owned by the scope's body: dropped on every exit, a failed
        // assertion included, so the waiting first call is released and
        // the failure is reported instead of a hang.
        let release_tx = release_tx;
        let first = s.spawn(|| env.call("list_items", json!({ "q": "bug" })));
        arrived_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first call reached its delivery");
        // The first call's owner is alive: this one reserves its own.
        let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
        assert!(ok, "{v}");
        assert_eq!(v["body"]["items"], json!([2]), "{v}");
        release_tx.send(()).unwrap();
        drop(release_tx);
        let (v, ok) = first.join().unwrap();
        assert!(ok, "{v}");
        assert_eq!(v["body"]["items"], json!([1]), "{v}");
    });
    let calls = env.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.delivered));
}

#[test]
fn a_doubled_call_without_a_key_is_refused() {
    let env = Env::new(LIST_TWICE, Token::Env);
    // A key file that cannot be read or written.
    let key = env.cfg.path().join("run-origin.key");
    let _ = std::fs::remove_file(&key);
    std::fs::create_dir_all(key.join("blocked")).unwrap();
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("eval_call_key_unavailable"),
        "{v}"
    );
    assert!(env.calls().is_empty(), "no capture line");
    std::fs::remove_dir_all(&key).unwrap();
    // No `times` was used: the first answer is still there.
    let (v, _) = env.call("list_items", json!({ "q": "bug" }));
    assert_eq!(v["body"]["items"], json!([1]), "{v}");
}

/// Whether a file or directory mode stops this process: false for a user
/// that writes any file or lists any directory (root, `CAP_DAC_OVERRIDE`,
/// `CAP_DAC_READ_SEARCH`), where the fault tests below that
/// inject an I/O error through a mode cannot run and are skipped.
fn modes_bind() -> bool {
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("ro");
    std::fs::write(&f, b"x").unwrap();
    set_mode(&f, 0o400);
    let file_binds = std::fs::OpenOptions::new().append(true).open(&f).is_err();
    // A directory of mode 0300 must not list either: `CAP_DAC_READ_SEARCH`
    // reads it while it still cannot write the file above.
    let d = t.path().join("wx");
    std::fs::create_dir(&d).unwrap();
    set_mode(&d, 0o300);
    let dir_binds = std::fs::read_dir(&d).is_err();
    set_mode(&d, 0o700);
    let binds = file_binds && dir_binds;
    if !binds {
        eprintln!(
            "skipped: this process writes a file of mode 0400 or lists a directory of mode 0300"
        );
    }
    binds
}

/// Makes the capture unwritable at `hook` the first time it runs.
fn unwritable_once(env: &Env, hook: Hook) {
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let file = env.run.join(CALLS_FILE);
    deadline_hooks::set(hook, &env.run, move |_| {
        if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            set_mode(&file, 0o400);
        }
    });
}

#[test]
fn an_unrecorded_delivery_returns_no_answer() {
    if !modes_bind() {
        return;
    }
    let env = Env::new(LIST_TWICE, Token::Env);
    unwritable_once(&env, Hook::DoubleBeforeDeliver);
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("eval_call_delivery_unrecorded"),
        "{v}"
    );
    assert!(v.get("body").is_none(), "no answer handed out: {v}");
    assert!(!env.calls()[0].delivered);
    set_mode(&env.run.join(CALLS_FILE), 0o600);
    // The retry of the same call gets the same answer, no `times` used.
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    assert_eq!(v["body"]["items"], json!([1]), "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].delivered);
    // Two calls journaled, one reservation and its replay captured.
    assert_eq!(env.call_outcomes().len(), 2);
    let rows = env.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 1 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles").0,
        CheckStatus::Passed,
        "{rows:?}"
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]").0,
        CheckStatus::Passed,
        "{rows:?}"
    );
}

const SEND: &str =
    "mock-tracker:\n  notify_person:\n    - answer: { status: 200, body: { id: m1 } }\n";

#[test]
fn an_undelivered_send_fails_the_repetition() {
    if !modes_bind() {
        return;
    }
    let env = Env::new(SEND, Token::Env);
    unwritable_once(&env, Hook::DoubleBeforeDeliver);
    let args = json!({ "to": "ann", "text": "hello" });
    let (v, _) = env.send("notify_person", args.clone(), "greet");
    assert_eq!(v["error"]["code"], "config", "{v}");
    assert_eq!(env.settlements(), ["confirmed"]);
    set_mode(&env.run.join(CALLS_FILE), 0o600);
    // The ledger answers the repeat; it never reaches the answer step.
    let (v, ok) = env.send("notify_person", args, "greet");
    assert!(ok, "{v}");
    assert_eq!(v["already_sent"], true, "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        !calls[0].delivered,
        "the reservation stays undelivered, never read as no call"
    );
    // The checks over the capture: the repetition fails, and a call count
    // over the undelivered send is indeterminate.
    let rows = env.check_rows(
        SEND,
        "    - { function: mock-tracker.notify_person, count: 1 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles"),
        (
            CheckStatus::Failed,
            Some("1 call(s) reserved and never delivered: mock-tracker.notify_person at a")
        )
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.notify_person]"),
        (
            CheckStatus::Failed,
            Some(
                "indeterminate: 1 call(s) of mock-tracker.notify_person reserved and never delivered"
            )
        )
    );
}

#[test]
fn a_crash_after_dispatch_leaves_a_reservation_the_checks_fail_on() {
    let env = Env::new(LIST_TWICE, Token::Env);
    crash_once(&env, Hook::DoubleBeforeDispatch);
    assert!(crashes(|| env.call("list_items", json!({ "q": "bug" }))));
    // Read through a fresh handle: the reservation was written first.
    let calls = read_calls(&env.run).unwrap();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].delivered);
    assert!(env.call_outcomes().is_empty(), "nothing journaled");
    // The checks name it, and `count: 0` never reads it as no call.
    let rows = env.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 0 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles"),
        (
            CheckStatus::Failed,
            Some("1 call(s) reserved and never delivered: mock-tracker.list_items at a")
        )
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]"),
        (
            CheckStatus::Failed,
            Some(
                "indeterminate: 1 call(s) of mock-tracker.list_items reserved and never delivered"
            )
        )
    );
}

/// Before a reservation's call is dispatched, the run directory is synced
/// (once per process), a replayed reservation's included. A directory that
/// cannot be synced refuses the call: nothing is dispatched or journaled,
/// so a power loss can never take a dispatched call's capture with it.
#[test]
fn the_capture_directory_is_synced_before_the_first_dispatch() {
    if !modes_bind() {
        return;
    }
    let env = Env::new(LIST_TWICE, Token::Env);
    let synced_before_dispatch = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = std::sync::Arc::clone(&synced_before_dispatch);
    deadline_hooks::set(Hook::DoubleBeforeDispatch, &env.run, move |_| {
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let syncs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&syncs);
    let dir = env.run.clone();
    // No read permission on the run directory: it cannot be opened to sync.
    deadline_hooks::set(Hook::DoubleBeforeCaptureDirSync, &env.run, move |_| {
        if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            set_mode(&dir, 0o300);
        }
    });
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    set_mode(&env.run, 0o700);
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("could not record this reservation"),
        "{v}"
    );
    assert!(
        !synced_before_dispatch.load(std::sync::atomic::Ordering::SeqCst),
        "the call was dispatched before its capture's directory was synced"
    );
    assert!(env.call_outcomes().is_empty(), "nothing journaled");
    // The same call replays the reservation the refused call left, and the
    // directory is synced again before that replay is dispatched.
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    assert_eq!(v["body"]["items"], json!([1]), "{v}");
    assert_eq!(syncs.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(synced_before_dispatch.load(std::sync::atomic::Ordering::SeqCst));
    let calls = env.calls();
    assert_eq!(
        calls.len(),
        1,
        "the replay wrote no second reservation: {calls:?}"
    );
    assert_eq!(calls[0].seq, 0);
    assert!(calls[0].delivered);
    env.assert_nothing_sent();
}

/// A capture removed after a dispatched call never reads as "no call": the
/// run journal's `connector_call` says a doubled call was made.
#[test]
fn a_deleted_capture_fails_every_connector_check() {
    let env = Env::new(LIST_TWICE, Token::Env);
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(ok, "{v}");
    std::fs::remove_file(env.run.join(CALLS_FILE)).unwrap();
    assert_missing(&env);
}

/// The capture file exists from the run's start: a capture removed with
/// nothing in the journal (no call journaled, or its journal line gone
/// too) still fails every connector check, `count: 0` included.
#[test]
fn a_removed_capture_fails_the_checks_with_an_empty_journal() {
    let env = Env::new(LIST_TWICE, Token::Env);
    assert!(env.call_outcomes().is_empty());
    std::fs::remove_file(env.run.join(CALLS_FILE)).unwrap();
    assert_missing(&env);
    // A doubled call refuses to answer without its capture.
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
}

fn assert_missing(env: &Env) {
    let rows = env.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 0 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles"),
        (
            CheckStatus::Failed,
            Some(apb_engine::eval::doubles::CAPTURE_MISSING)
        )
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]"),
        (
            CheckStatus::Failed,
            Some("indeterminate: the call capture does not read")
        )
    );
}

/// A doubled call the run journal cannot record is refused before its
/// answer is handed out: the reservation stays undelivered and the checks
/// fail on it.
#[test]
fn a_doubled_call_the_journal_cannot_record_is_refused() {
    if !modes_bind() {
        return;
    }
    let env = Env::new(LIST_TWICE, Token::Env);
    let journal = env.run.join("events.jsonl");
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let path = journal.clone();
    deadline_hooks::set(Hook::DoubleBeforeDispatch, &env.run, move |_| {
        if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
            set_mode(&path, 0o400);
        }
    });
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    set_mode(&journal, 0o600);
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("eval_call_unjournaled"),
        "{v}"
    );
    assert!(v.get("body").is_none(), "no answer handed out: {v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].delivered, "nothing delivered");
    assert!(env.call_outcomes().is_empty(), "nothing journaled");
    let rows = env.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 0 }\n",
    );
    assert_eq!(row(&rows, "connector_doubles").0, CheckStatus::Failed);
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]").0,
        CheckStatus::Failed
    );
}

/// A capture cut short never passes: one line less leaves a reservation
/// never delivered, one whole call less is `capture_incomplete`.
#[test]
fn a_truncated_capture_fails_every_connector_check() {
    let env = Env::new(LIST_TWICE, Token::Env);
    for _ in 0..2 {
        let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
        assert!(ok, "{v}");
    }
    let whole = env.capture_text();
    let lines: Vec<&str> = whole.lines().collect();
    assert_eq!(lines.len(), 4, "{whole}");
    let entry = "    - { function: mock-tracker.list_items, count: 2 }\n";
    let keep = |n: usize| {
        let text: String = lines[..n].iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(env.run.join(CALLS_FILE), text).unwrap();
        env.check_rows(LIST_TWICE, entry)
    };
    // One line less: the second reservation was never delivered.
    let rows = keep(3);
    assert_eq!(row(&rows, "connector_doubles").0, CheckStatus::Failed);
    let r = row(&rows, "connector_calls[mock-tracker.list_items]");
    assert_eq!(r.0, CheckStatus::Failed);
    assert!(r.1.unwrap().starts_with("indeterminate:"), "{rows:?}");
    // One call less: the journal holds a call the capture does not.
    let rows = keep(2);
    let why = "capture_incomplete: the call capture holds fewer calls than the run journal records: mock-tracker.list_items (2 journaled, 1 captured)";
    assert_eq!(
        row(&rows, "connector_doubles"),
        (CheckStatus::Failed, Some(why))
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]"),
        (
            CheckStatus::Failed,
            Some(format!("indeterminate: {why}").as_str())
        )
    );
    // When journal and capture agree there was none, `count: 0` passes
    // (one Env at a time: each holds the process-wide env lock).
    drop(env);
    let none = Env::new(LIST_TWICE, Token::Env);
    let rows = none.check_rows(
        LIST_TWICE,
        "    - { function: mock-tracker.list_items, count: 0 }\n",
    );
    assert_eq!(row(&rows, "connector_doubles").0, CheckStatus::Passed);
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]").0,
        CheckStatus::Passed
    );
}

#[test]
fn the_capture_and_the_journal_are_read_by_their_own_readers() {
    if !modes_bind() {
        return;
    }
    let env = Env::new(
        "mock-tracker:\n  notify_person:\n    - answer: { status: 200, body: { id: m1 } }\n  list_items:\n    - answer: { status: 200, body: { items: [] } }\n",
        Token::Env,
    );
    // A tracked send the journal settled whose delivery was not recorded.
    unwritable_once(&env, Hook::DoubleBeforeDeliver);
    let args = json!({ "to": "ann", "text": "hello" });
    env.send("notify_person", args.clone(), "greet");
    set_mode(&env.run.join(CALLS_FILE), 0o600);
    // A delivered call the journal does not hold (its append was lost).
    let mut text = env.capture_text();
    let seq = text.lines().count();
    let mut reserved: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    reserved["seq"] = json!(seq);
    reserved["function"] = json!("list_items");
    text.push_str(&format!(
        "{}\n{}\n",
        reserved,
        json!({ "kind": "delivered", "reservation": seq })
    ));
    std::fs::write(env.run.join(CALLS_FILE), text).unwrap();
    // The capture's reader: one delivered call, one undelivered.
    let calls = env.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(!calls[0].delivered && calls[0].function == "notify_person");
    assert!(calls[1].delivered && calls[1].function == "list_items");
    // The journal's readers: the send is sent, a repeat already_sent, and
    // the journal holds only the call it recorded.
    let (v, _) = env.send("notify_person", args, "greet");
    assert_eq!(v["already_sent"], true, "{v}");
    assert_eq!(env.call_outcomes().len(), 1);
    // The checks read the capture alone: the delivered listing counts, the
    // undelivered send fails the repetition.
    let rows = env.check_rows(
        "mock-tracker:\n  notify_person:\n    - answer: { status: 200, body: { id: m1 } }\n  list_items:\n    - answer: { status: 200, body: { items: [] } }\n",
        "    - { function: mock-tracker.list_items, count: 1 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles"),
        (
            CheckStatus::Failed,
            Some("1 call(s) reserved and never delivered: mock-tracker.notify_person at a")
        )
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.list_items]").0,
        CheckStatus::Passed
    );
    // The run outcome reads the journal alone: only the call it recorded.
    let events = read_all_lossy_tail(&env.run).unwrap();
    let outcome_calls: Vec<String> =
        apb_engine::run_outcome::calls::classified_calls(&env.run, &events)
            .iter()
            .map(|c| c.target())
            .collect();
    assert_eq!(outcome_calls, ["mock-tracker.notify_person"]);
}

#[test]
fn connector_calls_args_are_the_input_args() {
    let env = Env::new(
        "mock-tracker:\n  post_note:\n    - answer: { status: 200, body: { id: n1 } }\n",
        Token::Env,
    );
    let forged = "<!-- apb-intent nonce=aaaa -->";
    let title = "Hello *world*";
    let text = format!("/deploy now {forged}");
    let input = json!({ "title": title, "text": text });
    let (v, ok) = env.send("post_note", input.clone(), "note");
    assert!(ok, "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    let args = &calls[0].args;
    assert_eq!(calls[0].args_kept, ArgsKept::Whole);
    // The input text, before the literal mode escaped it and before a
    // marker was appended; the forged marker as the caller wrote it.
    assert_eq!(args, &input);
    assert!(args_hold(&json!({ "title": title }), args), "{args}");
    assert!(args_contain(args, "*world*"), "{args}");
    assert!(args_contain(args, "/deploy now"), "{args}");
    assert!(args_contain(args, forged), "{args}");
    assert!(!args_contain(args, "apb-quoted-intent"), "{args}");
    assert!(!args_contain(args, "\\*"), "{args}");
    // Exactly the input: no marker was appended to `text`.
    assert_eq!(args["text"], json!(text));
    // The same through the checks over the capture.
    let rows = env.check_rows(
        "mock-tracker:\n  post_note:\n    - answer: { status: 200, body: { id: n1 } }\n",
        &format!(
            "    - {{ function: mock-tracker.post_note, args: {{ title: {title:?} }}, count: 1 }}\n    - {{ function: mock-tracker.post_note, contains: {forged:?}, count: 1 }}\n    - {{ function: mock-tracker.post_note, contains: apb-quoted-intent, count: 0 }}\n    - {{ function: mock-tracker.post_note, contains: '\\*', count: 0 }}\n"
        ),
    );
    assert_eq!(
        row(&rows, "connector_doubles").0,
        CheckStatus::Passed,
        "{rows:?}"
    );
    let statuses: Vec<CheckStatus> = rows
        .iter()
        .filter(|r| r.0 == "connector_calls[mock-tracker.post_note]")
        .map(|r| r.1)
        .collect();
    assert_eq!(statuses, [CheckStatus::Passed; 4], "{rows:?}");
}

/// A marker send whose result is unknown is read back through the doubled
/// identity and listing on its repeat: both are answered by the case, so
/// nothing is unmatched and the reconcile reads the listing, not "own
/// identity could not be read".
#[test]
fn a_doubled_marker_reconcile_reads_the_doubled_identity_and_listing() {
    let doubles = "mock-tracker:\n  post_note:\n    - answer: { status: 500, body: { error: boom } }\n  whoami:\n    - answer: { status: 200, body: { login: me } }\n  list_notes:\n    - answer: { status: 200, body: [] }\n";
    let env = Env::new(doubles, Token::Env);
    let args = json!({ "title": "t", "text": "hello" });
    let (v, ok) = env.send("post_note", args.clone(), "note");
    assert!(!ok, "{v}");
    assert_eq!(env.settlements(), ["unknown"]);
    let (v, ok) = env.send("post_note", args, "note");
    assert!(!ok, "the read-back found nothing: {v}");
    let text = v.to_string();
    assert!(!text.contains("own identity could not be read"), "{v}");
    let calls = env.calls();
    let functions: Vec<&str> = calls.iter().map(|c| c.function.as_str()).collect();
    assert_eq!(
        functions,
        ["post_note", "whoami", "list_notes"],
        "{calls:?}"
    );
    assert!(
        calls.iter().all(|c| c.delivered && c.answer.is_some()),
        "{calls:?}"
    );
    let rows = env.check_rows(
        doubles,
        "    - { function: mock-tracker.whoami, count: 1 }\n",
    );
    assert_eq!(
        row(&rows, "connector_doubles").0,
        CheckStatus::Passed,
        "{rows:?}"
    );
    assert_eq!(
        row(&rows, "connector_calls[mock-tracker.whoami]").0,
        CheckStatus::Passed
    );
    env.assert_nothing_sent();
}

#[test]
fn a_repeated_doubled_send_is_answered_already_sent() {
    let env = Env::new(SEND, Token::Env);
    let args = json!({ "to": "ann", "text": "hello" });
    let (v, ok) = env.send("notify_person", args.clone(), "greet");
    assert!(ok, "{v}");
    let (v, ok) = env.send("notify_person", args, "greet");
    assert!(ok, "{v}");
    assert_eq!(v["already_sent"], true, "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 1, "one captured call: {calls:?}");
    assert!(calls[0].delivered);
    env.assert_nothing_sent();
}

#[test]
fn a_double_answering_500_on_a_send_leaves_it_unknown() {
    let env = Env::new(
        "mock-tracker:\n  notify_person:\n    - answer: { status: 500, body: { error: boom } }\n",
        Token::Env,
    );
    let (v, ok) = env.send("notify_person", json!({ "to": "ann", "text": "hi" }), "g");
    assert!(!ok, "{v}");
    assert_eq!(env.settlements(), ["unknown"]);
    assert_eq!(env.calls().len(), 1);
    env.assert_nothing_sent();
}

#[test]
fn a_doubled_call_with_invalid_args_is_refused_as_a_real_one() {
    let env = Env::new(LIST_TWICE, Token::Env);
    let (v, ok) = env.call("list_items", json!({ "nope": 1 }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "invalid_args", "{v}");
    assert!(env.calls().is_empty(), "no answer recorded");
    // No `times` was used.
    let (v, _) = env.call("list_items", json!({ "q": "bug" }));
    assert_eq!(v["body"]["items"], json!([1]), "{v}");
}

#[test]
fn a_dry_run_of_a_doubled_run_stays_a_dry_run() {
    let env = Env::new(LIST_TWICE, Token::Env);
    let (v, ok) = env.call_full("list_items", json!({ "q": "bug" }), None, true);
    assert!(ok, "the preview is answered: {v}");
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["doubled"], true, "{v}");
    assert!(env.calls().is_empty(), "nothing reserved: {v}");
    assert!(env.call_outcomes().is_empty(), "nothing journaled: {v}");
    let (v, _) = env.call("list_items", json!({ "q": "bug" }));
    assert_eq!(v["body"]["items"], json!([1]), "no `times` used: {v}");
    env.assert_nothing_sent();
}

#[test]
fn an_unreadable_run_config_refuses_the_call() {
    let env = Env::new(LIST_TWICE, Token::Env);
    std::fs::write(env.run.join("run.yaml"), "eval: { doubles: { mock").unwrap();
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
    assert!(env.calls().is_empty());
    env.assert_nothing_sent();
}

#[test]
fn a_doubled_inbox_read_returns_the_recorded_envelope() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    let yaml = "name: echo-hooks\nversion: 0.1.0\nwebhook:\n  signature:\n    scheme: hmac_sha256_hex\n    header: X-Hub-Signature-256\n    prefix: \"sha256=\"\n    secret: \"{{secret.app_secret}}\"\n  dedupe_path: id\naccount_fields:\n  - name: app_secret\n    required: true\n    secret: true\nfunctions:\n  - name: inbox_read\n    description: Read pending inbound events\n    read_only: true\n    args_schema:\n      type: object\n      properties:\n        consumer: { type: string }\n        limit: { type: integer }\n    inbox:\n      op: read\n";
    let mut m = RunExecutionManifest::default();
    m.connectors.push(apb_engine::manifest::ManifestConnector {
        name: "echo-hooks".into(),
        digest: "sha256:test".into(),
        accounts: Vec::new(),
        doubled_accounts: vec![apb_engine::manifest::DoubledAccount {
            name: "main".into(),
            default: true,
        }],
    });
    m.connector_grants.insert(
        NODE.into(),
        vec![apb_engine::manifest::ManifestConnectorGrant {
            connector: "echo-hooks".into(),
            accounts: vec!["main".into()],
            functions: vec!["inbox_read".into()],
            max_calls: None,
            on_unknown: None,
        }],
    );
    manifest::write(&run, &m).unwrap();
    std::fs::create_dir_all(run.join("connectors")).unwrap();
    std::fs::write(run.join("connectors/echo-hooks.yaml"), yaml).unwrap();
    let envelope = json!({ "events": [{ "seq": 1, "body": { "n": 1 } }], "cursor": 0 });
    let doubles: Doubles = serde_json::from_value(json!({
        "echo-hooks": { "inbox_read": [{ "answer": { "status": 200, "body": envelope } }] }
    }))
    .unwrap();
    write_run_config(
        &run,
        &RunConfig {
            eval: Some(EvalRunSettings {
                doubles: Some(doubles),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();
    // As preparation leaves a doubled run: a journal and an empty capture.
    EventLog::create(&run).unwrap();
    apb_engine::eval::doubles::create_capture(&run).unwrap();
    let (v, ok) = execute_with(
        CallRequest {
            run_dir: &run,
            root: root.path(),
            node_id: NODE,
            connector: "echo-hooks",
            function: "inbox_read",
            account: None,
            args: json!({ "consumer": "worker" }),
            dry_run: false,
            full: false,
        },
        CallContext::default(),
    );
    assert!(ok, "{v}");
    assert_eq!(v["body"], envelope, "{v}");
    let calls = read_calls(&run).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function, "inbox_read");
    // An invalid read is refused as a real one, before the double.
    let (v, ok) = execute_with(
        CallRequest {
            run_dir: &run,
            root: root.path(),
            node_id: NODE,
            connector: "echo-hooks",
            function: "inbox_read",
            account: None,
            args: json!({ "limit": "many" }),
            dry_run: false,
            full: false,
        },
        CallContext::default(),
    );
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "invalid_args", "{v}");
    assert_eq!(read_calls(&run).unwrap().len(), 1);
}

/// What an apb that does not know doubles sees of a doubled run: no
/// doubles in `run.yaml`, no `eval_doubles` provenance, and no
/// `doubled_accounts` in the manifest. Its call fails on the account.
#[test]
fn an_apb_blind_to_doubles_cannot_call_from_a_doubled_snapshot() {
    let env = Env::new(LIST_TWICE, Token::Command);
    write_run_config(&env.run, &RunConfig::default()).unwrap();
    let mut m = manifest::read(&env.run).unwrap().unwrap();
    for c in &mut m.connectors {
        assert!(c.accounts.is_empty());
        c.doubled_accounts.clear();
    }
    std::fs::remove_file(env.run.join("manifest.yaml")).unwrap();
    manifest::write(&env.run, &m).unwrap();
    let events: Vec<String> = std::fs::read_to_string(env.run.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|l| !l.contains("run_provenance"))
        .map(str::to_string)
        .collect();
    std::fs::write(env.run.join("events.jsonl"), events.join("\n") + "\n").unwrap();
    let (v, ok) = env.call("list_items", json!({ "q": "bug" }));
    assert!(!ok, "{v}");
    assert_eq!(v["error"]["code"], "config", "{v}");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("`main`"), "{message}");
    env.assert_nothing_sent();
}

#[test]
fn a_function_mock_does_not_bypass_the_double() {
    let env = Env::new(
        "mock-tracker:\n  mocked:\n    - answer: { status: 200, body: { from: double } }\n      times: 1\n",
        Token::Env,
    );
    let (v, ok) = env.call("mocked", json!({}));
    assert!(ok, "{v}");
    assert_eq!(v["body"]["from"], "double", "{v}");
    let calls = env.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].answer, Some(0));
    // Its `times` is used up: the next call is unmatched, never the mock.
    let (v, _) = env.call("mocked", json!({}));
    assert_eq!(v["error"]["code"], "double_unmatched", "{v}");
}
