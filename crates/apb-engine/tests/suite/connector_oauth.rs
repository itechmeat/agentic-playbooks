//! Engine tests for the `oauth2_refresh` auth kind: the token source that
//! mints a bearer access token from a refresh token, the refresh-and-retry
//! after a 401, scope checks before any service request, the redaction of
//! every minted credential, and the one-deadline budget over minting and
//! the service request alike. Every test runs offline against a scripted
//! HTTP server that plays both the token endpoint and the API.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::connector::marker::new_nonce;
use apb_engine::connector::call::{
    AttemptClaim, CallContext, CallRequest, execute, execute_with, execute_within,
};
use apb_engine::connector::intent::Ledger;
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{Reply, spawn_script};

const NODE: &str = "n";
const CONNECTOR: &str = "goauth";
const CLIENT_SECRET: &str = "gocsp-test-0123456789";
const REFRESH_A: &str = "refresh-a-0123456789";
const REFRESH_B: &str = "refresh-b-0123456789";
const ACCESS_A: &str = "ya29-access-a-0123456789";
const ACCESS_B: &str = "ya29-access-b-0123456789";
const SECRET_VAR_CS: &str = "APB_GOAUTH_TEST_CLIENT_SECRET";
const SECRET_VAR_RT_A: &str = "APB_GOAUTH_TEST_REFRESH_A";
const SECRET_VAR_RT_B: &str = "APB_GOAUTH_TEST_REFRESH_B";
const SCOPE_A: &str = "https://www.googleapis.test/auth/scope.a";
const SCOPE_B: &str = "https://www.googleapis.test/auth/scope.b";

