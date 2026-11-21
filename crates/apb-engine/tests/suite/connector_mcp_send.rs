//! Send once over MCP (design D3, D4 "The order of one call"): a write is
//! recorded in the send ledger only once its server started, listed its
//! tools and passed the pin check; from the hand-over of `tools/call` on,
//! every answer but a completed one leaves the effect unknown. A
//! successful call is settled before its session closes; a failed call
//! closes first, so its settlement carries the end's warning.
//!
//! The fixture is the one of `connector_mcp.rs`, with an attempt journaled
//! so the calls bind to it. Every function here that uses a `timeout_sec`
//! under 15 sets `protocol: modern`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use apb_core::schema::OnUnknown;
use apb_engine::connector::call::{AttemptClaim, CallContext, execute_with};
use apb_engine::event::{EventLog, EventPayload, read_all_lossy_tail};
use apb_mcp_client::testing::{StubCall, StubServer};
use serde_json::{Value, json};

use crate::connector_mcp::{Fx, NODE, error_code, tools};

/// The marker declaration of `create_comment` as `connector_yaml` writes
/// it, from its `idempotency:` line to the end of the definition.
fn without_marker(yaml: &str, replacement: &str) -> String {
    let at = yaml
        .find("    idempotency:\n      mode: marker")
        .expect("the marker block");
    format!("{}{replacement}", &yaml[..at])
}

/// `create_comment` declared `idempotency: { mode: none }`.
fn none_yaml(yaml: String) -> String {
    without_marker(&yaml, "    idempotency: { mode: none }\n")
}

/// One fixture with an attempt of `NODE` journaled and claimed.
struct Sx {
    fx: Fx,
    claim: Mutex<AttemptClaim>,
}

impl Sx {
    fn new() -> Sx {
        Sx::with(|y| y)
    }

    fn with(edit: impl FnOnce(String) -> String) -> Sx {
        let sx = Sx {
            fx: Fx::with(edit),
            claim: Mutex::new(AttemptClaim::default()),
        };
        sx.fx.pin();
        sx.start_attempt(1);
        sx
    }

    fn journal(&self, payload: EventPayload) {
        EventLog::open(&self.fx.run)
            .or_else(|_| EventLog::create(&self.fx.run))
            .unwrap()
            .append(payload)
            .unwrap();
    }

