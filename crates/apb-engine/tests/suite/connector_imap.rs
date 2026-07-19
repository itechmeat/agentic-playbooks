//! Slice-4 imap execution tests (spec 3.2 - 3.4, wave 2). A blocking std-thread
//! IMAP listener records every client line so we can assert the silent-read
//! guarantee directly: read ops open with `EXAMINE` (never `SELECT`), content is
//! fetched only with `BODY.PEEK[]`, and no composed FETCH carries a bare
//! `BODY[`. TLS is not exercised here (a real handshake needs a self-signed
//! cert); every listener speaks plaintext on 127.0.0.1:0 with `use_tls: false`.
//!
//! No process-global state is touched (each test binds its own listener), so no
//! shared env lock is needed.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use apb_core::connector::def::ImapSpec;
use apb_engine::connector_call::CallErrorCode;
use apb_engine::connector_imap::{ImapBuild, build};
use serde_json::{Value, json};

/// One canned message for a search-shape `UID FETCH` (FLAGS/ENVELOPE response).
#[derive(Clone)]
struct Env {
    uid: u32,
    subject: String,
    from_mailbox: String,
    from_host: String,
    seen: bool,
    size: u32,
}

/// What a search `UID FETCH` returns, or the body a fetch `UID FETCH` returns.
#[derive(Clone)]
enum FetchResp {
    None,
    Envelopes(Vec<Env>),
    Literal { seen: bool, raw: Vec<u8> },
}

/// The per-test listener script: how the fake server answers each command.
#[derive(Clone)]
struct Script {
    /// Send the greeting, then sleep and never answer (timeout test).
    stall: bool,
    /// When set, LOGIN / AUTHENTICATE answer `NO <text>`.
    login_reject: Option<String>,
    /// When set, EXAMINE answers `NO <text>`.
    examine_reject: Option<String>,
    /// The ids the `UID SEARCH` answer reports.
    search_ids: Vec<u32>,
    /// The `UID FETCH` answer.
    fetch: FetchResp,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            stall: false,
            login_reject: None,
            examine_reject: None,
            search_ids: Vec::new(),
            fetch: FetchResp::None,
        }
    }
}

/// What the listener recorded, for assertions.
#[derive(Default, Clone)]
struct Recorded {
    /// Every command line the client sent (trimmed), in order.
    lines: Vec<String>,
    /// The base64 SASL line the client sent after `AUTHENTICATE`.
    auth_payload: Option<String>,
}

struct ImapTestServer {
    host: String,
    port: u16,
    rec: Arc<Mutex<Recorded>>,
    handle: Option<JoinHandle<()>>,
}

impl ImapTestServer {
    fn recorded(&self) -> Recorded {
        self.rec.lock().unwrap().clone()
    }
}

impl Drop for ImapTestServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = TcpStream::connect((self.host.as_str(), self.port));
            let _ = h.join();
        }
    }
}

/// Spawns a one-connection IMAP listener driven by `script`.
fn spawn(script: Script) -> ImapTestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let rec = Arc::new(Mutex::new(Recorded::default()));
    let slot = rec.clone();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            serve(&mut stream, &slot, &script);
        }
    });
    ImapTestServer {
        host: addr.ip().to_string(),
        port: addr.port(),
        rec,
        handle: Some(handle),
    }
}

fn read_line(reader: &mut BufReader<&TcpStream>) -> String {
    let mut s = String::new();
    let _ = reader.read_line(&mut s);
    s.trim_end().to_string()
}

fn ok(w: &mut TcpStream, tag: &str) {
    let _ = w.write_all(format!("{tag} OK done\r\n").as_bytes());
}

fn no(w: &mut TcpStream, tag: &str, text: &str) {
    let _ = w.write_all(format!("{tag} NO {text}\r\n").as_bytes());
}

fn send_mailbox(w: &mut TcpStream, tag: &str, mode: &str) {
    let _ = w.write_all(b"* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n");
    let _ = w.write_all(b"* 3 EXISTS\r\n");
    let _ = w.write_all(b"* 0 RECENT\r\n");
    let _ = w.write_all(b"* OK [UIDVALIDITY 1] UIDs valid\r\n");
    let _ = w.write_all(b"* OK [UIDNEXT 200] Predicted next UID\r\n");
    let _ = w.write_all(format!("{tag} OK {mode} done\r\n").as_bytes());
}

