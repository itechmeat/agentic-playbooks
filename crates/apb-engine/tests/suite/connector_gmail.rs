//! Engine tests for the `gmail` connector (D4): a live `create_draft` call
//! composes the RFC 5322 message (From from the account email, the
//! threading headers, the kept Bcc), base64url-encodes it into
//! `message.raw`, and neither the minted access token nor the refresh
//! token reaches the printed result, the journal, or any file under the
//! run dir or the project `.apb` dir. Every test runs offline against a
//! scripted HTTP server playing both the token endpoint and the Gmail
//! API, over the checked-in `connectors/gmail/connector.yaml`.

use std::collections::BTreeMap;

use apb_core::connector::marker::new_nonce;
use apb_engine::connector::call::{AttemptClaim, CallContext, CallRequest, execute, execute_with};
use apb_engine::event::{EventLog, EventPayload};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use base64::Engine as _;
use serde_json::{Value, json};

use crate::common::{Reply, spawn_script};

/// The checked-in manifest the run snapshot serves from: the same bytes an
/// installed `gmail` connector carries.
const GMAIL_YAML: &str = include_str!("../../../../connectors/gmail/connector.yaml");

const NODE: &str = "n";
const CONNECTOR: &str = "gmail";
const CLIENT_SECRET: &str = "gocsp-gmail-test-0123456789";
const REFRESH_A: &str = "1//0-gmail-refresh-a-0123456789";
const REFRESH_B: &str = "1//0-gmail-refresh-b-0123456789";
const ACCESS_A: &str = "ya29-gmail-access-a-0123456789";
const ACCESS_B: &str = "ya29-gmail-access-b-0123456789";
const SECRET_VAR_CS: &str = "APB_GMAIL_TEST_CLIENT_SECRET";
const SECRET_VAR_RT_A: &str = "APB_GMAIL_TEST_REFRESH_A";
const SECRET_VAR_RT_B: &str = "APB_GMAIL_TEST_REFRESH_B";
const ACCOUNT_EMAIL: &str = "agent@acme.example";
/// Any one of `create_draft`'s declared scopes is enough; say two.
const GRANTED_SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.modify https://mail.google.com/";

/// The run account: every non-secret field the manifest templates need,
/// with `api_base` and `token_url` pointed at the scripted server.
fn account_named(base: &str, refresh_var: &str) -> ManifestAccount {
    ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("api_base".to_string(), base.to_string()),
            ("token_url".to_string(), format!("{base}/token")),
            ("client_id".to_string(), "gmail-test-client".to_string()),
            (
                "client_secret".to_string(),
                format!("{{{{env.{SECRET_VAR_CS}}}}}"),
            ),
            (
                "refresh_token".to_string(),
                format!("{{{{env.{refresh_var}}}}}"),
            ),
            ("email".to_string(), ACCOUNT_EMAIL.to_string()),
        ]),
        env: BTreeMap::from([
            ("client_secret".to_string(), SECRET_VAR_CS.to_string()),
            ("refresh_token".to_string(), refresh_var.to_string()),
        ]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct1".to_string(),
    }
}

/// A seeded run: the checked-in gmail connector snapshot, one account, a
/// `create_draft` grant, the project secrets, and an open attempt 1.
struct Rig {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
    current: AttemptClaim,
}

fn rig(base: &str) -> Rig {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account_named(base, SECRET_VAR_RT_A)],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["create_draft".to_string()],
            max_calls: None,
            on_unknown: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), GMAIL_YAML).unwrap();
    let secrets = format!(
        "{SECRET_VAR_CS}={CLIENT_SECRET}\n{SECRET_VAR_RT_A}={REFRESH_A}\n{SECRET_VAR_RT_B}={REFRESH_B}\n"
    );
    let path = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, secrets).unwrap();

    let mut log = EventLog::create(run.path()).unwrap();
    log.append(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    })
    .unwrap();
    let token = new_nonce();
    log.append(EventPayload::AttemptStarted {
        item: None,
        cloud: None,
        node: NODE.into(),
        attempt: 1,
        agent: "claude".into(),
        soul_delivery: None,
        skills_mode: None,
        pid: Some(std::process::id()),
        spawn_ms: None,
        model: None,
        workdir: None,
        transcript: None,
        token: Some(token.clone()),
        isolation: None,
        exec: None,
    })
    .unwrap();
    Rig {
        run,
        root,
        current: AttemptClaim {
            item: None,
            attempt: Some(1),
            token: Some(token),
            host_task: None,
        },
    }
}

