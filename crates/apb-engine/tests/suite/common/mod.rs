//! Shared test helpers for the engine (schema 2). Not a separate test binary -
//! declared once as `mod common;` in `../main.rs` and reached from every suite
//! module via `use crate::common;`.
#![allow(dead_code)]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

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
        if let Some(h) = self.handle.take() {
            // If the test under test errored out before ever connecting, the
            // one-shot `accept()` would block forever and this join would
            // deadlock. Make a throwaway connection to unblock it (best-effort:
            // if the thread already served and exited, the listener is gone and
            // this simply fails).
            let _ = std::net::TcpStream::connect(self.addr);
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
        if let Ok((mut stream, _)) = listener.accept() {
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
        handle: Some(handle),
    }
}

/// Reads one HTTP request (request line + headers, then a `Content-Length`
/// body when present) and returns the raw text.
fn read_http_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(rest) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
        {
            content_length = rest.parse().unwrap_or(0);
        }
        let done = line == "\r\n" || line == "\n";
        head.push_str(&line);
        if done {
            break;
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        if reader.read_exact(&mut body).is_ok() {
            head.push_str(&String::from_utf8_lossy(&body));
        }
    }
    head
}
