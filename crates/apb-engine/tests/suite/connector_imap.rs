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
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use apb_core::connector::def::ImapSpec;
use apb_core::secret::Secret;
use apb_engine::connector::call::CallErrorCode;
use apb_engine::connector::imap::{ImapBuild, ImapExtras, build};

use crate::common::{self, spawn_script};
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
    Literal {
        seen: bool,
        raw: Vec<u8>,
    },
    /// The partial-section answer `save_attachments` reads:
    /// `BODY[]<0> {n}`, with whatever size the server advertises alongside
    /// it (RFC822.SIZE, never trusted by the client).
    Partial {
        raw: Vec<u8>,
        advertised_size: Option<u64>,
    },
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
    /// When set, the LIST answer names a folder carrying the `\Drafts`
    /// special-use attribute (D7).
    drafts_special_use: bool,
    /// When set, the LIST answer also names a folder with this name.
    extra_folder: Option<String>,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            stall: false,
            login_reject: None,
            examine_reject: None,
            search_ids: Vec::new(),
            fetch: FetchResp::None,
            drafts_special_use: false,
            extra_folder: None,
        }
    }
}

/// What the listener recorded, for assertions.
#[derive(Default, Clone, Debug)]
struct Recorded {
    /// Every command line the client sent (trimmed), in order.
    lines: Vec<String>,
    /// The base64 SASL line the client sent after `AUTHENTICATE`.
    auth_payload: Option<String>,
    /// The literals `APPEND` received (D7), in order.
    appends: Vec<Vec<u8>>,
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

/// The partial-section answer `save_attachments` reads: `BODY[]<0> {n}`,
/// optionally advertising an RFC822.SIZE the client must not trust.
fn send_partial(w: &mut TcpStream, tag: &str, uid: u32, raw: &[u8], advertised_size: Option<u64>) {
    let mut attrs = vec![format!("UID {uid}")];
    if let Some(size) = advertised_size {
        attrs.push(format!("RFC822.SIZE {size}"));
    }
    // The octet count ends the FETCH line (like `send_literal`'s plain
    // `BODY[]`): the literal bytes follow it, then the closing paren.
    let header = format!(
        "* 1 FETCH ({} BODY[]<0> {{{}}}\r\n",
        attrs.join(" "),
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
        // Answer nothing and keep the connection open until the client gives
        // up and closes it. A fixed sleep closed the socket first on a
        // loaded host, with the client's command unread, and the client saw
        // a reset instead of its own timeout.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut reader, &mut sink);
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
                if script.drafts_special_use {
                    let _ = w.write_all(b"* LIST (\\HasNoChildren \\Drafts) \".\" \"Drafts\"\r\n");
                }
                if let Some(name) = &script.extra_folder {
                    let _ = w.write_all(
                        format!("* LIST (\\HasNoChildren) \".\" \"{name}\"\r\n").as_bytes(),
                    );
                }
                let _ = w.write_all(b"* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n");
                let _ = w.write_all(b"* LIST (\\HasNoChildren \\Sent) \".\" \"Sent\"\r\n");
                ok(&mut w, &tag);
            }
            "APPEND" => {
                // `APPEND "mailbox" (flags) {n}`: answer the continuation,
                // read exactly n literal bytes plus their closing CRLF,
                // record them, then complete.
                let len: usize = cmd
                    .rsplit('{')
                    .next()
                    .and_then(|s| s.trim_end_matches('}').trim().parse().ok())
                    .unwrap_or(0);
                let _ = w.write_all(b"+ Ready\r\n");
                let mut buf = vec![0u8; len];
                if len > 0 {
                    let _ = reader.read_exact(&mut buf);
                }
                let _ = read_line(&mut reader);
                rec.lock().unwrap().appends.push(buf);
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
                        FetchResp::Partial {
                            raw,
                            advertised_size,
                        } => {
                            // Echo the uid the client asked for (token 3, as
                            // the STORE arm reads its uid-set).
                            let uid = cmd
                                .split_whitespace()
                                .nth(3)
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(42);
                            send_partial(&mut w, &tag, uid, raw, *advertised_size)
                        }
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
    yaml_serde::from_str(&yaml).unwrap()
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

fn secrets(password: &str) -> BTreeMap<String, Secret> {
    BTreeMap::from([("password".into(), Secret::new(password))])
}

/// Builds a real (non-dry-run) call and unwraps it, or panics.
fn build_call(
    spec: &ImapSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, Secret>,
) -> Box<apb_engine::connector::imap::ImapCall> {
    match build(
        spec,
        account,
        args,
        secrets,
        Vec::new(),
        false,
        15,
        &ImapExtras::default(),
    )
    .unwrap()
    {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    }
}

// -- Tests --

#[test]
fn a_built_call_never_prints_its_secret() {
    let spec = spec_yaml("verify", "  {}\n");
    let call = build_call(
        &spec,
        &account("127.0.0.1", 1, "password"),
        &json!({}),
        &secrets("imap-pass-not-for-logs"),
    );
    let shown = format!("{call:?}");
    assert!(
        !shown.contains("imap-pass-not-for-logs"),
        "Debug of an imap call carried the password"
    );
}

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

/// Set only in the child process of the test below.
const DEBUG_CHILD: &str = "APB_TEST_IMAP_DEBUG_CHILD";
const DEBUG_PASSWORD: &str = "imap-debug-pass-not-for-stderr";

/// The child half of `the_environment_never_turns_on_imap_protocol_debugging`:
/// a real LOGIN through `send`. A normal run does nothing here.
#[test]
fn imap_debug_child_logs_in() {
    if std::env::var_os(DEBUG_CHILD).is_none() {
        return;
    }
    let srv = spawn(Script::default());
    let call = build_call(
        &spec_yaml("verify", "  {}\n"),
        &account(&srv.host, srv.port, "password"),
        &json!({}),
        &secrets(DEBUG_PASSWORD),
    );
    call.send().expect("verify should succeed");
    let r = srv.recorded();
    assert!(
        r.lines
            .iter()
            .any(|l| l.contains("LOGIN") && l.contains(DEBUG_PASSWORD)),
        "the LOGIN carried the password: {:?}",
        r.lines
    );
}

/// The imap crate's protocol debugging prints every line, LOGIN and the
/// XOAUTH2 payload included, to stderr. A real call made with the old
/// debug variable set prints none of it: the child process logs in with
/// `APB_IMAP_DEBUG` set and its stderr is read whole.
#[test]
fn the_environment_never_turns_on_imap_protocol_debugging() {
    let module = module_path!()
        .split_once("::")
        .map(|(_, rest)| rest)
        .unwrap_or(module_path!());
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .arg(format!("{module}::imap_debug_child_logs_in"))
        .args(["--exact", "--nocapture", "--test-threads=1"])
        .env(DEBUG_CHILD, "1")
        .env("APB_IMAP_DEBUG", "1")
        .output()
        .expect("the test binary runs again");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(
        stdout.contains("1 passed"),
        "the child did not run the login: {stdout}"
    );
    for text in [&stdout, &stderr] {
        assert!(
            !text.contains(DEBUG_PASSWORD),
            "the password reached the output: {text}"
        );
        assert!(
            !text.contains("C: ") && !text.contains("S: "),
            "protocol lines reached the output: {text}"
        );
    }
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
    let err = build(
        &spec,
        &acct,
        &args,
        &secrets("pw"),
        Vec::new(),
        false,
        15,
        &ImapExtras::default(),
    )
    .unwrap_err();
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
        &ImapExtras::default(),
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
        &ImapExtras::default(),
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
            &ImapExtras::default(),
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

/// The OAuth2 pieces an `oauth2` test needs: the auth block, the account
/// fields naming the scripted token endpoint, and the secrets map.
fn oauth_extras<'a>(
    auth: &'a apb_core::connector::def::AuthSpec,
    connector: &'a str,
    account_name: &'a str,
) -> ImapExtras<'a> {
    ImapExtras {
        auth: Some(auth),
        files: None,
        attachments_dir: None,
        connector,
        account: account_name,
        deadline: None,
    }
}

fn oauth_auth(token_url: &str) -> apb_core::connector::def::AuthSpec {
    use apb_core::connector::def::AuthSpec;
    AuthSpec::Oauth2Refresh {
        token_url: token_url.to_string(),
        authorize_url: None,
        client_id_template: "{{account.client_id}}".to_string(),
        client_secret_template: None,
        refresh_token_template: "{{secret.refresh_token}}".to_string(),
        granted_scopes_template: None,
        consent_scopes: None,
        authorize_params: BTreeMap::new(),
    }
}

fn oauth_account(host: &str, port: u16, token_url: &str) -> BTreeMap<String, String> {
    let mut map = account(host, port, "oauth2");
    map.insert("token_url".into(), token_url.to_string());
    map.insert("client_id".into(), "client-id-1".into());
    map
}

fn oauth_secrets() -> BTreeMap<String, Secret> {
    BTreeMap::from([
        ("password".into(), Secret::new("pw")),
        ("refresh_token".into(), Secret::new("imap-refresh-1")),
    ])
}

const IMAP_MINTED: &str = "imap-minted-access-1";
const IMAP_ROTATED: &str = "imap-rotated-refresh-2";

#[test]
fn oauth2_method_mints_and_sends_the_token_over_xoauth2() {
    use base64::Engine;
    let token_server = spawn_script(vec![common::Reply::Respond(
        200,
        json!({ "access_token": IMAP_MINTED, "expires_in": 3600 }).to_string(),
    )]);
    let srv = spawn(Script::default());
    let spec = spec_yaml("verify", "  {}\n");
    let auth = oauth_auth(&format!("{}/token", token_server.base_url));
    let extras = oauth_extras(&auth, "imap", "acct1");
    let call = match build(
        &spec,
        &oauth_account(
            &srv.host,
            srv.port,
            &format!("{}/token", token_server.base_url),
        ),
        &json!({}),
        &oauth_secrets(),
        Vec::new(),
        false,
        15,
        &extras,
    )
    .unwrap()
    {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    };
    let ok = call.send().expect("oauth2 verify should succeed");
    assert_eq!(ok.body()["authenticated"], json!(true));

    let r = srv.recorded();
    let payload = r.auth_payload.expect("expected a SASL payload line");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .expect("payload is base64");
    assert_eq!(
        decoded,
        format!("user=u\x01auth=Bearer {IMAP_MINTED}\x01\x01").into_bytes()
    );
    // The token request carried the refresh token of the account.
    let token_req = &token_server.requests()[0];
    assert!(
        token_req.starts_with("POST /token HTTP/1.1"),
        "{}",
        token_req
    );
    assert!(
        token_req.contains("grant_type=refresh_token"),
        "{}",
        token_req
    );
}

/// The caller's deadline bounds the token request too: a token endpoint
/// that never answers cannot hold the build for the function's whole
/// `timeout_sec`.
#[test]
fn oauth2_minting_honours_the_callers_deadline() {
    // A token endpoint that accepts and never answers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    let _hold = std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().take(2).collect();
        std::thread::sleep(Duration::from_secs(20));
        drop(held);
    });
    let srv = spawn(Script::default());
    let spec = spec_yaml("verify", "  {}\n");
    let auth = oauth_auth(&token_url);
    let extras = ImapExtras {
        deadline: Some(std::time::Instant::now() + Duration::from_secs(1)),
        ..oauth_extras(&auth, "imap", "acct1")
    };
    let started = std::time::Instant::now();
    let result = build(
        &spec,
        &oauth_account(&srv.host, srv.port, &token_url),
        &json!({}),
        &oauth_secrets(),
        Vec::new(),
        false,
        15,
        &extras,
    );
    let elapsed = started.elapsed();
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("a token endpoint that never answers cannot mint"),
    };
    assert_eq!(err.code, CallErrorCode::Timeout, "{}", err.message);
    assert!(
        elapsed < Duration::from_secs(5),
        "minting ran past the caller's deadline: {elapsed:?}"
    );
}

