//! The stdio session against the scripted stub server
//! (`apb_mcp_client::testing`).
#![cfg(unix)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use apb_mcp_client::testing::{StubCall, StubEra, StubServer};
use apb_mcp_client::{
    CallFailure, Ended, EnvValue, Era, Failure, FailureKind, GroupEnd, Lifecycle, MAX_LINE_BYTES,
    ProgramFile, ProgramRefusal, REPLY_CLEANUP_ALLOWANCE, Server, Session, StartFrom, StdioServer,
    ToolAnswer,
};
use serde_json::{Map, Value, json};

const PAGES: u32 = 20;
const TOOLS: usize = 500;
const BYTES: usize = 2 * 1024 * 1024;

/// Variables the shell itself puts in its environment.
const SHELL_NAMES: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

fn stub(era: StubEra) -> (tempfile::TempDir, StubServer) {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let stub = StubServer::write(tmp.path(), era).expect("the stub is written");
    (tmp, stub)
}

fn base_env() -> Vec<(String, EnvValue)> {
    vec![("PATH".into(), EnvValue::Plain("/usr/bin:/bin".into()))]
}

/// The reaped end of a server whose group was seen empty.
const fn reaped(killed: bool) -> Ended {
    Ended::Reaped {
        killed,
        group: GroupEnd::Empty,
    }
}

/// Headroom over a computed bound, so a loaded runner does not flake; the
/// bounds still prove the wait ends (a hang would not).
const HEADROOM: Duration = Duration::from_secs(5);

fn program_of(path: &std::path::Path, start: StartFrom) -> ProgramFile {
    ProgramFile::open(path, start).expect("the program opens")
}

fn server_with(stub: &StubServer, args: &[&str], env: Vec<(String, EnvValue)>) -> Server {
    server_from(
        program_of(&stub.program(), StartFrom::platform()),
        args,
        env,
    )
}

fn server_from(program: ProgramFile, args: &[&str], env: Vec<(String, EnvValue)>) -> Server {
    Server::Stdio(StdioServer {
        program,
        args: args.iter().map(|a| a.to_string()).collect(),
        env,
    })
}

fn open_in(stub: &StubServer, lifecycle: Lifecycle, secs: u64) -> Result<Session, Failure> {
    Session::connect(
        server_with(stub, &[], base_env()),
        lifecycle,
        "apb-test",
        "1.0.0",
        Instant::now() + Duration::from_secs(secs),
    )
}

fn open(stub: &StubServer) -> Session {
    open_in(stub, Lifecycle::Auto, 10).expect("the session opens")
}

fn soon(secs: u64) -> Instant {
    Instant::now() + Duration::from_secs(secs)
}

fn list(session: &Session) -> Result<Vec<Value>, Failure> {
    session.list_tools(soon(10), PAGES, TOOLS, BYTES)
}

fn call(session: &Session, secs: u64) -> Result<ToolAnswer, CallFailure> {
    session.call_tool("t", Map::new(), soon(secs))
}

fn lost(r: Result<ToolAnswer, CallFailure>) -> Failure {
    match r {
        Err(CallFailure::Lost(f)) => f,
        other => panic!("expected a lost answer, got {other:?}"),
    }
}

fn tool(name: &str) -> Value {
    json!({ "name": name, "inputSchema": { "type": "object" } })
}

#[test]
fn a_modern_server_opens_modern() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut s = open(&stub);
    assert_eq!(s.era(), &Era::Modern("2026-07-28"));
    assert_eq!(s.close(), reaped(false));
    let methods = stub.methods();
    assert_eq!(methods.first().map(String::as_str), Some("server/discover"));
    assert!(!methods.iter().any(|m| m == "initialize"), "{methods:?}");
}

#[test]
fn a_legacy_server_falls_back() {
    let (_tmp, stub) = stub(StubEra::Legacy);
    let mut s = open(&stub);
    assert_eq!(s.era(), &Era::Legacy("2025-11-25"));
    s.close();
    let methods = stub.methods();
    assert!(
        methods.iter().any(|m| m == "server/discover"),
        "{methods:?}"
    );
    assert!(methods.iter().any(|m| m == "initialize"), "{methods:?}");
}

#[test]
fn a_silent_legacy_server_falls_back_after_the_probe() {
    let (_tmp, stub) = stub(StubEra::LegacySilent);
    let mut s = open_in(&stub, Lifecycle::Auto, 15).expect("the fallback opens");
    assert_eq!(s.era(), &Era::Legacy("2025-11-25"));
    s.close();
}

#[test]
fn modern_mode_refuses_a_legacy_server() {
    let (_tmp, stub) = stub(StubEra::Legacy);
    let err = open_in(&stub, Lifecycle::Modern, 10)
        .err()
        .expect("refused");
    assert_eq!(err.kind, FailureKind::Unsupported, "{err:?}");
    assert!(matches!(err.ended, Ended::Reaped { .. }), "{err:?}");
}

