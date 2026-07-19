//! Slice-3 smtp execution tests. A blocking std-thread SMTP listener records
//! the whole conversation (EHLO, AUTH, MAIL/RCPT/DATA, QUIT) so we can assert
//! on the rendered envelope and MIME structure without a network or a real TLS
//! stack. STARTTLS itself is not exercised here (a real handshake needs a
//! self-signed cert); live smoke tests cover real TLS. The non-TLS send and
//! verify paths, plus a use_tls-refuses-plaintext unit, are covered here.
//!
//! No process-global state is touched (each test binds its own 127.0.0.1:0
//! listener), so no shared env lock is needed.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use apb_core::connector::def::SmtpSpec;
use apb_engine::connector_call::CallErrorCode;
use apb_engine::connector_smtp::{SmtpBuild, build};
use serde_json::json;

/// What the listener recorded, for assertions.
#[derive(Default, Clone)]
struct Recorded {
    ehlo: bool,
    auth_plain: Option<String>,
    mail_from: Option<String>,
    rcpt_to: Vec<String>,
    data: String,
    quit: bool,
}

struct SmtpTestServer {
    host: String,
    port: u16,
    rec: Arc<Mutex<Recorded>>,
    handle: Option<JoinHandle<()>>,
}

impl SmtpTestServer {
    fn recorded(&self) -> Recorded {
        self.rec.lock().unwrap().clone()
    }
}
impl Drop for SmtpTestServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = TcpStream::connect((self.host.as_str(), self.port));
            let _ = h.join();
        }
    }
}

/// Spawns a one-connection SMTP listener. `advertise_starttls` controls whether
/// EHLO advertises STARTTLS; `advertise_auth` whether it advertises AUTH.
fn spawn_smtp(advertise_starttls: bool, advertise_auth: bool) -> SmtpTestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let rec = Arc::new(Mutex::new(Recorded::default()));
    let slot = rec.clone();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            serve(&mut stream, &slot, advertise_starttls, advertise_auth);
        }
    });
    SmtpTestServer {
        host: addr.ip().to_string(),
        port: addr.port(),
        rec,
        handle: Some(handle),
    }
}

fn line(reader: &mut BufReader<&TcpStream>) -> String {
    let mut s = String::new();
    let _ = reader.read_line(&mut s);
    s.trim_end().to_string()
}

fn serve(stream: &mut TcpStream, rec: &Arc<Mutex<Recorded>>, starttls: bool, auth: bool) {
    let mut w = stream.try_clone().unwrap();
    let mut reader = BufReader::new(&*stream);
    let _ = w.write_all(b"220 test ESMTP\r\n");
    loop {
        let cmd = line(&mut reader);
        let upper = cmd.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            rec.lock().unwrap().ehlo = true;
            let mut resp = String::from("250-test\r\n");
            if starttls {
                resp.push_str("250-STARTTLS\r\n");
            }
            if auth {
                resp.push_str("250-AUTH PLAIN LOGIN\r\n");
            }
            resp.push_str("250 SMTPUTF8\r\n");
            let _ = w.write_all(resp.as_bytes());
        } else if upper.starts_with("AUTH PLAIN") {
            rec.lock().unwrap().auth_plain = Some(cmd.clone());
            let _ = w.write_all(b"235 2.7.0 Authentication successful\r\n");
        } else if upper.starts_with("MAIL FROM") {
            rec.lock().unwrap().mail_from = Some(cmd.clone());
            let _ = w.write_all(b"250 OK\r\n");
        } else if upper.starts_with("RCPT TO") {
            rec.lock().unwrap().rcpt_to.push(cmd.clone());
            let _ = w.write_all(b"250 OK\r\n");
        } else if upper.starts_with("DATA") {
            let _ = w.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n");
            let mut body = String::new();
            loop {
                let l = line(&mut reader);
                if l == "." {
                    break;
                }
                body.push_str(&l);
                body.push('\n');
            }
            rec.lock().unwrap().data = body;
            let _ = w.write_all(b"250 2.0.0 Ok: queued\r\n");
        } else if upper.starts_with("QUIT") {
            rec.lock().unwrap().quit = true;
            let _ = w.write_all(b"221 Bye\r\n");
            break;
        } else if upper.starts_with("STARTTLS") {
            // Not negotiated in tests; refuse so a plaintext-refusing client
            // never proceeds. (use_tls path is asserted via the unit below.)
            let _ = w.write_all(b"454 TLS not available\r\n");
        } else if cmd.is_empty() {
            break;
        } else {
            let _ = w.write_all(b"250 OK\r\n");
        }
    }
}