/// The inline connector manifest: one `oauth2_refresh` auth block, a read
/// function that needs one of two scopes, a slow read (2 s timeout) for the
/// deadline tests, an imap verify over the same token (8 s timeout), and a
/// `mode: none` send for the ledger tests.
fn connector_yaml() -> String {
    let scope_pair = format!(r#"["{SCOPE_A}", "{SCOPE_B}"]"#);
    let scope_a = format!(r#"["{SCOPE_A}"]"#);
    format!(
        r#"
name: {CONNECTOR}
version: 0.1.0
auth:
  kind: oauth2_refresh
  token_url: "{{{{account.token_url}}}}"
  client_id_template: "{{{{account.client_id}}}}"
  client_secret_template: "{{{{secret.client_secret}}}}"
  refresh_token_template: "{{{{secret.refresh_token}}}}"
  granted_scopes_template: "{{{{account.granted_scopes}}}}"
account_fields:
  - {{ name: api_base, required: true }}
  - {{ name: token_url, required: true }}
  - {{ name: client_id, required: true }}
  - {{ name: granted_scopes }}
  - {{ name: client_secret, required: true, secret: true }}
  - {{ name: refresh_token, required: true, secret: true }}
  - {{ name: imap_host }}
  - {{ name: imap_port }}
functions:
  - name: read_thing
    description: d
    read_only: true
    method: GET
    url: "{{{{account.api_base}}}}/things"
    response_pick: [id, echo]
    scopes: {scope_pair}
  - name: read_slow
    description: d
    read_only: true
    method: GET
    url: "{{{{account.api_base}}}}/slow"
    response_pick: [id]
    scopes: {scope_a}
    timeout_sec: 2
  - name: imap_verify
    description: d
    read_only: true
    imap:
      connection:
        host: "{{{{account.imap_host}}}}"
        port: "{{{{account.imap_port}}}}"
        use_tls: "false"
        auth_method: oauth2
        username: u
        password: ""
      op: verify
    timeout_sec: 8
  - name: write_thing
    description: d
    method: POST
    url: "{{{{account.api_base}}}}/things"
    body: {{ t: "{{{{args.t}}}}" }}
    idempotency: {{ mode: none }}
    timeout_sec: 2
"#
    )
}

/// One manifest account backed by env vars for the client secret and the
/// refresh token.
fn account_named(
    name: &str,
    default: bool,
    base: &str,
    refresh_var: &str,
    granted: Option<&str>,
) -> ManifestAccount {
    let mut fields = BTreeMap::from([
        ("api_base".to_string(), base.to_string()),
        ("token_url".to_string(), format!("{base}/token")),
        ("client_id".to_string(), "client-id-1".to_string()),
        (
            "client_secret".to_string(),
            format!("{{{{env.{SECRET_VAR_CS}}}}}"),
        ),
        (
            "refresh_token".to_string(),
            format!("{{{{env.{refresh_var}}}}}"),
        ),
    ]);
    if let Some(g) = granted {
        fields.insert("granted_scopes".to_string(), g.to_string());
    }
    ManifestAccount {
        name: name.to_string(),
        default,
        fields,
        env: BTreeMap::from([
            ("client_secret".to_string(), SECRET_VAR_CS.to_string()),
            ("refresh_token".to_string(), refresh_var.to_string()),
        ]),
        cmd: BTreeMap::new(),
        digest: format!("sha256:{name}"),
    }
}

/// A seeded run: the inline connector snapshot, one or two granted accounts,
/// the project secrets, and an open attempt 1 for the send tests.
struct Rig {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
    current: AttemptClaim,
}

fn rig_with(
    accounts: &[ManifestAccount],
    functions: &[&str],
    max_calls: Option<u32>,
    attempt: bool,
) -> Rig {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: accounts.to_vec(),
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: accounts.iter().map(|a| a.name.clone()).collect(),
            functions: functions.iter().map(|f| f.to_string()).collect(),
            max_calls,
            on_unknown: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), connector_yaml()).unwrap();
    let mut secrets = format!("{SECRET_VAR_CS}={CLIENT_SECRET}\n{SECRET_VAR_RT_A}={REFRESH_A}\n");
    secrets.push_str(&format!("{SECRET_VAR_RT_B}={REFRESH_B}\n"));
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
    // An open attempt 1 only where the test reads the ledger: a plain call
    // without a claim must not have to name an attempt the journal shows.
    if !attempt {
        return Rig {
            run,
            root,
            current: AttemptClaim::default(),
        };
    }
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

fn rig(base: &str, granted: Option<&str>) -> Rig {
    rig_with(
        &[account_named("acct1", true, base, SECRET_VAR_RT_A, granted)],
        &["read_thing", "read_slow", "write_thing"],
        None,
        false,
    )
}

impl Rig {
    fn run_dir(&self) -> &Path {
        self.run.path()
    }

    /// A plain node call with no label and no attempt claim.
    fn call(&self, function: &str, args: Value) -> (Value, bool) {
        execute(CallRequest {
            run_dir: self.run.path(),
            root: self.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: None,
            args,
            dry_run: false,
            full: false,
        })
    }

    /// A dry-run render.
    fn call_dry(&self, function: &str) -> (Value, bool) {
        execute(CallRequest {
            run_dir: self.run.path(),
            root: self.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: None,
            args: json!({}),
            dry_run: true,
            full: false,
        })
    }

    /// A node call under the caller bound `max_secs`.
    fn call_within(&self, function: &str, max_secs: u64) -> (Value, bool) {
        execute_within(
            CallRequest {
                run_dir: self.run.path(),
                root: self.root.path(),
                node_id: NODE,
                connector: CONNECTOR,
                function,
                account: None,
                args: json!({}),
                dry_run: false,
                full: false,
            },
            max_secs,
        )
    }

    /// A node call as the open attempt, with a `--intent` label.
    fn call_as_attempt(&self, function: &str, args: Value, intent: &str) -> (Value, bool) {
        execute_with(
            CallRequest {
                run_dir: self.run.path(),
                root: self.root.path(),
                node_id: NODE,
                connector: CONNECTOR,
                function,
                account: None,
                args,
                dry_run: false,
                full: false,
            },
            CallContext::node(Some(intent), self.current.clone()),
        )
    }

    fn ledger(&self) -> Ledger {
        Ledger::fold(&read_all(self.run.path()).unwrap())
    }

    fn connector_calls(&self) -> usize {
        read_all(self.run.path())
            .unwrap()
            .into_iter()
            .filter(|e| matches!(e.payload, EventPayload::ConnectorCall { .. }))
            .count()
    }

    /// Every text file under the run dir and the project `.apb` dir, as
    /// lossy strings, for the token-leak greps. The project secrets file is
    /// left out: it is the person's own store, written before the call.
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

/// The captured request line (`GET /things HTTP/1.1`).
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

/// One decoded form value of a captured request body.
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
        json!({ "access_token": access, "expires_in": 3600, "scope": format!("{SCOPE_A} {SCOPE_B}") })
            .to_string(),
    )
}