#[test]
fn oauth2_without_an_auth_block_is_a_config_error() {
    let srv = spawn(Script::default());
    let spec = spec_yaml("verify", "  {}\n");
    let err = build(
        &spec,
        &account(&srv.host, srv.port, "oauth2"),
        &json!({}),
        &secrets("pw"),
        Vec::new(),
        false,
        15,
        &ImapExtras::default(),
    )
    .unwrap_err();
    assert_eq!(err.code, CallErrorCode::Config);
    assert!(err.message.contains("oauth2_refresh"), "{}", err.message);
}

#[test]
fn oauth2_rotation_warns_through_the_imap_result() {
    let token_server = spawn_script(vec![common::Reply::Respond(
        200,
        json!({
            "access_token": IMAP_MINTED,
            "expires_in": 3600,
            "refresh_token": IMAP_ROTATED,
        })
        .to_string(),
    )]);
    let srv = spawn(Script::default());
    let spec = spec_yaml("verify", "  {}\n");
    let auth = oauth_auth(&format!("{}/token", token_server.base_url));
    let extras = oauth_extras(&auth, "imap", "acct1");
    let call = match build(
        &spec,
        &oauth_account(
            &srv.host,
            srv.port,
            &format!("{}/token", token_server.base_url),
        ),
        &json!({}),
        &oauth_secrets(),
        Vec::new(),
        false,
        15,
        &extras,
    )
    .unwrap()
    {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    };
    let warnings = call.warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("refresh token"), "{warnings:?}");
    // The rotated value never travels: not in the warnings, not in the
    // redaction labels.
    assert!(!warnings[0].contains(IMAP_ROTATED));
    call.send().expect("the login itself succeeds");
}

