//! Send-once protection end to end: the real, checked-in connector manifests
//! (`connectors/<name>/connector.yaml`) driven through
//! `connector::call::execute_with` against a scripted stub server, with the
//! run journal as the only memory between calls, the way a retried or
//! resumed attempt meets an earlier one.
//!
//! Every assertion reads the journal or what the stub received; result
//! values are checked by field and never printed.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::schema::OnUnknown;
use apb_engine::connector::call::{AttemptClaim, CallContext, CallRequest, execute_with};
use apb_engine::connector::intent::{IntentState, Ledger};
use apb_engine::event::{EventLog, EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{self, Reply, request_json, spawn_script};

const NODE: &str = "announce";
const SECRET_VAR: &str = "APB_SEND_ONCE_TEST_TOKEN";

/// A run with one node granted `functions` of `connector`, its snapshot
/// copied from the repository, a project secret, and an open attempt 1.
struct Run {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
    connector: &'static str,
    /// The claim of the node's current attempt, as its environment names it.
    current: std::sync::Mutex<AttemptClaim>,
}

struct Grant<'a> {
    functions: &'a [&'a str],
    on_unknown: Option<OnUnknown>,
    max_calls: Option<u32>,
}

impl<'a> Grant<'a> {
    fn of(functions: &'a [&'a str]) -> Self {
        Grant {
            functions,
            on_unknown: None,
            max_calls: None,
        }
    }
}

fn manifest_yaml(connector: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../connectors/{connector}/connector.yaml"));
    std::fs::read_to_string(path).expect("repository connector manifest")
}

fn run_with(connector: &'static str, base_url: &str, grant: Grant<'_>) -> Run {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: connector.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![ManifestAccount {
            name: "main".to_string(),
            default: true,
            fields: BTreeMap::from([
                ("api_base".to_string(), base_url.to_string()),
                ("token".to_string(), format!("{{{{env.{SECRET_VAR}}}}}")),
            ]),
            env: BTreeMap::from([("token".to_string(), SECRET_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: "sha256:acct".to_string(),
        }],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: connector.to_string(),
            accounts: vec!["main".to_string()],
            functions: grant.functions.iter().map(|f| f.to_string()).collect(),
            max_calls: grant.max_calls,
            on_unknown: grant.on_unknown,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(
        cdir.join(format!("{connector}.yaml")),
        manifest_yaml(connector),
    )
    .unwrap();
    let secrets = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(secrets.parent().unwrap()).unwrap();
    std::fs::write(secrets, format!("{SECRET_VAR}=t0ken\n")).unwrap();
    let r = Run {
        run,
        root,
        connector,
        current: std::sync::Mutex::new(AttemptClaim::default()),
    };
    r.journal(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    });
    r.start_attempt(1);
    r
}

impl Run {
    fn journal(&self, payload: EventPayload) -> u64 {
        EventLog::open(self.run.path())
            .or_else(|_| EventLog::create(self.run.path()))
            .unwrap()
            .append(payload)
            .unwrap()
            .seq
    }

    /// Journals attempt `attempt` with a fresh token and makes it the one
    /// [`Run::call`] claims; returns its seq.
    fn start_attempt(&self, attempt: u32) -> u64 {
        let token = apb_core::connector::marker::new_nonce();
        let seq = self.journal(EventPayload::AttemptStarted {
            item: None,
            node: NODE.into(),
            attempt,
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
        });
        *self.current.lock().unwrap() = AttemptClaim {
            item: None,
            attempt: Some(attempt),
            token: Some(token),
            host_task: None,
        };
        seq
    }

    /// Journals the end of the attempt made current (a node's or an
    /// item's).
    fn finish_attempt(&self, status: &str) {
        let (attempt, item) = {
            let current = self.current.lock().unwrap();
            (current.attempt.unwrap_or(1), current.item.clone())
        };
        self.journal(EventPayload::AttemptFinished {
            item,
            node: NODE.into(),
            attempt,
            status: status.into(),
            duration_ms: None,
            session: None,
            summary: None,
            rejected_output: None,
            partial_output: None,
            failure_kind: None,
            usage: None,
            output_truncated: Vec::new(),
            transcript_incomplete: false,
        });
    }

    fn finish_node(&self, status: &str) {
        self.finish_attempt(status);
        self.journal(EventPayload::NodeFinished {
            node: NODE.into(),
            status: status.into(),
            attempt: 1,
            output: String::new(),
            artifacts: Vec::new(),
        });
    }

    fn call_as(
        &self,
        function: &str,
        args: Value,
        intent: Option<&str>,
        claim: AttemptClaim,
    ) -> (Value, bool) {
        execute_with(
            CallRequest {
                run_dir: self.run.path(),
                root: self.root.path(),
                node_id: NODE,
                connector: self.connector,
                function,
                account: None,
                args,
                dry_run: false,
                full: false,
            },
            CallContext::node(intent, claim),
        )
    }

    /// A call from the node's current attempt.
    fn call(&self, function: &str, args: Value, intent: Option<&str>) -> (Value, bool) {
        let claim = self.current.lock().unwrap().clone();
        self.call_as(function, args, intent, claim)
    }

    fn ledger(&self) -> Ledger {
        Ledger::fold(&read_all(self.run.path()).unwrap())
    }

    fn calls(&self) -> Vec<EventPayload> {
        read_all(self.run.path())
            .unwrap()
            .into_iter()
            .map(|e| e.payload)
            .filter(|p| matches!(p, EventPayload::ConnectorCall { .. }))
            .collect()
    }
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

fn discord_args() -> Value {
    json!({ "channel_id": "111", "content": "Release 1.4.0 is out." })
}

// -- Discord: a key the service deduplicates by --------------------------

/// A lost answer leaves the send unknown; the repeat carries the same nonce,
/// so Discord answers with the one message and the intent is confirmed by
/// the replay; a third call answers from the ledger without a request.
#[test]
fn a_discord_send_with_a_lost_answer_is_replayed_with_its_nonce_and_then_answered() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Cut,
        Reply::Respond(200, r#"{"id":"m-1","content":"x"}"#.into()),
    ]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));

    let (first, ok) = run.call("send_message", discord_args(), Some("announce-1.4.0"));
    assert!(!ok);
    assert_eq!(first["effect"], "unknown");
    assert_eq!(first["intent"]["label"], "announce-1.4.0");

    let (second, ok) = run.call("send_message", discord_args(), Some("announce-1.4.0"));
    assert!(ok, "the replay must succeed");
    assert!(second.get("effect").is_none());

    let (third, ok) = run.call("send_message", discord_args(), Some("announce-1.4.0"));
    assert!(ok);
    assert_eq!(third["already_sent"], true);
    assert_eq!(third["ref"], "m-1");

    let requests = stub.requests();
    assert_eq!(requests.len(), 2, "the third call must not reach Discord");
    let (a, b) = (request_json(&requests[0]), request_json(&requests[1]));
    assert_eq!(a["nonce"], b["nonce"], "a replay carries the same nonce");
    assert_eq!(a["enforce_nonce"], true);

    let ledger = run.ledger();
    let records = ledger.records();
    assert_eq!(records.len(), 2);
    assert!(matches!(records[0].state, IntentState::Unknown { .. }));
    assert_eq!(
        records[1].replay_of.as_deref(),
        Some(records[0].id.as_str())
    );
    assert_eq!(
        records[1].state,
        IntentState::Confirmed {
            source: "key_replay".into(),
            effect_ref: Some("m-1".into())
        }
    );
    assert_eq!(
        records[0].superseded_by.as_deref(),
        Some(records[1].id.as_str())
    );
}