fn send_envelopes(w: &mut TcpStream, tag: &str, envs: &[Env]) {
    for (i, e) in envs.iter().enumerate() {
        let flags = if e.seen { "\\Seen" } else { "" };
        let line = format!(
            "* {seq} FETCH (UID {uid} RFC822.SIZE {size} INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" FLAGS ({flags}) ENVELOPE (\"Mon, 01 Jan 2024 00:00:00 +0000\" \"{subject}\" ((\"Sender\" NIL \"{fmbox}\" \"{fhost}\")) NIL NIL ((\"Recipient\" NIL \"bob\" \"example.com\")) NIL NIL NIL \"<{uid}@example.com>\"))\r\n",
            seq = i + 1,
            uid = e.uid,
            size = e.size,
            subject = e.subject,
            fmbox = e.from_mailbox,
            fhost = e.from_host,
        );
        let _ = w.write_all(line.as_bytes());
    }
    ok(w, tag);
}

fn send_literal(w: &mut TcpStream, tag: &str, seen: bool, raw: &[u8]) {
    let flags = if seen { "\\Seen" } else { "" };
    let header = format!(
        "* 1 FETCH (UID 42 FLAGS ({flags}) BODY[] {{{}}}\r\n",
        raw.len()
    );
    let _ = w.write_all(header.as_bytes());
    let _ = w.write_all(raw);
    let _ = w.write_all(b")\r\n");
    ok(w, tag);
}

fn serve(stream: &mut TcpStream, rec: &Arc<Mutex<Recorded>>, script: &Script) {
    let mut w = stream.try_clone().unwrap();
    let mut reader = BufReader::new(&*stream);
    let _ = w.write_all(b"* OK apb-test ready\r\n");
    if script.stall {
        std::thread::sleep(Duration::from_secs(5));
        return;
    }
    loop {
        let cmd = read_line(&mut reader);
        if cmd.is_empty() {
            break;
        }
        rec.lock().unwrap().lines.push(cmd.clone());
        let mut toks = cmd.split_whitespace();
        let tag = toks.next().unwrap_or("").to_string();
        let word = toks.next().unwrap_or("").to_ascii_uppercase();
        match word.as_str() {
            "CAPABILITY" => {
                let _ = w.write_all(b"* CAPABILITY IMAP4rev1 AUTH=XOAUTH2\r\n");
                ok(&mut w, &tag);
            }
            "LOGIN" => match &script.login_reject {
                Some(t) => no(&mut w, &tag, t),
                None => ok(&mut w, &tag),
            },
            "AUTHENTICATE" => {
                let _ = w.write_all(b"+ \r\n");
                let payload = read_line(&mut reader);
                {
                    let mut g = rec.lock().unwrap();
                    g.auth_payload = Some(payload.clone());
                    g.lines.push(payload);
                }
                match &script.login_reject {
                    Some(t) => no(&mut w, &tag, t),
                    None => ok(&mut w, &tag),
                }
            }
            "EXAMINE" => match &script.examine_reject {
                Some(t) => no(&mut w, &tag, t),
                None => send_mailbox(&mut w, &tag, "[READ-ONLY]"),
            },
            "SELECT" => send_mailbox(&mut w, &tag, "[READ-WRITE]"),
            "LIST" => {
                let _ = w.write_all(b"* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n");
                let _ = w.write_all(b"* LIST (\\HasNoChildren \\Sent) \".\" \"Sent\"\r\n");
                ok(&mut w, &tag);
            }
            "UID" => {
                let sub = toks.next().unwrap_or("").to_ascii_uppercase();
                match sub.as_str() {
                    "SEARCH" => {
                        let ids: Vec<String> =
                            script.search_ids.iter().map(|u| u.to_string()).collect();
                        let _ = w.write_all(format!("* SEARCH {}\r\n", ids.join(" ")).as_bytes());
                        ok(&mut w, &tag);
                    }
                    "FETCH" => match &script.fetch {
                        FetchResp::Envelopes(envs) => send_envelopes(&mut w, &tag, envs),
                        FetchResp::Literal { seen, raw } => send_literal(&mut w, &tag, *seen, raw),
                        FetchResp::None => ok(&mut w, &tag),
                    },
                    "STORE" => {
                        // Emit one FLAGS FETCH per uid in the uid-set (token 3).
                        let set = cmd.split_whitespace().nth(3).unwrap_or("");
                        for (i, uid) in set.split(',').enumerate() {
                            let _ = w.write_all(
                                format!("* {} FETCH (UID {uid} FLAGS (\\Seen))\r\n", i + 1)
                                    .as_bytes(),
                            );
                        }
                        ok(&mut w, &tag);
                    }
                    _ => ok(&mut w, &tag),
                }
            }
            "LOGOUT" => {
                let _ = w.write_all(b"* BYE apb-test logging out\r\n");
                ok(&mut w, &tag);
                break;
            }
            _ => ok(&mut w, &tag),
        }
    }
}

// -- Spec construction helpers --

fn spec_yaml(op: &str, params: &str) -> ImapSpec {
    let yaml = format!(
        "connection:\n  host: \"{{{{account.host}}}}\"\n  port: \"{{{{account.port}}}}\"\n  use_tls: \"{{{{account.use_tls}}}}\"\n  auth_method: \"{{{{account.auth_method}}}}\"\n  username: \"{{{{account.username}}}}\"\n  password: \"{{{{secret.password}}}}\"\nop: {op}\nparams:\n{params}",
    );
    serde_yaml_ng::from_str(&yaml).unwrap()
}

fn account(host: &str, port: u16, auth_method: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("host".into(), host.into()),
        ("port".into(), port.to_string()),
        ("use_tls".into(), "false".into()),
        ("auth_method".into(), auth_method.into()),
        ("username".into(), "u".into()),
    ])
}