impl Rig {
    /// A send under the open attempt, with a `--intent` label.
    fn create_draft(&self, intent: &str, args: Value) -> (Value, bool) {
        execute_with(
            CallRequest {
                run_dir: self.run.path(),
                root: self.root.path(),
                node_id: NODE,
                connector: CONNECTOR,
                function: "create_draft",
                account: None,
                args,
                dry_run: false,
                full: false,
            },
            CallContext::node(Some(intent), self.current.clone()),
        )
    }

    /// Every text file under the run dir and the project `.apb` dir, as
    /// lossy strings, for the token-leak greps. The project secrets file
    /// is left out: it is the person's own store, written before the call.
    fn every_file_text(&self) -> Vec<String> {
        let mut texts = Vec::new();
        let store = self.root.path().join(".apb/secrets.env");
        for dir in [self.run.path(), &self.root.path().join(".apb")] {
            let mut stack = vec![dir.to_path_buf()];
            while let Some(d) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p == store {
                        continue;
                    }
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(bytes) = std::fs::read(&p) {
                        texts.push(String::from_utf8_lossy(&bytes).into_owned());
                    }
                }
            }
        }
        texts
    }
}

/// The args of a threaded reply draft: to, subject, body, the threading
/// headers, and a bcc the API body has no envelope slot for.
fn draft_args() -> Value {
    json!({
        "to": "peer@example.org",
        "bcc": "quiet@example.org",
        "subject": "Re: plan",
        "body_text": "Here is the reviewed plan.",
        "in_reply_to": "<orig@example.org>",
        "references": "<first@example.org> <orig@example.org>",
        "thread_id": "t1",
    })
}

/// The captured request line (`POST /users/me/drafts HTTP/1.1`).
fn request_line(raw: &str) -> String {
    raw.lines().next().unwrap_or("").to_string()
}

fn request_path(raw: &str) -> String {
    request_line(raw)
        .split(' ')
        .nth(1)
        .unwrap_or("")
        .to_string()
}

fn request_method(raw: &str) -> String {
    request_line(raw)
        .split(' ')
        .next()
        .unwrap_or("")
        .to_string()
}

/// A request header value, case-insensitive.
fn request_header(raw: &str, name: &str) -> Option<String> {
    raw.lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
}

fn request_body(raw: &str) -> &str {
    raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("")
}

/// One decoded form value of a captured token request body.
fn form_value(body: &str, key: &str) -> Option<String> {
    for pair in body.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return Some(
                v.replace("%2F", "/")
                    .replace("%3D", "=")
                    .replace("%26", "&"),
            );
        }
    }
    None
}

fn token_ok(access: &str) -> Reply {
    Reply::Respond(
        200,
        json!({
            "access_token": access,
            "expires_in": 3600,
            "scope": GRANTED_SCOPES,
        })
        .to_string(),
    )
}

fn draft_created() -> Reply {
    Reply::Respond(
        200,
        json!({ "id": "d777", "message": { "id": "m9", "threadId": "t1" } }).to_string(),
    )
}

/// Decodes `message.raw` of a captured drafts.create body (URL-safe
/// base64) into the RFC 5322 text.
fn decoded_raw(captured: &str) -> String {
    let body = serde_json::from_str::<Value>(request_body(captured)).unwrap();
    let raw = body["message"]["raw"].as_str().unwrap_or_else(|| {
        panic!("the body must carry message.raw, got {body}");
    });
    // The URL-safe alphabet only: a standard-base64 encoder leaks `+` or
    // `/` into the JSON body, which a URL-safe reader refuses.
    assert!(
        !raw.contains('+') && !raw.contains('/'),
        "message.raw must be URL-safe base64url, got {raw}"
    );
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(raw)
        .unwrap_or_else(|e| panic!("message.raw must be URL-safe base64url: {e}"));
    String::from_utf8(bytes).unwrap_or_else(|e| panic!("the message must be UTF-8: {e}"))
}

