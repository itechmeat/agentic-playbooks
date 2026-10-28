//! Shared test helpers for the engine (schema 2). Not a separate test binary -
//! declared once as `mod common;` in `../main.rs` and reached from every suite
//! module via `use crate::common;`.
#![allow(dead_code)]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_engine::event::{Event, EventPayload};

/// One process-wide lock serializing every test that mutates shared env
/// (`APB_AGENT_CMD`, `APB_CONFIG_DIR`, `HOME`, `PATH`,
/// `APB_SUPERVISOR_HEARTBEAT_MS`, `APB_TEST_DUMP`, ...). Before consolidation
/// each file was its own process, so per-file `static ENV_LOCK`s sufficed;
/// now that all 46 former files run as modules (threads) of one binary, only a
/// single shared lock prevents one module's env mutation from racing another's.
/// `std::sync::Mutex` (not `tokio::sync::Mutex`) is correct here: every
/// env-mutating test in this crate is a plain `#[test]` - none are
/// `#[tokio::test]` and none hold the guard across an `.await` - so a sync
/// mutex, poison-tolerant via `unwrap_or_else(|e| e.into_inner())`, is the
/// minimal correct choice.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// One process env var set for the life of the guard and restored on drop,
/// even when the test panics. Hold [`env_lock`] for as long as the guard
/// lives: the lock is what keeps every other env-touching test out while
/// the value is changed.
pub struct EnvVar {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvVar {
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: the caller holds `env_lock`, so no other test reads or
        // writes the process env while this runs.
        unsafe { std::env::set_var(key, value) };
        EnvVar { key, prior }
    }