fn token_ok_with_scope(access: &str, scope: &str) -> Reply {
    Reply::Respond(
        200,
        json!({ "access_token": access, "expires_in": 3600, "scope": scope }).to_string(),
    )
}

fn api_ok() -> Reply {
    Reply::Respond(200, json!({ "id": "x" }).to_string())
}

fn api_status(status: u16) -> Reply {
    Reply::Respond(status, json!({ "message": "nope" }).to_string())
}

/// The settlement outcome of the one intent labelled `label`, if any.
fn settled_outcome(rig: &Rig, label: &str) -> Option<String> {
    use apb_engine::connector::intent::ledger::IntentState;
    rig.ledger()
        .records()
        .iter()
        .find(|r| r.key.label == label)
        .map(|r| {
            match r.state {
                IntentState::Pending { .. } => "pending",
                IntentState::Confirmed { .. } => "confirmed",
                IntentState::NotSent { .. } => "not_sent",
                IntentState::Unknown { .. } => "unknown",
            }
            .to_string()
        })
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

fn message(value: &Value) -> &str {
    value["error"]["message"].as_str().unwrap_or("")
}

#[test]
fn refresh_then_call_sends_the_bearer_token() {
    let server = spawn_script(vec![token_ok(ACCESS_A), api_ok()]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(request_method(&requests[0]), "POST");
    assert_eq!(request_path(&requests[0]), "/token");
    let body = request_body(&requests[0]);
    assert_eq!(
        form_value(body, "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(
        form_value(body, "refresh_token").as_deref(),
        Some(REFRESH_A)
    );
    assert_eq!(
        form_value(body, "client_id").as_deref(),
        Some("client-id-1")
    );
    assert_eq!(
        form_value(body, "client_secret").as_deref(),
        Some(CLIENT_SECRET)
    );
    assert_eq!(request_path(&requests[1]), "/things");
    assert_eq!(
        request_header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_A}").as_str())
    );
    assert_eq!(value["body"]["id"], "x");
}

#[test]
fn a_401_refreshes_once_and_retries_once() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_status(401),
        token_ok(ACCESS_B),
        api_ok(),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert_eq!(request_path(&requests[1]), "/things");
    assert_eq!(request_path(&requests[2]), "/token");
    assert_eq!(request_path(&requests[3]), "/things");
    assert_eq!(
        request_header(&requests[3], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_B}").as_str())
    );
}

#[test]
fn a_second_401_is_final() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_status(401),
        token_ok(ACCESS_B),
        api_status(401),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "auth");
    assert_eq!(value["error"]["http_status"], 401);
    assert_eq!(server.requests().len(), 4, "{:?}", server.requests());
}

