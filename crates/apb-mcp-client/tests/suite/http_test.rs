//! The HTTP session against a scripted Streamable HTTP server in this file:
//! a std `TcpListener` on `127.0.0.1`, one canned answer per POST by
//! JSON-RPC method, every request's headers, method and id recorded.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apb_core::secret::Secret;
use apb_mcp_client::{
    CallFailure, Ended, EnvValue, Era, Failure, FailureKind, HttpServer, Lifecycle, MAX_LINE_BYTES,
    Server, Session, ToolAnswer,
};
use serde_json::{Map, Value, json};

const PAGES: u32 = 20;
const TOOLS: usize = 500;
const BYTES: usize = 2 * 1024 * 1024;

/// One request the scripted server read.
#[derive(Clone, Debug)]
struct Req {
    method: String,
    id: Value,
    headers: Vec<(String, String)>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// What the scripted server answers one request with.
enum Reply {
    /// The whole response written, then the connection closed.
    Raw(Vec<u8>),
    /// The bytes written, the connection kept open for the duration, then
    /// the second bytes written and the connection closed.
    Hold(Vec<u8>, Duration, Vec<u8>),
    /// No answer: the connection is held open for the duration.
    Silent(Duration),
}

type ReplyFn = dyn Fn(&Req) -> Reply + Send + Sync;
type Handler = Arc<ReplyFn>;

struct Script {
    addr: SocketAddr,
    reqs: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    accepted: Arc<AtomicUsize>,
    /// Connections whose request was read and recorded, or whose read
    /// ended without one.
    read: Arc<AtomicUsize>,
    /// The accept thread, joined by [`Script::stop`].
    acceptor: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Script {
    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn reqs(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }

    fn methods(&self) -> Vec<String> {
        self.reqs().into_iter().map(|r| r.method).collect()
    }

    fn calls(&self) -> usize {
        self.methods().iter().filter(|m| *m == "tools/call").count()
    }

    /// Stops accepting and closes the listener, so a later connection is
    /// refused: the accept thread takes every connection already queued,
    /// ends and is joined (its listener dropped with it), and every
    /// accepted connection's request is read and recorded before this
    /// returns. What was sent before the stop is in [`Script::reqs`] after
    /// it, with no wait standing in for that.
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(acceptor) = self.acceptor.lock().unwrap().take() {
            acceptor.join().unwrap();
        }
        let until = Instant::now() + Duration::from_secs(10);
        while self.read.load(Ordering::SeqCst) < self.accepted.load(Ordering::SeqCst) {
            assert!(Instant::now() < until, "an accepted request was never read");
            std::thread::yield_now();
        }
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn serve(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Script {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let reqs = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicUsize::new(0));
    let read = Arc::new(AtomicUsize::new(0));
    let handler: Handler = Arc::new(handler);
    let (r, s, a, d) = (reqs.clone(), stop.clone(), accepted.clone(), read.clone());
    let acceptor = std::thread::spawn(move || {
        loop {
            // Read before the accept: a connection queued before the stop
            // is still taken, and the loop ends at the first empty accept
            // after it.
            let stopping = s.load(Ordering::SeqCst);
            match listener.accept() {
                Ok((stream, _)) => {
                    a.fetch_add(1, Ordering::SeqCst);
                    let (r, h, d) = (r.clone(), handler.clone(), d.clone());
                    std::thread::spawn(move || answer_one(stream, &r, &h, &d));
                }
                Err(_) if stopping => break,
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    });
    Script {
        addr,
        reqs,
        stop,
        accepted,
        read,
        acceptor: Mutex::new(Some(acceptor)),
    }
}

/// Reads one request, records it and counts the connection as read
/// (also when its read ends without a request), then answers it.
fn answer_one(stream: TcpStream, reqs: &Mutex<Vec<Req>>, handler: &Handler, read: &AtomicUsize) {
    let req = read_request(&stream).inspect(|req| reqs.lock().unwrap().push(req.clone()));
    read.fetch_add(1, Ordering::SeqCst);
    let Some(req) = req else {
        return;
    };
    let mut stream = stream;
    match handler(&req) {
        Reply::Raw(bytes) => {
            let _ = stream.write_all(&bytes);
        }
        Reply::Hold(first, wait, second) => {
            let _ = stream.write_all(&first);
            let _ = stream.flush();
            std::thread::sleep(wait);
            let _ = stream.write_all(&second);
        }
        Reply::Silent(wait) => std::thread::sleep(wait),
    }
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// The request of one connection: its headers, JSON-RPC method and id.
fn read_request(stream: &TcpStream) -> Option<Req> {
    stream.set_nonblocking(false).ok();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut headers = Vec::new();
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return None;
    }
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return None;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((n, v)) = l.split_once(':') {
            headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Some(Req {
        method: body
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        id: body.get("id").cloned().unwrap_or(Value::Null),
        headers,
    })
}

/// A whole HTTP/1.1 response.
fn response(status: u16, content_type: &str, extra: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} X\r\n");
    if !content_type.is_empty() {
        out.push_str(&format!("content-type: {content_type}\r\n"));
    }
    for (n, v) in extra {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    out.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// The head of an event-stream response with no length; the body follows
/// and the connection closes at its end.
fn stream_head(content_type: &str) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n")
        .into_bytes()
}

fn sse(content_type: &str, body: &str) -> Reply {
    let mut bytes = stream_head(content_type);
    bytes.extend_from_slice(body.as_bytes());
    Reply::Raw(bytes)
}

fn json_reply(value: &Value) -> Reply {
    Reply::Raw(response(
        200,
        "application/json",
        &[],
        value.to_string().as_bytes(),
    ))
}

fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn discover(id: &Value) -> Value {
    result(
        id,
        json!({
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": { "tools": {} },
            "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "scripted", "version": "1" } },
            "ttlMs": 0,
            "cacheScope": "private"
        }),
    )
}

fn tool(name: &str) -> Value {
    json!({ "name": name, "inputSchema": { "type": "object" } })
}

fn listing(id: &Value, tools: Value) -> Value {
    result(
        id,
        json!({ "tools": tools, "resultType": "complete", "ttlMs": 0, "cacheScope": "private" }),
    )
}

fn called(id: &Value, text: &str) -> Value {
    result(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "resultType": "complete" }),
    )
}

/// The default answers: discover, one tool, a call answered `ok`, and a
/// notification accepted.
fn default_reply(req: &Req) -> Reply {
    match req.method.as_str() {
        "server/discover" => json_reply(&discover(&req.id)),
        "tools/list" => json_reply(&listing(&req.id, json!([tool("t")]))),
        "tools/call" => json_reply(&called(&req.id, "ok")),
        _ => Reply::Raw(response(202, "", &[], b"")),
    }
}

/// The default answers with `method` answered by `f`.
fn with(method: &'static str, f: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Script {
    serve(move |req| {
        if req.method == method {
            f(req)
        } else {
            default_reply(req)
        }
    })
}

fn http_server(url: String) -> Server {
    Server::Http(HttpServer {
        url,
        headers: Vec::new(),
        bearer: None,
    })
}

fn connect_to(server: Server, secs: u64) -> Result<Session, Failure> {
    Session::connect(
        server,
        Lifecycle::Modern,
        "apb-test",
        "1.0.0",
        Instant::now() + Duration::from_secs(secs),
    )
}

fn open(script: &Script) -> Session {
    connect_to(http_server(script.url()), 10).expect("the session opens")
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

fn text_of(answer: &ToolAnswer) -> String {
    match answer {
        ToolAnswer::Complete { content, .. } => content[0]["text"].as_str().unwrap().to_string(),
        other => panic!("not complete: {other:?}"),
    }
}

fn err_of<T>(r: Result<T, Failure>) -> Failure {
    match r {
        Ok(_) => panic!("expected a failure"),
        Err(f) => f,
    }
}

#[test]
fn a_modern_http_server_lists_and_calls() {
    let script = serve(default_reply);
    let session = open(&script);
    assert_eq!(session.era(), &Era::Modern("2026-07-28"));
    let tools = list(&session).unwrap();
    assert_eq!(tools, vec![tool("t")]);
    let answer = call(&session, 10).unwrap();
    assert_eq!(text_of(&answer), "ok");
    assert_eq!(
        script.methods(),
        vec!["server/discover", "tools/list", "tools/call"]
    );
}

#[test]
fn the_raw_tools_list_is_captured() {
    let raw = json!({ "name": "t", "inputSchema": { "type": "object" }, "execution": { "taskSupport": "never", "x-extra": [1, 2] } });
    let listed = raw.clone();
    let script = with("tools/list", move |req| {
        json_reply(&listing(&req.id, json!([listed.clone()])))
    });
    let session = open(&script);
    assert_eq!(list(&session).unwrap(), vec![raw]);
}

#[test]
fn the_headers_and_bearer_are_sent() {
    let script = serve(default_reply);
    let session = connect_to(
        Server::Http(HttpServer {
            url: script.url(),
            headers: vec![
                ("X-Api-Key".into(), EnvValue::Secret(Secret::new("key-1"))),
                ("X-Tenant".into(), EnvValue::Plain("acme".into())),
            ],
            bearer: Some(Secret::new("tok-9")),
        }),
        10,
    )
    .unwrap();
    call(&session, 10).unwrap();
    let reqs = script.reqs();
    let call = reqs.iter().find(|r| r.method == "tools/call").unwrap();
    let accept = call.header("accept").unwrap();
    assert!(accept.contains("application/json") && accept.contains("text/event-stream"));
    assert_eq!(call.header("content-type"), Some("application/json"));
    assert_eq!(call.header("mcp-protocol-version"), Some("2026-07-28"));
    assert_eq!(call.header("mcp-method"), Some("tools/call"));
    assert_eq!(call.header("x-api-key"), Some("key-1"));
    assert_eq!(call.header("x-tenant"), Some("acme"));
    assert_eq!(call.header("authorization"), Some("Bearer tok-9"));
    for r in &reqs {
        assert_eq!(
            r.header("authorization"),
            Some("Bearer tok-9"),
            "{}",
            r.method
        );
    }
}

/// The marker of the child process a proxy test runs its body in.
const CHILD: &str = "APB_HTTP_TEST_CHILD";

#[test]
fn the_callers_proxy_is_ignored() {
    if let Ok(url) = std::env::var(CHILD) {
        let session = connect_to(http_server(url), 10).expect("the session opens");
        list(&session).expect("the listing");
        call(&session, 10).expect("the call");
        return;
    }
    let script = serve(default_reply);
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "http_test::the_callers_proxy_is_ignored",
            "--nocapture",
        ])
        .env(CHILD, script.url())
        .env("HTTPS_PROXY", &proxy_url)
        .env("HTTP_PROXY", &proxy_url)
        .env("https_proxy", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .status()
        .unwrap();
    assert!(status.success(), "the child's body passed");
    assert!(proxy.accept().is_err(), "the proxy saw no connection");
    assert_eq!(
        script.methods(),
        vec!["server/discover", "tools/list", "tools/call"]
    );
}

#[test]
fn a_redirect_is_not_followed() {
    let second = serve(default_reply);
    let target = second.url();
    let script =
        serve(move |_| Reply::Raw(response(307, "", &[("location", target.as_str())], b"")));
    let f = err_of(connect_to(http_server(script.url()), 10));
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert!(second.reqs().is_empty(), "the second server saw nothing");
}

#[test]
fn a_401_while_connecting_is_auth() {
    let script = with("server/discover", |_| {
        Reply::Raw(response(401, "", &[], b""))
    });
    let f = err_of(connect_to(http_server(script.url()), 10));
    assert_eq!(f.kind, FailureKind::Auth { status: 401 }, "{f:?}");
    assert_eq!(f.ended, Ended::NotStarted);
}

#[test]
fn a_401_on_the_call_is_lost_auth() {
    let script = with("tools/call", |_| Reply::Raw(response(401, "", &[], b"")));
    let session = open(&script);
    match call(&session, 10) {
        Err(CallFailure::Lost(f)) => assert_eq!(f.kind, FailureKind::Auth { status: 401 }),
        other => panic!("not lost: {other:?}"),
    }
}

#[test]
fn a_json_rpc_error_is_answered_rejected() {
    for status in [200u16, 500] {
        let script = with("tools/call", move |req| {
            let body = json!({ "jsonrpc": "2.0", "id": req.id, "error": { "code": -32602, "message": "bad" } });
            Reply::Raw(response(
                status,
                "application/json",
                &[],
                body.to_string().as_bytes(),
            ))
        });
        let session = open(&script);
        match call(&session, 10) {
            Err(CallFailure::Answered(f)) => {
                assert_eq!(f.kind, FailureKind::Rejected { code: -32602 }, "{status}")
            }
            other => panic!("{status}: not answered: {other:?}"),
        }
    }
}

fn event(value: &Value) -> String {
    format!("data: {value}\n\n")
}

fn note() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": { "level": "info", "data": "x" } })
}

#[test]
fn an_event_stream_answer_is_read() {
    let script = with("tools/call", |req| {
        sse(
            "text/event-stream",
            &format!(
                "{}{}{}",
                event(&note()),
                event(&note()),
                event(&called(&req.id, "streamed"))
            ),
        )
    });
    let session = open(&script);
    assert_eq!(text_of(&call(&session, 10).unwrap()), "streamed");
}

/// The `tools/call` answer `result` as a JSON body, or as the answering
/// event of an event stream after a notification's event.
fn call_answered(result_json: &'static str, stream: bool) -> Script {
    with("tools/call", move |req| {
        let raw: Value = serde_json::from_str(result_json).unwrap();
        let answer = result(&req.id, raw);
        if stream {
            sse(
                "text/event-stream",
                &format!("{}{}", event(&note()), event(&answer)),
            )
        } else {
            json_reply(&answer)
        }
    })
}

/// Over HTTP as over stdio, an answer whose `result` is no call result
/// (none of `content`, `structuredContent` and `isError`) stays an
/// unexpected answer, as apb 1.0 read it, and the call is lost (a write
/// `unknown`), never complete: in a JSON body and as the answering event
/// of a stream, whose frame must pass the line tap before rmcp decodes it.
#[test]
fn an_answer_that_is_no_call_result_stays_unexpected() {
    for raw in [
        "{}",
        r#"{"resultType":"complete"}"#,
        r#"{"_meta":{"example.com/replica":"r1"}}"#,
        r#"{"content":null}"#,
    ] {
        for stream in [false, true] {
            let script = call_answered(raw, stream);
            let session = open(&script);
            match call(&session, 10) {
                Err(CallFailure::Lost(f)) => {
                    assert_eq!(f.kind, FailureKind::Transport, "{raw} {stream}: {f:?}");
                    assert_eq!(f.message, "Unexpected response type", "{raw} {stream}");
                }
                other => panic!("{raw} {stream}: not lost: {other:?}"),
            }
            assert_eq!(script.calls(), 1, "{raw} {stream}");
        }
    }
}

/// An empty `content` list is a call result over HTTP too, in a JSON body
/// and in a stream: a complete answer.
#[test]
fn an_empty_content_list_is_a_complete_answer() {
    for stream in [false, true] {
        let script = call_answered(r#"{"content":[]}"#, stream);
        let session = open(&script);
        match call(&session, 10) {
            Ok(ToolAnswer::Complete {
                content,
                structured,
                is_error,
            }) => {
                assert!(content.is_empty(), "{stream}");
                assert_eq!(structured, None, "{stream}");
                assert!(!is_error, "{stream}");
            }
            other => panic!("{stream}: not complete: {other:?}"),
        }
    }
}

#[test]
fn an_event_stream_past_the_message_bound_is_too_large() {
    let script = with("tools/list", |req| {
        let mut body = String::new();
        let one = event(&note());
        for _ in 0..10_001 {
            body.push_str(&one);
        }
        body.push_str(&event(&listing(&req.id, json!([]))));
        sse("text/event-stream", &body)
    });
    let session = open(&script);
    let f = err_of(list(&session));
    assert_eq!(f.kind, FailureKind::TooLarge, "{f:?}");
    assert!(f.message.starts_with("mcp_message_too_large:"), "{f:?}");
}

#[test]
fn a_large_answer_is_too_large() {
    let script = with("tools/list", |req| {
        let pad = "a".repeat(MAX_LINE_BYTES);
        json_reply(&listing(
            &req.id,
            json!([{ "name": "t", "description": pad, "inputSchema": { "type": "object" } }]),
        ))
    });
    let session = open(&script);
    let f = err_of(list(&session));
    assert_eq!(f.kind, FailureKind::TooLarge, "{f:?}");
    assert!(f.message.starts_with("mcp_message_too_large:"), "{f:?}");
}

#[test]
fn a_session_id_from_the_server_is_refused() {
    let script = with("server/discover", |req| {
        Reply::Raw(response(
            200,
            "application/json",
            &[("mcp-session-id", "sess-ECHO-7731")],
            discover(&req.id).to_string().as_bytes(),
        ))
    });
    let f = err_of(connect_to(http_server(script.url()), 10));
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert!(f.message.contains("stateless"), "{f:?}");
    assert!(!f.message.contains("7731"), "{f:?}");
}

#[test]
fn close_of_an_http_session_is_not_started() {
    let script = serve(default_reply);
    let mut session = open(&script);
    list(&session).unwrap();
    assert_eq!(session.close(), Ended::NotStarted);
    assert_eq!(session.close(), Ended::NotStarted);
}

#[test]
fn an_http_session_works_inside_a_tokio_runtime() {
    let script = serve(default_reply);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let session = open(&script);
        assert_eq!(list(&session).unwrap().len(), 1);
        assert_eq!(text_of(&call(&session, 10).unwrap()), "ok");
    });
}

fn listing_body(body: &'static [u8]) -> Script {
    with("tools/list", move |req| {
        let text = String::from_utf8_lossy(body).replace("@ID@", &req.id.to_string());
        let mut bytes = Vec::new();
        if body.starts_with(b"\xEF\xBB\xBF") {
            bytes.extend_from_slice(b"\xEF\xBB\xBF");
            bytes.extend_from_slice(text.trim_start_matches('\u{feff}').as_bytes());
        } else {
            bytes.extend_from_slice(text.as_bytes());
        }
        Reply::Raw(response(200, "application/json", &[], &bytes))
    })
}

#[test]
fn a_byte_order_mark_body_never_reaches_rmcp() {
    let script =
        listing_body(b"\xEF\xBB\xBF{\"jsonrpc\":\"2.0\",\"id\":@ID@,\"result\":{\"tools\":[]}}");
    let session = open(&script);
    let f = err_of(list(&session));
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}");
    assert!(f.message.contains("byte order mark"), "{f:?}");
}

#[test]
fn a_duplicated_member_in_a_body_is_refused() {
    let script = listing_body(
        br#"{"jsonrpc":"2.0","id":@ID@,"result":{"tools":[{"name":"a","name":"t","inputSchema":{"type":"object"}}]}}"#,
    );
    let session = open(&script);
    let f = err_of(list(&session));
    assert!(f.message.contains("each member named once"), "{f:?}");
}

#[test]
fn garbage_after_the_json_body_is_refused() {
    let script = listing_body(br#"{"jsonrpc":"2.0","id":@ID@,"result":{"tools":[]}} x"#);
    let session = open(&script);
    let f = err_of(list(&session));
    assert!(f.message.contains("before or after"), "{f:?}");
}

#[test]
fn a_bad_event_before_the_answer_hands_on_nothing() {
    let script = with("tools/call", |req| {
        sse(
            "text/event-stream",
            &format!("data: not a frame\n\n{}", event(&called(&req.id, "late"))),
        )
    });
    let session = open(&script);
    match call(&session, 10) {
        Err(CallFailure::Lost(f)) => {
            assert_eq!(f.kind, FailureKind::Transport);
            assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}");
        }
        other => panic!("not lost: {other:?}"),
    }
}

#[test]
fn an_unknown_event_field_is_refused() {
    let script = with("tools/list", |req| {
        sse(
            "text/event-stream",
            &format!(
                "foo: bar\ndata: {}\n\n",
                listing(&req.id, json!([tool("t")]))
            ),
        )
    });
    let session = open(&script);
    let f = err_of(list(&session));
    assert!(f.message.contains("event field"), "{f:?}");
}

#[test]
fn a_priming_event_without_data_is_skipped() {
    let script = with("tools/list", |req| {
        sse(
            "text/event-stream",
            &format!(
                "id: 1\ndata:\n\n: a comment\n{}",
                event(&listing(&req.id, json!([tool("t")])))
            ),
        )
    });
    let session = open(&script);
    assert_eq!(list(&session).unwrap(), vec![tool("t")]);
}

#[test]
fn the_field_value_loses_one_leading_space() {
    for (prefix, ok) in [("data:  ", false), ("data: ", true), ("data:", true)] {
        let script = with("tools/list", move |req| {
            sse(
                "text/event-stream",
                &format!("{prefix}{}\n\n", listing(&req.id, json!([tool("t")]))),
            )
        });
        let session = open(&script);
        assert_eq!(list(&session).is_ok(), ok, "{prefix:?}");
    }
}

#[test]
fn the_stream_is_closed_after_the_answer() {
    let script = with("tools/call", |req| {
        let mut first = stream_head("text/event-stream");
        first.extend_from_slice(event(&called(&req.id, "early")).as_bytes());
        Reply::Hold(first, Duration::from_secs(15), b"foo: bar\n\n".to_vec())
    });
    let session = open(&script);
    let started = Instant::now();
    assert_eq!(text_of(&call(&session, 30).unwrap()), "early");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "answered before the server's later bytes: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_media_type_with_parameters_is_read() {
    let script = with("tools/list", |req| {
        Reply::Raw(response(
            200,
            "application/json; charset=utf-8",
            &[],
            listing(&req.id, json!([tool("t")])).to_string().as_bytes(),
        ))
    });
    assert_eq!(list(&open(&script)).unwrap().len(), 1);
    let script = with("tools/list", |req| {
        sse(
            "Text/Event-Stream",
            &event(&listing(&req.id, json!([tool("t")]))),
        )
    });
    assert_eq!(list(&open(&script)).unwrap().len(), 1);
    let script = with("tools/list", |req| {
        Reply::Raw(response(
            200,
            "application/jsonx",
            &[],
            listing(&req.id, json!([tool("t")])).to_string().as_bytes(),
        ))
    });
    let f = err_of(list(&open(&script)));
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert!(f.message.contains("content type"), "{f:?}");
}

#[test]
fn an_accepted_request_is_a_transport_fault() {
    let script = with("tools/list", |_| Reply::Raw(response(202, "", &[], b"")));
    let session = open(&script);
    let started = Instant::now();
    let f = err_of(session.list_tools(soon(20), PAGES, TOOLS, BYTES));
    assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
    assert!(f.message.contains("without answering"), "{f:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "no wait to the deadline"
    );
}

#[test]
fn nothing_is_posted_after_the_seal() {
    let script = with("tools/call", |_| Reply::Silent(Duration::from_secs(5)));
    let session = open(&script);
    match call(&session, 1) {
        Err(CallFailure::Lost(f)) => assert_eq!(f.kind, FailureKind::Timeout, "{f:?}"),
        other => panic!("not lost: {other:?}"),
    }
    // A listing after the seal is not posted either: the seal stops
    // every message, not only a call.
    assert!(list(&session).is_err());
    // The session closed and the server stopped: every request it was
    // sent is recorded.
    drop(session);
    script.stop();
    assert_eq!(
        script.methods(),
        vec!["server/discover", "tools/call"],
        "nothing after the seal"
    );
}

#[test]
fn a_tool_with_a_header_annotation_fails_the_http_listing_closed() {
    let annotated = [
        json!({ "name": "t", "inputSchema": { "type": "object", "properties": { "region": { "type": "string", "x-mcp-header": "Region" } } } }),
        json!({ "name": "t", "inputSchema": { "type": "object", "properties": { "region": { "type": "string", "x-mcp-header": 5 } } } }),
    ];
    for annotated_tool in annotated {
        let listed = annotated_tool.clone();
        let script = with("tools/list", move |req| {
            json_reply(&listing(&req.id, json!([tool("ok"), listed.clone()])))
        });
        let session = open(&script);
        let f = err_of(list(&session));
        assert_eq!(f.kind, FailureKind::Transport, "{f:?}");
        assert!(
            f.message.starts_with("mcp_header_annotation_unsupported:"),
            "{f:?}"
        );
        assert!(matches!(call(&session, 5), Err(CallFailure::NotSent(_))));
        assert_eq!(script.calls(), 0, "no tools/call");
    }
}

#[test]
fn a_reserved_server_header_is_refused() {
    for name in ["Content-Type", "mcp-method", "HOST"] {
        let script = serve(default_reply);
        let f = err_of(connect_to(
            Server::Http(HttpServer {
                url: script.url(),
                headers: vec![(name.into(), EnvValue::Plain("x".into()))],
                bearer: None,
            }),
            10,
        ));
        assert!(
            f.message.starts_with("mcp_header_unsupported:"),
            "{name}: {f:?}"
        );
        assert!(script.reqs().is_empty(), "{name}: the server saw nothing");
    }
    // `Authorization` is reserved only beside a bearer token.
    let script = serve(default_reply);
    let f = err_of(connect_to(
        Server::Http(HttpServer {
            url: script.url(),
            headers: vec![("Authorization".into(), EnvValue::Plain("x".into()))],
            bearer: Some(Secret::new("tok")),
        }),
        10,
    ));
    assert!(f.message.starts_with("mcp_header_unsupported:"), "{f:?}");
    assert!(script.reqs().is_empty());
}

#[test]
fn a_refused_connection_after_the_call_started_is_lost() {
    let script = serve(default_reply);
    let session = open(&script);
    script.stop();
    let before = script.accepted.load(Ordering::SeqCst);
    match call(&session, 5) {
        Err(CallFailure::Lost(f)) => assert_eq!(f.kind, FailureKind::Transport, "{f:?}"),
        other => panic!("not lost: {other:?}"),
    }
    assert_eq!(script.accepted.load(Ordering::SeqCst), before);
}

#[test]
fn a_call_after_the_seal_is_not_sent_and_never_posted() {
    let script = with("tools/call", |_| Reply::Silent(Duration::from_secs(4)));
    let session = open(&script);
    assert!(matches!(call(&session, 1), Err(CallFailure::Lost(_))));
    match call(&session, 5) {
        Err(CallFailure::NotSent(_)) => {}
        other => panic!("not refused unsent: {other:?}"),
    }
    drop(session);
    script.stop();
    assert_eq!(script.calls(), 1, "the second call was never posted");
}

#[test]
fn no_fault_echoes_the_server() {
    const ECHO: &str = "ECHO-7731";
    let replies: Vec<Box<ReplyFn>> = vec![
        Box::new(|_| Reply::Raw(response(500, "text/plain", &[], ECHO.as_bytes()))),
        Box::new(|_| {
            Reply::Raw(response(
                401,
                "text/plain",
                &[("www-authenticate", ECHO)],
                ECHO.as_bytes(),
            ))
        }),
        Box::new(|_| Reply::Raw(response(200, "text/ECHO-7731", &[], ECHO.as_bytes()))),
        Box::new(|req| {
            Reply::Raw(response(
                200,
                "application/json",
                &[("mcp-session-id", ECHO)],
                listing(&req.id, json!([])).to_string().as_bytes(),
            ))
        }),
        Box::new(|_| Reply::Raw(response(200, "application/json", &[], ECHO.as_bytes()))),
        Box::new(|_| sse("text/event-stream", "data: ECHO-7731\n\n")),
        Box::new(|_| sse("text/event-stream", "ECHO-7731: x\n\n")),
    ];
    for (n, reply) in replies.into_iter().enumerate() {
        let reply: Arc<ReplyFn> = Arc::from(reply);
        let script = with("tools/list", move |req| reply(req));
        let session = open(&script);
        let f = err_of(list(&session));
        assert!(
            !f.message
                .to_ascii_lowercase()
                .contains(&ECHO.to_ascii_lowercase()),
            "case {n}: {f:?}"
        );
    }
}

#[test]
fn a_plain_text_body_ends_the_session() {
    let script = with("tools/list", |_| {
        Reply::Raw(response(200, "application/json", &[], b"hello"))
    });
    let session = open(&script);
    let f = err_of(list(&session));
    assert!(f.message.starts_with("mcp_message_invalid:"), "{f:?}");
    assert!(f.message.contains("bytes before or after"), "{f:?}");
    assert_eq!(session.dropped_lines(), 0);
}

/// A body built from the request's id.
type BodyFn = dyn Fn(&Value) -> Value + Send + Sync;

/// A 2xx JSON body that does not answer the request (a server request, or
/// another id's response) ends the session `mcp_message_invalid` before
/// rmcp sees it: rmcp never answers a server request with one more POST,
/// and the call does not wait to its deadline (Fix round 1).
#[test]
fn a_json_body_that_is_not_the_answer_is_never_handed_on() {
    let bodies: Vec<Box<BodyFn>> = vec![
        Box::new(|_| json!({ "jsonrpc": "2.0", "id": 99, "method": "ping" })),
        Box::new(|id| {
            let other = if id == &json!(77) {
                json!(78)
            } else {
                json!(77)
            };
            called(&other, "not yours")
        }),
    ];
    for (n, body) in bodies.into_iter().enumerate() {
        let body: Arc<BodyFn> = Arc::from(body);
        let script = with("tools/call", move |req| json_reply(&body(&req.id)));
        let session = open(&script);
        let started = Instant::now();
        match call(&session, 20) {
            Err(CallFailure::Lost(f)) => {
                assert!(
                    f.message.starts_with("mcp_message_invalid:"),
                    "case {n}: {f:?}"
                );
            }
            other => panic!("case {n}: not lost invalid: {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "case {n}: no wait to the deadline"
        );
        drop(session);
        script.stop();
        assert_eq!(
            script.methods(),
            vec!["server/discover", "tools/call"],
            "case {n}: no further POST"
        );
    }
}

/// A response head of `content_type` whose body starts and then stalls
/// past the request's deadline: the client hangs up first.
fn stalled(content_type: &str) -> Reply {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: 64\r\nconnection: close\r\n\r\n{{\"jsonrpc\":"
    );
    Reply::Hold(head.into_bytes(), Duration::from_secs(8), Vec::new())
}

#[test]
fn a_stalled_body_of_a_listing_is_a_timeout_for_both_readers() {
    for content_type in ["application/json", "text/event-stream"] {
        let script = with("tools/list", move |_| stalled(content_type));
        let session = open(&script);
        let started = Instant::now();
        let f = err_of(session.list_tools(soon(1), PAGES, TOOLS, BYTES));
        assert_eq!(f.kind, FailureKind::Timeout, "{content_type}: {f:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{content_type}: ended at the deadline"
        );
    }
}

#[test]
fn a_stalled_body_of_a_call_is_a_lost_timeout_for_both_readers() {
    for content_type in ["application/json", "text/event-stream"] {
        let script = with("tools/call", move |_| stalled(content_type));
        let session = open(&script);
        match call(&session, 1) {
            // Handed over and never answered: lost, so a write settles
            // unknown, and the class is the timeout's.
            Err(CallFailure::Lost(f)) => {
                assert_eq!(f.kind, FailureKind::Timeout, "{content_type}: {f:?}")
            }
            other => panic!("{content_type}: not lost: {other:?}"),
        }
    }
}
