//! Shared test-only utilities for the consolidated apb-server integration
//! binary (see `../main.rs`).
//!
//! `meta_api_test` and `profiles_api_test` each mutate process-wide env vars
//! (`HOME`, `APB_CONFIG_DIR`) and were originally written on the assumption
//! that they ran in their own cargo test process, so no lock was needed.
//! Consolidating every `tests/*.rs` file into one binary means their test
//! functions now run as threads in the same process, and cargo test runs
//! test functions in parallel by default - so without serialization these
//! two race on the shared env and fail intermittently (observed directly
//! during consolidation: `agents_models_and_skills_endpoints` failed when
//! `profiles_list_then_create_then_trusted` overwrote `HOME`/`APB_CONFIG_DIR`
//! mid-run). Any test that mutates process env must take this lock for the
//! duration of its run.
//!
//! This uses `tokio::sync::Mutex` rather than `std::sync::Mutex`: both
//! affected tests hold the guard across several `.await` points (the router
//! calls under test), and clippy's `await_holding_lock` correctly flags a
//! std mutex guard held that way as a potential executor stall. The
//! async-aware guard is fine to hold across awaits.

use tokio::sync::{Mutex, MutexGuard};

pub static ENV_LOCK: Mutex<()> = Mutex::const_new(());

pub async fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

// --- Ephemeral one-shot HTTP server (mirrors apb-engine's tests/suite/common/mod.rs) ---

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::JoinHandle;

/// A canned one-shot HTTP server on `127.0.0.1:0`: serves a single request
/// with a fixed response and captures the raw request text for assertions
/// (e.g. "was the auth header injected"). `base_url` is the
/// `http://127.0.0.1:<port>` origin to point a connector account's
/// `base_url` at. The serving thread joins on drop.
pub struct TestHttpServer {
    pub base_url: String,
    addr: std::net::SocketAddr,
    request: Arc<StdMutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl TestHttpServer {
    pub fn captured_request(&self) -> Option<String> {
        self.request.lock().unwrap().clone()
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = std::net::TcpStream::connect(self.addr);
            let _ = h.join();
        }
    }
}

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

    let request = Arc::new(StdMutex::new(None));
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

fn read_http_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
        head.push_str(&line);
    }
    head.push_str("\r\n");
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
        head.push_str(&String::from_utf8_lossy(&body));
    }
    head
}