#[test]
fn legacy_mode_skips_discover() {
    let (_tmp, stub) = stub(StubEra::Legacy);
    let mut s = open_in(&stub, Lifecycle::Legacy, 10).expect("opens");
    assert_eq!(s.era(), &Era::Legacy("2025-11-25"));
    s.close();
    let methods = stub.methods();
    assert!(
        !methods.iter().any(|m| m == "server/discover"),
        "{methods:?}"
    );
    assert_eq!(methods.first().map(String::as_str), Some("initialize"));
}

#[test]
fn list_tools_follows_cursors() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([[tool("a"), tool("b")], [tool("c")], [tool("d")]]))
        .unwrap();
    let mut s = open(&stub);
    let tools = list(&s).expect("the list");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["a", "b", "c", "d"]);
    s.close();
    let listed = stub.methods().iter().filter(|m| *m == "tools/list").count();
    assert_eq!(listed, 3);
}

#[test]
fn list_tools_stops_at_a_repeated_cursor() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([[tool("a")], [tool("b")]])).unwrap();
    stub.set_next_cursor(Some("again")).unwrap();
    let mut s = open(&stub);
    let err = list(&s).expect_err("a list that does not end");
    assert_eq!(err.kind, FailureKind::TooLarge, "{err:?}");
    assert!(err.message.contains("did not end"), "{err:?}");
    s.close();
    let listed = stub.methods().iter().filter(|m| *m == "tools/list").count();
    assert_eq!(listed, 2, "the repeated cursor is not asked again");
}

#[test]
fn list_tools_stops_at_the_page_limit() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let pages: Vec<Value> = (0..5).map(|i| json!([tool(&format!("t{i}"))])).collect();
    stub.set_tools(&Value::Array(pages)).unwrap();
    let mut s = open(&stub);
    let err = s
        .list_tools(soon(10), 3, TOOLS, BYTES)
        .expect_err("too many pages");
    assert_eq!(err.kind, FailureKind::TooLarge, "{err:?}");
    s.close();
    let listed = stub.methods().iter().filter(|m| *m == "tools/list").count();
    assert_eq!(listed, 3);
}

#[test]
fn list_tools_stops_at_the_tool_limit() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let tools: Vec<Value> = (0..10).map(|i| tool(&format!("t{i}"))).collect();
    stub.set_tools(&Value::Array(tools)).unwrap();
    let mut s = open(&stub);
    let err = s
        .list_tools(soon(10), PAGES, 5, BYTES)
        .expect_err("too many tools");
    assert_eq!(err.kind, FailureKind::TooLarge, "{err:?}");
    let all = s.list_tools(soon(10), PAGES, 10, BYTES).expect("ten fit");
    assert_eq!(all.len(), 10);
    s.close();
}

#[test]
fn list_tools_stops_at_the_byte_limit() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let big = |n: &str| json!({ "name": n, "inputSchema": { "type": "object" }, "description": "d".repeat(1000) });
    stub.set_tools(&json!([[big("a")], [big("b")], [big("c")]]))
        .unwrap();
    let mut s = open(&stub);
    let err = s
        .list_tools(soon(10), PAGES, TOOLS, 2500)
        .expect_err("past the byte limit");
    assert_eq!(err.kind, FailureKind::TooLarge, "{err:?}");
    s.close();
}

#[test]
fn a_failed_handshake_reaps_a_server_that_ignores_end_of_input() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_ignore_eof(true).unwrap();
    let err = open_in(&stub, Lifecycle::Legacy, 10).err().expect("fails");
    assert_eq!(err.ended, reaped(true), "{err:?}");
    let pid = stub.last_pid().expect("a pid");
    assert!(!StubServer::is_alive(pid), "the server is gone at return");
}

/// The session's own deadline bounds the handshake, the 10 s discover
/// probe of a silent server included: the worker's bounded close runs and
/// answers before the caller's reply timeout (the deadline plus
/// `REPLY_CLEANUP_ALLOWANCE`). Without the internal deadline the caller's
/// timeout answers first, with `Ended::Unconfirmed`, so `reaped(true)`
/// and the bound below both fail.
#[test]
fn a_handshake_failing_at_the_deadline_is_reaped_before_connect_returns() {
    let (_tmp, stub) = stub(StubEra::LegacySilent);
    stub.set_ignore_eof(true).unwrap();
    let deadline = Duration::from_secs(3);
    let started = Instant::now();
    let err = Session::connect(
        server_with(&stub, &[], base_env()),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        started + deadline,
    )
    .err()
    .expect("times out");
    let took = started.elapsed();
    assert_eq!(err.kind, FailureKind::Timeout, "{err:?}");
    assert_eq!(err.ended, reaped(true), "{err:?}");
    let pid = stub.last_pid().expect("a pid");
    assert!(!StubServer::is_alive(pid), "the server is gone at return");
    assert!(
        took < deadline + REPLY_CLEANUP_ALLOWANCE,
        "took {took:?}: the caller's reply timeout answered, not the session"
    );
}

