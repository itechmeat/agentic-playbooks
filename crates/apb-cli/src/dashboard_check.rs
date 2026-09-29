//! The `apb doctor` line about a running dashboard (issue #177): a dashboard
//! left running on an older binary after an in-place upgrade shares the
//! config dir, and its registry code is the one that used to empty
//! `projects.json`. Doctor names the version it serves when that differs from
//! this binary.
//!
//! Read-only and bounded: one `serve.lock` read, one liveness check, and one
//! `GET /api/health` on the loopback with a short timeout. A dashboard that
//! does not answer produces no line (doctor stays quiet about what it cannot
//! see).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Budget for the whole health probe: connect, request and reply.
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
/// A health reply is a few dozen bytes; anything bigger is not ours.
const MAX_REPLY: usize = 64 * 1024;

/// The doctor line for the dashboard serving this config dir, when one is
/// running and answers: `Ok(detail)` when it runs this binary's version,
/// `Err(detail)` (a warning) when it does not.
pub(crate) fn dashboard_version_line() -> Option<Result<String, String>> {
    let dir = apb_core::config::config_dir()?;
    let lock = apb_server::lock::read_global_lock(&dir)?;
    if !apb_engine::liveness::apb_pid_is_live(lock.pid) {
        return None;
    }
    let served = probe_health_version(lock.port, PROBE_TIMEOUT)?;
    Some(compare_versions(
        env!("CARGO_PKG_VERSION"),
        &served,
        lock.pid,
        lock.port,
    ))
}

fn compare_versions(binary: &str, served: &str, pid: u32, port: u16) -> Result<String, String> {
    if binary == served {
        Ok(format!(
            "the dashboard (pid {pid}, port {port}) runs this version ({binary})"
        ))
    } else {
        Err(format!(
            "the dashboard (pid {pid}, port {port}) runs apb {served}, this binary is {binary}; restart it so both use the same version (they share the projects registry and the global store)"
        ))
    }
}

/// `version` from `GET /api/health` on `127.0.0.1:<port>`, or `None` when
/// nothing answers in time or the reply is not a health document.
fn probe_health_version(port: u16, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, budget).ok()?;
    let remaining = || deadline.checked_duration_since(Instant::now());
    stream.set_write_timeout(remaining()).ok()?;
    let request = format!(
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut reply = Vec::new();
    let mut buf = [0u8; 4096];
    // Every read is bounded by what is left of the budget, so a server that
    // accepts and then stalls cannot hold doctor up.
    loop {
        let left = remaining().filter(|d| !d.is_zero())?;
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                reply.extend_from_slice(&buf[..n]);
                if reply.len() > MAX_REPLY {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    parse_health_version(&reply)
}

fn parse_health_version(reply: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(reply).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status_ok = head
        .lines()
        .next()
        .is_some_and(|l| l.split_whitespace().nth(1) == Some("200"));
    if !status_ok {
        return None;
    }
    let doc: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    doc.get("version")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_matching_version_is_ok_and_a_different_one_warns() {
        assert!(compare_versions("0.22.1", "0.22.1", 7, 7321).is_ok());
        let warn = compare_versions("0.22.1", "0.20.0", 7, 7321).unwrap_err();
        assert!(warn.contains("0.20.0") && warn.contains("0.22.1"), "{warn}");
    }

    #[test]
    fn the_version_is_read_from_a_health_reply() {
        let reply = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 45\r\n\r\n{\"status\":\"ok\",\"build_id\":\"x\",\"version\":\"0.20.0\"}";
        assert_eq!(parse_health_version(reply).as_deref(), Some("0.20.0"));
        let refused = b"HTTP/1.1 403 Forbidden\r\n\r\n{\"version\":\"0.20.0\"}";
        assert_eq!(parse_health_version(refused), None);
        assert_eq!(parse_health_version(b"garbage"), None);
    }

    /// The probe against a real socket: a server that answers is read, one
    /// that accepts and never answers costs at most the budget.
    #[test]
    fn the_probe_reads_a_live_reply_and_gives_up_on_a_silent_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accepts with a deadline, so a probe that never connects cannot leave
        // the thread (and the join below) waiting forever.
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut conn = loop {
                match listener.accept() {
                    Ok((conn, _)) => break conn,
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => panic!("timed out after 10s waiting for the probe to connect: {e}"),
                }
            };
            conn.set_nonblocking(false).unwrap();
            conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf);
            let body = "{\"status\":\"ok\",\"version\":\"9.9.9\"}";
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        assert_eq!(
            probe_health_version(port, Duration::from_secs(5)).as_deref(),
            Some("9.9.9")
        );
        server.join().unwrap();

        let silent = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let started = Instant::now();
        assert_eq!(probe_health_version(port, Duration::from_millis(200)), None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the probe waited {:?} on a server that never answered",
            started.elapsed()
        );
        drop(silent);
    }
}