#[test]
fn a_403_is_not_retried() {
    let server = spawn_script(vec![token_ok(ACCESS_A), api_status(403)]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "auth");
    assert_eq!(value["error"]["http_status"], 403);
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn invalid_grant_names_the_oauth_command() {
    let server = spawn_script(vec![Reply::Respond(
        400,
        json!({ "error": "invalid_grant" }).to_string(),
    )]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "auth");
    let msg = message(&value);
    assert!(msg.contains("invalid_grant"), "{msg}");
    assert!(msg.contains("apb connector oauth"), "{msg}");
    assert!(msg.contains("goauth"), "{msg}");
    assert!(msg.contains("acct1"), "{msg}");
    // No service request left: the token endpoint was the only request.
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_token_error_description_is_never_echoed() {
    let server = spawn_script(vec![Reply::Respond(
        400,
        json!({
            "error": "invalid_grant",
            "error_description": format!("Malformed auth code header, {{}}: {REFRESH_A}"),
        })
        .to_string(),
    )]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    let msg = message(&value);
    assert!(msg.contains("invalid_grant"), "{msg}");
    assert!(
        !msg.contains(REFRESH_A) && !msg.contains("Malformed"),
        "the error echoed the endpoint's description: {msg}"
    );
}

#[test]
fn account_scopes_missing_fail_before_any_request() {
    let server = spawn_script(vec![]);
    let rig = rig(&server.base_url, Some("https://other.example/scope.x"));
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "auth");
    let msg = message(&value);
    assert!(msg.contains("apb connector oauth"), "{msg}");
    assert!(msg.contains(SCOPE_A), "{msg}");
    assert!(server.requests().is_empty());
}

#[test]
fn token_scope_missing_fails_before_the_service_request() {
    let server = spawn_script(vec![token_ok_with_scope(
        ACCESS_A,
        "https://other.example/x",
    )]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "auth");
    assert!(message(&value).contains("apb connector oauth"));
    assert_eq!(server.requests().len(), 1, "only the token request");
    assert_eq!(request_path(&server.requests()[0]), "/token");
}

#[test]
fn any_one_scope_is_enough() {
    let server = spawn_script(vec![token_ok_with_scope(ACCESS_A, SCOPE_B), api_ok()]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
}

#[test]
fn a_rotated_refresh_token_warns_and_is_redacted() {
    let new_refresh = "brand-new-refresh-987654321";
    let server = spawn_script(vec![
        Reply::Respond(
            200,
            json!({
                "access_token": ACCESS_A,
                "expires_in": 3600,
                "scope": format!("{SCOPE_A} {SCOPE_B}"),
                "refresh_token": new_refresh,
            })
            .to_string(),
        ),
        api_ok(),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    let warnings = value["warnings"].as_array().expect("warnings list");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("refresh token"))),
        "{warnings:?}"
    );
    let printed = value.to_string();
    assert!(
        !printed.contains(new_refresh),
        "the new refresh leaked: {printed}"
    );
}

#[test]
fn an_echoed_access_token_is_redacted() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        Reply::Respond(200, json!({ "id": "x", "echo": ACCESS_A }).to_string()),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    assert_eq!(value["body"]["echo"], "[redacted:oauth:access_token]");
    assert!(!value.to_string().contains(ACCESS_A));
}

#[test]
fn tokens_never_reach_the_journal() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_status(401),
        token_ok(ACCESS_B),
        api_ok(),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    let texts = rig.every_file_text();
    assert!(!texts.is_empty(), "the walk must see the run's files");
    for secret in [ACCESS_A, ACCESS_B, REFRESH_A, REFRESH_B, CLIENT_SECRET] {
        assert!(
            !texts.iter().any(|t| t.contains(secret)),
            "the value of a token or secret appears under the run dir or the project dir: {secret}"
        );
    }
    // The events.jsonl line check, explicit.
    let journal = std::fs::read_to_string(rig.run_dir().join("events.jsonl")).unwrap();
    for secret in [ACCESS_A, ACCESS_B, REFRESH_A, REFRESH_B] {
        assert!(!journal.contains(secret), "the journal carries {secret}");
    }
}

#[test]
fn one_call_one_event_one_budget_unit_after_a_retry() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_status(401),
        token_ok(ACCESS_B),
        api_ok(),
    ]);
    let rig = rig_with(
        &[account_named(
            "acct1",
            true,
            &server.base_url,
            SECRET_VAR_RT_A,
            None,
        )],
        &["read_thing"],
        Some(1),
        false,
    );
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    assert_eq!(rig.connector_calls(), 1, "one call, one event");
    // A second read is refused by the budget: the retry did not cost a unit.
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "permission");
    assert!(message(&value).contains("max_calls"), "{}", message(&value));
}

/// Two accounts over one token (same endpoint, client and refresh token)
/// share the cached token, never each other's declared scope set: the one
/// that declares none reads the endpoint's own answer.
#[test]
fn a_declared_scope_set_does_not_leak_through_the_shared_token_cache() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_ok(),
        Reply::Respond(200, json!({ "id": "slow" }).to_string()),
    ]);
    let base = &server.base_url;
    let rig = rig_with(
        &[
            account_named("narrow", true, base, SECRET_VAR_RT_A, Some(SCOPE_B)),
            account_named("plain", false, base, SECRET_VAR_RT_A, None),
        ],
        &["read_thing", "read_slow"],
        None,
        false,
    );
    let call = |function: &str, account: &str| {
        execute(CallRequest {
            run_dir: rig.run.path(),
            root: rig.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: Some(account),
            args: json!({}),
            dry_run: false,
            full: false,
        })
    };
    // `narrow` declares scope B only and mints the token (scope A and B).
    let (value, ok) = call("read_thing", "narrow");
    assert!(ok, "{value}");
    // `plain` declares nothing: the endpoint granted scope A, so a function
    // that needs A runs on the cached token.
    let (value, ok) = call("read_slow", "plain");
    assert!(ok, "{value}");
    assert_eq!(server.requests().len(), 3, "{:?}", server.requests());
}