#[test]
fn call_tool_returns_the_result() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::Result(json!({
        "content": [{ "type": "text", "text": "{\"n\":1}" }],
        "structuredContent": { "n": 1 }
    })))
    .unwrap();
    let mut s = open(&stub);
    let mut args = Map::new();
    args.insert("q".into(), json!("x"));
    let answer = s.call_tool("lookup", args, soon(10)).expect("an answer");
    match answer {
        ToolAnswer::Complete {
            content,
            structured,
            is_error,
        } => {
            assert_eq!(
                content,
                vec![json!({ "type": "text", "text": "{\"n\":1}" })]
            );
            assert_eq!(structured, Some(json!({ "n": 1 })));
            assert!(!is_error);
        }
        other => panic!("{other:?}"),
    }
    s.close();
    let calls = stub.calls();
    assert_eq!(calls.len(), 1);
    let line: Value = serde_json::from_str(&calls[0]).unwrap();
    assert_eq!(line["params"]["name"], "lookup");
    assert_eq!(line["params"]["arguments"], json!({ "q": "x" }));
}

#[test]
fn an_is_error_result_is_complete_with_is_error() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::Result(json!({
        "content": [{ "type": "text", "text": "bad input" }],
        "isError": true
    })))
    .unwrap();
    let mut s = open(&stub);
    match call(&s, 10).expect("an answer") {
        ToolAnswer::Complete {
            is_error,
            structured,
            ..
        } => {
            assert!(is_error);
            assert_eq!(structured, None);
        }
        other => panic!("{other:?}"),
    }
    s.close();
}

#[test]
fn a_json_rpc_error_is_answered() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::Error {
        code: -32000,
        message: "the service said no".into(),
    })
    .unwrap();
    let mut s = open(&stub);
    match call(&s, 10) {
        Err(CallFailure::Answered(f)) => {
            assert_eq!(f.kind, FailureKind::Rejected { code: -32000 });
            assert!(f.message.contains("the service said no"), "{f:?}");
        }
        other => panic!("{other:?}"),
    }
    s.close();
}

#[test]
fn an_exit_after_read_is_lost() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::ExitAfterRead).unwrap();
    let mut s = open(&stub);
    let f = lost(call(&s, 10));
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert_eq!(stub.calls().len(), 1);
    assert!(s.close() == reaped(false));
}

#[test]
fn a_hang_is_lost_with_timeout() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::Hang).unwrap();
    let mut s = open(&stub);
    let started = Instant::now();
    let f = lost(call(&s, 2));
    assert_eq!(f.kind, FailureKind::Timeout, "{f:?}");
    assert!(started.elapsed() < Duration::from_secs(2) + REPLY_CLEANUP_ALLOWANCE + HEADROOM);
    s.close();
}

#[test]
fn input_required_is_reported() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::RawResult(
        r#"{"resultType":"input_required","requestState":"opaque"}"#.into(),
    ))
    .unwrap();
    let mut s = open(&stub);
    assert!(matches!(call(&s, 10), Ok(ToolAnswer::InputRequired)));
    s.close();
}

/// An answer whose `result` is no call result (none of `content`,
/// `structuredContent` and `isError`) stays an unexpected answer, as apb
/// 1.0 read it: rmcp 3.11 reads such an answer as an empty successful
/// call, which would settle `confirmed` a send that 1.0 settles `unknown`.
/// The last answer is a frame with one more member, whose `result` rmcp
/// still reads.
#[test]
fn an_answer_that_is_no_call_result_stays_unexpected() {
    for raw in [
        "{}",
        r#"{"_meta":{"example.com/replica":"r1"}}"#,
        r#"{"resultType":"complete"}"#,
        r#"{"resultType":"complete","_meta":{"example.com/replica":"r1"}}"#,
        r#"{"content":null}"#,
        r#"{},"x":1"#,
    ] {
        let (_tmp, stub) = stub(StubEra::Modern);
        stub.set_call(StubCall::RawResult(raw.into())).unwrap();
        let mut s = open(&stub);
        let f = lost(call(&s, 10));
        assert_eq!(f.kind, FailureKind::Transport, "{raw}: {f:?}");
        assert_eq!(f.message, "Unexpected response type", "{raw}");
        s.close();
    }
}

/// An empty `content` list is a call result: it stays a complete answer.
#[test]
fn an_empty_content_list_is_a_complete_answer() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::RawResult(r#"{"content":[]}"#.into()))
        .unwrap();
    let mut s = open(&stub);
    match call(&s, 10).expect("an answer") {
        ToolAnswer::Complete {
            content,
            structured,
            is_error,
        } => {
            assert!(content.is_empty());
            assert_eq!(structured, None);
            assert!(!is_error);
        }
        other => panic!("{other:?}"),
    }
    s.close();
}