#[test]
fn oauth2_token_echoed_in_a_server_no_is_redacted() {
    let token_server = spawn_script(vec![common::Reply::Respond(
        200,
        json!({ "access_token": IMAP_MINTED, "expires_in": 3600 }).to_string(),
    )]);
    let srv = spawn(Script {
        login_reject: Some(format!("bad token {IMAP_MINTED}")),
        ..Script::default()
    });
    let spec = spec_yaml("verify", "  {}\n");
    let auth = oauth_auth(&format!("{}/token", token_server.base_url));
    let extras = oauth_extras(&auth, "imap", "acct1");
    let call = match build(
        &spec,
        &oauth_account(
            &srv.host,
            srv.port,
            &format!("{}/token", token_server.base_url),
        ),
        &json!({}),
        &oauth_secrets(),
        Vec::new(),
        false,
        15,
        &extras,
    )
    .unwrap()
    {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    };
    let err = call.send().unwrap_err();
    assert_eq!(err.code, CallErrorCode::Auth);
    assert!(
        !err.message.contains(IMAP_MINTED),
        "the NO text leaked the token: {}",
        err.message
    );
    assert!(
        err.message.contains("[redacted:oauth:access_token]"),
        "{}",
        err.message
    );
}

#[test]
fn oauth2_token_echoed_in_a_successful_result_is_redacted() {
    let token_server = spawn_script(vec![common::Reply::Respond(
        200,
        json!({ "access_token": IMAP_MINTED, "expires_in": 3600 }).to_string(),
    )]);
    let srv = spawn(Script {
        extra_folder: Some(format!("leak-{IMAP_MINTED}")),
        ..Script::default()
    });
    let spec = spec_yaml("list_folders", "  {}\n");
    let auth = oauth_auth(&format!("{}/token", token_server.base_url));
    let extras = oauth_extras(&auth, "imap", "acct1");
    let call = match build(
        &spec,
        &oauth_account(
            &srv.host,
            srv.port,
            &format!("{}/token", token_server.base_url),
        ),
        &json!({}),
        &oauth_secrets(),
        Vec::new(),
        false,
        15,
        &extras,
    )
    .unwrap()
    {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    };
    let ok = call.send().expect("the listing succeeds");
    let body = ok.body().to_string();
    assert!(
        !body.contains(IMAP_MINTED),
        "the listing leaked the token: {body}"
    );
    assert!(body.contains("[redacted:oauth:access_token]"), "{body}");
}