#[test]
fn create_draft_composes_the_message_and_keeps_bcc() {
    let server = spawn_script(vec![token_ok(ACCESS_A), draft_created()]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.create_draft("d1", draft_args());
    assert!(ok, "{value}");

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    // The token request: the refresh token minted a bearer access token.
    assert_eq!(request_method(&requests[0]), "POST");
    assert_eq!(request_path(&requests[0]), "/token");
    let token_body = request_body(&requests[0]);
    assert_eq!(
        form_value(token_body, "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(
        form_value(token_body, "refresh_token").as_deref(),
        Some(REFRESH_A)
    );
    assert_eq!(request_path(&requests[1]), "/users/me/drafts");
    assert_eq!(
        request_header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_A}").as_str())
    );

    // The body carries the thread and the composed message, base64url.
    let raw = decoded_raw(&requests[1]);
    assert!(
        raw.contains(&format!("From: {ACCOUNT_EMAIL}")),
        "the From header must be the account email: {raw}"
    );
    assert!(
        raw.contains("In-Reply-To: <orig@example.org>"),
        "the In-Reply-To header must carry the arg: {raw}"
    );
    assert!(
        raw.contains("References: <first@example.org> <orig@example.org>"),
        "the References header must carry the arg: {raw}"
    );
    assert!(
        raw.contains("Bcc: quiet@example.org"),
        "the Bcc header must be kept: the API takes no separate envelope: {raw}"
    );
    assert!(raw.contains("Subject: Re: plan"), "{raw}");
    assert!(raw.contains("To: peer@example.org"), "{raw}");
    assert!(raw.contains("Here is the reviewed plan."), "{raw}");
    let body = serde_json::from_str::<Value>(request_body(&requests[1])).unwrap();
    assert_eq!(body["message"]["threadId"], "t1");

    // The result carries the draft id and neither minted nor stored token.
    assert_eq!(value["ok"], true, "{value}");
    assert!(
        !value.to_string().contains(ACCESS_A),
        "the result leaked the access token: {value}"
    );
    assert!(
        !value.to_string().contains(REFRESH_A),
        "the result leaked the refresh token: {value}"
    );

    // The journal event carries neither token either.
    let journal = std::fs::read_to_string(rig.run.path().join("events.jsonl")).unwrap();
    for secret in [ACCESS_A, REFRESH_A, REFRESH_B, CLIENT_SECRET] {
        assert!(!journal.contains(secret), "the journal carries {secret}");
    }
}

#[test]
fn two_calls_leave_no_token_in_any_file() {
    // The minted access token is cached in process memory per binding, so
    // the second call reuses it: one token request, two drafts.
    let server = spawn_script(vec![token_ok(ACCESS_A), draft_created(), draft_created()]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.create_draft("d1", draft_args());
    assert!(ok, "{value}");
    assert!(!value.to_string().contains(ACCESS_A), "{value}");
    let (value, ok) = rig.create_draft("d2", draft_args());
    assert!(ok, "{value}");
    assert!(!value.to_string().contains(ACCESS_A), "{value}");

    let requests = server.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert_eq!(request_path(&requests[0]), "/token");
    assert_eq!(
        form_value(request_body(&requests[0]), "refresh_token").as_deref(),
        Some(REFRESH_A)
    );
    assert_eq!(request_path(&requests[1]), "/users/me/drafts");
    assert_eq!(request_path(&requests[2]), "/users/me/drafts");
    assert_eq!(
        request_header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_A}").as_str())
    );
    assert_eq!(
        request_header(&requests[2], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_A}").as_str())
    );

    // No file under the run dir or the project `.apb` dir holds the minted
    // token or any stored secret, including the unused REFRESH_B sitting
    // in the dotenv; the secrets dotenv itself is the person's own store
    // and stays out of the walk.
    let texts = rig.every_file_text();
    assert!(!texts.is_empty(), "the walk must see the run's files");
    for secret in [ACCESS_A, ACCESS_B, REFRESH_A, REFRESH_B, CLIENT_SECRET] {
        assert!(
            !texts.iter().any(|t| t.contains(secret)),
            "the value of a token or secret appears under the run dir or the project dir: {secret}"
        );
    }
    let journal = std::fs::read_to_string(rig.run.path().join("events.jsonl")).unwrap();
    for secret in [ACCESS_A, ACCESS_B, REFRESH_A, REFRESH_B] {
        assert!(!journal.contains(secret), "the journal carries {secret}");
    }
}

#[test]
fn a_dry_run_shows_the_preview_and_contacts_nothing() {
    let server = spawn_script(vec![]);
    let rig = rig(&server.base_url);
    let (value, ok) = execute(CallRequest {
        run_dir: rig.run.path(),
        root: rig.root.path(),
        node_id: NODE,
        connector: CONNECTOR,
        function: "create_draft",
        account: None,
        args: draft_args(),
        dry_run: true,
        full: false,
    });
    assert!(ok, "{value}");
    assert_eq!(value["dry_run"], true);
    // The readable preview sits at message.raw; no base64 anywhere.
    assert_eq!(value["body"]["message"]["raw"]["subject"], "Re: plan");
    assert_eq!(value["body"]["message"]["threadId"], "t1");
    let printed = value.to_string();
    assert!(!printed.contains("Subject: Re: plan"), "{printed}");
    assert!(server.requests().is_empty());
}