#[test]
fn list_tools_keeps_members_the_struct_drops() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let t = json!({
        "name": "t",
        "inputSchema": { "type": "object" },
        "execution": { "taskSupport": "forbidden" },
        "x-vendor": [1, 2]
    });
    stub.set_tools(&json!([t.clone()])).unwrap();
    let mut s = open(&stub);
    assert_eq!(list(&s).expect("the list"), vec![t]);
    s.close();
}

#[test]
fn a_listing_answered_at_once_is_captured_every_time() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([tool("a")])).unwrap();
    for i in 0..100 {
        let mut s = open(&stub);
        let tools = list(&s).unwrap_or_else(|e| panic!("session {i}: {e:?}"));
        assert_eq!(tools, vec![tool("a")], "session {i}");
        s.close();
    }
}

#[test]
fn a_small_body_with_a_large_dropped_block_is_refused_by_the_line_limit() {
    let (_tmp, stub) = stub(StubEra::Modern);
    // The line is `{"jsonrpc":"2.0","id":1,"result":` + raw + `}`.
    let head = r#"{"content":[{"type":"text","text":"hi"},{"type":"image","mimeType":"image/png","data":""#;
    let tail = r#""}]}"#;
    let envelope = r#"{"jsonrpc":"2.0","id":1,"result":"#.len() + 1;
    let data = MAX_LINE_BYTES + 1 - envelope - head.len() - tail.len();
    stub.set_call(StubCall::RawResult(format!(
        "{head}{}{tail}",
        "A".repeat(data)
    )))
    .unwrap();
    let mut s = open(&stub);
    let f = lost(call(&s, 30));
    assert!(f.message.starts_with("mcp_message_too_large:"), "{f:?}");
    s.close();
}

#[test]
fn an_oversize_answer_is_refused_by_name() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call(StubCall::Oversize(MAX_LINE_BYTES + 1))
        .unwrap();
    let mut s = open(&stub);
    let started = Instant::now();
    let f = lost(call(&s, 60));
    assert!(f.message.starts_with("mcp_message_too_large:"), "{f:?}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "well before the deadline"
    );
    s.close();
}

#[test]
fn an_oversize_listing_is_too_large() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([{
        "name": "t",
        "inputSchema": { "type": "object" },
        "description": "a".repeat(MAX_LINE_BYTES + 1)
    }]))
    .unwrap();
    let mut s = open(&stub);
    let err = s
        .list_tools(soon(30), PAGES, TOOLS, usize::MAX)
        .expect_err("refused");
    assert_eq!(err.kind, FailureKind::TooLarge, "{err:?}");
    assert!(err.message.starts_with("mcp_message_too_large:"), "{err:?}");
    s.close();
}

#[test]
fn a_message_flood_is_refused_by_name() {
    let (_tmp, stub) = stub(StubEra::Modern);
    // 10 001 whole notifications before the listing's answer.
    let note = r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"x"}}"#;
    let mut raw = String::new();
    for _ in 0..10_001 {
        raw.push_str(note);
        raw.push('\n');
    }
    stub.set_list_before(&raw).unwrap();
    let mut s = open(&stub);
    let f = list(&s).expect_err("refused");
    assert_eq!(f.kind, FailureKind::TooLarge, "{f:?}");
    assert!(f.message.starts_with("mcp_message_too_large:"), "{f:?}");
    assert!(f.message.contains("10000"), "{f:?}");
    s.close();
}

#[test]
fn the_environment_is_exactly_the_given_one() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut env = base_env();
    env.push(("PLAIN_VAR".into(), EnvValue::Plain("plain value".into())));
    env.push((
        "TRACKER_TOKEN".into(),
        EnvValue::Secret(apb_core::secret::Secret::new("test-token-value")),
    ));
    let mut s = Session::connect(
        server_with(&stub, &["--flag"], env),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        soon(10),
    )
    .expect("opens");
    s.close();
    let seen: BTreeSet<String> = stub
        .env_seen()
        .into_iter()
        .filter(|n| !SHELL_NAMES.contains(&n.as_str()))
        .collect();
    let want: BTreeSet<String> = ["PATH", "PLAIN_VAR", "TRACKER_TOKEN"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(seen, want);
    assert_eq!(
        stub.env_value_seen("PLAIN_VAR").as_deref(),
        Some("plain value")
    );
    assert_eq!(
        stub.env_value_seen("TRACKER_TOKEN").as_deref(),
        Some("test-token-value")
    );
    assert_eq!(stub.args_seen(), vec!["--flag"]);
}

#[test]
fn the_working_directory_is_fresh_private_and_removed() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut s = open(&stub);
    let cwd = stub.cwd_seen().expect("a working directory");
    assert_ne!(
        cwd.canonicalize().ok(),
        Some(stub.dir.canonicalize().unwrap()),
        "never the caller's directory"
    );
    assert!(
        cwd.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("apb-mcp-")),
        "{cwd:?}"
    );
    assert!(stub.cwd_entries_seen().is_empty(), "empty at the start");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cwd)
            .expect("it exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "private: {mode:o}");
    }
    assert_eq!(s.close(), reaped(false));
    assert!(!cwd.exists(), "removed after the close: {cwd:?}");
}