// -- D7: append_draft and save_attachments --

/// An append_draft spec, with or without the optional `folder` param, and
/// the connection fields every imap spec carries.
fn append_draft_spec(with_folder_param: bool) -> ImapSpec {
    let yaml = format!(
        "connection:\n  host: \"{{{{account.host}}}}\"\n  port: \"{{{{account.port}}}}\"\n  use_tls: \"{{{{account.use_tls}}}}\"\n  auth_method: \"{{{{account.auth_method}}}}\"\n  username: \"{{{{account.username}}}}\"\n  password: \"{{{{secret.password}}}}\"\nop: append_draft\nparams:\n{}\nmessage:\n  from_email: \"{{{{account.from_email}}}}\"\n  from_name: \"{{{{account.from_name}}}}\"\n  to: \"{{{{args.to}}}}\"\n  cc: \"{{{{args.cc}}}}\"\n  bcc: \"{{{{args.bcc}}}}\"\n  subject: \"{{{{args.subject}}}}\"\n  in_reply_to: \"{{{{args.in_reply_to}}}}\"\n  body_text: \"{{{{args.body_text}}}}\"\n",
        if with_folder_param {
            "  folder: \"{{args.folder}}\"\n"
        } else {
            ""
        },
    );
    yaml_serde::from_str(&yaml).unwrap()
}

/// A save_attachments spec: folder and uid come from the args.
fn save_attachments_spec() -> ImapSpec {
    let yaml = "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  auth_method: \"{{account.auth_method}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nop: save_attachments\nparams:\n  folder: \"{{args.folder}}\"\n  uid: \"{{args.uid}}\"\n";
    yaml_serde::from_str(yaml).unwrap()
}

/// The account map of an append_draft test: the sender the Message-ID
/// domain comes from.
fn sender_account(host: &str, port: u16) -> BTreeMap<String, String> {
    let mut acct = account(host, port, "password");
    acct.insert("from_email".into(), "agent@acme.example".into());
    acct.insert("from_name".into(), "The Agent".into());
    acct
}

/// Builds a real call with explicit extras (the run directory pieces the
/// D7 ops read), or panics.
fn build_call_with(
    spec: &ImapSpec,
    acct: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, Secret>,
    extras: &ImapExtras<'_>,
) -> Box<apb_engine::connector::imap::ImapCall> {
    match build(spec, acct, args, secrets, Vec::new(), false, 30, extras).unwrap() {
        ImapBuild::Call(c) => c,
        _ => panic!("expected a call"),
    }
}