/// Past the key window the service would no longer deduplicate, so an
/// unknown send is refused rather than sent blind.
#[test]
fn an_unknown_discord_send_past_its_key_window_is_refused() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, "{}".into())]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    seed_old_unknown_intent(&run, "send_message", "announce", "key");

    let (refused, ok) = run.call("send_message", discord_args(), Some("announce"));
    assert!(!ok);
    assert_eq!(code(&refused), "unknown_effect");
    assert!(stub.requests().is_empty());
}

/// Writes an intent of `label` recorded and settled unknown ten minutes ago.
fn seed_old_unknown_intent(run: &Run, function: &str, label: &str, mode: &str) {
    let old = apb_core::clock::now_ms() - 600_000;
    let events = read_all(run.run.path()).unwrap();
    let next = events.last().map_or(0, |e| e.seq + 1);
    let lines = [
        apb_engine::event::Event {
            seq: next,
            ts: old,
            payload: EventPayload::SendIntentRecorded {
                item: None,
                intent: "si-old".into(),
                nonce: "oldnonce".into(),
                node: NODE.into(),
                attempt: Some(1),
                attempt_seq: None,
                connector: run.connector.into(),
                function: function.into(),
                account: "main".into(),
                mode: mode.into(),
                label: label.into(),
                pid: 1,
                deadline_ms: 0,
                replay_of: None,
                resend_of: None,
            },
        },
        apb_engine::event::Event {
            seq: next + 1,
            ts: old,
            payload: EventPayload::SendIntentSettled {
                intent: "si-old".into(),
                outcome: "unknown".into(),
                source: "response".into(),
                detail: None,
                effect_ref: None,
                by: None,
            },
        },
    ];
    let mut text = std::fs::read_to_string(run.run.path().join("events.jsonl")).unwrap();
    for line in lines {
        text.push_str(&serde_json::to_string(&line).unwrap());
        text.push('\n');
    }
    std::fs::write(run.run.path().join("events.jsonl"), text).unwrap();
}

/// A dry-run shows where the stamp goes and never hands out a nonce.
#[test]
fn a_dry_run_of_a_send_shows_the_stamp_placeholder_and_records_nothing() {
    let _lock = common::env_lock();
    let run = run_with(
        "discord",
        "https://discord.example",
        Grant::of(&["send_message"]),
    );
    let (value, ok) = execute_with(
        CallRequest {
            run_dir: run.run.path(),
            root: run.root.path(),
            node_id: NODE,
            connector: "discord",
            function: "send_message",
            account: None,
            args: discord_args(),
            dry_run: true,
            full: false,
        },
        CallContext::default(),
    );
    assert!(ok);
    assert_eq!(value["body"]["nonce"], "(set at send)");
    assert!(run.ledger().records().is_empty());
}