#[test]
fn stderr_never_reaches_the_answer() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let token = "stderr-token-4f1d";
    let mut env = base_env();
    env.push(("STUB_STDERR".into(), EnvValue::Plain(token.into())));
    let mut s = Session::connect(
        server_with(&stub, &[], env),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        soon(10),
    )
    .expect("opens");
    let answer = call(&s, 10).expect("an answer");
    assert!(!format!("{answer:?}").contains(token));
    stub.set_call(StubCall::ExitAfterRead).unwrap();
    let f = lost(call(&s, 10));
    assert!(!format!("{f:?}").contains(token));
    s.close();
}

#[test]
fn close_after_end_of_input_reaps_without_kill() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut s = open(&stub);
    assert_eq!(s.close(), reaped(false));
    assert!(!StubServer::is_alive(stub.last_pid().unwrap()));
}

#[test]
fn a_server_that_ignores_end_of_input_is_killed_and_reaped() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_ignore_eof(true).unwrap();
    let mut s = open(&stub);
    let started = Instant::now();
    assert_eq!(s.close(), reaped(true));
    assert!(started.elapsed() < REPLY_CLEANUP_ALLOWANCE + HEADROOM);
    assert!(!StubServer::is_alive(stub.last_pid().unwrap()));
}

#[test]
fn a_second_close_is_not_started() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut s = open(&stub);
    assert_eq!(s.close(), reaped(false));
    assert_eq!(s.close(), Ended::NotStarted);
}

#[test]
fn a_read_only_process_starts_no_server() {
    apb_core::process_mode::testing::in_read_only_child(
        "stdio_test::a_read_only_process_starts_no_server",
        || {
            let (_tmp, stub) = stub(StubEra::Modern);
            let err = open_in(&stub, Lifecycle::Auto, 10).err().expect("refused");
            assert_eq!(err.kind, FailureKind::Refused, "{err:?}");
            assert_eq!(err.ended, Ended::NotStarted);
            assert!(!stub.dir.join("starts.log").exists());
        },
    );
}

#[tokio::test]
async fn a_session_works_inside_a_tokio_runtime() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut s = open(&stub);
    assert!(matches!(call(&s, 10), Ok(ToolAnswer::Complete { .. })));
    assert_eq!(s.close(), reaped(false));
}

#[test]
fn a_spawn_failure_names_the_program_only() {
    let tmp = tempfile::tempdir().unwrap();
    // A regular file that is not executable: it opens and hashes, and the
    // start fails.
    let path = tmp.path().join("not-executable-server");
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let err = Session::connect(
        server_from(
            program_of(&path, StartFrom::platform()),
            &["--token=secret-arg-77"],
            base_env(),
        ),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        soon(10),
    )
    .err()
    .expect("refused");
    assert_eq!(err.kind, FailureKind::Spawn, "{err:?}");
    assert_eq!(err.ended, Ended::NotStarted);
    assert!(err.message.contains("not-executable-server"), "{err:?}");
    assert!(!err.message.contains("secret-arg-77"), "{err:?}");
}

#[test]
fn a_program_that_is_missing_or_a_link_is_refused_before_any_start() {
    let (tmp, stub) = stub(StubEra::Modern);
    assert_eq!(
        ProgramFile::open(&tmp.path().join("no-such-server"), StartFrom::platform()).err(),
        Some(ProgramRefusal::NotARegularFile)
    );
    let link = tmp.path().join("link-to-stub");
    std::os::unix::fs::symlink(stub.program(), &link).unwrap();
    assert_eq!(
        ProgramFile::open(&link, StartFrom::platform()).err(),
        Some(ProgramRefusal::NotARegularFile),
        "a link to identical bytes is never followed"
    );
    assert_eq!(
        ProgramFile::open(tmp.path(), StartFrom::platform()).err(),
        Some(ProgramRefusal::NotARegularFile),
        "a directory"
    );
    assert!(stub.starts().is_empty());
}