/// A raw multipart message carrying one attachment per `parts` entry
/// (declared name, raw body bytes, no transfer encoding).
fn multipart_with_attachments(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend_from_slice(
        concat!(
            "From: Alice <alice@example.com>\r\n",
            "To: Bob <bob@example.com>\r\n",
            "Subject: Files\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"ATT\"\r\n",
            "\r\n",
        )
        .as_bytes(),
    );
    for (name, body) in parts {
        raw.extend_from_slice(
            format!(
                "--ATT\r\nContent-Type: application/octet-stream; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\r\n"
            )
            .as_bytes(),
        );
        raw.extend_from_slice(body);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"--ATT--\r\n");
    raw
}

const MIB: usize = 1024 * 1024;

/// The `attachments_dir` value `prepare` composes for node `n1` with no
/// list item.
const NODE_ATTACH_DIR: &str = "attachments/n1";

#[test]
fn append_draft_detects_the_drafts_folder_and_sets_draft_flags() {
    let srv = spawn(Script {
        drafts_special_use: true,
        ..Script::default()
    });
    let spec = append_draft_spec(false);
    let args = json!({
        "to": "peer@example.org",
        "cc": "watcher@example.org",
        "bcc": "quiet@example.org",
        "subject": "Draft reply",
        "in_reply_to": "<orig@example.org>",
        "body_text": "Please look at the plan.",
    });
    let call = build_call_with(
        &spec,
        &sender_account(&srv.host, srv.port),
        &args,
        &secrets("pw"),
        &ImapExtras::default(),
    );
    let ok = call.send().expect("append_draft should succeed");
    let body = ok.body();
    assert_eq!(body["folder"], json!("Drafts"));
    assert_eq!(body["appended"], json!(true));
    assert_eq!(body["subject"], json!("Draft reply"));
    let message_id = body["message_id"].as_str().expect("a message_id");
    assert!(message_id.starts_with("apb."), "{message_id}");
    assert!(message_id.ends_with("@acme.example"), "{message_id}");

    let r = srv.recorded();
    // The drafts folder came from the LIST, without a folder param.
    assert!(
        r.lines.iter().any(|l| l.contains("LIST")),
        "expected a LIST, got {:?}",
        r.lines
    );
    let append_line = r
        .lines
        .iter()
        .find(|l| l.contains("APPEND"))
        .expect("an APPEND line");
    assert!(
        append_line.contains("APPEND \"Drafts\" (\\Draft \\Seen)"),
        "append line: {append_line}"
    );
    // The appended literal carries the composed message, its Bcc header,
    // the threading header and the generated Message-ID.
    let literal = r.appends.first().expect("the APPEND literal");
    let text = String::from_utf8_lossy(literal);
    assert!(text.contains("Bcc: quiet@example.org"), "{text}");
    assert!(text.contains("In-Reply-To: <orig@example.org>"), "{text}");
    assert!(
        text.contains(&format!("Message-ID: {message_id}")),
        "{text}"
    );
    assert!(text.contains("Please look at the plan."), "{text}");
}

#[test]
fn append_draft_without_a_drafts_folder_asks_for_one() {
    let srv = spawn(Script::default());
    let spec = append_draft_spec(false);
    let args = json!({
        "to": "peer@example.org",
        "subject": "Draft reply",
        "body_text": "text",
    });
    let err = build_call_with(
        &spec,
        &sender_account(&srv.host, srv.port),
        &args,
        &secrets("pw"),
        &ImapExtras::default(),
    )
    .send()
    .unwrap_err();
    assert_eq!(
        err.code,
        CallErrorCode::InvalidArgs,
        "message: {}",
        err.message
    );
    assert!(
        err.message.contains("\\Drafts") && err.message.contains("folder"),
        "message: {}",
        err.message
    );

    // With an explicit folder the call appends there and never lists.
    let srv2 = spawn(Script::default());
    let spec2 = append_draft_spec(true);
    let args2 = json!({
        "folder": "MyDrafts",
        "to": "peer@example.org",
        "subject": "Draft reply",
        "body_text": "text",
    });
    let ok = build_call_with(
        &spec2,
        &sender_account(&srv2.host, srv2.port),
        &args2,
        &secrets("pw"),
        &ImapExtras::default(),
    )
    .send()
    .expect("append_draft with a folder should succeed");
    assert_eq!(ok.body()["folder"], json!("MyDrafts"));
    let r2 = srv2.recorded();
    assert!(
        !r2.lines.iter().any(|l| l.contains(" LIST")),
        "an explicit folder must not LIST: {:?}",
        r2.lines
    );
    assert!(
        r2.lines
            .iter()
            .any(|l| l.contains("APPEND \"MyDrafts\" (\\Draft \\Seen)")),
        "{:?}",
        r2.lines
    );
}