// -- GitHub: a hidden marker read back from the listing -------------------

fn comment_args() -> Value {
    json!({ "owner": "acme", "repo": "site", "number": 42, "body": "Triage summary." })
}

/// The listing a reconciliation reads: one comment by `author` whose body is
/// the body the first request sent (marker included).
fn listing_echoing_first_send(author: &'static str) -> Reply {
    listing_echoing(
        author,
        "https://github.com/acme/site/issues/42#issuecomment-9",
    )
}

/// A listing of one item at `url` by `author` whose body is the body the
/// first request sent.
fn listing_echoing(author: &'static str, url: &'static str) -> Reply {
    Reply::From(Box::new(move |seen: &[String]| {
        let sent = request_json(&seen[0]);
        let item = json!([{
            "html_url": url,
            "user": { "login": author },
            "body": sent["body"],
        }]);
        (200, item.to_string())
    }))
}

/// A cut connection leaves the comment unknown; the repeat reads the own
/// account and the recent comments, finds its own marker, and answers with
/// the comment's link instead of posting again.
#[test]
fn a_github_comment_with_a_lost_answer_is_found_by_its_marker_and_not_posted_again() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Cut,
        Reply::Respond(200, r#"{"login":"triage-bot","id":7}"#.into()),
        listing_echoing_first_send("triage-bot"),
    ]);
    let run = run_with(
        "github",
        &stub.base_url,
        Grant {
            functions: &["comment_issue"],
            on_unknown: None,
            max_calls: Some(2),
        },
    );

    let (first, ok) = run.call("comment_issue", comment_args(), Some("triage-summary"));
    assert!(!ok);
    assert_eq!(first["effect"], "unknown");

    let (second, ok) = run.call("comment_issue", comment_args(), Some("triage-summary"));
    assert!(ok, "the earlier comment must be found");
    assert_eq!(second["already_sent"], true);
    assert_eq!(
        second["ref"],
        "https://github.com/acme/site/issues/42#issuecomment-9"
    );

    let requests = stub.requests();
    assert_eq!(requests.len(), 3, "no second POST");
    assert!(requests[0].starts_with("POST /repos/acme/site/issues/42/comments"));
    assert!(requests[1].starts_with("GET /user"));
    assert!(requests[2].starts_with("GET /repos/acme/site/issues/42/comments?"));
    assert!(
        requests[2].contains("since="),
        "the listing starts near the intent"
    );
    let sent = request_json(&requests[0]);
    let body = sent["body"].as_str().unwrap_or_default();
    assert!(body.starts_with("Triage summary.\n\n<!-- apb-intent v1 run="));

    let ledger = run.ledger();
    assert!(matches!(
        &ledger.records()[0].state,
        IntentState::Confirmed { source, .. } if source == "reconcile"
    ));
    // The two reads are apb's own and are not charged to max_calls.
    let origins: Vec<Option<String>> = run
        .calls()
        .into_iter()
        .map(|p| match p {
            EventPayload::ConnectorCall { origin, .. } => origin,
            _ => None,
        })
        .collect();
    assert_eq!(
        origins,
        vec![None, Some("reconcile".into()), Some("reconcile".into())]
    );
}

/// No match, or the right marker under another account, keeps the comment
/// unknown: the repeat is refused and nothing is posted.
#[test]
fn a_github_comment_not_found_or_found_under_another_account_is_refused() {
    for listing in [
        Reply::Respond(200, "[]".into()),
        listing_echoing_first_send("someone-else"),
    ] {
        let _lock = common::env_lock();
        let stub = spawn_script(vec![
            Reply::Cut,
            Reply::Respond(200, r#"{"login":"triage-bot"}"#.into()),
            listing,
        ]);
        let run = run_with("github", &stub.base_url, Grant::of(&["comment_issue"]));
        run.call("comment_issue", comment_args(), Some("triage-summary"));
        let (refused, ok) = run.call("comment_issue", comment_args(), Some("triage-summary"));
        assert!(!ok);
        assert_eq!(code(&refused), "unknown_effect");
        let requests = stub.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[1..].iter().all(|r| r.starts_with("GET ")));
    }
}

// -- Telegram: no key, no readback ----------------------------------------

fn telegram_args() -> Value {
    json!({ "chat_id": "-100", "text": "Release 1.4.0 is out." })
}