fn secrets(password: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("password".into(), password.into())])
}

/// Builds a real (non-dry-run) call and unwraps it, or panics.
fn build_call(
    spec: &ImapSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, String>,
) -> Box<apb_engine::connector_imap::ImapCall> {
    match build(spec, account, args, secrets, Vec::new(), false, 15).unwrap() {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    }
}

// -- Tests --

#[test]
fn verify_ok_over_plaintext() {
    let srv = spawn(Script::default());
    let spec = spec_yaml("verify", "  {}\n");
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &json!({}),
        &secrets("pw"),
    );
    let ok = call.send().expect("verify should succeed");
    assert_eq!(ok.body()["authenticated"], json!(true));
    let r = srv.recorded();
    assert!(
        r.lines.iter().any(|l| l.contains("LOGIN")),
        "expected a LOGIN, got {:?}",
        r.lines
    );
    assert!(
        r.lines.iter().any(|l| l.contains("LOGOUT")),
        "expected a LOGOUT, got {:?}",
        r.lines
    );
}

#[test]
fn login_rejected_maps_to_auth_error() {
    let srv = spawn(Script {
        login_reject: Some("[AUTHENTICATIONFAILED] nope".to_string()),
        ..Script::default()
    });
    let spec = spec_yaml("verify", "  {}\n");
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &json!({}),
        &secrets("pw"),
    );
    let err = call.send().unwrap_err();
    assert_eq!(err.code, CallErrorCode::Auth, "message: {}", err.message);
}

#[test]
fn xoauth2_sends_expected_sasl_payload() {
    use base64::Engine;
    let srv = spawn(Script::default());
    let spec = spec_yaml("verify", "  {}\n");
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "xoauth2"),
        &json!({}),
        &secrets("tok"),
    );
    let ok = call.send().expect("xoauth2 verify should succeed");
    assert_eq!(ok.body()["authenticated"], json!(true));

    let r = srv.recorded();
    let payload = r.auth_payload.expect("expected a SASL payload line");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .expect("payload is base64");
    assert_eq!(decoded, b"user=u\x01auth=Bearer tok\x01\x01");
    assert!(
        r.lines.iter().any(|l| l.contains("AUTHENTICATE XOAUTH2")),
        "expected AUTHENTICATE XOAUTH2, got {:?}",
        r.lines
    );
}