#[test]
fn save_attachments_writes_sanitized_names_and_never_overwrites() {
    let _scratch = std::env::var("APB_SCRATCH_DEBUG").is_ok();
    let run = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let body = multipart_with_attachments(&[
        ("../../evil.sh", b"evil bytes".as_slice()),
        ("a.txt", b"first a".as_slice()),
        ("a.txt", b"second a".as_slice()),
    ]);
    let srv = spawn(Script {
        fetch: FetchResp::Partial {
            raw: body,
            advertised_size: None,
        },
        ..Script::default()
    });
    // A file already sitting where the first `a.txt` would land.
    let uid_dir = run.path().join(NODE_ATTACH_DIR).join("7");
    std::fs::create_dir_all(&uid_dir).unwrap();
    std::fs::write(uid_dir.join("a.txt"), b"keep me").unwrap();

    let spec = save_attachments_spec();
    let args = json!({ "folder": "INBOX", "uid": 7 });
    let extras = ImapExtras {
        files: Some(run.path()),
        attachments_dir: Some(NODE_ATTACH_DIR),
        ..ImapExtras::default()
    };
    let call = build_call_with(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
        &extras,
    );
    let ok = match call.send() {
        Ok(ok) => ok,
        Err(e) => panic!(
            "save_attachments failed: {e:?}\nrecorded: {:?}",
            srv.recorded()
        ),
    };
    let body = ok.body();
    assert_eq!(body["folder"], json!("INBOX"));
    assert_eq!(body["uid"], json!(7));
    let saved = body["saved"].as_array().unwrap();
    let names: Vec<&str> = saved
        .iter()
        .map(|s| s["path"].as_str().unwrap().rsplit('/').next().unwrap())
        .collect();
    assert_eq!(names, vec!["evil.sh", "a-2.txt", "a-3.txt"], "{saved:?}");
    assert_eq!(saved[0]["filename"], json!("../../evil.sh"));
    assert_eq!(saved[0]["mime"], json!("application/octet-stream"));
    assert_eq!(saved[0]["size"], json!(10));
    for entry in saved {
        let path = entry["path"].as_str().unwrap();
        assert!(
            path.starts_with(uid_dir.to_str().unwrap()),
            "saved outside the uid dir: {path}"
        );
    }
    assert!(body["skipped"].as_array().unwrap().is_empty(), "{body}");

    // The sanitized name lost its path parts, the pre-existing file kept
    // its bytes, and each written file holds its own attachment's bytes.
    assert_eq!(
        std::fs::read(uid_dir.join("evil.sh")).unwrap(),
        b"evil bytes"
    );
    assert_eq!(std::fs::read(uid_dir.join("a.txt")).unwrap(), b"keep me");
    assert_eq!(std::fs::read(uid_dir.join("a-2.txt")).unwrap(), b"first a");
    assert_eq!(std::fs::read(uid_dir.join("a-3.txt")).unwrap(), b"second a");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);

    // Silent-read guarantee on the wire: EXAMINE and one BODY.PEEK[]
    // partial fetch, never SELECT, never a bare BODY[.
    let r = srv.recorded();
    assert!(
        r.lines.iter().any(|l| l.contains("EXAMINE \"INBOX\"")),
        "{:?}",
        r.lines
    );
    let fetch = r
        .lines
        .iter()
        .find(|l| l.contains("UID FETCH"))
        .expect("a UID FETCH line");
    assert!(fetch.contains("BODY.PEEK[]<0.52428801>"), "fetch: {fetch}");
    for line in &r.lines {
        assert!(
            !line.contains("SELECT"),
            "save_attachments must not SELECT: {line}"
        );
    }
}

#[test]
fn save_attachments_outside_a_run_is_refused() {
    let spec = save_attachments_spec();
    let args = json!({ "folder": "INBOX", "uid": 7 });
    let err = build(
        &spec,
        &account("127.0.0.1", 1, "password"),
        &args,
        &secrets("pw"),
        Vec::new(),
        false,
        15,
        &ImapExtras::default(),
    )
    .unwrap_err();
    assert_eq!(err.code, CallErrorCode::Permission, "{}", err.message);
    assert!(
        err.message.contains("only inside a run"),
        "message: {}",
        err.message
    );
}