#[test]
fn two_accounts_get_two_tokens_in_one_process() {
    let token_reply = |which: &'static str, access: &'static str| {
        Reply::From(Box::new(move |seen: &[String]| {
            let last = seen.last().cloned().unwrap_or_default();
            let refresh = form_value(request_body(&last), "refresh_token").unwrap_or_default();
            let want = if which == "a" { REFRESH_A } else { REFRESH_B };
            let status = if refresh == want { 200 } else { 500 };
            (
                status,
                json!({ "access_token": access, "expires_in": 3600 }).to_string(),
            )
        }))
    };
    let server = spawn_script(vec![
        token_reply("a", ACCESS_A),
        api_ok(),
        token_reply("b", ACCESS_B),
        api_ok(),
    ]);
    let base = &server.base_url;
    let rig = rig_with(
        &[
            account_named("acct1", true, base, SECRET_VAR_RT_A, None),
            account_named("acct2", false, base, SECRET_VAR_RT_B, None),
        ],
        &["read_thing"],
        None,
        false,
    );
    for (name, access) in [("acct1", ACCESS_A), ("acct2", ACCESS_B)] {
        let (value, ok) = execute(CallRequest {
            run_dir: rig.run.path(),
            root: rig.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function: "read_thing",
            account: Some(name),
            args: json!({}),
            dry_run: false,
            full: false,
        });
        assert!(ok, "{name}: {value}");
        assert_eq!(value["body"]["id"], "x");
        let _ = access;
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert_eq!(
        form_value(request_body(&requests[0]), "refresh_token").as_deref(),
        Some(REFRESH_A)
    );
    assert_eq!(
        form_value(request_body(&requests[2]), "refresh_token").as_deref(),
        Some(REFRESH_B)
    );
    assert_eq!(
        request_header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_A}").as_str())
    );
    assert_eq!(
        request_header(&requests[3], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS_B}").as_str())
    );
}

#[test]
fn dry_run_contacts_no_token_endpoint() {
    let server = spawn_script(vec![]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call_dry("read_thing");
    assert!(ok, "{value}");
    assert_eq!(value["dry_run"], true);
    assert!(server.requests().is_empty());
}

#[test]
fn a_token_timeout_settles_a_none_send_not_sent() {
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let server = spawn_script(vec![
        Reply::Held {
            body: json!({ "access_token": ACCESS_A }).to_string(),
            arrived: arrived_tx,
            release: release_rx,
        },
        token_ok(ACCESS_A),
        api_ok(),
    ]);
    let rig = rig(&server.base_url, None);
    let started = Instant::now();
    let _ = &arrived_rx;
    let (value, ok) = rig.call_as_attempt("write_thing", json!({ "t": "x" }), "l1");
    let elapsed = started.elapsed();
    assert!(!ok);
    assert_eq!(code(&value), "timeout", "{value}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the token timeout took {elapsed:?}"
    );
    // No connector_call event: nothing reached the service.
    assert_eq!(rig.connector_calls(), 0);
    assert_eq!(settled_outcome(&rig, "l1").as_deref(), Some("not_sent"));
    // The repeat's token request must be answered, so release the held
    // first one now.
    let _ = release_tx.send(());
    // A repeat with the same intent is sent, not refused unknown.
    let (value, ok) = rig.call_as_attempt("write_thing", json!({ "t": "x" }), "l1");
    assert!(ok, "{value}");
    assert_eq!(server.requests().len(), 3, "{:?}", server.requests());
}

#[test]
fn a_token_status_is_never_the_http_status() {
    let server = spawn_script(vec![Reply::Respond(
        503,
        json!({ "error": "backend", "error_description": "x" }).to_string(),
    )]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(!ok);
    assert_eq!(code(&value), "service");
    assert!(
        value["error"]["http_status"].is_null(),
        "a token status reached the result: {value}"
    );
}

#[test]
fn a_refresh_failure_after_a_401_keeps_the_401() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_status(401),
        Reply::Respond(500, json!({ "error": "boom" }).to_string()),
    ]);
    let rig = rig(&server.base_url, None);
    let started = Instant::now();
    let _ = started;
    let (value, ok) = rig.call_as_attempt("write_thing", json!({ "t": "x" }), "l1");
    assert!(!ok);
    assert_eq!(code(&value), "auth", "{value}");
    assert_eq!(value["error"]["http_status"], 401);
    assert!(
        message(&value).contains("renewing the token failed"),
        "{}",
        message(&value)
    );
    // The refused send settled not_sent.
    assert_eq!(settled_outcome(&rig, "l1").as_deref(), Some("not_sent"));
    assert_eq!(
        rig.connector_calls(),
        1,
        "the 401 reached the service: one event"
    );
}