#[test]
fn an_unknown_telegram_send_is_refused_unless_the_binding_allows_a_resend() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Cut]);
    let run = run_with("telegram", &stub.base_url, Grant::of(&["send_message"]));
    run.call("send_message", telegram_args(), None);
    let (refused, ok) = run.call("send_message", telegram_args(), None);
    assert!(!ok);
    assert_eq!(code(&refused), "unknown_effect");
    assert_eq!(stub.requests().len(), 1);

    let stub = spawn_script(vec![
        Reply::Cut,
        Reply::Respond(200, r#"{"ok":true,"result":{"message_id":5}}"#.into()),
    ]);
    let run = run_with(
        "telegram",
        &stub.base_url,
        Grant {
            functions: &["send_message"],
            on_unknown: Some(OnUnknown::Resend),
            max_calls: None,
        },
    );
    run.call("send_message", telegram_args(), None);
    let (resent, ok) = run.call("send_message", telegram_args(), None);
    assert!(ok, "the binding allows a resend");
    assert_eq!(stub.requests().len(), 2, "the resend reaches Telegram");
    assert_eq!(
        resent["intent"]["label"]
            .as_str()
            .map(|l| l.starts_with("args:")),
        Some(true)
    );
    let ledger = run.ledger();
    let records = ledger.records();
    assert_eq!(
        records[1].resend_of.as_deref(),
        Some(records[0].id.as_str())
    );
    assert_eq!(
        records[1].state,
        IntentState::Confirmed {
            source: "response".into(),
            effect_ref: Some("5".into())
        }
    );
}

#[test]
fn a_refused_telegram_send_did_not_go_out_and_may_be_sent_again() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Respond(400, r#"{"ok":false,"description":"Bad Request"}"#.into()),
        Reply::Respond(200, r#"{"ok":true,"result":{"message_id":6}}"#.into()),
    ]);
    let run = run_with("telegram", &stub.base_url, Grant::of(&["send_message"]));
    let (_, ok) = run.call("send_message", telegram_args(), Some("note"));
    assert!(!ok);
    let (_, ok) = run.call("send_message", telegram_args(), Some("note"));
    assert!(ok);
    let ledger = run.ledger();
    let records = ledger.records();
    assert!(matches!(records[0].state, IntentState::NotSent { .. }));
    assert_eq!(
        records[1].resend_of.as_deref(),
        Some(records[0].id.as_str())
    );
}

/// A person's decision settles an unknown send: "sent" makes a repeat
/// answer `already_sent` without a request, "not sent" lets it go out
/// again; a decided send is not decided twice.
#[test]
fn a_persons_decision_answers_or_releases_the_repeat_of_an_unknown_send() {
    use apb_engine::connector::intent::resolve_by_person;
    let _lock = common::env_lock();
    for sent in [true, false] {
        let mut script = vec![Reply::Cut];
        if !sent {
            script.push(Reply::Respond(
                200,
                r#"{"ok":true,"result":{"message_id":7}}"#.into(),
            ));
        }
        let stub = spawn_script(script);
        let run = run_with("telegram", &stub.base_url, Grant::of(&["send_message"]));
        run.call("send_message", telegram_args(), Some("hello"));
        let id = run.ledger().records()[0].id.clone();
        resolve_by_person(run.run.path(), &id, sent, Some("checked".into()), "ana").unwrap();
        assert!(
            resolve_by_person(run.run.path(), &id, !sent, None, "ana").is_err(),
            "a decided send is not decided twice"
        );
        let (answer, ok) = run.call("send_message", telegram_args(), Some("hello"));
        assert!(ok);
        if sent {
            assert_eq!(answer["already_sent"], json!(true));
            assert_eq!(stub.requests().len(), 1, "no second request");
        } else {
            assert_eq!(stub.requests().len(), 2, "sent again");
            assert_eq!(
                run.ledger().records()[1].resend_of.as_deref(),
                Some(id.as_str())
            );
        }
    }
}

/// A resend is a separate send: when a person confirms the resend, the
/// earlier unknown send stays open and a person may still decide it, since
/// it may have gone out too.
#[test]
fn a_person_may_decide_an_earlier_unknown_send_after_its_resend_is_confirmed() {
    use apb_engine::connector::intent::resolve_by_person;
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Cut, Reply::Cut]);
    let run = run_with(
        "telegram",
        &stub.base_url,
        Grant {
            functions: &["send_message"],
            on_unknown: Some(OnUnknown::Resend),
            max_calls: None,
        },
    );
    run.call("send_message", telegram_args(), Some("hello"));
    run.call("send_message", telegram_args(), Some("hello"));
    let ids: Vec<String> = run
        .ledger()
        .records()
        .iter()
        .map(|r| r.id.clone())
        .collect();
    assert_eq!(ids.len(), 2, "the second call is a resend");
    let dir = run.run.path();
    resolve_by_person(dir, &ids[1], true, None, "ana").unwrap();
    resolve_by_person(dir, &ids[0], true, None, "ana").unwrap();
    let ledger = run.ledger();
    for id in &ids {
        assert!(matches!(
            &ledger.get(id).unwrap().state,
            IntentState::Confirmed { source, .. } if source == "person"
        ));
    }
}

// -- Durability ----------------------------------------------------------

