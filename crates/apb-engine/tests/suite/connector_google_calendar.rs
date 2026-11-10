//! Engine tests for the official `google-calendar` connector, driving the
//! real, checked-in `connectors/google-calendar/connector.yaml` through
//! `connector::call` against one scripted server that plays both Google's
//! token endpoint (`token_url = {base}/token`) and the Calendar API
//! (`api_base = {base}`):
//!
//! - a `list_events` read mints one access token and sends it as a bearer
//!   token, with the fixed `singleEvents` and `orderBy` pairs;
//! - a `create_event` whose answer was lost is repeated with the same
//!   client-chosen event id, and Google's 409 to that id settles the send
//!   as confirmed (`conflict: confirms`) instead of leaving it unknown;
//! - an `update_event` whose answer was lost is never sent again on its
//!   own: a repeat is refused `unknown_effect` (a repeat could mail every
//!   guest again, so the function is `mode: none`, not `repeatable`);
//! - a dry-run shows the key preview, never an encoded nonce.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::connector::marker::new_nonce;
use apb_engine::connector::call::{AttemptClaim, CallContext, CallRequest, execute, execute_with};
use apb_engine::connector::intent::Ledger;
use apb_engine::connector::intent::ledger::IntentState;
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{self, Reply, request_json, spawn_script};

const NODE: &str = "plan";
const CONNECTOR: &str = "google-calendar";
const CLIENT_ID: &str = "1234-test.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "gcal-client-secret-0123456789";
const VAR_CLIENT_SECRET: &str = "APB_GCAL_TEST_CLIENT_SECRET";
const VAR_REFRESH: &str = "APB_GCAL_TEST_REFRESH_TOKEN";
/// What the default consent grants: `calendar.events` covers every write,
/// `calendar.readonly` every read.
const GRANTED: &str = "https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/calendar.readonly";

/// The real, checked-in connector manifest, so a drift between this test
/// and the shipped `connector.yaml` is impossible.
fn calendar_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/google-calendar/connector.yaml")
        .canonicalize()
        .expect("repository connectors/google-calendar/connector.yaml must exist");
    std::fs::read_to_string(path).unwrap()
}

/// One test's credentials. Access tokens are cached per process by token
/// endpoint, client and refresh token, and a later test may get an earlier
/// server's port, so every test mints from a refresh token of its own.
struct Tokens {
    refresh: String,
    access: String,
}

impl Tokens {
    fn new() -> Self {
        Tokens {
            refresh: format!("gcal-refresh-{}", new_nonce()),
            access: format!("gcal-access-{}", new_nonce()),
        }
    }

    /// The token endpoint's answer: the access token, valid for an hour,
    /// with the scopes of the default consent.
    fn minted(&self) -> Reply {
        Reply::Respond(
            200,
            json!({
                "access_token": self.access,
                "expires_in": 3599,
                "scope": GRANTED,
                "token_type": "Bearer",
            })
            .to_string(),
        )
    }
}

/// A seeded run: the manifest snapshot, one default account on the scripted
/// server, its secrets in the project dotenv, and an open attempt 1 of
/// `NODE` that the calls claim.
struct Rig {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
    claim: AttemptClaim,
}

fn rig(base: &str, tokens: &Tokens) -> Rig {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();

    let account = ManifestAccount {
        name: "me".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("api_base".to_string(), base.to_string()),
            ("token_url".to_string(), format!("{base}/token")),
            ("client_id".to_string(), CLIENT_ID.to_string()),
            (
                "client_secret".to_string(),
                format!("{{{{env.{VAR_CLIENT_SECRET}}}}}"),
            ),
            (
                "refresh_token".to_string(),
                format!("{{{{env.{VAR_REFRESH}}}}}"),
            ),
        ]),
        env: BTreeMap::from([
            ("client_secret".to_string(), VAR_CLIENT_SECRET.to_string()),
            ("refresh_token".to_string(), VAR_REFRESH.to_string()),
        ]),
        cmd: BTreeMap::new(),
        digest: "sha256:me".to_string(),
    };
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["me".to_string()],
            functions: ["list_events", "create_event", "update_event"]
                .iter()
                .map(|f| f.to_string())
                .collect(),
            max_calls: None,
            on_unknown: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), calendar_yaml()).unwrap();

    let secrets = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(secrets.parent().unwrap()).unwrap();
    std::fs::write(
        &secrets,
        format!(
            "{VAR_CLIENT_SECRET}={CLIENT_SECRET}\n{VAR_REFRESH}={}\n",
            tokens.refresh
        ),
    )
    .unwrap();

    let token = new_nonce();
    let mut log = EventLog::create(run.path()).unwrap();
    log.append(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    })
    .unwrap();
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
        claim: AttemptClaim {
            item: None,
            attempt: Some(1),
            token: Some(token),
            host_task: None,
        },
    }
}