#[test]
fn save_attachments_skips_a_file_over_the_cap() {
    let run = tempfile::tempdir().unwrap();
    let mut big = vec![b'A'; 11 * MIB];
    big[0] = b'B';
    let body = multipart_with_attachments(&[("big.bin", &big), ("small.txt", b"tiny".as_slice())]);
    let srv = spawn(Script {
        fetch: FetchResp::Partial {
            raw: body,
            advertised_size: Some(100),
        },
        ..Script::default()
    });
    let spec = save_attachments_spec();
    let args = json!({ "folder": "INBOX", "uid": 7 });
    let extras = ImapExtras {
        files: Some(run.path()),
        attachments_dir: Some(NODE_ATTACH_DIR),
        ..ImapExtras::default()
    };
    let call = build_call_with(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
        &extras,
    );
    let ok = call.send().expect("save_attachments should succeed");
    let body = ok.body();
    let saved = body["saved"].as_array().unwrap();
    assert_eq!(saved.len(), 1, "{body}");
    assert_eq!(saved[0]["filename"], json!("small.txt"));
    let skipped = body["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{body}");
    assert_eq!(skipped[0]["filename"], json!("big.bin"));
    assert_eq!(skipped[0]["reason"], json!("too_large"));

    let uid_dir = run.path().join(NODE_ATTACH_DIR).join("7");
    assert_eq!(std::fs::read(uid_dir.join("small.txt")).unwrap(), b"tiny");
    assert!(!uid_dir.join("big.bin").exists());
}