/// Replaces the file at `path` with a script that leaves `marker` behind
/// when it runs, by a rename (a new file at the same path).
fn replace_with_marker_script(path: &std::path::Path, marker: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let swap = path.with_extension("swap");
    std::fs::write(
        &swap,
        format!("#!/bin/sh\necho ran > '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&swap, path).unwrap();
}

#[test]
fn a_program_replaced_after_the_hash_is_refused_on_the_path_checked_again() {
    // The portable path (every platform but Linux), driven on any platform
    // by its argument: the path is hashed again right before the spawn.
    let (tmp, stub) = stub(StubEra::Modern);
    let program = program_of(&stub.program(), StartFrom::PathCheckedAgain);
    let marker = tmp.path().join("replacement-ran");
    replace_with_marker_script(&stub.program(), &marker);
    let err = Session::connect(
        server_from(program, &[], base_env()),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        soon(10),
    )
    .err()
    .expect("refused");
    assert_eq!(err.kind, FailureKind::ProgramChanged, "{err:?}");
    assert_eq!(err.ended, Ended::NotStarted);
    assert!(!marker.exists(), "the replacement never ran");
    assert!(stub.starts().is_empty(), "nothing started");
}

#[test]
fn an_unchanged_program_starts_on_the_path_checked_again() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let program = program_of(&stub.program(), StartFrom::PathCheckedAgain);
    let mut s = Session::connect(
        server_from(program, &[], base_env()),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        soon(10),
    )
    .expect("opens");
    assert_eq!(s.close(), reaped(false));
    assert_eq!(
        stub.argv0_seen(),
        Some(stub.program().display().to_string())
    );
}

// Linux only: the descriptor start (`/proc/self/fd/<n>`) exists on Linux;
// every other platform takes the path checked again, tested above.
#[cfg(target_os = "linux")]
#[test]
fn a_program_replaced_after_the_hash_never_runs_from_the_descriptor() {
    let (tmp, stub) = stub(StubEra::Modern);
    let program = program_of(&stub.program(), StartFrom::Descriptor);
    let marker = tmp.path().join("replacement-ran");
    replace_with_marker_script(&stub.program(), &marker);
    let mut env = base_env();
    env.push((
        "TRACKER_TOKEN".into(),
        EnvValue::Secret(apb_core::secret::Secret::new("descriptor-token")),
    ));
    let mut s = Session::connect(
        server_from(program, &[], env),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        soon(10),
    )
    .expect("the hashed file starts");
    assert_eq!(s.close(), reaped(false));
    assert!(
        !marker.exists(),
        "the replacement never ran with the secrets"
    );
    assert_eq!(stub.starts().len(), 1, "the hashed stub ran");
    assert_eq!(
        stub.env_value_seen("TRACKER_TOKEN").as_deref(),
        Some("descriptor-token")
    );
    // A script with an interpreter line: the interpreter is handed
    // `/proc/self/fd/<n>` as the script's name.
    assert!(
        stub.argv0_seen()
            .is_some_and(|a| a.starts_with("/proc/self/fd/")),
        "{:?}",
        stub.argv0_seen()
    );
}

#[test]
fn the_server_leads_its_own_process_group() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let mut s = open(&stub);
    let pid = stub.last_pid().expect("a pid");
    assert_eq!(StubServer::process_group(pid), Some(pid));
    assert_ne!(
        StubServer::process_group(std::process::id()),
        Some(pid),
        "not the caller's group"
    );
    assert_eq!(s.close(), reaped(false));
}

#[test]
fn a_worker_the_server_forked_is_ended_with_its_group() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_fork_worker(true).unwrap();
    let mut s = open(&stub);
    let pid = stub.last_pid().expect("a pid");
    let worker = wait_for(|| stub.worker_pid()).expect("the worker started");
    assert_eq!(StubServer::process_group(worker), Some(pid), "in the group");
    assert_eq!(s.close(), reaped(false));
    // The worker was killed with the group; whoever adopted it may not
    // have reaped it yet, so a zombie counts as ended.
    assert!(
        wait_for(|| StubServer::has_exited(worker).then_some(())).is_some(),
        "the worker outlived the close"
    );
}

/// Polls `f` for up to 10 s.
fn wait_for<T>(f: impl Fn() -> Option<T>) -> Option<T> {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_call_to_a_server_that_already_exited_is_not_sent() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_exit_after_list(true).unwrap();
    let mut s = open(&stub);
    let pid = stub.last_pid().expect("a pid");
    list(&s).expect("the list");
    assert!(
        wait_for(|| StubServer::has_exited(pid).then_some(())).is_some(),
        "the stub exits after the list"
    );
    match call(&s, 10) {
        Err(CallFailure::NotSent(f)) => assert_eq!(f.kind, FailureKind::Transport, "{f:?}"),
        other => panic!("expected not sent, got {other:?}"),
    }
    assert!(stub.calls().is_empty());
    s.close();
}