#[test]
fn search_uses_examine_and_composes_criteria() {
    let srv = spawn(Script {
        search_ids: vec![101, 102, 103],
        fetch: FetchResp::Envelopes(vec![Env {
            uid: 101,
            subject: "Subject 101".into(),
            from_mailbox: "alice".into(),
            from_host: "example.com".into(),
            seen: false,
            size: 100,
        }]),
        ..Script::default()
    });
    let params = "  folder: \"{{args.folder}}\"\n  unread_only: \"{{args.unread_only}}\"\n  from_contains: \"{{args.from_contains}}\"\n  subject_contains: \"{{args.subject_contains}}\"\n  since_days: \"{{args.since_days}}\"\n  limit: \"{{args.limit}}\"\n";
    let spec = spec_yaml("search", params);
    let args = json!({
        "folder": "INBOX",
        "unread_only": "true",
        "from_contains": "bob@example.com",
        "subject_contains": "he said \"hi\"",
        "since_days": "7",
        "limit": "50",
    });
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
    );
    call.send().expect("search should succeed");

    let r = srv.recorded();
    assert!(
        r.lines.iter().any(|l| l.contains("EXAMINE \"INBOX\"")),
        "read op must EXAMINE, got {:?}",
        r.lines
    );
    assert!(
        !r.lines.iter().any(|l| l.contains("SELECT")),
        "a search must never SELECT, got {:?}",
        r.lines
    );
    let search = r
        .lines
        .iter()
        .find(|l| l.contains("UID SEARCH"))
        .expect("a UID SEARCH line");
    assert!(search.contains("UNSEEN"), "criteria: {search}");
    assert!(
        search.contains("FROM \"bob@example.com\""),
        "criteria: {search}"
    );
    assert!(search.contains("SINCE "), "criteria: {search}");
    // The double quotes in the subject value arrive backslash-escaped.
    assert!(
        search.contains("SUBJECT \"he said \\\"hi\\\"\""),
        "escaped subject expected, got: {search}"
    );
}

#[test]
fn search_result_shape_and_order() {
    let srv = spawn(Script {
        search_ids: vec![101, 102, 103],
        fetch: FetchResp::Envelopes(vec![
            Env {
                uid: 101,
                subject: "Oldest".into(),
                from_mailbox: "a".into(),
                from_host: "x.test".into(),
                seen: true,
                size: 10,
            },
            Env {
                uid: 102,
                subject: "Middle".into(),
                from_mailbox: "b".into(),
                from_host: "x.test".into(),
                seen: false,
                size: 20,
            },
            Env {
                uid: 103,
                subject: "Newest".into(),
                from_mailbox: "c".into(),
                from_host: "x.test".into(),
                seen: false,
                size: 30,
            },
        ]),
        ..Script::default()
    });
    let params = "  folder: \"{{args.folder}}\"\n  limit: \"{{args.limit}}\"\n";
    let spec = spec_yaml("search", params);
    let args = json!({ "folder": "INBOX", "limit": "50" });
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
    );
    let ok = call.send().expect("search should succeed");
    let body = ok.body();
    assert_eq!(body["folder"], json!("INBOX"));
    assert_eq!(body["total_matched"], json!(3));
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    // Newest first: uid 103, then 102, then 101.
    assert_eq!(msgs[0]["uid"], json!(103));
    assert_eq!(msgs[0]["subject"], json!("Newest"));
    assert_eq!(msgs[0]["from"], json!("c@x.test"));
    assert_eq!(msgs[2]["uid"], json!(101));
    assert_eq!(msgs[2]["seen"], json!(true));
}

#[test]
fn crlf_in_search_values_is_invalid_args() {
    // A control character in a search value is rejected before any connection,
    // so no listener is spawned.
    let params = "  folder: \"{{args.folder}}\"\n  subject_contains: \"{{args.subject_contains}}\"\n  limit: \"{{args.limit}}\"\n";
    let spec = spec_yaml("search", params);
    let args = json!({
        "folder": "INBOX",
        "subject_contains": "hi\r\nA00 DELETE INBOX",
        "limit": "10",
    });
    let acct = account("127.0.0.1", 1, "password");
    let err = build(&spec, &acct, &args, &secrets("pw"), Vec::new(), false, 15).unwrap_err();
    assert_eq!(
        err.code,
        CallErrorCode::InvalidArgs,
        "message: {}",
        err.message
    );
}