impl Rig {
    fn request(&self, function: &'static str, args: Value, dry_run: bool) -> CallRequest<'_> {
        CallRequest {
            run_dir: self.run.path(),
            root: self.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: None,
            args,
            dry_run,
            full: false,
        }
    }

    /// A read as the open attempt.
    fn read(&self, function: &'static str, args: Value) -> (Value, bool) {
        execute_with(
            self.request(function, args, false),
            CallContext::node(None, self.claim.clone()),
        )
    }

    /// A send as the open attempt, under the `--intent` label `intent`.
    fn send(&self, function: &'static str, args: Value, intent: &str) -> (Value, bool) {
        execute_with(
            self.request(function, args, false),
            CallContext::node(Some(intent), self.claim.clone()),
        )
    }

    fn dry_run(&self, function: &'static str, args: Value) -> (Value, bool) {
        execute(self.request(function, args, true))
    }

    fn ledger(&self) -> Ledger {
        Ledger::fold(&read_all(self.run.path()).unwrap())
    }
}

/// The captured request line's method and target.
fn method_and_target(raw: &str) -> (String, String) {
    let line = raw.lines().next().unwrap_or("");
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    (method, target)
}

/// A request header value, case-insensitive.
fn header(raw: &str, name: &str) -> Option<String> {
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

/// One form value of a captured token request. The values these tests send
/// hold only unreserved characters, so they arrive unencoded.
fn form_value(raw: &str, key: &str) -> Option<String> {
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b)?;
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

/// The nonce written in base32hex, the alphabet Google allows in an event
/// id: each character of `a-z2-7` becomes the one at the same position of
/// `0-9a-v`.
fn base32hex(nonce: &str) -> String {
    const FROM: &str = "abcdefghijklmnopqrstuvwxyz234567";
    const TO: &[u8] = b"0123456789abcdefghijklmnopqrstuv";
    nonce
        .chars()
        .map(|c| TO[FROM.find(c).expect("a nonce character")] as char)
        .collect()
}

fn release_window() -> Value {
    json!({
        "calendar_id": "primary",
        "summary": "Release window 1.4.0",
        "description": "Deploy and smoke tests.",
        "start": { "dateTime": "2026-11-12T10:00:00+01:00", "timeZone": "Europe/Berlin" },
        "end": { "dateTime": "2026-11-12T11:00:00+01:00", "timeZone": "Europe/Berlin" },
        "attendees": [ { "email": "release-manager@example.com" } ],
        "send_updates": "all",
    })
}

#[test]
fn list_events_mints_one_token_and_sends_it_as_bearer() {
    let _lock = common::env_lock();
    let tokens = Tokens::new();
    let page = json!({
        "kind": "calendar#events",
        "etag": "\"p1\"",
        "summary": "me@example.com",
        "items": [
            {
                "kind": "calendar#event",
                "etag": "\"e1\"",
                "id": "e1",
                "status": "confirmed",
                "summary": "Standup",
                "start": { "dateTime": "2026-11-10T09:00:00+01:00" },
                "end": { "dateTime": "2026-11-10T09:15:00+01:00" },
                "organizer": { "email": "me@example.com", "self": true },
            }
        ],
    });
    let server = spawn_script(vec![tokens.minted(), Reply::Respond(200, page.to_string())]);
    let rig = rig(&server.base_url, &tokens);

    let (value, ok) = rig.read(
        "list_events",
        json!({
            "calendar_id": "primary",
            "time_min": "2026-11-10T00:00:00Z",
            "time_max": "2026-11-11T00:00:00Z",
        }),
    );
    assert!(ok, "{value}");

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "one token request, one API request: {requests:?}"
    );
    assert_eq!(
        method_and_target(&requests[0]),
        ("POST".to_string(), "/token".to_string())
    );
    assert_eq!(
        form_value(&requests[0], "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(
        form_value(&requests[0], "refresh_token").as_deref(),
        Some(tokens.refresh.as_str())
    );
    assert_eq!(
        form_value(&requests[0], "client_secret").as_deref(),
        Some(CLIENT_SECRET)
    );
    assert_eq!(
        method_and_target(&requests[1]),
        (
            "GET".to_string(),
            "/calendars/primary/events?orderBy=startTime&singleEvents=true&timeMax=2026-11-11T00%3A00%3A00Z&timeMin=2026-11-10T00%3A00%3A00Z"
                .to_string()
        )
    );
    assert_eq!(
        header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {}", tokens.access).as_str())
    );

    // One page, projected: the pick keeps what an agent reads.
    assert_eq!(value["pages"], 1, "{value}");
    assert_eq!(value["more"], false, "{value}");
    assert_eq!(
        value["body"],
        json!({
            "items": [
                {
                    "id": "e1",
                    "status": "confirmed",
                    "summary": "Standup",
                    "start": { "dateTime": "2026-11-10T09:00:00+01:00" },
                    "end": { "dateTime": "2026-11-10T09:15:00+01:00" },
                    "organizer": { "email": "me@example.com" },
                }
            ]
        })
    );

    // Neither token reaches the journal.
    let journal = std::fs::read_to_string(rig.run.path().join("events.jsonl")).unwrap();
    for secret in [&tokens.access, &tokens.refresh, CLIENT_SECRET] {
        assert!(
            !journal.contains(secret),
            "the journal carries a credential"
        );
    }
}

/// Design D3: the first `create_event` request leaves and its answer is
/// lost, so the send is unknown. The repeat of the same intent carries the
/// same client-chosen event id, Google answers 409 because that id exists,
/// and the 409 proves the first request created the event: the send
/// settles `confirmed` by `key_conflict` and the repeat answers
/// `already_sent` with the event id.
#[test]
fn a_lost_create_answer_is_confirmed_by_the_409_of_its_repeat() {
    let _lock = common::env_lock();
    let tokens = Tokens::new();
    let conflict = json!({
        "error": {
            "code": 409,
            "message": "The requested identifier already exists.",
            "errors": [ { "domain": "global", "reason": "duplicate", "message": "The requested identifier already exists." } ],
        }
    });
    let server = spawn_script(vec![
        tokens.minted(),
        Reply::Cut,
        Reply::Respond(409, conflict.to_string()),
    ]);
    let rig = rig(&server.base_url, &tokens);

    let (first, ok) = rig.send("create_event", release_window(), "release-window");
    assert!(!ok, "{first}");
    assert_eq!(first["effect"], "unknown", "{first}");

    let (repeat, ok) = rig.send("create_event", release_window(), "release-window");
    assert!(
        ok,
        "the 409 of the repeat confirms the first create: {repeat}"
    );
    assert_eq!(repeat["already_sent"], true, "{repeat}");
    assert_eq!(repeat["sent_in_attempt"], 1, "{repeat}");
    assert_eq!(repeat["intent"]["label"], "release-window", "{repeat}");

    // The access token was minted once and reused; both creates went to
    // the events of the primary calendar with the same event id.
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert_eq!(method_and_target(&requests[0]).1, "/token");
    let creates: Vec<&String> = requests[1..].iter().collect();
    for raw in &creates {
        assert_eq!(
            method_and_target(raw),
            (
                "POST".to_string(),
                "/calendars/primary/events?sendUpdates=all".to_string()
            )
        );
        assert_eq!(
            header(raw, "Authorization").as_deref(),
            Some(format!("Bearer {}", tokens.access).as_str())
        );
    }
    let first_id = request_json(creates[0])["id"].clone();
    let repeat_id = request_json(creates[1])["id"].clone();
    assert_eq!(first_id, repeat_id, "a repeat must ask for the same event");
    let id = first_id.as_str().expect("the create carries an event id");
    assert!(
        id.len() == 24 && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='v')),
        "Google takes base32hex event ids only: {id}"
    );
    assert_eq!(request_json(creates[0])["summary"], "Release window 1.4.0");