/// The deadline passes while the request is queued behind a held writer:
/// the server stopped reading after the first page, and the client's
/// request for the next page (a cursor larger than any pipe buffer) filled
/// the pipe. The seal proves the call unwritten, so it is `NotSent`, and
/// no `tools/call` ever reaches the server afterwards: not the queued one
/// once the server reads again, not a later call on the same session.
#[test]
fn a_call_queued_behind_a_held_writer_is_not_sent_and_never_written() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([[tool("t")]])).unwrap();
    stub.set_next_cursor(Some(&"c".repeat(256 * 1024))).unwrap();
    stub.set_stall_after_list(true).unwrap();
    let mut s = open(&stub);
    let err = s
        .list_tools(soon(1), PAGES, TOOLS, 4 * BYTES)
        .expect_err("the second page never comes");
    assert_eq!(err.kind, FailureKind::Timeout, "{err:?}");
    match call(&s, 1) {
        Err(CallFailure::NotSent(f)) => assert_eq!(f.kind, FailureKind::Timeout, "{f:?}"),
        other => panic!("expected not sent, got {other:?}"),
    }
    stub.set_stall_after_list(false).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    match call(&s, 2) {
        Err(CallFailure::NotSent(_)) => {}
        other => panic!("a sealed session sends nothing, got {other:?}"),
    }
    s.close();
    assert!(stub.calls().is_empty(), "{:?}", stub.calls());
    assert!(
        !stub.methods().iter().any(|m| m == "tools/call"),
        "{:?}",
        stub.methods()
    );
}

/// A version the server sends is compared whole with the supported set:
/// one with anything after it, or one this client does not know, refuses
/// the session by name and never echoes what the server sent.
#[test]
fn an_unsupported_protocol_version_is_refused_without_echo() {
    for reported in ["2025-11-25 sk-reflected-7731", "2099-01-01"] {
        let (_tmp, stub) = stub(StubEra::Legacy);
        stub.set_protocol_version(reported).unwrap();
        let err = open_in(&stub, Lifecycle::Legacy, 10)
            .err()
            .expect("refused");
        assert_eq!(err.kind, FailureKind::Unsupported, "{err:?}");
        assert!(
            err.message.starts_with("mcp_protocol_unsupported:"),
            "{err:?}"
        );
        assert!(!err.message.contains("7731"), "{err:?}");
        assert!(!err.message.contains("2099"), "{err:?}");
        assert_eq!(err.ended, reaped(false), "{err:?}");
    }
}

#[test]
fn a_supported_older_version_is_kept_as_the_constant() {
    let (_tmp, stub) = stub(StubEra::Legacy);
    stub.set_protocol_version("2025-06-18").unwrap();
    let mut s = open_in(&stub, Lifecycle::Legacy, 10).expect("opens");
    assert_eq!(s.era(), &Era::Legacy("2025-06-18"));
    s.close();
}

/// Arguments nested past the parser's limit inside the request line are
/// refused by the session before anything is queued: `NotSent` with the
/// named reason, and no `tools/call` reaches the server.
#[test]
fn arguments_past_the_parser_limit_are_never_sent() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let s = open(&stub);
    let mut v = json!(0);
    for _ in 0..126 {
        v = json!([v]);
    }
    let mut args = Map::new();
    args.insert("x".into(), v);
    match s.call_tool("t", args, soon(5)) {
        Err(CallFailure::NotSent(f)) => {
            assert_eq!(f.kind, FailureKind::Refused, "{f:?}");
            assert!(f.message.starts_with("mcp_arguments_too_deep:"), "{f:?}");
        }
        other => panic!("expected not sent, got {other:?}"),
    }
    // The session still works, and the refused call never went out.
    assert!(matches!(call(&s, 5), Ok(ToolAnswer::Complete { .. })));
    assert_eq!(stub.calls().len(), 1, "{:?}", stub.calls());
}

/// The review's sequence: before the answer, the server sends a frame
/// with the listing's id that holds a method beside an old tool list,
/// then the whole response with the new one. The first is no JSON-RPC
/// frame shape, so the tap refuses its line and neither list reaches rmcp
/// or the hash: the listing fails closed.
#[test]
fn a_frame_with_a_method_never_stands_in_for_the_listing() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([tool("new")])).unwrap();
    stub.set_list_before(
        "{\"jsonrpc\":\"2.0\",\"id\":@ID@,\"method\":\"ping\",\"result\":{\"tools\":[{\"name\":\"old\",\"inputSchema\":{\"type\":\"object\"}}]}}\n",
    )
    .unwrap();
    let mut s = open(&stub);
    match list(&s) {
        Err(f) => assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}"),
        Ok(tools) => panic!("the listing was used: {tools:?}"),
    }
    s.close();
}

/// A listing whose answer lines the tap refuses: `mcp_message_invalid`,
/// and no tool list is returned.
fn refused_listing(stub: &StubServer, before: &[u8]) -> Failure {
    let mut lines = before.to_vec();
    lines.push(b'\n');
    stub.set_list_before(&lines).unwrap();
    let mut s = open(stub);
    let f = match list(&s) {
        Err(f) => f,
        Ok(tools) => panic!("the listing was used: {tools:?}"),
    };
    s.close();
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}");
    f
}

fn task_tool(support: &str) -> Value {
    json!({ "name": "t", "inputSchema": { "type": "object" },
            "execution": { "taskSupport": support } })
}