    fn start_attempt(&self, attempt: u32) {
        let token = apb_core::connector::marker::new_nonce();
        self.journal(EventPayload::AttemptStarted {
            item: None,
            node: NODE.into(),
            attempt,
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
        *self.claim.lock().unwrap() = AttemptClaim {
            item: None,
            attempt: Some(attempt),
            token: Some(token),
            host_task: None,
        };
    }

    fn finish_attempt(&self, attempt: u32) {
        self.journal(EventPayload::AttemptFinished {
            item: None,
            node: NODE.into(),
            attempt,
            status: "failed".into(),
            duration_ms: None,
            session: None,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: None,
            usage: None,
            rejected_for: None,
            rejection_fingerprint: None,
            stalled: false,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
        });
    }

    fn call(&self, function: &'static str, args: Value) -> (Value, bool) {
        let claim = self.claim.lock().unwrap().clone();
        execute_with(
            self.fx.request(function, args),
            CallContext::node(None, claim),
        )
    }

    fn comment(&self) -> (Value, bool) {
        self.call("create_comment", comment_args())
    }

    fn settlements(&self) -> Vec<String> {
        self.fx
            .events()
            .into_iter()
            .filter_map(|p| match p {
                EventPayload::SendIntentSettled { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect()
    }

    /// The `tools/call` lines of the stub that called `tool`.
    fn calls_of(&self, tool: &str) -> Vec<String> {
        let needle = format!("\"name\":\"{tool}\"");
        self.fx
            .stub
            .calls()
            .into_iter()
            .filter(|l| l.contains(&needle))
            .collect()
    }
}

fn comment_args() -> Value {
    json!({ "issue": "42", "body": "Triage summary." })
}

fn ok_answer() -> StubCall {
    StubCall::Result(json!({ "content": [{ "type": "text", "text": "commented" }] }))
}

fn tool_error_answer() -> StubCall {
    StubCall::Result(json!({
        "content": [{ "type": "text", "text": "the tracker failed" }],
        "isError": true
    }))
}

#[test]
fn a_confirmed_write_settles_confirmed() {
    let sx = Sx::new();
    sx.fx.set_call(ok_answer());
    let (v, ok) = sx.comment();
    assert!(ok, "{v}");
    assert_eq!(sx.settlements(), vec!["confirmed".to_string()]);
    assert_eq!(sx.fx.intents(), 1);
}

#[test]
fn a_server_that_exits_after_reading_the_call_leaves_it_unknown() {
    let sx = Sx::with(none_yaml);
    sx.fx.set_call(StubCall::ExitAfterRead);
    let (v, ok) = sx.comment();
    assert!(!ok, "{v}");
    assert_eq!(v["effect"], "unknown", "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    sx.fx.set_call(ok_answer());
    let (again, ok) = sx.comment();
    assert!(!ok);
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
    assert_eq!(sx.calls_of("create_comment").len(), 1);
}

#[test]
fn a_call_that_times_out_is_unknown() {
    let sx = Sx::with(|y| {
        none_yaml(y)
            .replace(
                "  transport: stdio\n",
                "  transport: stdio\n  protocol: modern\n",
            )
            .replace(
                "    description: Add a comment to an issue\n",
                "    description: Add a comment to an issue\n    timeout_sec: 2\n",
            )
    });
    sx.fx.set_call(StubCall::Hang);
    let started = Instant::now();
    let (v, ok) = sx.comment();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "timeout", "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_tool_error_on_a_write_is_unknown_and_a_marker_repeat_reconciles() {
    let sx = Sx::new();
    sx.fx.set_call(tool_error_answer());
    let (v, ok) = sx.comment();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "tool_error", "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    // The body the tool got carries the marker; the tracker lists it.
    let sent: Value = serde_json::from_str(&sx.calls_of("create_comment")[0]).unwrap();
    let body = sent["params"]["arguments"]["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(body.contains("apb-intent"), "{body}");
    sx.fx.set_call(StubCall::Result(json!({
        "structuredContent": {
            "id": "bot-7",
            "comments": [{ "body": body, "author": { "id": "bot-7" }, "url": "https://tracker.test/c/1" }]
        }
    })));
    // The second attempt repeats the send: the ledger reconciles it.
    sx.finish_attempt(1);
    sx.start_attempt(2);
    let (again, ok) = sx.comment();
    assert!(ok, "{again}");
    assert_eq!(again["already_sent"], true, "{again}");
    assert_eq!(sx.calls_of("create_comment").len(), 1);
}

#[test]
fn a_tool_error_on_a_none_write_is_refused_on_repeat() {
    let sx = Sx::with(none_yaml);
    sx.fx.set_call(tool_error_answer());
    let (v, _) = sx.comment();
    assert_eq!(error_code(&v), "tool_error", "{v}");
    sx.fx.set_call(ok_answer());
    let (again, ok) = sx.comment();
    assert!(!ok);
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
    assert_eq!(sx.calls_of("create_comment").len(), 1);
}

#[test]
fn on_unknown_resend_allows_the_repeat() {
    let sx = Sx::with(none_yaml);
    sx.fx
        .seed_grant(sx.fx.account(), None, Some(OnUnknown::Resend));
    sx.fx.set_call(tool_error_answer());
    sx.comment();
    sx.fx.set_call(ok_answer());
    let (again, ok) = sx.comment();
    assert!(ok, "{again}");
    assert_eq!(sx.calls_of("create_comment").len(), 2);
}

#[test]
fn a_request_error_after_acting_is_unknown() {
    let sx = Sx::with(none_yaml);
    // The stub appends the call to calls.log (it acted), then answers -32602.
    sx.fx.set_call(StubCall::Error {
        code: -32602,
        message: "invalid params".into(),
    });
    let (v, _) = sx.comment();
    assert_eq!(error_code(&v), "tool_error", "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    let (again, _) = sx.comment();
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
    assert_eq!(sx.fx.stub.calls().len(), 1);
}

#[test]
fn a_server_error_on_a_write_is_unknown() {
    let sx = Sx::with(none_yaml);
    sx.fx.set_call(StubCall::Error {
        code: -32603,
        message: "internal".into(),
    });
    let (v, _) = sx.comment();
    assert_eq!(error_code(&v), "tool_error", "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    let (again, _) = sx.comment();
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
}

#[test]
fn input_required_settles_unknown() {
    let sx = Sx::with(none_yaml);
    sx.fx.set_call(StubCall::RawResult(
        r#"{"resultType":"input_required","requestState":"opaque"}"#.into(),
    ));
    let (v, _) = sx.comment();
    assert_eq!(error_code(&v), "tool_error", "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    let (again, _) = sx.comment();
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
}

#[test]
fn a_task_answer_is_unknown() {
    let sx = Sx::with(none_yaml);
    sx.fx.set_call(StubCall::RawResult(
        r#"{"resultType":"task","taskId":"t1","status":"working","createdAt":"2026-11-20T00:00:00Z","lastUpdatedAt":"2026-11-20T00:00:00Z","ttlMs":60000}"#.into(),
    ));
    let (v, _) = sx.comment();
    assert_eq!(error_code(&v), "tool_error", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("task apb does not follow"),
        "{v}"
    );
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
}

fn no_send_journaled(sx: &Sx) {
    assert_eq!(sx.fx.intents(), 0, "{:?}", sx.fx.events());
    assert!(sx.fx.connector_calls().is_empty(), "{:?}", sx.fx.events());
}

#[test]
fn a_server_that_cannot_start_records_no_intent() {
    use std::os::unix::fs::PermissionsExt;
    let sx = Sx::new();
    // The digest still matches: only the spawn fails.
    std::fs::set_permissions(&sx.fx.program, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (v, ok) = sx.comment();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "network", "{v}");
    no_send_journaled(&sx);
}

#[test]
fn a_pin_refusal_records_no_intent() {
    let sx = Sx::new();
    sx.fx.pin_edited(|p| {
        p.tools.remove("create_comment");
    });
    let (v, ok) = sx.comment();
    assert!(!ok);
    assert_eq!(error_code(&v), "permission", "{v}");
    no_send_journaled(&sx);
    sx.fx.assert_not_started();
}

/// The variable that makes a kill test's own name, run again in a child
/// process, make the one call its parent kills.
const KILL_CHILD: &str = "APB_MCP_SEND_KILL_CHILD";

/// In a child process started by [`spawn_call_child`] for `test`: makes
/// the one `create_comment` call the parent kills, and answers `true` (the
/// test body then returns). `false` in an ordinary run, where the same test
/// is the parent.
fn child_call(test: &str) -> bool {
    let Ok(spec) = std::env::var(KILL_CHILD) else {
        return false;
    };
    let spec: Value = serde_json::from_str(&spec).unwrap();
    if spec["test"] != test {
        return false;
    }
    let path = |k: &str| std::path::PathBuf::from(spec[k].as_str().unwrap());
    let claim = AttemptClaim {
        item: None,
        attempt: Some(1),
        token: Some(spec["token"].as_str().unwrap().to_string()),
        host_task: None,
    };
    let (run, root) = (path("run"), path("root"));
    // The test binary's isolation picked its own config dir at start: use
    // the parent's, where the pins are.
    // SAFETY: this child process runs this one test on one thread.
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", path("cfg"));
        std::env::set_var("APB_TEST_DEFAULT_CONFIG_DIR", path("cfg"));
    }
    let answer = execute_with(
        apb_engine::connector::call::CallRequest {
            run_dir: &run,
            root: &root,
            node_id: NODE,
            connector: crate::connector_mcp::CONNECTOR,
            function: "create_comment",
            account: None,
            args: comment_args(),
            dry_run: false,
            full: false,
        },
        CallContext::node(None, claim),
    );
    // Reached only when the parent did not kill the call.
    eprintln!("{answer:?}");
    true
}

/// The child process of a kill test, killed and reaped when it drops, so a
/// failed assertion never leaves it running.
struct CallChild {
    child: std::process::Child,
    log: std::path::PathBuf,
}

impl CallChild {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for CallChild {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Runs `test` again in a child test process, which makes the one call
/// (`child_call`).
fn spawn_call_child(sx: &Sx, test: &str) -> CallChild {
    let token = sx.claim.lock().unwrap().token.clone().unwrap();
    let spec = json!({
        "test": test,
        "run": sx.fx.run,
        "root": sx.fx.root,
        "cfg": sx.fx.cfg,
        "token": token,
    });
    let log = sx.fx.base().join(format!("{test}.log"));
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            &format!("connector_mcp_send::{test}"),
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(KILL_CHILD, spec.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    CallChild { child, log }
}

/// Polls `done` for up to 30 s; panics with `what` and the child's log.
fn wait_until(child: &CallChild, what: &str, done: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(
            Instant::now() < until,
            "{what} did not happen; child log: {}",
            child.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Writes one empty page into the FIFO a blocked stub reads; opened
/// without blocking, so a FIFO nobody reads fails the test instead of
/// hanging it.
fn release_fifo(fifo: &std::path::Path, within: Duration) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let until = Instant::now() + within;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
        {
            Ok(mut f) => {
                let _ = f.write_all(b"[]");
                return Ok(());
            }
            Err(e) if Instant::now() >= until => return Err(format!("nobody reads the FIFO: {e}")),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// A FIFO a stub blocks on, released when it drops, so a failed assertion
/// never leaves the stub blocked.
struct Fifo(std::path::PathBuf);

impl Drop for Fifo {
    fn drop(&mut self) {
        if self.0.exists() {
            // One try: a stub still blocked on it reads it at once.
            let _ = release_fifo(&self.0, Duration::ZERO);
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

#[test]
fn a_kill_between_connect_and_call_leaves_no_intent() {
    if child_call("a_kill_between_connect_and_call_leaves_no_intent") {
        return;
    }
    let sx = Sx::new();
    // The listing hangs: the stub's one page is a FIFO nobody writes yet.
    let tools_file = sx.fx.stub.dir.join("tools.0.json");
    std::fs::write(sx.fx.stub.dir.join("tools.pages"), "1").unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(&tools_file)
        .status()
        .unwrap();
    assert!(made.success());
    let fifo = Fifo(tools_file.clone());
    let mut child = spawn_call_child(&sx, "a_kill_between_connect_and_call_leaves_no_intent");
    wait_until(&child, "the listing request", || {
        sx.fx.stub.methods().iter().any(|m| m == "tools/list")
    });
    let server = sx.fx.stub.last_pid().expect("the server started");
    child.kill();
    assert_eq!(sx.fx.intents(), 0, "{:?}", sx.fx.events());
    if cfg!(target_os = "linux") {
        // The server got SIGKILL with the death of the thread that
        // started it, blocked on the FIFO or not.
        wait_until(&child, "the server's end", || {
            StubServer::has_exited(server)
        });
    } else {
        // Release the server blocked on the FIFO (it then finds its
        // caller gone).
        release_fifo(&tools_file, Duration::from_secs(10)).unwrap();
    }
    // Serve the tools again.
    drop(fifo);
    std::fs::remove_file(sx.fx.stub.dir.join("tools.pages")).unwrap();
    sx.fx.stub.set_tools(&tools()).unwrap();
    sx.fx.set_call(ok_answer());
    let (v, ok) = sx.comment();
    assert!(ok, "the repeat goes out: {v}");
    assert_eq!(sx.fx.intents(), 1);
    assert_eq!(sx.calls_of("create_comment").len(), 1);
}

/// What the tracker lists to a reconcile: the identity, and the comments
/// with `bodies`.
fn listed_comments(bodies: &[&str]) -> StubCall {
    let comments: Vec<Value> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| json!({ "body": b, "author": { "id": "bot-7" }, "url": format!("https://tracker.test/c/{i}") }))
        .collect();
    StubCall::Result(json!({ "structuredContent": { "id": "bot-7", "comments": comments } }))
}

#[test]
fn a_kill_once_the_intent_is_recorded_is_never_sent_again_blindly() {
    if child_call("a_kill_once_the_intent_is_recorded_is_never_sent_again_blindly") {
        return;
    }
    let sx = Sx::new();
    sx.fx.set_call(StubCall::Hang);
    let mut child = spawn_call_child(
        &sx,
        "a_kill_once_the_intent_is_recorded_is_never_sent_again_blindly",
    );
    // Killed as soon as the intent is in the journal: before or right
    // after the hand-over of `tools/call`, which nobody can tell apart
    // afterwards, so the repeat must not send it blindly either way.
    wait_until(&child, "the intent record", || sx.fx.intents() == 1);
    let server = sx.fx.stub.last_pid().expect("the server started");
    child.kill();
    // Count the hand-over only once the server has ended: until then it
    // may still read a `tools/call` the killed caller left in its pipe and
    // record it, which is the original call and not a repeat.
    // On Linux the server got SIGKILL with the death of the thread that
    // started it; elsewhere there is no parent-death signal, so the test
    // ends the server it left.
    if !cfg!(target_os = "linux") {
        let _ = std::process::Command::new("kill")
            .args(["-9", &server.to_string()])
            .status();
    }
    wait_until(&child, "the server's end", || {
        StubServer::has_exited(server)
    });
    let handed = sx.calls_of("create_comment").len();
    assert!(handed <= 1);
    // The tracker lists no comment with the marker: the send stays
    // unknown and the repeat is refused.
    sx.fx.set_call(listed_comments(&["an older comment"]));
    let (again, ok) = sx.comment();
    assert!(!ok, "{again}");
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
    assert_eq!(
        sx.calls_of("create_comment").len(),
        handed,
        "nothing sent again"
    );
}

#[test]
fn a_kill_after_the_hand_over_is_reconciled_by_its_marker() {
    if child_call("a_kill_after_the_hand_over_is_reconciled_by_its_marker") {
        return;
    }
    let sx = Sx::new();
    sx.fx.set_call(StubCall::Hang);
    // A server that ignores the end of its input: only a signal ends it.
    sx.fx.stub.set_ignore_eof(true).unwrap();
    let mut child = spawn_call_child(
        &sx,
        "a_kill_after_the_hand_over_is_reconciled_by_its_marker",
    );
    wait_until(&child, "the tools/call", || {
        sx.calls_of("create_comment").len() == 1
    });
    let server = sx.fx.stub.last_pid().expect("the server started");
    child.kill();
    sx.fx.stub.set_ignore_eof(false).unwrap();
    if cfg!(target_os = "linux") {
        // The caller was killed mid-call: the server, in its own process
        // group, got SIGKILL with the death of the thread that started it.
        wait_until(&child, "the server's end", || {
            StubServer::has_exited(server)
        });
    } else {
        // No parent-death signal on this platform: the test ends the
        // server it left (design D4 "The order of one call").
        let _ = std::process::Command::new("kill")
            .args(["-9", &server.to_string()])
            .status();
    }
    let sent: Value = serde_json::from_str(&sx.calls_of("create_comment")[0]).unwrap();
    let body = sent["params"]["arguments"]["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(body.contains("apb-intent"), "{body}");
    // The tracker acted: it lists the comment with the marker.
    sx.fx.set_call(listed_comments(&[&body]));
    let (again, ok) = sx.comment();
    assert!(ok, "{again}");
    assert_eq!(again["already_sent"], true, "{again}");
    assert_eq!(sx.calls_of("create_comment").len(), 1, "never sent twice");
}

#[test]
fn a_server_gone_before_the_call_settles_not_sent() {
    let sx = Sx::with(none_yaml);
    // The stub closes its stdin, answers the listing and exits: the
    // intent is recorded after the start, and the call never reaches it.
    sx.fx.stub.set_exit_after_list(true).unwrap();
    let (v, ok) = sx.comment();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "service", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("never reached the MCP server"),
        "{v}"
    );
    assert_eq!(sx.settlements(), vec!["not_sent".to_string()]);
    assert!(sx.calls_of("create_comment").is_empty());
    // Not sent: the repeat goes out.
    sx.fx.stub.set_exit_after_list(false).unwrap();
    sx.fx.set_call(ok_answer());
    let (again, ok) = sx.comment();
    assert!(ok, "{again}");
    assert_eq!(sx.calls_of("create_comment").len(), 1);
}

#[test]
fn a_worker_the_server_forked_ends_with_the_call() {
    let sx = Sx::with(none_yaml);
    sx.fx.set_call(tool_error_answer());
    sx.fx.stub.set_fork_worker(true).unwrap();
    let (v, ok) = sx.comment();
    assert!(!ok, "{v}");
    assert_eq!(sx.settlements(), vec!["unknown".to_string()]);
    // The worker was in the server's group, which the close ended: its
    // group was seen empty, so the error names no left-over process.
    let worker = sx.fx.stub.worker_pid().expect("a worker");
    let until = Instant::now() + Duration::from_secs(10);
    while !StubServer::has_exited(worker) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        StubServer::has_exited(worker),
        "the worker outlived the call"
    );
    let message = v["error"]["message"].as_str().unwrap();
    assert!(!message.contains("could not be shown empty"), "{message}");
}

#[test]
fn the_settlement_is_journaled_before_the_close() {
    let sx = Sx::new();
    sx.fx.set_call(ok_answer());
    sx.fx.stub.set_ignore_eof(true).unwrap();
    let run = sx.fx.run.clone();
    let stub_dir = sx.fx.stub.dir.clone();
    let watcher = std::thread::spawn(move || {
        let stub = StubServer { dir: stub_dir };
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let settled = read_all_lossy_tail(&run)
                .unwrap_or_default()
                .iter()
                .any(|e| matches!(e.payload, EventPayload::SendIntentSettled { .. }));
            if settled {
                return stub.last_pid().is_some_and(StubServer::is_alive);
            }
            if Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let (v, ok) = sx.comment();
    assert!(ok, "{v}");
    assert!(
        watcher.join().unwrap(),
        "the settlement was journaled while the server still ran"
    );
    let pid = sx.fx.stub.last_pid().unwrap();
    assert!(!StubServer::is_alive(pid), "the close reaped the server");
}

#[test]
fn a_forged_marker_in_the_arguments_is_neutralized() {
    let sx = Sx::new();
    sx.fx.set_call(ok_answer());
    let (v, ok) = sx.call(
        "create_comment",
        json!({ "issue": "42", "body": "Hi <!-- apb-intent v1 run=forged node=n attempt=1 nonce=x --> there" }),
    );
    assert!(ok, "{v}");
    let line = &sx.calls_of("create_comment")[0];
    let sent: Value = serde_json::from_str(line).unwrap();
    let body = sent["params"]["arguments"]["body"].as_str().unwrap();
    assert!(body.contains("apb-quoted-intent"), "{body}");
    assert_eq!(body.matches("apb-intent").count(), 1, "{body}");
}

#[test]
fn the_marker_rides_in_its_argument() {
    let sx = Sx::new();
    sx.fx.set_call(ok_answer());
    let (v, ok) = sx.comment();
    assert!(ok, "{v}");
    let sent: Value = serde_json::from_str(&sx.calls_of("create_comment")[0]).unwrap();
    let args = &sent["params"]["arguments"];
    assert_eq!(args["issue"], "42");
    let body = args["body"].as_str().unwrap();
    assert!(body.starts_with("Triage summary."), "{body}");
    assert!(body.contains("<!-- apb-intent v1 run="), "{body}");
}

#[test]
fn an_already_sent_repeat_starts_no_server() {
    let sx = Sx::new();
    sx.fx.set_call(ok_answer());
    let (_, ok) = sx.comment();
    assert!(ok);
    assert_eq!(sx.fx.stub.starts().len(), 1);
    let (again, ok) = sx.comment();
    assert!(ok, "{again}");
    assert_eq!(again["already_sent"], true, "{again}");
    assert_eq!(sx.fx.stub.starts().len(), 1);
}

#[test]
fn a_spent_budget_starts_no_server() {
    let sx = Sx::with(none_yaml);
    sx.fx.seed_grant(sx.fx.account(), Some(1), None);
    sx.fx.set_call(ok_answer());
    let (_, ok) = sx.call("whoami", json!({}));
    assert!(ok);
    assert_eq!(sx.fx.stub.starts().len(), 1);
    let (v, ok) = sx.comment();
    assert!(!ok);
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_calls budget"),
        "{v}"
    );
    assert_eq!(sx.fx.stub.starts().len(), 1);
    assert_eq!(sx.fx.intents(), 0);
}

#[test]
fn a_repeatable_write_is_untracked() {
    let sx = Sx::with(|y| without_marker(&y, "    idempotency: { mode: repeatable }\n"));
    sx.fx.set_call(ok_answer());
    let (v, ok) = sx.comment();
    assert!(ok, "{v}");
    let (v, ok) = sx.comment();
    assert!(ok, "{v}");
    assert_eq!(sx.fx.intents(), 0);
    assert_eq!(sx.calls_of("create_comment").len(), 2);
}