#[test]
fn fetch_uses_body_peek_and_parses_mime() {
    let raw = concat!(
        "From: Alice <alice@example.com>\r\n",
        "To: Bob <bob@example.com>\r\n",
        "Cc: Carol <carol@example.com>\r\n",
        "Subject: Hello\r\n",
        "Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"MIX\"\r\n",
        "\r\n",
        "--MIX\r\n",
        "Content-Type: multipart/alternative; boundary=\"ALT\"\r\n",
        "\r\n",
        "--ALT\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "\r\n",
        "Hello in plain text\r\n",
        "--ALT\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "\r\n",
        "<p>Hello in html</p>\r\n",
        "--ALT--\r\n",
        "--MIX\r\n",
        "Content-Type: application/pdf; name=\"doc.pdf\"\r\n",
        "Content-Disposition: attachment; filename=\"doc.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "aGVsbG8=\r\n",
        "--MIX--\r\n",
    );
    let srv = spawn(Script {
        fetch: FetchResp::Literal {
            seen: true,
            raw: raw.as_bytes().to_vec(),
        },
        ..Script::default()
    });
    let params = "  folder: \"{{args.folder}}\"\n  uid: \"{{args.uid}}\"\n";
    let spec = spec_yaml("fetch", params);
    let args = json!({ "folder": "INBOX", "uid": "42" });
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
    );
    let ok = call.send().expect("fetch should succeed");
    let body = ok.body();
    assert_eq!(body["uid"], json!(42));
    assert_eq!(body["seen"], json!(true));
    assert_eq!(body["from"], json!("alice@example.com"));
    assert_eq!(body["subject"], json!("Hello"));
    assert!(body["text"].as_str().unwrap().contains("plain text"));
    assert!(body["html"].as_str().unwrap().contains("html"));
    assert_eq!(body["truncated"], json!(false));
    let atts = body["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 1);
    assert_eq!(atts[0]["filename"], json!("doc.pdf"));
    assert_eq!(atts[0]["mime"], json!("application/pdf"));

    // Silent-read guarantee on the wire: BODY.PEEK[] was used and no composed
    // FETCH ever carried a bare BODY[.
    let r = srv.recorded();
    let fetch = r
        .lines
        .iter()
        .find(|l| l.contains("UID FETCH"))
        .expect("a UID FETCH line");
    assert!(fetch.contains("BODY.PEEK[]"), "fetch: {fetch}");
    assert!(
        r.lines.iter().any(|l| l.contains("EXAMINE")),
        "fetch must EXAMINE, got {:?}",
        r.lines
    );
    for line in &r.lines {
        assert!(!line.contains("BODY["), "a bare BODY[ leaked: {line}");
        assert!(!line.contains("SELECT"), "a fetch must not SELECT: {line}");
    }
}

#[test]
fn set_flags_uses_select_and_store() {
    // Setting \Seen (+FLAGS).
    let srv = spawn(Script::default());
    let params =
        "  folder: \"{{args.folder}}\"\n  uids: \"{{args.uids}}\"\n  seen: \"{{args.seen}}\"\n";
    let spec = spec_yaml("set_flags", params);
    let args = json!({ "folder": "INBOX", "uids": "101,102", "seen": "true" });
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
    );
    let ok = call.send().expect("set_flags should succeed");
    assert_eq!(ok.body()["updated"], json!(2));
    assert_eq!(ok.body()["folder"], json!("INBOX"));
    let r = srv.recorded();
    assert!(
        r.lines.iter().any(|l| l.contains("SELECT \"INBOX\"")),
        "set_flags must SELECT, got {:?}",
        r.lines
    );
    assert!(
        r.lines
            .iter()
            .any(|l| l.contains("UID STORE 101,102 +FLAGS (\\Seen)")),
        "expected a +FLAGS STORE, got {:?}",
        r.lines
    );

    // Clearing \Seen (-FLAGS).
    let srv2 = spawn(Script::default());
    let args2 = json!({ "folder": "INBOX", "uids": "101,102", "seen": "false" });
    let call2 = build_call(
        &spec,
        &account(&srv2.host, srv2.port, "password"),
        &args2,
        &secrets("pw"),
    );
    call2.send().expect("clear set_flags should succeed");
    let r2 = srv2.recorded();
    assert!(
        r2.lines
            .iter()
            .any(|l| l.contains("UID STORE 101,102 -FLAGS (\\Seen)")),
        "expected a -FLAGS STORE, got {:?}",
        r2.lines
    );
}

