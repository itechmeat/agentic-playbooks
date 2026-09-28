//! Test support (feature `testing`): a multi-response HTTP stub.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// One scripted HTTP response.
#[derive(Debug, Clone)]
pub struct StubResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Held back this long before the response is written (a slow server).
    pub delay: Duration,
}

impl StubResponse {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        StubResponse {
            status,
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// An HTTP server on `127.0.0.1:0` that answers each request with the next
/// scripted response (and, once the script is spent, with the fallback
/// response, `500` unless set) and captures every raw request. The serving
/// thread stops and is joined on drop.
pub struct StubServer {
    pub base_url: String,
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StubServer {
    pub fn start(script: Vec<StubResponse>) -> Self {
        Self::start_with_fallback(
            script,
            StubResponse::json(500, r#"{"error":"script spent"}"#),
        )
    }

    /// Like [`Self::start`], answering every request past the script with
    /// `fallback`.
    pub fn start_with_fallback(script: Vec<StubResponse>, fallback: StubResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("a bound address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (req_slot, stop_flag) = (requests.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let mut script: VecDeque<StubResponse> = script.into();
            for stream in listener.incoming() {
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let raw = read_request(&mut stream);
                req_slot.lock().unwrap_or_else(|e| e.into_inner()).push(raw);
                let resp = script.pop_front().unwrap_or_else(|| fallback.clone());
                if !resp.delay.is_zero() {
                    std::thread::sleep(resp.delay);
                }
                let _ = write_response(&mut stream, &resp);
            }
        });
        StubServer {
            base_url: format!("http://{addr}"),
            addr,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// Every raw request (head and body) received so far.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How many requests were received so far.
    pub fn count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the thread sees the flag; it exits
        // without reading from this connection.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some(v) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(|v| v.trim().to_string())
        {
            content_length = v.parse().unwrap_or(0);
        }
        let end = line == "\r\n" || line == "\n";
        head.push_str(&line);
        if end {
            break;
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    head.push_str(&String::from_utf8_lossy(&body));
    head
}

fn write_response(stream: &mut TcpStream, resp: &StubResponse) -> std::io::Result<()> {
    let mut out = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.body.len()
    );
    if !resp
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        out.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&resp.body);
    stream.write_all(out.as_bytes())?;
    stream.flush()
}