/// The intent is on disk when the request reaches the service (the stub
/// checks the journal as the request arrives), before the call's own
/// record, and both carry the attempt the call belongs to.
#[test]
fn the_intent_is_on_disk_before_the_request_and_both_records_carry_the_attempt() {
    let _lock = common::env_lock();
    let journal: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>> =
        std::sync::Arc::default();
    let seen = journal.clone();
    let stub = spawn_script(vec![Reply::From(Box::new(move |_| {
        let path = seen.lock().unwrap().clone().unwrap_or_default();
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains("\"type\":\"send_intent_recorded\"") {
            (200, r#"{"id":"m-2"}"#.into())
        } else {
            (500, "{}".into())
        }
    }))]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    *journal.lock().unwrap() = Some(run.run.path().join("events.jsonl"));
    let attempt_seq = run.start_attempt(2);
    let (_, ok) = run.call("send_message", discord_args(), None);
    assert!(ok, "the stub answers 200 only when the intent is on disk");
    let kinds: Vec<(&str, Option<u64>)> = read_all(run.run.path())
        .unwrap()
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::SendIntentRecorded { attempt_seq, .. } => {
                Some(("recorded", *attempt_seq))
            }
            EventPayload::ConnectorCall { attempt_seq, .. } => Some(("call", *attempt_seq)),
            EventPayload::SendIntentSettled { .. } => Some(("settled", None)),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("recorded", Some(attempt_seq)),
            ("call", Some(attempt_seq)),
            ("settled", None)
        ]
    );
}

/// A journal that cannot take the intent refuses the call: nothing is sent.
/// Root ignores file modes, so the test does not apply there.
#[cfg(unix)]
#[test]
fn a_journal_that_cannot_record_the_intent_refuses_the_send() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, "{}".into())]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let journal = run.run.path().join("events.jsonl");
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .is_ok()
    {
        eprintln!("skipped: this user writes read-only files (root)");
        return;
    }
    let (refused, ok) = run.call("send_message", discord_args(), None);
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!ok);
    assert_eq!(code(&refused), "config");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("could not record the send intent; nothing was sent"))
    );
    assert!(stub.requests().is_empty());
}

/// A send whose secret is missing fails after its intent is recorded and
/// before anything leaves: the intent is settled as not sent (`unsent`, not
/// a trust refusal), so a repeat is not blocked as unknown.
#[test]
fn a_send_that_fails_before_it_leaves_is_settled_as_not_sent() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, "{}".into())]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    std::fs::remove_file(run.root.path().join(".apb/secrets.env")).unwrap();
    let (_, ok) = run.call("send_message", discord_args(), Some("once"));
    assert!(!ok);
    assert!(stub.requests().is_empty());
    assert!(matches!(
        &run.ledger().records()[0].state,
        IntentState::NotSent { source } if source == "unsent"
    ));
}

/// A second call of an intent still in flight is told so, not sent.
#[test]
fn a_concurrent_repeat_of_an_intent_in_flight_is_refused() {
    let _lock = common::env_lock();
    let (arrived_tx, arrived) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let stub = spawn_script(vec![Reply::Held {
        body: r#"{"id":"m-3"}"#.into(),
        arrived: arrived_tx,
        release: release_rx,
    }]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    std::thread::scope(|s| {
        let first = s.spawn(|| run.call("send_message", discord_args(), Some("once")));
        arrived.recv().unwrap();
        let (refused, ok) = run.call("send_message", discord_args(), Some("once"));
        // Release before asserting, so a failure cannot leave `first` held.
        release.send(()).unwrap();
        assert!(!ok);
        assert_eq!(code(&refused), "in_flight");
        assert!(first.join().unwrap().1);
    });
    assert_eq!(stub.requests().len(), 1);
}

/// Many calls of one intent at the same moment: the journal lock admits
/// exactly one, and the service sees exactly one request.
#[test]
fn simultaneous_calls_of_one_intent_send_exactly_once() {
    const CALLERS: usize = 6;
    let _lock = common::env_lock();
    let (arrived_tx, _arrived) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let stub = spawn_script(vec![Reply::Held {
        body: r#"{"id":"m-7"}"#.into(),
        arrived: arrived_tx,
        release: release_rx,
    }]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let barrier = std::sync::Barrier::new(CALLERS);
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        for _ in 0..CALLERS {
            let done_tx = done_tx.clone();
            let (run, barrier) = (&run, &barrier);
            s.spawn(move || {
                barrier.wait();
                let _ = done_tx.send(run.call("send_message", discord_args(), Some("once")));
            });
        }
        // Every caller but the one the stub holds comes back on its own;
        // each result is kept, so a caller that hangs shows as a timeout.
        let refused: Vec<_> = (1..CALLERS)
            .map(|_| done.recv_timeout(std::time::Duration::from_secs(60)))
            .collect();
        // Released before any assertion, so the held call never blocks.
        release.send(()).unwrap();
        let sent = done.recv_timeout(std::time::Duration::from_secs(60));
        let timed_out = refused.iter().filter(|r| r.is_err()).count();
        assert_eq!(timed_out, 0, "{timed_out} caller(s) did not return in 60 s");
        let in_flight = refused
            .iter()
            .filter(|r| matches!(r, Ok((v, ok)) if !ok && code(v) == "in_flight"))
            .count();
        assert_eq!(in_flight, CALLERS - 1, "every other caller is in_flight");
        assert!(
            sent.is_ok_and(|(_, ok)| ok),
            "the one admitted call succeeds"
        );
    });
    assert_eq!(stub.requests().len(), 1);
    assert_eq!(run.ledger().records().len(), 1);
}

