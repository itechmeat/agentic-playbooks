//! The `inbox` function kind end to end through `connector::call::execute`:
//! the same grant gate, account selection, max_calls budget, args_schema
//! validation and ConnectorCall event logging every other kind goes through,
//! reading the local store instead of the network.
//!
//! Takes `common::env_lock()` because the store resolves through
//! `APB_CONFIG_DIR`, which is process-wide.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::connector::inbox::Inbox;
use apb_engine::connector::call::{CallRequest, execute};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const NODE: &str = "n";
const CONNECTOR: &str = "echo-hooks";

const CONNECTOR_YAML: &str = r#"
name: echo-hooks
version: 0.1.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: id
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending inbound events without consuming them
    read_only: true
    response_pick: [events, cursor]
    args_schema:
      type: object
      properties:
        consumer: { type: string }
        limit: { type: integer }
    inbox:
      op: read
  - name: inbox_ack
    description: Advance the consumer cursor after processing
    args_schema:
      type: object
      properties:
        consumer: { type: string }
        up_to_seq: { type: integer }
      required: [up_to_seq]
    inbox:
      op: ack
  - name: inbox_depth
    description: How many inbound events are pending
    read_only: true
    response_pick: [pending]
    inbox:
      op: peek_depth
"#;

fn account() -> ManifestAccount {
    ManifestAccount {
        name: "main".to_string(),
        default: true,
        fields: BTreeMap::from([(
            "app_secret".to_string(),
            "{{env.APB_ECHO_HOOKS_SECRET}}".to_string(),
        )]),
        env: BTreeMap::from([(
            "app_secret".to_string(),
            "APB_ECHO_HOOKS_SECRET".to_string(),
        )]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct".to_string(),
    }
}

fn seed_run(run_dir: &Path, functions: &[&str], max_calls: Option<u32>) {
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account()],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["main".to_string()],
            functions: functions.iter().map(|s| s.to_string()).collect(),
            max_calls,
        }],
    );
    manifest::write(run_dir, &m).unwrap();
    let cdir = run_dir.join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), CONNECTOR_YAML).unwrap();
}

fn call(run_dir: &Path, root: &Path, function: &str, args: serde_json::Value) -> serde_json::Value {
    let (value, _ok) = execute(CallRequest {
        run_dir,
        root,
        node_id: NODE,
        connector: CONNECTOR,
        function,
        account: None,
        args,
        dry_run: false,
        full: false,
    });
    value
}

/// Sets `APB_CONFIG_DIR` for the duration of the test and seeds three
/// deliveries into `echo-hooks/main`.
fn seed_inbox(cfg: &Path) {
    let base = cfg.join("connector-inbox");
    let inbox = Inbox::at(&base, CONNECTOR, "main").unwrap();
    for i in 1..=3u32 {
        inbox
            .append(&format!("m{i}"), &serde_json::json!({ "n": i }))
            .unwrap();
    }
}