#[test]
fn save_attachments_respects_the_call_cap_before_writing() {
    let run = tempfile::tempdir().unwrap();
    let nine = vec![b'A'; 9 * MIB];
    let one = vec![b'A'; MIB];
    let body = multipart_with_attachments(&[
        ("a1.bin", &nine),
        ("a2.bin", &nine),
        ("a3.bin", &nine),
        ("a4.bin", &one),
    ]);
    let srv = spawn(Script {
        fetch: FetchResp::Partial {
            raw: body,
            advertised_size: None,
        },
        ..Script::default()
    });
    let spec = save_attachments_spec();
    let args = json!({ "folder": "INBOX", "uid": 7 });
    let extras = ImapExtras {
        files: Some(run.path()),
        attachments_dir: Some(NODE_ATTACH_DIR),
        ..ImapExtras::default()
    };
    let call = build_call_with(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
        &extras,
    );
    let ok = call.send().expect("save_attachments should succeed");
    let body = ok.body();
    let saved = body["saved"].as_array().unwrap();
    let names: Vec<&str> = saved
        .iter()
        .map(|s| s["filename"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["a1.bin", "a2.bin", "a4.bin"], "{body}");
    let skipped = body["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{body}");
    assert_eq!(skipped[0]["filename"], json!("a3.bin"));
    assert_eq!(skipped[0]["reason"], json!("call_cap"));

    // 19 MiB landed: two 9 MiB files and the 1 MiB one, each exact.
    let uid_dir = run.path().join(NODE_ATTACH_DIR).join("7");
    let mut total = 0usize;
    let mut written = 0usize;
    for entry in std::fs::read_dir(&uid_dir).unwrap() {
        let len = entry.unwrap().metadata().unwrap().len() as usize;
        total += len;
        written += 1;
    }
    assert_eq!(written, 3);
    assert_eq!(total, 19 * MIB);
}

#[test]
fn save_attachments_stops_at_twenty_files() {
    let run = tempfile::tempdir().unwrap();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..21 {
        parts.push((format!("f{i:02}.txt"), b"payload".to_vec()));
    }
    let refs: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let body = multipart_with_attachments(&refs);
    let srv = spawn(Script {
        fetch: FetchResp::Partial {
            raw: body,
            advertised_size: None,
        },
        ..Script::default()
    });
    let spec = save_attachments_spec();
    let args = json!({ "folder": "INBOX", "uid": 7 });
    let extras = ImapExtras {
        files: Some(run.path()),
        attachments_dir: Some(NODE_ATTACH_DIR),
        ..ImapExtras::default()
    };
    let call = build_call_with(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
        &extras,
    );
    let ok = call.send().expect("save_attachments should succeed");
    let body = ok.body();
    let saved = body["saved"].as_array().unwrap();
    assert_eq!(saved.len(), 20, "{body}");
    let skipped = body["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{body}");
    assert_eq!(skipped[0]["filename"], json!("f20.txt"));
    assert_eq!(skipped[0]["reason"], json!("file_cap"));
    let uid_dir = run.path().join(NODE_ATTACH_DIR).join("7");
    assert_eq!(std::fs::read_dir(&uid_dir).unwrap().count(), 20);
}

#[test]
fn save_attachments_ignores_the_advertised_size() {
    // The server advertises 100 bytes but sends a body over the 50 MiB
    // message cap for the partial fetch: the call fails invalid_args and
    // nothing is written.
    let run = tempfile::tempdir().unwrap();
    let mut raw = b"From: a@x.org\r\nSubject: Huge\r\nContent-Type: text/plain\r\n\r\n".to_vec();
    raw.resize(raw.len() + 52428801, b'A');
    let srv = spawn(Script {
        fetch: FetchResp::Partial {
            raw,
            advertised_size: Some(100),
        },
        ..Script::default()
    });
    let spec = save_attachments_spec();
    let args = json!({ "folder": "INBOX", "uid": 7 });
    let extras = ImapExtras {
        files: Some(run.path()),
        attachments_dir: Some(NODE_ATTACH_DIR),
        ..ImapExtras::default()
    };
    let err = build_call_with(
        &spec,
        &account(&srv.host, srv.port, "password"),
        &args,
        &secrets("pw"),
        &extras,
    )
    .send()
    .unwrap_err();
    assert_eq!(
        err.code,
        CallErrorCode::InvalidArgs,
        "message: {}",
        err.message
    );
    assert!(err.message.contains("50 MiB"), "message: {}", err.message);
    let node_dir = run.path().join(NODE_ATTACH_DIR);
    assert!(
        !node_dir.exists() || std::fs::read_dir(&node_dir).unwrap().next().is_none(),
        "a failed save wrote into the run"
    );

    // An absent RFC822.SIZE changes nothing: the same fetch with a small
    // body and no advertised size succeeds.
    let run2 = tempfile::tempdir().unwrap();
    let body = multipart_with_attachments(&[("s.txt", b"bytes".as_slice())]);
    let srv2 = spawn(Script {
        fetch: FetchResp::Partial {
            raw: body,
            advertised_size: None,
        },
        ..Script::default()
    });
    let extras2 = ImapExtras {
        files: Some(run2.path()),
        attachments_dir: Some(NODE_ATTACH_DIR),
        ..ImapExtras::default()
    };
    let ok = build_call_with(
        &spec,
        &account(&srv2.host, srv2.port, "password"),
        &args,
        &secrets("pw"),
        &extras2,
    )
    .send()
    .expect("a small message saves without an advertised size");
    assert_eq!(ok.body()["saved"].as_array().unwrap().len(), 1);
}

#[cfg(unix)]
mod symlink_refusals {
    use super::*;

    /// Runs a save_attachments call against `run`, where a pre-created
    /// directory component is a symlink to `outside`; the call must fail
    /// and nothing may appear outside.
    fn refused_with_link(
        linked: &std::path::Path,
        outside: &std::path::Path,
        run: &std::path::Path,
    ) {
        let srv = spawn(Script {
            fetch: FetchResp::Partial {
                raw: multipart_with_attachments(&[("x.txt", b"bytes".as_slice())]),
                advertised_size: None,
            },
            ..Script::default()
        });
        let spec = save_attachments_spec();
        let args = json!({ "folder": "INBOX", "uid": 7 });
        let extras = ImapExtras {
            files: Some(run),
            attachments_dir: Some(NODE_ATTACH_DIR),
            ..ImapExtras::default()
        };
        let err = build_call_with(
            &spec,
            &account(&srv.host, srv.port, "password"),
            &args,
            &secrets("pw"),
            &extras,
        )
        .send()
        .unwrap_err();
        assert_eq!(err.code, CallErrorCode::Permission, "{}", err.message);
        assert!(
            std::fs::read_dir(outside).unwrap().next().is_none(),
            "something was written outside the run at {linked:?}"
        );
    }

    #[test]
    fn save_attachments_refuses_a_symlinked_attachments_dir() {
        let run = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), run.path().join("attachments")).unwrap();
        refused_with_link(&run.path().join("attachments"), outside.path(), run.path());
    }

    #[test]
    fn save_attachments_refuses_a_symlinked_node_dir() {
        let run = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(run.path().join("attachments")).unwrap();
        std::os::unix::fs::symlink(outside.path(), run.path().join(NODE_ATTACH_DIR)).unwrap();
        refused_with_link(
            run.path().join(NODE_ATTACH_DIR).as_path(),
            outside.path(),
            run.path(),
        );
    }

    #[test]
    fn save_attachments_refuses_a_symlinked_uid_dir() {
        let run = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(run.path().join(NODE_ATTACH_DIR)).unwrap();
        std::os::unix::fs::symlink(outside.path(), run.path().join(NODE_ATTACH_DIR).join("7"))
            .unwrap();
        refused_with_link(
            run.path().join(NODE_ATTACH_DIR).join("7").as_path(),
            outside.path(),
            run.path(),
        );
    }
}