// -- Attempt binding -----------------------------------------------------

/// Attempt numbers restart with every execution of a node: an agent left
/// behind by an earlier execution claims the same number, but its token is
/// another attempt's, and it may not send.
#[test]
fn a_stale_claim_from_an_earlier_execution_of_the_node_may_not_send() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, "{}".into())]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let stale = run.current.lock().unwrap().clone();
    run.finish_node("failed");
    run.journal(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    });
    run.start_attempt(1);
    let (refused, ok) = run.call_as("send_message", discord_args(), None, stale);
    assert!(!ok);
    assert_eq!(code(&refused), "attempt_closed");
    assert!(stub.requests().is_empty());
}

/// The right claim after its attempt finished, and a call that claims no
/// attempt at all, may not send in the attempt's name.
#[test]
fn a_finished_attempt_or_an_empty_claim_may_not_send() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, "{}".into())]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let (refused, ok) = run.call_as(
        "send_message",
        discord_args(),
        None,
        AttemptClaim::default(),
    );
    assert!(!ok);
    assert_eq!(code(&refused), "attempt_closed");
    run.finish_attempt("failed");
    let (refused, ok) = run.call("send_message", discord_args(), None);
    assert!(!ok);
    assert_eq!(code(&refused), "attempt_closed");
    assert!(stub.requests().is_empty());
}

/// A host task a resumed drive adopted keeps the number of the attempt that
/// opened it; its task id binds it to the adopting attempt.
#[test]
fn a_call_from_an_adopted_host_task_binds_to_the_adopting_attempt() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-4"}"#.into())]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let adopting = run.start_attempt(1);
    run.journal(EventPayload::HostTaskRequested {
        task_id: "announce-t1".into(),
        node: NODE.into(),
        attempt: 1,
        prompt_ref: String::new(),
        role_prompt_ref: None,
        skills: Vec::new(),
        workdir: String::new(),
        outputs: None,
        deadline_ms: None,
        model_hint: None,
        hint_source: None,
        fallback_of: None,
        hint_note: None,
    });
    let (_, ok) = run.call_as(
        "send_message",
        discord_args(),
        None,
        AttemptClaim {
            item: None,
            attempt: Some(3),
            token: Some("the-opening-attempts-token".into()),
            host_task: Some("announce-t1".into()),
        },
    );
    assert!(ok);
    assert!(matches!(
        run.calls().as_slice(),
        [EventPayload::ConnectorCall { attempt: Some(1), attempt_seq: Some(s), .. }] if *s == adopting
    ));
}

// -- Effect scope --------------------------------------------------------

/// A failed visit and its retry share one scope, so the retry gets the
/// earlier result; a loop visit after a success starts afresh.
#[test]
fn a_retry_after_a_failed_visit_is_answered_and_a_loop_after_success_sends_again() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Respond(200, r#"{"id":"m-5"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-6"}"#.into()),
    ]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    assert!(run.call("send_message", discord_args(), Some("weekly")).1);

    run.finish_node("failed");
    run.journal(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    });
    run.start_attempt(1);
    let (answer, ok) = run.call("send_message", discord_args(), Some("weekly"));
    assert!(ok);
    assert_eq!(answer["already_sent"], true);
    assert_eq!(answer["ref"], "m-5");

    run.finish_node("succeeded");
    run.journal(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    });
    run.start_attempt(1);
    let (again, ok) = run.call("send_message", discord_args(), Some("weekly"));
    assert!(ok);
    assert!(again.get("already_sent").is_none());
    let requests = stub.requests();
    assert_eq!(requests.len(), 2);
    assert_ne!(
        request_json(&requests[0])["nonce"],
        request_json(&requests[1])["nonce"],
        "a new send draws a new nonce"
    );
}

// -- Replay chains -------------------------------------------------------

/// A lost answer, then a replay the service refuses (429): the replay did
/// not act, so the first send is still unknown, and the next call replays
/// it again with the same nonce rather than sending anew.
#[test]
fn a_replay_that_did_not_go_out_is_replayed_again_with_the_first_nonce() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Cut,
        Reply::Respond(
            429,
            r#"{"message":"rate limited","retry_after":0.1}"#.into(),
        ),
        Reply::Respond(200, r#"{"id":"m-8"}"#.into()),
    ]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let label = Some("announce-1.4.0");
    assert!(!run.call("send_message", discord_args(), label).1);
    assert!(!run.call("send_message", discord_args(), label).1);
    let (_, ok) = run.call("send_message", discord_args(), label);
    assert!(ok);
    let requests = stub.requests();
    assert_eq!(requests.len(), 3);
    let nonces: Vec<Value> = requests
        .iter()
        .map(|r| request_json(r)["nonce"].clone())
        .collect();
    assert!(
        nonces.iter().all(|n| *n == nonces[0]),
        "one nonce throughout"
    );
    let ledger = run.ledger();
    let records = ledger.records();
    assert_eq!(
        records[2].replay_of.as_deref(),
        Some(records[1].id.as_str())
    );
    assert!(matches!(records[1].state, IntentState::NotSent { .. }));
}