    /// `key` unset for the life of the guard, restored on drop.
    pub fn remove(key: &'static str) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: as in `set`.
        unsafe { std::env::remove_var(key) };
        EnvVar { key, prior }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        // SAFETY: as in `set`, the env lock is still held.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Writes `content` to `path` and fsyncs. On Linux, `fs::write` without
/// fsync can cause the following `execve` to fail with `ETXTBSY` when the
/// file is a shebang script executed directly (not via `sh script.sh`),
/// because the page cache may still hold dirty pages after close. Use this
/// for any file that will be exec'd in the same test.
pub fn write_sync(path: &Path, content: &str) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.sync_all().unwrap();
}

/// Seeds a profile to disk: `<root>/.apb/profiles/<name>/{profile.yaml,SOUL.md}`.
/// The agent/model under the stub agent (APB_AGENT_CMD) do not matter - what matters
/// is only that the profile resolves and builds an invocation chain.
pub fn seed_profile(root: &Path, name: &str, agent: &str, model: &str, fallbacks: &[(&str, &str)]) {
    let dir = root.join(".apb/profiles").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut y =
        format!("name: {name}\ndescription: test\nexecutor:\n  agent: {agent}\n  model: {model}\n");
    if !fallbacks.is_empty() {
        y.push_str("  fallbacks:\n");
        for (a, m) in fallbacks {
            y.push_str(&format!("    - {{ agent: {a}, model: {m} }}\n"));
        }
    }
    std::fs::write(dir.join("profile.yaml"), y).unwrap();
    std::fs::write(dir.join("SOUL.md"), "").unwrap();
}

/// Profile `main` under the stub agent (a single executor, no fallbacks).
pub fn seed_main(root: &Path) {
    seed_profile(root, "main", "claude-code", "haiku", &[]);
}

/// The one journal shape every cancellation write-off must produce (#89): a
/// paired `NodeStarted` immediately before the cancelled `NodeFinished`, and
/// `output: "cancelled"` on that finish. Shared across `stop_run_test`,
/// `failure_stop_test` and `parallel_e2e_test` because each pins the same
/// invariant against a different write-off site in `apb-engine`'s scheduler
/// (the batch admission loop's two cases, `stop_on_unhandled_failure`'s
/// frontier cancel, and `advance_frontier`'s join:any sibling cancel) - one
/// assertion for one shape rather than four near-duplicates.
pub fn assert_paired_cancelled_shape(events: &[Event], fixture: &str) {
    let cancelled: Vec<(usize, String, String)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.payload {
            EventPayload::NodeFinished {
                node,
                status,
                output,
                ..
            } if status == "cancelled" => Some((i, node.clone(), output.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !cancelled.is_empty(),
        "{fixture} must produce at least one cancelled member"
    );
    for (at, node, output) in cancelled {
        assert!(
            events[..at].iter().any(|e| matches!(
                &e.payload,
                EventPayload::NodeStarted { node: n, .. } if *n == node
            )),
            "{fixture}: cancelled member {node} has no preceding NodeStarted"
        );
        assert_eq!(
            output, "cancelled",
            "{fixture}: both cancel paths must render the same output text for {node}"
        );
    }
}

// --- Ephemeral one-shot HTTP server (shared with the connector-call tests) ---

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A canned one-shot HTTP server on `127.0.0.1:0`: it serves a single request
/// with a fixed response and captures the raw request text (head + body) for
/// assertions such as "was the auth header injected". `base_url` is the
/// `http://127.0.0.1:<port>` origin to point a connector account's `base_url`
/// at. The serving thread joins on drop.
pub struct TestHttpServer {
    pub base_url: String,
    addr: std::net::SocketAddr,
    request: Arc<Mutex<Option<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl TestHttpServer {
    /// The raw request the server received, once it has served (call after the
    /// request under test has completed).
    pub fn captured_request(&self) -> Option<String> {
        self.request.lock().unwrap().clone()
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            // A test that errored out before ever connecting leaves the
            // thread waiting; it polls `stop` and leaves on its own.
            let _ = h.join();
        }
    }
}

/// Spawns a [`TestHttpServer`] that answers one request with `status`/`reason`,
/// the given extra `headers`, and `body`. `Content-Length` is always set from
/// `body`; `Content-Type: application/json` is added when the caller supplies
/// no content type of its own.
pub fn spawn_http(
    status: u16,
    reason: &str,
    headers: &[(&str, &str)],
    body: String,
) -> TestHttpServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();

    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n",
        body.len()
    );
    let mut has_ctype = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-type") {
            has_ctype = true;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_ctype {
        head.push_str("Content-Type: application/json\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    let mut response = head.into_bytes();
    response.extend_from_slice(body.as_bytes());

    let request = Arc::new(Mutex::new(None));
    let req_slot = request.clone();
    let handle = std::thread::spawn(move || {
        if let Some(mut stream) = accept_until_stopped(&listener, &stopped) {
            let captured = read_http_request(&mut stream);
            *req_slot.lock().unwrap() = Some(captured);
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });

    TestHttpServer {
        base_url,
        addr,
        request,
        stop,
        handle: Some(handle),
    }
}

/// Reads one HTTP request (request line + headers, then a `Content-Length`
/// or `Transfer-Encoding: chunked` body when present) and returns the raw
/// text: the head followed by the decoded body.
fn read_http_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut content_length = 0usize;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:").map(str::trim) {
            content_length = rest.parse().unwrap_or(0);
        }
        if let Some(rest) = lower.strip_prefix("transfer-encoding:") {
            chunked = rest.split(',').any(|t| t.trim() == "chunked");
        }
        let done = line == "\r\n" || line == "\n";
        head.push_str(&line);
        if done {
            break;
        }
    }
    if chunked {
        // ureq 3 sends a body-less POST as `transfer-encoding: chunked` and
        // writes the `0\r\n\r\n` terminator as a separate write. Replying and
        // closing with those bytes still unread makes the kernel send an RST,
        // and on macOS the client's next `setsockopt` then fails with EINVAL
        // ("Invalid argument (os error 22)"). Drain every chunk, the zero
        // chunk and the trailers before replying.
        head.push_str(&read_chunked_body(&mut reader));
    } else if content_length > 0 {
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_ok() {
            head.push_str(&String::from_utf8_lossy(&body));
        }
    }
    head
}

/// Decodes a chunked request body up to and including the zero-size chunk
/// and the trailer section, returning the concatenated chunk data. Stops
/// early (best-effort) on EOF or a malformed size line.
fn read_chunked_body(reader: &mut impl BufRead) -> String {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line).unwrap_or(0) == 0 {
            return String::from_utf8_lossy(&body).into_owned();
        }
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_hex, 16) else {
            return String::from_utf8_lossy(&body).into_owned();
        };
        if size == 0 {
            break;
        }
        let mut data = vec![0u8; size + 2];
        if reader.read_exact(&mut data).is_err() {
            return String::from_utf8_lossy(&body).into_owned();
        }
        body.extend_from_slice(&data[..size]);
    }
    // Optional trailers, ended by an empty line.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// The stub must drain a chunked request body through the zero chunk and the