    // The ledger: the send, unknown, and its replay with the same nonce,
    // confirmed by the conflict, its reference the encoded key.
    let ledger = rig.ledger();
    let records = ledger.records();
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(
        records[1].replay_of.as_deref(),
        Some(records[0].id.as_str())
    );
    assert_eq!(records[0].nonce, records[1].nonce);
    assert_eq!(id, base32hex(&records[0].nonce));
    match &records[1].state {
        IntentState::Confirmed { source, effect_ref } => {
            assert_eq!(source, "key_conflict");
            assert_eq!(effect_ref.as_deref(), Some(id));
        }
        other => panic!("the 409 of the repeat must confirm the send: {other:?}"),
    }
    assert_eq!(repeat["ref"], id, "{repeat}");
}

/// Design D3 "Wrong implementations" 4: `update_event` is `mode: none`, not
/// `repeatable`. An update whose answer was lost may have reached the
/// service and mailed every guest; repeating it under the same intent is
/// refused until a person decides, and no second request leaves.
#[test]
fn a_lost_update_answer_is_refused_on_repeat() {
    let _lock = common::env_lock();
    let tokens = Tokens::new();
    let server = spawn_script(vec![tokens.minted(), Reply::Cut]);
    let rig = rig(&server.base_url, &tokens);
    let moved = json!({
        "calendar_id": "primary",
        "event_id": "0123456789abcdefghijklmn",
        "start": { "dateTime": "2026-11-12T14:00:00+01:00", "timeZone": "Europe/Berlin" },
        "end": { "dateTime": "2026-11-12T15:00:00+01:00", "timeZone": "Europe/Berlin" },
        "send_updates": "all",
    });

    let (first, ok) = rig.send("update_event", moved.clone(), "move-review");
    assert!(!ok, "{first}");
    assert_eq!(first["effect"], "unknown", "{first}");

    let (repeat, ok) = rig.send("update_event", moved, "move-review");
    assert!(!ok, "{repeat}");
    assert_eq!(code(&repeat), "unknown_effect", "{repeat}");

    let requests = server.requests();
    let patches = requests
        .iter()
        .filter(|r| method_and_target(r).0 == "PATCH")
        .count();
    assert_eq!(patches, 1, "the repeat sent nothing: {requests:?}");
    assert_eq!(
        requests.len(),
        2,
        "one token request, one update: {requests:?}"
    );
    assert_eq!(
        method_and_target(&requests[1]).1,
        "/calendars/primary/events/0123456789abcdefghijklmn?sendUpdates=all"
    );
    let records = rig.ledger().records().to_vec();
    assert_eq!(
        records.len(),
        1,
        "a refused repeat records nothing: {records:?}"
    );
    assert!(
        matches!(records[0].state, IntentState::Unknown { .. }),
        "{:?}",
        records[0].state
    );
}

/// Design D3 "Wrong implementations" 3: a dry-run shows where the key goes
/// with the plain preview, never a nonce, encoded or not, and mints no
/// token.
#[test]
fn a_create_dry_run_shows_the_key_preview() {
    let _lock = common::env_lock();
    let tokens = Tokens::new();
    let server = spawn_script(vec![]);
    let rig = rig(&server.base_url, &tokens);
    let (value, ok) = rig.dry_run("create_event", release_window());
    assert!(ok, "{value}");
    assert_eq!(value["dry_run"], true, "{value}");
    assert_eq!(value["body"]["id"], "(set at send)", "{value}");
    assert_eq!(value["body"]["summary"], "Release window 1.4.0", "{value}");
    assert!(server.requests().is_empty(), "a dry-run contacts nothing");
}