/// A service that dedupes by key and answers a conflict says the key was
/// seen, not what became of the first request: unknown, and the next call
/// replays the same nonce.
#[test]
fn a_key_conflict_is_unknown_and_replayed_with_the_same_nonce() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Respond(409, r#"{"message":"conflict"}"#.into()),
        Reply::Respond(200, r#"{"id":"m-9"}"#.into()),
    ]);
    let run = run_with("discord", &stub.base_url, Grant::of(&["send_message"]));
    let (first, ok) = run.call("send_message", discord_args(), Some("once"));
    assert!(!ok);
    assert_eq!(first["effect"], "unknown");
    let (_, ok) = run.call("send_message", discord_args(), Some("once"));
    assert!(ok);
    let requests = stub.requests();
    assert_eq!(
        request_json(&requests[0])["nonce"],
        request_json(&requests[1])["nonce"]
    );
    assert!(matches!(
        &run.ledger().records()[1].state,
        IntentState::Confirmed { source, .. } if source == "key_replay"
    ));
}

/// A repeat the ledger answers costs no budget: with `max_calls: 1` spent
/// by the send, its repeat still gets the earlier result, and only a new
/// send is refused.
#[test]
fn a_repeat_answered_from_the_ledger_is_not_refused_by_a_spent_budget() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-10"}"#.into())]);
    let run = run_with(
        "discord",
        &stub.base_url,
        Grant {
            functions: &["send_message"],
            on_unknown: None,
            max_calls: Some(1),
        },
    );
    assert!(run.call("send_message", discord_args(), Some("once")).1);
    let (answer, ok) = run.call("send_message", discord_args(), Some("once"));
    assert!(ok);
    assert_eq!(answer["already_sent"], true);
    let (refused, ok) = run.call("send_message", discord_args(), Some("another"));
    assert!(!ok);
    assert_eq!(code(&refused), "permission");
    assert_eq!(stub.requests().len(), 1);
    assert_eq!(
        run.ledger().records().len(),
        1,
        "a refused send records nothing"
    );
}

// -- GitHub issues --------------------------------------------------------

/// An issue whose answer was lost is found among the repository's recent
/// issues: the lookup reads the full listing (the function's projection
/// would drop the body that carries the marker) and the repeat is answered
/// with the issue's link.
#[test]
fn a_github_issue_with_a_lost_answer_is_found_among_the_recent_issues() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Cut,
        Reply::Respond(200, r#"{"login":"triage-bot"}"#.into()),
        listing_echoing("triage-bot", "https://github.com/acme/site/issues/43"),
    ]);
    let run = run_with("github", &stub.base_url, Grant::of(&["create_issue"]));
    let args =
        json!({ "owner": "acme", "repo": "site", "title": "Broken build", "body": "CI is red." });
    assert!(
        !run.call("create_issue", args.clone(), Some("broken-build"))
            .1
    );
    let (answer, ok) = run.call("create_issue", args, Some("broken-build"));
    assert!(ok);
    assert_eq!(answer["already_sent"], true);
    assert_eq!(answer["ref"], "https://github.com/acme/site/issues/43");
    let requests = stub.requests();
    assert_eq!(requests.len(), 3, "no second POST");
    assert!(requests[0].starts_with("POST /repos/acme/site/issues "));
    assert!(requests[2].starts_with("GET /repos/acme/site/issues?"));
    assert!(requests[2].contains("per_page=100") && requests[2].contains("since="));
}

// -- A person's decision -------------------------------------------------

/// A pending intent whose sender is alive is still in flight and cannot be
/// decided; once its sender is gone it reads as unknown and can be.
#[cfg(unix)]
#[test]
fn a_person_may_not_decide_a_send_in_flight_but_may_once_its_sender_is_gone() {
    use apb_engine::connector::intent::{ResolveError, resolve_by_person};
    let _lock = common::env_lock();
    let run = run_with(
        "telegram",
        "https://telegram.example",
        Grant::of(&["send_message"]),
    );
    // Above any pid the system hands out (Linux caps pid_max at 2^22), so
    // no process can hold it.
    let gone_pid = i32::MAX as u32;
    for (id, pid) in [("si-live", std::process::id()), ("si-gone", gone_pid)] {
        run.journal(EventPayload::SendIntentRecorded {
            item: None,
            intent: id.into(),
            nonce: format!("n-{id}"),
            node: NODE.into(),
            attempt: Some(1),
            attempt_seq: None,
            connector: "telegram".into(),
            function: "send_message".into(),
            account: "main".into(),
            mode: "none".into(),
            label: id.into(),
            pid,
            deadline_ms: u64::MAX,
            replay_of: None,
            resend_of: None,
        });
    }
    let dir = run.run.path();
    assert!(matches!(
        resolve_by_person(dir, "si-live", true, None, "ana"),
        Err(ResolveError::Refused(m)) if m.contains("still in flight")
    ));
    resolve_by_person(dir, "si-gone", false, None, "ana").unwrap();
    assert!(matches!(
        &run.ledger().get("si-gone").unwrap().state,
        IntentState::NotSent { source } if source == "person"
    ));
    assert!(matches!(
        resolve_by_person(dir, "si-none", true, None, "ana"),
        Err(ResolveError::NotFound(_))
    ));
}