fn send_spec() -> SmtpSpec {
    serde_yaml_ng::from_str(
        "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nmessage:\n  from_email: \"{{account.from_email}}\"\n  to: \"{{args.to}}\"\n  subject: \"{{args.subject}}\"\n  body_text: \"{{args.body_text}}\"\n  body_html: \"{{args.body_html}}\"\n",
    )
    .unwrap()
}

fn account(host: &str, port: u16, use_tls: bool) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("host".into(), host.into()),
        ("port".into(), port.to_string()),
        ("use_tls".into(), use_tls.to_string()),
        ("username".into(), "u".into()),
        ("from_email".into(), "a@b.c".into()),
    ])
}

fn secrets() -> BTreeMap<String, String> {
    BTreeMap::from([("password".into(), "pw".into())])
}

#[test]
fn send_over_plaintext_delivers_multipart() {
    let srv = spawn_smtp(false, true);
    let spec = send_spec();
    let args =
        json!({"to": "x@y.z, w@y.z", "subject": "Hi", "body_text": "T", "body_html": "<p>T</p>"});
    let call = match build(
        &spec,
        &account(&srv.host, srv.port, false),
        &args,
        &secrets(),
        Vec::new(),
        false,
        15,
    )
    .unwrap()
    {
        SmtpBuild::Call(c) => c,
        _ => panic!("expected a call"),
    };
    let ok = call.send().expect("send should succeed");
    let body = ok.body();
    assert_eq!(body["from"], json!("a@b.c"));
    assert_eq!(body["subject"], json!("Hi"));
    assert_eq!(body["accepted"], json!(["x@y.z", "w@y.z"]));

    let r = srv.recorded();
    assert!(r.ehlo && r.quit);
    assert!(r.auth_plain.is_some(), "AUTH PLAIN expected");
    assert_eq!(r.rcpt_to.len(), 2);
    assert!(r.data.contains("Subject: Hi"));
    assert!(r.data.to_lowercase().contains("multipart/alternative"));
    // No credential ever appears in the recorded message body.
    assert!(!r.data.contains("pw"));
}

#[test]
fn verify_authenticates_and_quits() {
    let srv = spawn_smtp(false, true);
    let spec: SmtpSpec = serde_yaml_ng::from_str(
        "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nverify: true\n",
    )
    .unwrap();
    let call = match build(
        &spec,
        &account(&srv.host, srv.port, false),
        &json!({}),
        &secrets(),
        Vec::new(),
        false,
        15,
    )
    .unwrap()
    {
        SmtpBuild::Call(c) => c,
        _ => panic!("expected call"),
    };
    let ok = call.send().unwrap();
    assert_eq!(ok.body()["verified"], json!(true));
    let r = srv.recorded();
    assert!(r.ehlo && r.auth_plain.is_some() && r.quit);
    assert!(r.mail_from.is_none(), "verify must not send mail");
}

#[test]
fn use_tls_refuses_plaintext_when_starttls_absent() {
    // Server advertises no STARTTLS; a use_tls call must refuse before AUTH/DATA.
    let srv = spawn_smtp(false, true);
    let spec = send_spec();
    let args = json!({"to": "x@y.z", "subject": "Hi", "body_text": "T"});
    let call = match build(
        &spec,
        &account(&srv.host, srv.port, true),
        &args,
        &secrets(),
        Vec::new(),
        false,
        15,
    )
    .unwrap()
    {
        SmtpBuild::Call(c) => c,
        _ => panic!("expected call"),
    };
    let err = call.send().unwrap_err();
    assert_eq!(err.code, CallErrorCode::Service);
    let r = srv.recorded();
    assert!(
        r.mail_from.is_none() && r.auth_plain.is_none(),
        "must not proceed in plaintext"
    );
}