/// trailers before it replies, and record the decoded data after the head.
#[test]
fn http_stub_consumes_chunked_request_body() {
    let server = spawn_http(200, "OK", &[], "{}".to_string());
    let mut client = TcpStream::connect(server.addr).unwrap();
    client
        .write_all(b"POST /x HTTP/1.1\r\nHost: t\r\nTransfer-Encoding: Chunked\r\n\r\n")
        .unwrap();
    client.write_all(b"5;ext=1\r\nhello\r\n").unwrap();
    client.write_all(b"6\r\n world\r\n").unwrap();
    client.write_all(b"0\r\nX-Trailer: y\r\n\r\n").unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let captured = server.captured_request().unwrap();
    assert!(
        captured.ends_with("Transfer-Encoding: Chunked\r\n\r\nhello world"),
        "{captured:?}"
    );
}

/// Makes an answer's status and body from the requests served so far.
pub type ReplyFn = Box<dyn FnOnce(&[String]) -> (u16, String) + Send>;

/// One answer of a [`ScriptedHttpServer`].
pub enum Reply {
    /// Answer with this status and JSON body.
    Respond(u16, String),
    /// Answer with a status and a body computed from every request served
    /// so far, this one last (to echo what an earlier request sent).
    From(ReplyFn),
    /// Read the request, then close the connection without an answer: the
    /// client sees a network failure, so it cannot know what happened.
    Cut,
    /// Signal `arrived` once the request is read, then wait for `release`
    /// before answering with status 200 and this body.
    Held {
        body: String,
        arrived: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    },
}

/// A stub HTTP server on `127.0.0.1:0` that serves one connection per
/// scripted [`Reply`], in order, and records every request (head + body).
/// After the script it accepts nothing more, so an unexpected extra request
/// fails to connect. The serving thread joins on drop.
pub struct ScriptedHttpServer {
    pub base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ScriptedHttpServer {
    /// Every request served so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ScriptedHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            // The serving thread polls `stop` while it waits for a
            // connection, so it leaves on its own: no wake-up connection
            // that could reach a port another test holds by now.
            let _ = h.join();
        }
    }
}

/// The next connection of `listener` (non-blocking), as a blocking stream,
/// or `None` once `stop` is set or the listener fails.
fn accept_until_stopped(
    listener: &TcpListener,
    stop: &std::sync::atomic::AtomicBool,
) -> Option<TcpStream> {
    loop {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // An accepted socket may inherit the listener's mode.
                stream.set_nonblocking(false).ok()?;
                return Some(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// Spawns a [`ScriptedHttpServer`] for `script`.
pub fn spawn_script(script: Vec<Reply>) -> ScriptedHttpServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (seen, stopped) = (requests.clone(), stop.clone());
    // Non-blocking, so the thread can watch `stop` while it waits.
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        for reply in script {
            let Some(mut stream) = accept_until_stopped(&listener, &stopped) else {
                return;
            };
            let captured = read_http_request(&mut stream);
            seen.lock().unwrap().push(captured);
            let (status, body) = match reply {
                Reply::Respond(status, body) => (status, body),
                Reply::From(make) => make(&seen.lock().unwrap()),
                Reply::Cut => continue,
                Reply::Held {
                    body,
                    arrived,
                    release,
                } => {
                    let _ = arrived.send(());
                    // Bounded, so a test that fails before it releases the
                    // request fails instead of hanging its client and this
                    // server's join.
                    let _ = release.recv_timeout(std::time::Duration::from_secs(60));
                    (200, body)
                }
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
        }
    });
    ScriptedHttpServer {
        base_url: format!("http://{addr}"),
        requests,
        stop,
        handle: Some(handle),
    }
}

/// The JSON body of a captured request (head + body as the stubs record it).
pub fn request_json(raw: &str) -> serde_json::Value {
    raw.split_once("\r\n\r\n")
        .and_then(|(_, body)| serde_json::from_str(body).ok())
        .unwrap_or(serde_json::Value::Null)
}