/// A made-up `APB_ITEM_ID` buys nothing: on a plain node and on a list node
/// whose item attempt is running, a caller that names an item with no
/// attempt of its own may neither send nor make another call past a spent
/// `max_calls`.
#[test]
fn a_made_up_item_id_may_neither_send_nor_reset_the_budget() {
    let _lock = common::env_lock();
    for list in [false, true] {
        let stub = spawn_script(vec![Reply::Respond(200, r#"{"id":"m-1"}"#.into())]);
        let run = run_with(
            "discord",
            &stub.base_url,
            Grant {
                functions: &["send_message"],
                on_unknown: None,
                max_calls: Some(1),
            },
        );
        if list {
            run.journal(EventPayload::ListStarted {
                node: NODE.into(),
                total: 1,
                filtered_out: 0,
                items_digest: String::new(),
                batch_size: 1,
                parallel: 1,
                reopened: false,
            });
            run.journal(EventPayload::ListBatchStarted {
                node: NODE.into(),
                batch: 1,
                items: vec!["a".into()],
            });
            let mut started = read_all(run.run.path())
                .unwrap()
                .into_iter()
                .rev()
                .find_map(|e| match e.payload {
                    p @ EventPayload::AttemptStarted { .. } => Some(p),
                    _ => None,
                })
                .unwrap();
            started.stamp_item("a");
            run.journal(started);
            run.current.lock().unwrap().item = Some("a".into());
        }
        assert!(run.call("send_message", discord_args(), Some("one")).1);
        let forged = AttemptClaim {
            item: Some("bogus".into()),
            ..run.current.lock().unwrap().clone()
        };
        let (refused, ok) = run.call_as("send_message", discord_args(), Some("two"), forged);
        assert!(!ok, "list node: {list}");
        assert_ne!(code(&refused), "", "list node: {list}");
        assert_eq!(stub.requests().len(), 1, "list node: {list}");
        assert_eq!(run.ledger().records().len(), 1, "list node: {list}");
    }
}

// -- List mode: an item's own retry ----------------------------------------

impl Run {
    /// Journals attempt `attempt` of list item `item` with a fresh token and
    /// makes it the one [`Run::call`] claims.
    fn start_item_attempt(&self, attempt: u32, item: &str) {
        let token = apb_core::connector::marker::new_nonce();
        self.journal(EventPayload::AttemptStarted {
            item: Some(item.into()),
            node: NODE.into(),
            attempt,
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
        });
        *self.current.lock().unwrap() = AttemptClaim {
            item: Some(item.into()),
            attempt: Some(attempt),
            token: Some(token),
            host_task: None,
        };
    }
}

fn asana_task() -> Value {
    json!({ "project": "1200", "name": "Reply to the invoice question" })
}

/// An item's attempt files its task and then fails on the infrastructure;
/// the retry of the same item repeats the send and the ledger answers
/// `already_sent` with the task's gid, without a request. Another item with
/// the same arguments is its own send.
#[test]
fn an_items_retry_after_a_send_is_answered_already_sent_by_the_ledger() {
    let _lock = common::env_lock();
    let stub = spawn_script(vec![
        Reply::Respond(201, r#"{"data":{"gid":"77","name":"n"}}"#.into()),
        Reply::Respond(201, r#"{"data":{"gid":"78","name":"n"}}"#.into()),
    ]);
    let run = run_with("asana", &stub.base_url, Grant::of(&["create_task"]));
    run.start_item_attempt(1, "m-1");
    let (first, ok) = run.call("create_task", asana_task(), None);
    assert!(ok, "the first send goes out");
    assert!(first.get("already_sent").is_none());
    run.finish_attempt("failed");

    run.start_item_attempt(2, "m-1");
    let (repeat, ok) = run.call("create_task", asana_task(), None);
    assert!(ok);
    assert_eq!(repeat["already_sent"], json!(true));
    assert_eq!(repeat["ref"], "77", "the task the first send filed");
    assert_eq!(stub.requests().len(), 1, "the repeat made no request");

    run.start_item_attempt(1, "m-2");
    let (other, ok) = run.call("create_task", asana_task(), None);
    assert!(ok);
    assert!(
        other.get("already_sent").is_none(),
        "another item sends its own"
    );
    assert_eq!(stub.requests().len(), 2);
}
