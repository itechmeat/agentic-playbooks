//! Task 19: the connectors feature end to end (spec
//! 2026-07-18-connectors-design, section 11 test plan). Unlike the per-piece
//! tests (`connector_run` drives the snapshot, `connector_call` drives the
//! executor, `connector_healthcheck` drives the probe), this module wires the
//! real `mock-tracker` fixture through the whole pipeline against a live
//! ephemeral server:
//!
//!   install the fixture into a temp `APB_CONFIG_DIR`
//!     -> write a project account config (two accounts, one default) and the
//!        project `secrets.env`
//!     -> `resolve_playbook` + `snapshot_connectors` into a run dir with a
//!        `{ functions: read_only, max_calls: 2 }` grant
//!     -> `connector_call::execute` the granted `list_items`, the ungranted
//!        `create_item`, and one call past the budget
//!     -> a trust-gated `connector_call::healthcheck` mock ping.
//!
//! Every test takes `common::env_lock()`: `APB_CONFIG_DIR` is process-wide
//! state that must not race another module's `set_var`, and secret resolution
//! reads the process environment.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

use apb_core::connector::config::{self, account_digest};
use apb_core::connector::resolve::resolve_playbook;
use apb_core::connector::store;
use apb_core::schema::Playbook;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::connector_call::{CallRequest, execute, healthcheck};
use apb_engine::connector_run::snapshot_connectors;
use apb_engine::event::{EventPayload, read_all};
use apb_engine::manifest::{self, RunExecutionManifest};

use crate::common;

const CONNECTOR: &str = "mock-tracker";
const NODE: &str = "a";
const SECRET_VAR: &str = "APB_E2E_TOKEN";
const SECRET_VALUE: &str = "e2e-secret-value-42";

/// Restores one process-wide env var to its prior value on drop, taken under
/// the shared `common::env_lock()` so a mutation never races another module.
struct VarGuard {
    var: &'static str,
    prior: Option<OsString>,
}
impl Drop for VarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }
}
fn set_var(var: &'static str, value: impl AsRef<std::ffi::OsStr>) -> VarGuard {
    let prior = std::env::var_os(var);
    unsafe {
        std::env::set_var(var, value);
    }
    VarGuard { var, prior }
}

/// Installs the `mock-tracker` fixture (the checked-in
/// `tests/fixtures/connectors/mock-tracker/` folder, verbatim) into
/// `<cfg>/connectors/mock-tracker/`, mirroring a real `apb` install (which is a
/// folder copy). The whole folder is copied so the tree digest matches the
/// on-disk fixture.
fn install_fixture(cfg: &Path) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connectors")
        .join(CONNECTOR);
    let dst = cfg.join("connectors").join(CONNECTOR);
    std::fs::create_dir_all(&dst).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
        }
    }
}

/// Writes the project account config for `mock-tracker` under `<root>`.
fn write_project_accounts(root: &Path, yaml: &str) {
    let path = config::project_config_path(root, CONNECTOR);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, yaml).unwrap();
}

/// Writes the project `secrets.env` with the token the accounts reference, so
/// secret resolution needs no process-env mutation.
fn write_project_secret(root: &Path) {
    let path = root.join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{SECRET_VAR}={SECRET_VALUE}\n")).unwrap();
}

/// A playbook whose single agent node binds `mock-tracker` with the
/// `read_only` shorthand and a two-call budget. Accounts are left unset, so the
/// grant covers every merged account.
fn bound_playbook() -> Playbook {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - id: a
    type: agent_task
    prompt: hi
    profile: x
    connectors: [{ name: mock-tracker, functions: read_only, max_calls: 2 }]
edges: []
"#;
    Playbook::from_yaml(yaml).unwrap()
}

/// Builds the two permit maps (connector digest, account digests) from a live
/// resolution - exactly what a correct policy gate hands the engine verbatim.
fn expected_maps(
    root: &Path,
    pb: &Playbook,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let out = resolve_playbook(root, pb).unwrap();
    let mut connectors = BTreeMap::new();
    let mut accounts = BTreeMap::new();
    for (name, resolved) in &out.connectors {
        connectors.insert(name.clone(), resolved.loaded.digest.clone());
        for a in &resolved.accounts {
            accounts.insert(format!("{name}/{}", a.name), account_digest(a));
        }
    }
    (connectors, accounts)
}