#[test]
fn a_stalled_token_endpoint_fails_within_the_deadline() {
    let (arrived_tx, _arrived_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = spawn_script(vec![Reply::Held {
        body: json!({ "access_token": ACCESS_A }).to_string(),
        arrived: arrived_tx,
        release: release_rx,
    }]);
    let rig = rig(&server.base_url, None);
    let started = Instant::now();
    let (value, ok) = rig.call("read_slow", json!({}));
    let elapsed = started.elapsed();
    assert!(!ok);
    assert_eq!(code(&value), "timeout", "{value}");
    assert!(
        elapsed >= Duration::from_millis(1500) && elapsed <= Duration::from_millis(3500),
        "the stalled token endpoint failed after {elapsed:?}"
    );
    let _ = release_tx.send(());
}

#[test]
fn execute_within_bounds_token_minting() {
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = spawn_script(vec![Reply::Held {
        body: json!({ "access_token": ACCESS_A }).to_string(),
        arrived: arrived_tx,
        release: release_rx,
    }]);
    let rig = rig(&server.base_url, None);
    // Release the endpoint after 2 s, so a call bounded to 1 s must give up
    // before the answer arrives.
    std::thread::spawn(move || {
        let _ = arrived_rx.recv_timeout(Duration::from_secs(10));
        std::thread::sleep(Duration::from_secs(2));
        let _ = release_tx.send(());
    });
    let started = Instant::now();
    let (value, ok) = rig.call_within("read_slow", 1);
    let elapsed = started.elapsed();
    assert!(!ok);
    assert_eq!(code(&value), "timeout", "{value}");
    assert!(
        elapsed <= Duration::from_millis(1900),
        "execute_within(1) took {elapsed:?}"
    );
}

/// The bound `execute_within` lowers the call's deadline to reaches imap
/// minting too: a token endpoint that never answers fails an imap call
/// bounded to 1 s after about 1 s, not after its 8 s `timeout_sec`.
#[test]
fn execute_within_bounds_imap_token_minting() {
    // A token endpoint that accepts and never answers.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let _hold = std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().take(2).collect();
        std::thread::sleep(Duration::from_secs(20));
        drop(held);
    });
    let mut account = account_named("acct1", true, &base, SECRET_VAR_RT_A, None);
    account
        .fields
        .insert("imap_host".to_string(), "127.0.0.1".to_string());
    account
        .fields
        .insert("imap_port".to_string(), "1".to_string());
    let rig = rig_with(&[account], &["imap_verify"], None, false);
    let started = Instant::now();
    let (value, ok) = rig.call_within("imap_verify", 1);
    let elapsed = started.elapsed();
    assert!(!ok);
    assert_eq!(code(&value), "timeout", "{value}");
    assert!(
        elapsed <= Duration::from_millis(2500),
        "execute_within(1) of an imap call took {elapsed:?}"
    );
}

#[test]
fn a_token_echoed_in_the_link_header_is_redacted() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        Reply::RespondWith(
            200,
            vec![(
                "Link".to_string(),
                format!("<http://127.0.0.1:9/things?page=2>; rel=\"next\", token={ACCESS_A}"),
            )],
            json!({ "id": "x" }).to_string(),
        ),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    let printed = value.to_string();
    assert!(
        !printed.contains(ACCESS_A),
        "the link echoed the token: {printed}"
    );
    assert!(
        printed.contains("[redacted:oauth:access_token]"),
        "{printed}"
    );
}

#[test]
fn a_token_echoed_in_the_retry_response_is_redacted() {
    let server = spawn_script(vec![
        token_ok(ACCESS_A),
        api_status(401),
        token_ok(ACCESS_B),
        Reply::Respond(200, json!({ "id": "x", "echo": ACCESS_B }).to_string()),
    ]);
    let rig = rig(&server.base_url, None);
    let (value, ok) = rig.call("read_thing", json!({}));
    assert!(ok, "{value}");
    assert_eq!(value["body"]["echo"], "[redacted:oauth:access_token]");
    assert!(!value.to_string().contains(ACCESS_B));
}
