//! A scripted HTTP service for tests that send through a connector: it
//! records every request and answers each path by its own script.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A service on `127.0.0.1:0` that records every request and answers each
/// path by its own script: `None` reads the request and closes the
/// connection without an answer (the caller cannot tell what happened).
pub(crate) struct Service {
    pub(crate) base_url: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

pub(crate) type Script = BTreeMap<&'static str, Vec<Option<(u16, &'static str)>>>;

impl Service {
    pub(crate) fn spawn(script: Script) -> Service {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopped) = (requests.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let mut served: BTreeMap<String, usize> = BTreeMap::new();
            while !stopped.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::Interrupted
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    // Said here, where it happens, not as an empty request
                    // list the test reads later.
                    Err(e) => panic!("http_service: accept failed: {e}"),
                };
                let _ = stream.set_nonblocking(false);
                let (path, body) = read_request(&mut stream);
                seen.lock().unwrap().push((path.clone(), body));
                let n = served.entry(path.clone()).or_default();
                let reply = script
                    .get(path.as_str())
                    .and_then(|replies| replies.get(*n).copied().flatten());
                *n += 1;
                if let Some((status, body)) = reply {
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                }
            }
        });
        Service {
            base_url,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// The bodies of the requests to `path`, in order.
    pub(crate) fn bodies(&self, path: &str) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, b)| serde_json::from_str(b).unwrap_or(serde_json::Value::Null))
            .collect()
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The path and body of one HTTP request.
fn read_request(stream: &mut TcpStream) -> (String, String) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return (String::new(), String::new()),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let length: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while raw.len() < head_end + length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
    let path = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8_lossy(&raw[head_end..]).into_owned();
    (path, body)
}
