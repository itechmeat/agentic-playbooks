//! `inbox_read` with `types` and `where` (spec
//! 2026-11-18-typed-inbox-reads): a filtered read skips and
//! acks for its own consumer only, reports `skipped` and the gap, refuses a
//! filter on `ack` or `peek_depth`, and moves no cursor under a grant of
//! read-only functions only or in a read-only process.
//!
//! Takes `common::env_lock()` because the store resolves through
//! `APB_CONFIG_DIR`, which is process-wide.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::connector::inbox::Inbox;
use apb_engine::connector::call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};

use crate::common;

const NODE: &str = "n";
const CONNECTOR: &str = "typed-hooks";

const CONNECTOR_YAML: &str = r#"
name: typed-hooks
version: 0.1.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  event_type_header: X-GitHub-Event
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending inbound events without consuming them
    read_only: true
    response_pick: [events, cursor, skipped, truncated]
    args_schema:
      type: object
      properties:
        consumer: { type: string }
        limit: { type: integer }
        types: { type: array, items: { type: string } }
        where: { type: array, items: { type: object } }
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
            on_unknown: None,
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

/// Seeds three deliveries into `typed-hooks/main` under the config dir
/// `cfg`: an untyped push, a typed `issues` delivery, another untyped push.
fn seed_inbox(cfg: &Path) {
    let base = cfg.join("connector-inbox");
    let inbox = Inbox::at(&base, CONNECTOR, "main").unwrap();
    inbox
        .append(
            "m1",
            &serde_json::json!({ "kind": "push", "action": "none" }),
        )
        .unwrap();
    inbox
        .append_typed(
            "m2",
            Some("issues"),
            &serde_json::json!({ "kind": "issue", "action": "opened" }),
        )
        .unwrap();
    inbox
        .append(
            "m3",
            &serde_json::json!({ "kind": "push", "action": "none" }),
        )
        .unwrap();
}

/// One test's environment: the env lock, `APB_CONFIG_DIR` pointed at a
/// fresh config dir (restored on drop, even on a panic), the seeded inbox
/// and a run whose node is granted `functions`.
struct Fixture {
    cfg: tempfile::TempDir,
    root: tempfile::TempDir,
    run: std::path::PathBuf,
    _var: common::EnvVar,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn fixture(functions: &[&str]) -> Fixture {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join(".apb/runs/r1");
    std::fs::create_dir_all(&run).unwrap();
    let _var = common::EnvVar::set("APB_CONFIG_DIR", cfg.path());
    seed_inbox(cfg.path());
    seed_run(&run, functions, None);
    Fixture {
        cfg,
        root,
        run,
        _var,
        _lock,
    }
}

impl Fixture {
    fn call(&self, function: &str, args: serde_json::Value) -> serde_json::Value {
        call(&self.run, self.root.path(), function, args)
    }