#[test]
fn read_ack_and_depth_go_through_the_grant_gate() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed_inbox(cfg.path());
    seed_run(&run, &["inbox_read", "inbox_ack", "inbox_depth"], None);

    let out = call(
        &run,
        root.path(),
        "inbox_read",
        serde_json::json!({"consumer": "worker"}),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    let events = out["body"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[0]["body"]["n"], 1);
    assert!(
        events[0].get("provider_id").is_none(),
        "the envelope carries no provider id: {out}"
    );
    assert_eq!(out["body"]["cursor"], 0);
    assert_eq!(out["picked"], true, "response_pick applied: {out}");

    // A second read sees the same events: read never consumes.
    let again = call(
        &run,
        root.path(),
        "inbox_read",
        serde_json::json!({"consumer": "worker"}),
    );
    assert_eq!(again["body"]["events"].as_array().unwrap().len(), 3);

    let depth = call(
        &run,
        root.path(),
        "inbox_depth",
        serde_json::json!({"consumer": "worker"}),
    );
    assert_eq!(depth["body"]["pending"], 3, "was: {depth}");

    let acked = call(
        &run,
        root.path(),
        "inbox_ack",
        serde_json::json!({"consumer": "worker", "up_to_seq": 2}),
    );
    assert_eq!(acked["ok"], true, "was: {acked}");
    assert_eq!(acked["body"]["acked_up_to"], 2);

    let after = call(
        &run,
        root.path(),
        "inbox_read",
        serde_json::json!({"consumer": "worker"}),
    );
    let events = after["body"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["seq"], 3);
    assert_eq!(after["body"]["cursor"], 2);

    let limited = call(
        &run,
        root.path(),
        "inbox_read",
        serde_json::json!({"consumer": "auditor", "limit": 2}),
    );
    assert_eq!(limited["body"]["events"].as_array().unwrap().len(), 2);

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

/// The read envelope carries a byte cap and says when it hit one.
///
/// `limit` bounds the event count, not the bytes: 500 events of the 256 KiB
/// the ingest listener allows each is ~128 MiB of provider-written content
/// handed to an agent in one call, while every other connector kind stops at
/// the 1 MiB `BODY_CAP` and marks the result truncated. Events go from the
/// newest end, so the oldest pending ones stay visible and the consumer can
/// ack forward instead of being handed the same unackable page forever.
#[test]
fn an_oversize_read_is_capped_from_the_newest_end_and_flagged() {
    use apb_core::connector::inbox::InboxEvent;
    use apb_engine::connector::inbox::{READ_BYTE_CAP, read_envelope};

    let filler = "x".repeat(64 * 1024);
    let events: Vec<InboxEvent> = (1..=40u64)
        .map(|seq| InboxEvent {
            seq,
            received_at: 1_700_000_000_000,
            provider_id: format!("m{seq}"),
            body: serde_json::json!({ "pad": filler }),
        })
        .collect();

    let envelope = read_envelope(&events, 0);
    assert_eq!(envelope["truncated"], true, "the cut is reported");
    let rows = envelope["events"].as_array().unwrap();
    assert!(
        rows.len() < events.len(),
        "something was actually dropped: {} of {}",
        rows.len(),
        events.len()
    );
    assert_eq!(rows[0]["seq"], 1, "the oldest pending event is kept");
    assert_eq!(
        rows.last().unwrap()["seq"],
        rows.len() as u64,
        "the kept events are the contiguous oldest run, not a sample"
    );
    let rendered = serde_json::to_string(&envelope).unwrap().len();
    assert!(
        rendered < READ_BYTE_CAP + 128 * 1024,
        "the envelope stays within a row of the cap: {rendered}"
    );

    // Under the cap nothing is cut, and the flag is still present so a reader
    // never has to infer "all of it" from an absent field.
    let small = read_envelope(&events[..2], 7);
    assert_eq!(small["truncated"], false);
    assert_eq!(small["events"].as_array().unwrap().len(), 2);
    assert_eq!(small["cursor"], 7);
}

#[test]
fn every_reached_inbox_call_logs_one_connectorcall_event_without_a_body() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed_inbox(cfg.path());
    seed_run(&run, &["inbox_read"], None);

    call(&run, root.path(), "inbox_read", serde_json::json!({}));
    let events = read_all(&run).unwrap();
    let calls: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ConnectorCall {
                connector,
                function,
                account,
                url,
                outcome,
                http_status,
                ..
            } => Some((
                connector.clone(),
                function.clone(),
                account.clone(),
                url.clone(),
                outcome.clone(),
                *http_status,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "one event per reached call");
    assert_eq!(calls[0].0, CONNECTOR);
    assert_eq!(calls[0].1, "inbox_read");
    assert_eq!(calls[0].2, "main");
    assert_eq!(
        calls[0].3, "inbox://echo-hooks/main",
        "the endpoint, not a URL"
    );
    assert_eq!(calls[0].4, "ok");
    assert_eq!(calls[0].5, None, "an inbox call has no HTTP status");

    // The raw event log must not carry the delivered payload anywhere.
    let raw = std::fs::read_to_string(run.join("events.jsonl")).unwrap();
    assert!(
        !raw.contains("\"n\":1") && !raw.contains("m1"),
        "an inbound body or provider id leaked into the run log: {raw}"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[test]
fn the_gate_refuses_an_ungranted_function_and_enforces_max_calls() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed_inbox(cfg.path());
    seed_run(&run, &["inbox_read"], Some(1));

    let refused = call(
        &run,
        root.path(),
        "inbox_ack",
        serde_json::json!({"up_to_seq": 1}),
    );
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "permission", "was: {refused}");

    assert_eq!(
        call(&run, root.path(), "inbox_read", serde_json::json!({}))["ok"],
        true
    );
    let over = call(&run, root.path(), "inbox_read", serde_json::json!({}));
    assert_eq!(over["ok"], false);
    assert_eq!(over["error"]["code"], "permission", "was: {over}");
    assert!(
        over["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_calls"),
        "was: {over}"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[test]
fn bad_arguments_are_refused_before_the_store_is_touched() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed_run(&run, &["inbox_read", "inbox_ack"], None);

    // args_schema: `up_to_seq` is required for ack.
    let missing = call(&run, root.path(), "inbox_ack", serde_json::json!({}));
    assert_eq!(missing["ok"], false);
    assert_eq!(missing["error"]["code"], "invalid_args", "was: {missing}");

    // A consumer name is an identifier; anything else is refused by the
    // executor rather than reaching the cursor file.
    let bad = call(
        &run,
        root.path(),
        "inbox_read",
        serde_json::json!({"consumer": "../escape"}),
    );
    assert_eq!(bad["ok"], false);
    assert_eq!(bad["error"]["code"], "invalid_args", "was: {bad}");

    // An absent inbox is empty, not an error: nothing has been delivered yet.
    let empty = call(&run, root.path(), "inbox_read", serde_json::json!({}));
    assert_eq!(empty["ok"], true, "was: {empty}");
    assert_eq!(empty["body"]["events"].as_array().unwrap().len(), 0);

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}

#[test]
fn a_dry_run_describes_the_call_without_touching_the_store_or_the_budget() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    unsafe {
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }
    seed_inbox(cfg.path());
    seed_run(&run, &["inbox_read"], Some(1));

    let (out, ok) = execute(CallRequest {
        run_dir: &run,
        root: root.path(),
        node_id: NODE,
        connector: CONNECTOR,
        function: "inbox_read",
        account: None,
        args: serde_json::json!({"consumer": "worker"}),
        dry_run: true,
        full: false,
    });
    assert!(ok);
    assert_eq!(out["dry_run"], true);
    assert_eq!(out["inbox"]["op"], "read");
    assert_eq!(out["inbox"]["consumer"], "worker");
    assert_eq!(out["inbox"]["endpoint"], "inbox://echo-hooks/main");
    assert!(
        out.get("events").is_none() && out["inbox"].get("events").is_none(),
        "a dry run reads nothing: {out}"
    );
    assert!(
        read_all(&run).unwrap().is_empty(),
        "a dry run logs no event"
    );

    unsafe {
        std::env::remove_var("APB_CONFIG_DIR");
    }
}