fn call(
    run_dir: &Path,
    root: &Path,
    function: &str,
    account: Option<&str>,
    args: serde_json::Value,
) -> (serde_json::Value, bool) {
    execute(CallRequest {
        run_dir,
        root,
        node_id: NODE,
        connector: CONNECTOR,
        function,
        account,
        args,
        dry_run: false,
        full: false,
    })
}

/// The full call pipeline: resolve + snapshot the fixture, then exercise the
/// grant (a granted read, an ungranted write, the call budget), the interim
/// secret redaction, and the snapshot's independence from later live edits.
#[test]
fn e2e_snapshot_and_call_flow() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());

    install_fixture(cfg.path());
    write_project_secret(root.path());

    // Two one-shot servers, one per account. Account `acct1` (the default)
    // echoes the token back in its body so the redaction path is exercised;
    // `acct2` answers a plain list.
    let echo_body = format!(r#"{{"items":[1],"echo":"{SECRET_VALUE}"}}"#);
    let server_a = common::spawn_http(200, "OK", &[], echo_body);
    let server_b = common::spawn_http(200, "OK", &[], r#"{"items":[2]}"#.to_string());
    write_project_accounts(
        root.path(),
        &format!(
            "accounts:\n\
             \x20 - name: acct1\n\
             \x20   default: true\n\
             \x20   base_url: {}\n\
             \x20   token: \"{{{{env.{SECRET_VAR}}}}}\"\n\
             \x20 - name: acct2\n\
             \x20   base_url: {}\n\
             \x20   token: \"{{{{env.{SECRET_VAR}}}}}\"\n",
            server_a.base_url, server_b.base_url
        ),
    );

    // Resolve + snapshot into the run dir, building the permit maps from the
    // live resolution the way a policy gate would.
    let pb = bound_playbook();
    let (expected_connectors, expected_accounts) = expected_maps(root.path(), &pb);
    let (connectors, grants) = snapshot_connectors(
        root.path(),
        run.path(),
        &pb,
        &expected_connectors,
        &expected_accounts,
    )
    .unwrap();

    // The read_only shorthand froze to exactly `list_items` and both accounts
    // are granted (accounts left unset in the binding = all).
    let node_grant = &grants[NODE][0];
    assert_eq!(node_grant.functions, vec!["list_items".to_string()]);
    assert_eq!(
        node_grant.accounts,
        vec!["acct1".to_string(), "acct2".to_string()]
    );
    assert_eq!(node_grant.max_calls, Some(2));

    let m = RunExecutionManifest {
        connectors,
        connector_grants: grants,
        ..Default::default()
    };
    manifest::write(run.path(), &m).unwrap();

    // Call 1: granted read on the default account. The server echoes the token,
    // so the interim redaction must replace it with `[redacted:VAR]` and the
    // raw secret must appear nowhere in the printed result. Budget: 1/2.
    let (v1, ok1) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({"q": "x"}),
    );
    assert!(ok1, "granted list_items should succeed: {v1}");
    assert_eq!(v1["status"], serde_json::json!(200));
    assert_eq!(v1["body"]["items"], serde_json::json!([1]));
    assert_eq!(
        v1["body"]["echo"],
        serde_json::json!(format!("[redacted:{SECRET_VAR}]"))
    );
    assert!(
        !v1.to_string().contains(SECRET_VALUE),
        "the raw secret leaked into the result: {v1}"
    );
    // The auth header carried the resolved secret to the server.
    let req = server_a
        .captured_request()
        .expect("acct1 server saw a request");
    assert!(
        req.contains(&format!("Authorization: Bearer {SECRET_VALUE}")),
        "auth header missing/wrong: {req}"
    );

    // The ungranted write is refused on the grant (function not in the
    // read_only set), never reaching HTTP. Assert this while the budget still
    // has room (1 of 2 spent) so the denial is unambiguously about the grant,
    // not the budget: a gate rejection appends no event, so it consumes nothing.
    let (vc, okc) = call(
        run.path(),
        root.path(),
        "create_item",
        None,
        serde_json::json!({"title": "t"}),
    );
    assert!(!okc, "create_item is not granted: {vc}");
    assert_eq!(vc["error"]["code"], serde_json::json!("permission"));
    let after_create = read_all(run.path())
        .unwrap()
        .iter()
        .filter(|e| matches!(e.payload, EventPayload::ConnectorCall { .. }))
        .count();
    assert_eq!(
        after_create, 1,
        "the ungranted call must not consume the budget (only call 1's event exists)"
    );

    // Editing the LIVE connector.yaml after the snapshot must change nothing:
    // every call reads the run-dir snapshot, never the live folder. Overwrite
    // the installed manifest with one that no longer has `list_items` - if a
    // call read it live, the next call would fail.
    std::fs::write(
        cfg.path().join("connectors").join(CONNECTOR).join("connector.yaml"),
        "name: mock-tracker\nversion: 9.9.9\nfunctions:\n  - name: gone\n    description: changed\n    mock: { status: 500, body: {} }\n",
    )
    .unwrap();

    // Call 2: granted read on the second account, proving the snapshot (not the
    // now-mangled live file) is what a call uses. Budget: 2/2.
    let (v2, ok2) = call(
        run.path(),
        root.path(),
        "list_items",
        Some("acct2"),
        serde_json::json!({"q": "y"}),
    );
    assert!(ok2, "snapshot must survive a live connector edit: {v2}");
    assert_eq!(v2["body"]["items"], serde_json::json!([2]));

    // Call 3: the budget of 2 is spent, so this is refused before any HTTP.
    let (v3, ok3) = call(
        run.path(),
        root.path(),
        "list_items",
        None,
        serde_json::json!({"q": "z"}),
    );
    assert!(!ok3, "the third call must hit the budget: {v3}");
    assert_eq!(v3["error"]["code"], serde_json::json!("permission"));
    let msg = v3["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("max_calls"),
        "message should name the budget: {msg}"
    );

    // Exactly the two executed calls left events; the two rejections did not.
    let events = read_all(run.path()).unwrap();
    let call_events: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ConnectorCall {
                outcome,
                account,
                url,
                ..
            } => Some((outcome.clone(), account.clone(), url.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(call_events.len(), 2, "only executed calls append events");
    assert!(call_events.iter().all(|(outcome, _, _)| outcome == "ok"));
    assert_eq!(call_events[0].1, "acct1");
    assert_eq!(call_events[1].1, "acct2");
    // The recorded URL is the pre-auth request URL.
    assert!(
        call_events[0].2.ends_with("/items?q=x"),
        "event url should be the pre-auth URL: {}",
        call_events[0].2
    );
    // No event body ever carries the raw secret.
    assert!(
        !events.iter().any(|e| serde_json::to_string(&e.payload)
            .unwrap()
            .contains(SECRET_VALUE)),
        "a recorded event leaked the raw secret"
    );
}

/// The healthcheck probe path: it loads the LIVE connector and account config,
/// so it is trust-gated. Approving the connector and account digests lets a
/// mock `ping` succeed without any network.
#[test]
fn e2e_healthcheck_mock_ping_after_approval() {
    let _lock = common::env_lock();
    let cfg = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _cfg = set_var("APB_CONFIG_DIR", cfg.path());
    let _tok = set_var(SECRET_VAR, SECRET_VALUE);

    install_fixture(cfg.path());
    write_project_accounts(
        root.path(),
        &format!(
            "accounts:\n\
             \x20 - name: acct1\n\
             \x20   base_url: https://unused.example\n\
             \x20   token: \"{{{{env.{SECRET_VAR}}}}}\"\n"
        ),
    );

    // Before any approval the probe refuses: the connector digest is unknown.
    let (denied, ok0) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(!ok0, "an unapproved connector must refuse: {denied}");
    assert_eq!(denied["error"]["code"], serde_json::json!("permission"));

    // Approve the connector tree digest and the account's non-secret digest.
    let loaded = store::load(CONNECTOR).unwrap();
    let accounts = config::load_merged(root.path(), CONNECTOR).unwrap();
    let acct = accounts.iter().find(|a| a.name == "acct1").unwrap();
    let acct_digest = account_digest(acct);
    {
        let mut trust = TrustStore::load();
        trust
            .approve_kind(
                &loaded.digest,
                CONNECTOR,
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        trust
            .approve_kind(
                &acct_digest,
                &account_trust_id(CONNECTOR, "acct1"),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
    }

    // The mock `ping` needs no network and answers the canned body.
    let (value, ok) = healthcheck(root.path(), CONNECTOR, "acct1");
    assert!(
        ok,
        "mock healthcheck should succeed after approval: {value}"
    );
    assert_eq!(value["status"], serde_json::json!(200));
    assert_eq!(value["body"], serde_json::json!({"ok": true}));
}