#[test]
fn unknown_folder_no_maps_to_service() {
    let srv = spawn(Script {
        examine_reject: Some("[NONEXISTENT] Unknown Mailbox".to_string()),
        ..Script::default()
    });
    let params = "  folder: \"{{args.folder}}\"\n  limit: \"{{args.limit}}\"\n";
    let spec = spec_yaml("search", params);
    let args = json!({ "folder": "Nope", "limit": "10" });
    let call = build_call(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
    );
    let err = call.send().unwrap_err();
    assert_eq!(err.code, CallErrorCode::Service, "message: {}", err.message);
}

#[test]
fn connection_refused_maps_to_network() {
    // Bind then drop the listener so the port is closed; connecting is refused.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let spec = spec_yaml("verify", "  {}\n");
    let call = build_call(
        &spec,
        &account(&addr.ip().to_string(), addr.port(), "password"),
        &json!({}),
        &secrets("pw"),
    );
    let err = call.send().unwrap_err();
    assert_eq!(err.code, CallErrorCode::Network, "message: {}", err.message);
}

#[test]
fn stalled_server_maps_to_timeout() {
    let srv = spawn(Script {
        stall: true,
        ..Script::default()
    });
    let spec = spec_yaml("verify", "  {}\n");
    let call = match build(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &json!({}),
        &secrets("pw"),
        Vec::new(),
        false,
        1,
    )
    .unwrap()
    {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    };
    let err = call.send().unwrap_err();
    assert_eq!(err.code, CallErrorCode::Timeout, "message: {}", err.message);
}

#[test]
fn dry_run_renders_without_connecting() {
    // No listener at all: a dry-run renders the endpoint and typed params
    // without connecting or touching the password.
    let params = "  folder: \"{{args.folder}}\"\n  limit: \"{{args.limit}}\"\n";
    let spec = spec_yaml("search", params);
    let args = json!({ "folder": "INBOX", "limit": "25" });
    let out = match build(
        &spec,
        &account("imap.example.com", 993, "password"),
        &args,
        &secrets("super-secret"),
        Vec::new(),
        true,
        30,
    )
    .unwrap()
    {
        ImapBuild::DryRun(v) => v,
        _ => panic!("expected a dry-run"),
    };
    assert_eq!(out["dry_run"], json!(true));
    assert_eq!(
        out["imap"]["endpoint"],
        json!("imap://imap.example.com:993")
    );
    assert_eq!(out["imap"]["op"], json!("search"));
    assert_eq!(out["imap"]["params"]["folder"], json!("INBOX"));
    assert_eq!(out["imap"]["params"]["limit"], json!(25));
    // The password value must appear nowhere in the render.
    assert!(
        !serde_json::to_string(&out)
            .unwrap()
            .contains("super-secret"),
        "password leaked into the dry-run: {out}"
    );
}

#[test]
fn bad_params_are_invalid_args() {
    let acct = account("127.0.0.1", 1, "password");
    let cases = [
        // limit 0 (below the 1..=100 range).
        ("  folder: \"INBOX\"\n  limit: \"0\"\n", "search"),
        // limit 101 (above the range).
        ("  folder: \"INBOX\"\n  limit: \"101\"\n", "search"),
        // empty uids list.
        (
            "  folder: \"INBOX\"\n  uids: \"\"\n  seen: \"true\"\n",
            "set_flags",
        ),
        // non-numeric uid.
        ("  folder: \"INBOX\"\n  uid: \"abc\"\n", "fetch"),
    ];
    for (params, op) in cases {
        let spec = spec_yaml(op, params);
        let err = build(
            &spec,
            &acct,
            &json!({}),
            &secrets("pw"),
            Vec::new(),
            false,
            15,
        )
        .unwrap_err();
        assert_eq!(
            err.code,
            CallErrorCode::InvalidArgs,
            "op {op} params {params:?} message {}",
            err.message
        );
    }
}