/// The trigger of review pass 3, both answers in one read: before the
/// stub's own answer (pinned `optional`), a byte order mark and an answer
/// for the same id whose unknown tool member says `required`. rmcp would
/// strip the mark and take the first; the tap refuses its line, so the
/// listing fails and neither list is used.
#[test]
fn a_byte_order_mark_never_reaches_rmcp() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([task_tool("optional")])).unwrap();
    let first =
        json!({ "jsonrpc": "2.0", "id": "@ID@", "result": { "tools": [task_tool("required")] } })
            .to_string()
            .replace("\"@ID@\"", "@ID@");
    let mut before = b"\xEF\xBB\xBF".to_vec();
    before.extend_from_slice(first.as_bytes());
    let f = refused_listing(&stub, &before);
    assert!(f.message.contains("byte order mark"), "{f:?}");
}

#[test]
fn a_duplicated_unknown_top_level_member_is_refused() {
    let (_tmp, stub) = stub(StubEra::Modern);
    refused_listing(
        &stub,
        br#"{"jsonrpc":"2.0","method":"notifications/x","x":1,"x":2}"#,
    );
}

#[test]
fn a_duplicated_member_inside_a_tool_definition_is_refused() {
    let (_tmp, stub) = stub(StubEra::Modern);
    // The stub's answer itself carries a tool with a member named twice.
    stub.set_tools_raw(
        r#"[{"name":"t","inputSchema":{"type":"object"},"execution":{"taskSupport":"optional","taskSupport":"required"}}]"#,
    )
    .unwrap();
    let mut s = open(&stub);
    let f = list(&s).expect_err("refused");
    s.close();
    assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}");
}

#[test]
fn garbage_after_the_json_value_is_refused() {
    let (_tmp, stub) = stub(StubEra::Modern);
    refused_listing(
        &stub,
        br#"{"jsonrpc":"2.0","method":"notifications/x"} garbage"#,
    );
}

/// A byte order mark on an ordinary call answer: the call was handed
/// over, the answer never reaches rmcp, so it is a lost answer.
#[test]
fn a_byte_order_mark_on_a_call_answer_is_a_lost_answer() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_call_prefix(b"\xEF\xBB\xBF").unwrap();
    let mut s = open(&stub);
    let f = lost(call(&s, 10));
    assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}");
    assert_eq!(stub.calls().len(), 1);
    s.close();
}

/// A legitimate server talks around the answer: notifications, a request
/// of its own, an error without an id and an answer to an id nobody
/// asked, all before the listing's answer. The listing works.
#[test]
fn a_chatty_legitimate_server_still_works() {
    let (_tmp, stub) = stub(StubEra::Modern);
    stub.set_tools(&json!([tool("a")])).unwrap();
    stub.set_list_before(concat!(
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{\"level\":\"info\",\"data\":\"x\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":\"srv-1\",\"method\":\"ping\"}\r\n",
        "{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32700,\"message\":\"parse\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}\n",
    ))
    .unwrap();
    let mut s = open(&stub);
    let tools = list(&s).expect("listed");
    assert_eq!(tools, vec![tool("a")]);
    assert!(matches!(call(&s, 5), Ok(ToolAnswer::Complete { .. })));
    s.close();
}

#[test]
fn a_deadline_passed_before_the_start_starts_nothing() {
    let (_tmp, stub) = stub(StubEra::Modern);
    let f = Session::connect(
        server_with(&stub, &[], base_env()),
        Lifecycle::Auto,
        "apb-test",
        "1.0.0",
        Instant::now(),
    )
    .err()
    .expect("refused");
    assert_eq!(f.kind, FailureKind::Timeout);
    assert_eq!(f.ended, Ended::NotStarted);
    assert!(stub.starts().is_empty(), "nothing was started");
}

/// A listing marked with a positive `ttlMs` is still read from the wire
/// the next time: no response cache answers it without its raw capture.
#[test]
fn a_listing_with_a_time_to_live_is_read_again_from_the_wire() {
    let (_tmp, stub) = stub(StubEra::Legacy);
    // The legacy stub adds nothing after the array, so the bytes give
    // the result its `ttlMs` member.
    stub.set_tools_raw(&format!("[{}],\"ttlMs\":60000", tool("a")))
        .unwrap();
    let mut s = open(&stub);
    assert_eq!(list(&s).expect("first listing"), vec![tool("a")]);
    assert_eq!(list(&s).expect("second listing"), vec![tool("a")]);
    let listed = stub.methods().iter().filter(|m| *m == "tools/list").count();
    assert_eq!(listed, 2, "each listing was a request");
    s.close();
}

#[test]
fn a_raw_listing_replaces_a_paged_one() {
    let (_tmp, stub) = stub(StubEra::Legacy);
    stub.set_tools(&json!([[tool("old")]])).unwrap();
    stub.set_tools_raw(&format!("[{}]", tool("new"))).unwrap();
    let mut s = open(&stub);
    assert_eq!(list(&s).expect("listing"), vec![tool("new")]);
    s.close();
}