    fn store(&self) -> Inbox {
        Inbox::at(&self.cfg.path().join("connector-inbox"), CONNECTOR, "main").unwrap()
    }
}

const ALL: &[&str] = &["inbox_read", "inbox_ack", "inbox_depth"];

#[test]
fn inbox_read_with_types_skips_and_acks_for_its_consumer() {
    let fx = fixture(ALL);
    let out = fx.call(
        "inbox_read",
        serde_json::json!({ "consumer": "triage", "types": ["issues"] }),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    let events = out["body"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "was: {out}");
    assert_eq!(events[0]["body"]["kind"], "issue");
    assert_eq!(
        events[0]["type"], "issues",
        "a typed event names its type in the envelope"
    );
    assert_eq!(out["body"]["skipped"], 2);
    assert_eq!(out["body"]["cursor"], 1, "the leading skip was acked");
    assert_eq!(out["body"]["truncated"], false);
    assert_eq!(fx.store().depth("triage").unwrap().cursor, 1);

    // Another consumer of the same account still reads every event.
    let plain = fx.call("inbox_read", serde_json::json!({ "consumer": "default" }));
    assert_eq!(plain["ok"], true, "was: {plain}");
    let events = plain["body"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 3, "the default consumer still reads all");
    assert_eq!(plain["body"]["cursor"], 0);
    assert!(
        plain["body"].get("skipped").is_none(),
        "an unfiltered read keeps its exact envelope: {plain}"
    );
}

#[test]
fn inbox_read_with_where_returns_skipped() {
    let fx = fixture(ALL);
    let out = fx.call(
        "inbox_read",
        serde_json::json!({
            "consumer": "triage",
            "where": [{ "path": "action", "equals": "opened" }],
        }),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    let events = out["body"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "was: {out}");
    assert_eq!(events[0]["body"]["action"], "opened");
    assert_eq!(out["body"]["skipped"], 2);
}

#[test]
fn a_filter_on_ack_is_invalid_args() {
    let fx = fixture(ALL);
    let out = fx.call(
        "inbox_ack",
        serde_json::json!({ "consumer": "t", "up_to_seq": 1, "types": ["issues"] }),
    );
    assert_eq!(out["ok"], false, "was: {out}");
    assert_eq!(out["error"]["code"], "invalid_args", "was: {out}");
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("only op `read` takes types or where"),
        "was: {out}"
    );

    let out = fx.call(
        "inbox_depth",
        serde_json::json!({ "consumer": "t", "where": [{ "path": "a", "equals": 1 }] }),
    );
    assert_eq!(out["ok"], false, "was: {out}");
    assert_eq!(out["error"]["code"], "invalid_args", "was: {out}");
}

/// A node granted read-only functions only (`functions: read_only`
/// resolves to `inbox_read` and `inbox_depth` here) never moves a cursor
/// through a filtered read: it reports the skips and acks none of them, so
/// another playbook's consumer of the same name loses nothing.
#[test]
fn a_read_only_grant_moves_no_cursor() {
    let fx = fixture(&["inbox_read", "inbox_depth"]);
    let out = fx.call(
        "inbox_read",
        serde_json::json!({ "consumer": "deploy", "types": ["nomatch"] }),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    assert_eq!(out["body"]["events"].as_array().unwrap().len(), 0);
    assert_eq!(out["body"]["skipped"], 3, "the skips are reported: {out}");
    assert_eq!(out["body"]["cursor"], 0, "no cursor moved: {out}");
    assert_eq!(fx.store().depth("deploy").unwrap().cursor, 0);
    assert_eq!(fx.store().depth("deploy").unwrap().pending, 3);
}

/// A filtered read that skips every remaining event still carries the gap
/// retention left before them, at the top level of the call result.
#[test]
fn a_filtered_read_reports_the_gap() {
    use apb_core::connector::inbox::Retention;
    let fx = fixture(ALL);
    // The size cap keeps only the newest delivery: seqs 1 to 3 are gone
    // before `triage` read them.
    let tiny = Retention {
        max_bytes: 1,
        max_age_ms: u64::MAX,
    };
    fx.store()
        .append_with("m4", &serde_json::json!({ "kind": "push" }), &tiny)
        .unwrap();
    let out = fx.call(
        "inbox_read",
        serde_json::json!({ "consumer": "triage", "types": ["issues"] }),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    assert_eq!(out["body"]["events"].as_array().unwrap().len(), 0);
    assert_eq!(out["body"]["skipped"], 1, "was: {out}");
    assert_eq!(
        out["gap"],
        serde_json::json!({ "from": 1, "through": 3 }),
        "a filter never hides a gap: {out}"
    );
}

/// A read-only process refuses every connector call at the gate, so a
/// filtered read through `execute` neither reads nor moves a cursor.
#[test]
fn a_read_only_process_moves_no_cursor() {
    let full = concat!(module_path!(), "::a_read_only_process_moves_no_cursor");
    let path = full.split_once("::").map_or(full, |(_, rest)| rest);
    let cfg = tempfile::tempdir().unwrap();
    let cfg_path = cfg.path().to_path_buf();
    seed_inbox(&cfg_path);
    let dir = cfg_path.clone();
    apb_core::process_mode::testing::in_read_only_child_after(
        path,
        move || unsafe {
            std::env::set_var("APB_CONFIG_DIR", &dir);
        },
        |()| {
            let root = tempfile::tempdir().unwrap();
            let run = root.path().join(".apb/runs/r1");
            std::fs::create_dir_all(&run).unwrap();
            seed_run(&run, ALL, None);
            let out = call(
                &run,
                root.path(),
                "inbox_read",
                serde_json::json!({ "consumer": "triage", "types": ["issues"] }),
            );
            assert_eq!(out["ok"], false, "a read-only process calls nothing: {out}");
            assert_eq!(out["error"]["code"], "permission", "was: {out}");
            let inbox = Inbox::at(
                &std::path::PathBuf::from(std::env::var("APB_CONFIG_DIR").unwrap())
                    .join("connector-inbox"),
                CONNECTOR,
                "main",
            )
            .unwrap();
            assert_eq!(inbox.depth("triage").unwrap().cursor, 0);
        },
    );
}

/// A filtered read that acks the events it skipped records the gap it found
/// in the run journal before the cursor moves past it, and moves no cursor
/// when the gap cannot be recorded: a crash between the two never makes the
/// gap disappear. The journal is made unwritable (its file is a directory),
/// the injected failure; the read still answers with the gap in its
/// envelope, and the next read, once the journal writes, records the gap and
/// only then acks.
#[test]
fn a_gap_is_journaled_before_the_cursor_moves_past_it() {
    use apb_core::connector::inbox::Retention;
    let fx = fixture(ALL);
    let tiny = Retention {
        max_bytes: 1,
        max_age_ms: u64::MAX,
    };
    fx.store()
        .append_with("m4", &serde_json::json!({ "kind": "push" }), &tiny)
        .unwrap();
    let journal = fx.run.join("events.jsonl");
    std::fs::create_dir_all(&journal).unwrap();
    let out = fx.call(
        "inbox_read",
        serde_json::json!({ "consumer": "triage", "types": ["issues"] }),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    assert_eq!(
        out["gap"],
        serde_json::json!({ "from": 1, "through": 3 }),
        "the gap stays in the envelope: {out}"
    );
    assert_eq!(
        fx.store().depth("triage").unwrap().cursor,
        0,
        "a gap the journal did not take moves no cursor: {out}"
    );
    assert_eq!(out["body"]["cursor"], 0, "was: {out}");

    std::fs::remove_dir(&journal).unwrap();
    let out = fx.call(
        "inbox_read",
        serde_json::json!({ "consumer": "triage", "types": ["issues"] }),
    );
    assert_eq!(out["ok"], true, "was: {out}");
    assert_eq!(fx.store().depth("triage").unwrap().cursor, 4, "was: {out}");
    let events = apb_engine::event::read_all(&fx.run).unwrap();
    let gaps: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            apb_engine::event::EventPayload::InboxGap { from, through, .. } => {
                Some((*from, *through))
            }
            _ => None,
        })
        .collect();
    assert_eq!(gaps, vec![(1, 3)], "the gap is in the journal once");
}
