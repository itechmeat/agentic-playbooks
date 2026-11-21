//! The call path of an MCP connector function (design D4 "The order of one
//! call", D2 "Checking pins"): every call checks its pin record before any
//! secret is resolved, starts the pinned program with a fixed environment,
//! lists the tools, compares the mapped tool with its pin and makes one
//! `tools/call`. The healthcheck checks the pins without a call, and a live
//! call outside a run is refused by name.
//!
//! The server is the scripted stub of `apb_mcp_client::testing`. A run
//! snapshot is hand-built as in `connector_call.rs`; the pin record is
//! written through `PinStore::record` into a config dir under `$TMPDIR`
//! that is also the default one, so the redirect rule holds only in its
//! own test. Every test takes `common::env_lock()`: the config dir and the
//! variables some tests set are process-wide.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use apb_core::connector::config;
use apb_core::connector::mcp::digest::{pin_digest, tool_digest};
use apb_core::connector::mcp::pins::{PinStore, ServerPin, ToolPin};
use apb_core::connector::store;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::connector::call::{
    AttemptClaim, CallContext, CallRequest, execute_with, healthcheck, play_call,
};
use apb_engine::event::{EventLog, EventPayload, read_all_lossy_tail};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use apb_mcp_client::testing::{StubCall, StubEra, StubServer};
use serde_json::{Value, json};

use crate::common::{self, EnvVar};

pub(crate) const CONNECTOR: &str = "tracker-mcp";
pub(crate) const NODE: &str = "n";
pub(crate) const ACCOUNT: &str = "work";
pub(crate) const TOKEN_VAR: &str = "APB_MCP_TEST_TOKEN";
pub(crate) const TOKEN: &str = "mcp-token-value-4242";
/// The tree digest the hand-built run snapshot carries.
pub(crate) const TREE: &str = "sha256:tree-one";
/// The account digest the hand-built run snapshot carries.
pub(crate) const ACCOUNT_DIGEST: &str = "sha256:account-one";
/// `call/wire.rs` `BODY_CAP`.
pub(crate) const BODY_CAP: usize = 1024 * 1024;
/// Variables the shell itself puts in its environment.
pub(crate) const SHELL_NAMES: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];
pub(crate) const ALL_FUNCTIONS: &[&str] = &[
    "whoami",
    "search_issues",
    "pick_issues",
    "list_comments",
    "create_comment",
];

/// The connector of design D1 "Example" with `command` set to `command`,
/// a `healthcheck`, a proxy field and a read-only function with a
/// projection over the same tool.
pub(crate) fn connector_yaml(command: &str) -> String {
    format!(
        r#"name: tracker-mcp
version: 0.1.0
healthcheck: whoami
mcp:
  transport: stdio
  command: {command}
  args: ["--workspace", "{{{{account.workspace}}}}"]
  env:
    TRACKER_TOKEN: "{{{{secret.token}}}}"
account_fields:
  - {{ name: workspace, required: true }}
  - {{ name: proxy }}
  - {{ name: token, required: true, secret: true }}
identity: {{ function: whoami, path: id }}
functions:
  - name: whoami
    description: The account the server acts as
    read_only: true
    mcp: {{ tool: whoami }}
  - name: search_issues
    description: Search issues by text
    read_only: true
    mcp: {{ tool: search_issues }}
    args_schema:
      type: object
      properties: {{ query: {{ type: string }} }}
      required: [query]
  - name: pick_issues
    description: Search issues, ids only
    read_only: true
    mcp: {{ tool: search_issues }}
    response_pick: [issues.id]
  - name: list_comments
    description: Recent comments of one issue
    read_only: true
    mcp: {{ tool: list_comments }}
    args_schema:
      type: object
      properties: {{ issue: {{ type: string }}, since: {{ type: string }} }}
      required: [issue]
  - name: create_comment
    description: Add a comment to an issue
    mcp: {{ tool: create_comment }}
    args_schema:
      type: object
      properties: {{ issue: {{ type: string }}, body: {{ type: string }} }}
      required: [issue, body]
    idempotency:
      mode: marker
      arg: body
      format: html_comment
      reconcile:
        function: list_comments
        args_from_send: [issue]
        since_arg: since
        items: comments
        text: body
        author: author.id
        item_ref: url
"#
    )
}

/// The tools the stub lists: one of them annotated read-only behind a
/// write, one annotated destructive behind a read, one with an
/// `execution` member no typed decoding keeps.
pub(crate) fn tools() -> Value {
    json!([
        {
            "name": "whoami",
            "description": "The account the server acts as",
            "inputSchema": { "type": "object" },
            "annotations": { "destructiveHint": true }
        },
        {
            "name": "search_issues",
            "description": "Search issues by text",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } } },
            "execution": { "taskSupport": "forbidden" }
        },
        {
            "name": "list_comments",
            "description": "Recent comments of one issue",
            "inputSchema": { "type": "object" }
        },
        {
            "name": "create_comment",
            "description": "Add a comment to an issue",
            "inputSchema": { "type": "object" },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

/// The tool definition named `name` in `tools`.
pub(crate) fn tool_of(tools: &Value, name: &str) -> Value {
    tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("no tool {name}"))
}

/// `sha256:<hex>` of a file's bytes.
pub(crate) fn file_digest(path: &Path) -> String {
    apb_core::content::sha256_hex(&std::fs::read(path).unwrap())
}

/// A pin record of `tools` for `program`, bound to `tree` and `account`.
pub(crate) fn server_pin(tree: &str, account: &str, program: &Path, tools: &Value) -> ServerPin {
    let program_s = program.to_string_lossy().to_string();
    let program_digest = file_digest(program);
    let pins: BTreeMap<String, ToolPin> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_string(),
                ToolPin {
                    digest: tool_digest(t),
                    description: String::new(),
                    members: BTreeMap::new(),
                },
            )
        })
        .collect();
    let digests = pins
        .iter()
        .map(|(k, v)| (k.clone(), v.digest.clone()))
        .collect();
    ServerPin {
        connector_digest: tree.to_string(),
        account_digest: account.to_string(),
        command: program_s.clone(),
        pin_digest: pin_digest(tree, account, &program_s, &program_digest, &digests),
        program: program_s,
        program_digest,
        tools: pins,
        approved_at_ms: 1,
    }
}

/// An executable shell script at `path`.
pub(crate) fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    common::write_sync(path, body);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// One run with an mcp connector snapshot, a stub server and a config dir
/// that is also the default one.
pub(crate) struct Fx {
    pub(crate) tmp: tempfile::TempDir,
    pub(crate) cfg: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) run: PathBuf,
    pub(crate) stub: StubServer,
    /// The stub's absolute canonical path.
    pub(crate) program: PathBuf,
    pub(crate) yaml: String,
    // Dropped last: the variables, then the lock.
    _vars: Vec<EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Fx {
    pub(crate) fn new() -> Fx {
        Fx::with(|y| y)
    }

    /// A fixture whose connector definition is `edit` of the default one.
    pub(crate) fn with(edit: impl FnOnce(String) -> String) -> Fx {
        let lock = common::env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let cfg = base.join("cfg");
        let root = base.join("root");
        let run = root.join(".apb/runs/r1");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        let vars = vec![
            EnvVar::set("APB_CONFIG_DIR", &cfg),
            EnvVar::set("APB_TEST_DEFAULT_CONFIG_DIR", &cfg),
            EnvVar::remove("APB_RUN_ID"),
            EnvVar::remove("APB_HOST_TASK"),
            EnvVar::remove(TOKEN_VAR),
        ];
        let stub = StubServer::write(&base.join("stub"), StubEra::Modern).unwrap();
        stub.set_tools(&tools()).unwrap();
        let program = std::fs::canonicalize(stub.program()).unwrap();
        let secrets = root.join(".apb/secrets.env");
        std::fs::write(&secrets, format!("{TOKEN_VAR}={TOKEN}\n")).unwrap();
        let yaml = edit(connector_yaml(&program.to_string_lossy()));
        let fx = Fx {
            tmp,
            cfg,
            root,
            run,
            stub,
            program,
            yaml,
            _vars: vars,
            _lock: lock,
        };
        fx.seed_run(fx.account(), None);
        fx
    }

    /// The granted account of the snapshot: `workspace: acme` and the
    /// token from the project's secrets.
    pub(crate) fn account(&self) -> ManifestAccount {
        ManifestAccount {
            name: ACCOUNT.to_string(),
            default: true,
            fields: BTreeMap::from([
                ("workspace".to_string(), "acme".to_string()),
                (
                    "proxy".to_string(),
                    "http://proxy.account.test:3128".to_string(),
                ),
                ("token".to_string(), format!("{{{{env.{TOKEN_VAR}}}}}")),
            ]),
            env: BTreeMap::from([("token".to_string(), TOKEN_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: ACCOUNT_DIGEST.to_string(),
        }
    }

    /// The account with its token from a `{{cmd:...}}` helper that leaves
    /// `helper-ran` behind when it runs.
    pub(crate) fn helper_account(&self) -> ManifestAccount {
        let helper = self.write_helper();
        let mut a = self.account();
        a.fields.insert(
            "token".to_string(),
            format!("{{{{cmd:{}}}}}", helper.display()),
        );
        a.env.clear();
        a.cmd = BTreeMap::from([("token".to_string(), helper.to_string_lossy().to_string())]);
        a
    }

    pub(crate) fn write_helper(&self) -> PathBuf {
        let helper = self.base().join("helper.sh");
        write_script(
            &helper,
            &format!(
                "#!/bin/sh\necho ran >> '{}'\nprintf '{TOKEN}\\n'\n",
                self.helper_marker().display()
            ),
        );
        helper
    }

    pub(crate) fn helper_marker(&self) -> PathBuf {
        self.base().join("helper-ran")
    }

    pub(crate) fn base(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path()).unwrap()
    }

    /// Writes the run manifest (one connector, one grant for `NODE`) and the
    /// copied connector snapshot.
    pub(crate) fn seed_run(&self, account: ManifestAccount, max_calls: Option<u32>) {
        self.seed_grant(account, max_calls, None);
    }

    /// [`Fx::seed_run`] with the grant's `on_unknown`.
    pub(crate) fn seed_grant(
        &self,
        account: ManifestAccount,
        max_calls: Option<u32>,
        on_unknown: Option<apb_core::schema::OnUnknown>,
    ) {
        let mut m = RunExecutionManifest::default();
        m.connectors.push(ManifestConnector {
            doubled_accounts: Vec::new(),
            name: CONNECTOR.to_string(),
            digest: TREE.to_string(),
            accounts: vec![account],
        });
        m.connector_grants.insert(
            NODE.to_string(),
            vec![ManifestConnectorGrant {
                connector: CONNECTOR.to_string(),
                accounts: vec![ACCOUNT.to_string()],
                functions: ALL_FUNCTIONS.iter().map(|s| s.to_string()).collect(),
                max_calls,
                on_unknown,
            }],
        );
        let _ = std::fs::remove_file(self.run.join("manifest.yaml"));
        manifest::write(&self.run, &m).unwrap();
        let cdir = self.run.join("connectors");
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), &self.yaml).unwrap();
    }

    /// The pin record of the stub's tools under the snapshot's digests.
    pub(crate) fn pin_record(&self) -> ServerPin {
        server_pin(TREE, ACCOUNT_DIGEST, &self.program, &tools())
    }

    pub(crate) fn pin(&self) {
        PinStore::record(CONNECTOR, ACCOUNT, self.pin_record()).unwrap();
    }

    pub(crate) fn pin_edited(&self, edit: impl FnOnce(&mut ServerPin)) {
        let mut p = self.pin_record();
        edit(&mut p);
        PinStore::record(CONNECTOR, ACCOUNT, p).unwrap();
    }

    pub(crate) fn request(&self, function: &'static str, args: Value) -> CallRequest<'_> {
        CallRequest {
            run_dir: &self.run,
            root: &self.root,
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: None,
            args,
            dry_run: false,
            full: false,
        }
    }

    pub(crate) fn call(&self, function: &'static str, args: Value) -> (Value, bool) {
        execute_with(
            self.request(function, args),
            CallContext::node(None, AttemptClaim::default()),
        )
    }

    pub(crate) fn call_with(&self, req: CallRequest<'_>, intent: Option<&str>) -> (Value, bool) {
        execute_with(req, CallContext::node(intent, AttemptClaim::default()))
    }

    pub(crate) fn search(&self) -> (Value, bool) {
        self.call("search_issues", json!({ "query": "bug" }))
    }

    pub(crate) fn events(&self) -> Vec<EventPayload> {
        read_all_lossy_tail(&self.run)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.payload)
            .collect()
    }

    pub(crate) fn connector_calls(&self) -> Vec<EventPayload> {
        self.events()
            .into_iter()
            .filter(|p| matches!(p, EventPayload::ConnectorCall { .. }))
            .collect()
    }

    pub(crate) fn intents(&self) -> usize {
        self.events()
            .iter()
            .filter(|p| matches!(p, EventPayload::SendIntentRecorded { .. }))
            .count()
    }

    /// The stub never started.
    pub(crate) fn assert_not_started(&self) {
        assert!(
            !self.stub.dir.join("starts.log").exists(),
            "the server was started: {:?}",
            self.stub.starts()
        );
    }

    pub(crate) fn set_call(&self, call: StubCall) {
        self.stub.set_call(call).unwrap();
    }

    /// Installs the connector into the config dir, writes the project's
    /// account file and approves both; answers the installed tree digest
    /// and the merged account's digest.
    pub(crate) fn install(&self, token: &str) -> (String, String) {
        let dir = self.cfg.join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connector.yaml"), &self.yaml).unwrap();
        // A project account may not read a command: a helper-sourced token
        // lives in the global account file.
        let path = if token.contains("{{cmd:") {
            config::global_config_path(CONNECTOR).unwrap()
        } else {
            self.root
                .join(".apb/connector-config")
                .join(format!("{CONNECTOR}.yaml"))
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                "accounts:\n  - name: {ACCOUNT}\n    default: true\n    workspace: acme\n    token: \"{token}\"\n"
            ),
        )
        .unwrap();
        let loaded = store::load(CONNECTOR).unwrap();
        let accounts = config::load_merged(&self.root, CONNECTOR).unwrap();
        let acct = accounts.iter().find(|a| a.name == ACCOUNT).unwrap();
        let account_digest = config::account_digest(acct);
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
                &account_digest,
                &account_trust_id(CONNECTOR, ACCOUNT),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        (loaded.digest, account_digest)
    }

    /// [`Fx::install`] with the token from the project's secrets, and the
    /// pins recorded for the installed tree and the merged account.
    pub(crate) fn install_pinned(&self) {
        let (tree, account) = self.install(&format!("{{{{env.{TOKEN_VAR}}}}}"));
        PinStore::record(
            CONNECTOR,
            ACCOUNT,
            server_pin(&tree, &account, &self.program, &tools()),
        )
        .unwrap();
    }
}

pub(crate) fn error_code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

pub(crate) fn error_message(v: &Value) -> String {
    v["error"]["message"].as_str().unwrap_or("").to_string()
}

/// Whether `text` holds a `sha256:` or a run of 16 or more hexadecimal
/// characters.
fn holds_a_digest(text: &str) -> bool {
    if text.contains("sha256:") {
        return true;
    }
    let mut run = 0;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            if run >= 16 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[test]
fn a_read_only_call_returns_structured_content() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(StubCall::Result(json!({
        "content": [{ "type": "text", "text": "{\"issues\":[1,2]}" }],
        "structuredContent": { "issues": [{ "id": 1, "title": "a" }] }
    })));
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "issues": [{ "id": 1, "title": "a" }] }));
    assert!(v.get("dropped_content").is_none(), "{v}");
    assert_eq!(fx.stub.calls().len(), 1);
}

#[test]
fn a_call_returns_text_parsed_as_json() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(StubCall::Result(json!({
        "content": [{ "type": "text", "text": "{\"issues\":[3]}" }]
    })));
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "issues": [3] }));
}

#[test]
fn a_call_returns_plain_text() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(StubCall::Result(json!({
        "content": [
            { "type": "text", "text": "first line" },
            { "type": "text", "text": "second line" }
        ]
    })));
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "text": "first line\nsecond line" }));
}

#[test]
fn non_text_blocks_are_counted_as_dropped() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(StubCall::Result(json!({
        "content": [
            { "type": "text", "text": "a picture" },
            { "type": "image", "data": "aGVsbG8=", "mimeType": "image/png" },
            { "type": "resource_link", "uri": "file:///x", "name": "x" }
        ]
    })));
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "text": "a picture" }));
    assert_eq!(v["dropped_content"], json!(2), "{v}");
}

fn picked_answer() -> StubCall {
    StubCall::Result(json!({
        "content": [],
        "structuredContent": { "issues": [{ "id": 1, "title": "a" }, { "id": 2, "title": "b" }] }
    }))
}

#[test]
fn response_pick_projects_the_body() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(picked_answer());
    let (v, ok) = fx.call("pick_issues", json!({}));
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "issues": [{ "id": 1 }, { "id": 2 }] }));
    assert_eq!(v["picked"], json!(true), "{v}");
}

#[test]
fn full_skips_the_projection() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(picked_answer());
    let mut req = fx.request("pick_issues", json!({}));
    req.full = true;
    let (v, ok) = fx.call_with(req, None);
    assert!(ok, "{v}");
    assert_eq!(
        v["body"],
        json!({ "issues": [{ "id": 1, "title": "a" }, { "id": 2, "title": "b" }] })
    );
    assert!(v.get("picked").is_none(), "{v}");
}

#[test]
fn a_secret_echoed_by_the_tool_is_redacted() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(StubCall::Result(json!({
        "content": [{ "type": "text", "text": format!("your token is {TOKEN}") }]
    })));
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert!(!v.to_string().contains(TOKEN), "{v}");
    assert_eq!(
        v["body"]["text"],
        json!(format!("your token is [redacted:{TOKEN_VAR}]"))
    );
}

#[test]
fn a_large_read_is_incomplete_response() {
    let fx = Fx::new();
    fx.pin();
    fx.set_call(StubCall::Result(json!({
        "content": [],
        "structuredContent": { "blob": "a".repeat(BODY_CAP + 10) }
    })));
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "incomplete_response", "{v}");
}

#[test]
fn a_body_at_the_cap_in_the_worst_escaping_is_returned() {
    let fx = Fx::new();
    fx.pin();
    // A body whose serialization is exactly the cap: a string of two-byte
    // characters and its two quotes.
    let n = (BODY_CAP - 2) / 2;
    let body = "\u{e9}".repeat(n);
    assert_eq!(serde_json::to_string(&body).unwrap().len(), BODY_CAP);
    // Every character written as `é` in `structuredContent`, and the
    // same serialization in a text block with its escapes escaped again.
    let structured = format!("\"{}\"", "\\u00e9".repeat(n));
    let text = format!("\"\\\"{}\\\"\"", "\\\\u00e9".repeat(n));
    let raw = format!(
        r#"{{"content":[{{"type":"text","text":{text}}}],"structuredContent":{structured}}}"#
    );
    assert!(
        raw.len() > 6 * BODY_CAP,
        "the worst escaping: {}",
        raw.len()
    );
    fx.set_call(StubCall::RawResult(raw));
    let (v, ok) = fx.search();
    assert!(ok, "{}", error_message(&v));
    assert_eq!(v["body"], Value::String(body));
}

#[test]
fn the_server_sees_only_the_fixed_set_and_its_declared_env() {
    let fx = Fx::new();
    fx.pin();
    let _caller = [
        EnvVar::set("APB_RUN_DIR", "/caller/run"),
        EnvVar::set("SOME_AGENT_TOKEN", "agent-token-value"),
        EnvVar::set("HTTPS_PROXY", "http://caller.proxy.test:1"),
        EnvVar::set("NODE_EXTRA_CA_CERTS", "/caller/ca.pem"),
        EnvVar::set("XDG_CONFIG_HOME", "/caller/xdg"),
        EnvVar::set("HOME", "/caller/home"),
    ];
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    let seen: std::collections::BTreeSet<String> = fx
        .stub
        .env_seen()
        .into_iter()
        .filter(|n| !SHELL_NAMES.contains(&n.as_str()))
        .collect();
    let want: std::collections::BTreeSet<String> = [
        "HOME",
        "USER",
        "LOGNAME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "PATH",
        "TRACKER_TOKEN",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(seen, want);
    let entry = apb_core::config::account_entry().expect("an account entry");
    let home = entry.home.to_string_lossy().to_string();
    assert_eq!(
        fx.stub.env_value_seen("HOME").as_deref(),
        Some(home.as_str())
    );
    assert_eq!(
        fx.stub.env_value_seen("USER").as_deref(),
        Some(entry.name.as_str())
    );
    assert_eq!(
        fx.stub.env_value_seen("XDG_CONFIG_HOME"),
        Some(format!("{home}/.config"))
    );
    // The system directories first, the program's own directory last.
    let dir = fx.program.parent().unwrap().to_string_lossy().to_string();
    let homebrew = if cfg!(target_os = "macos") {
        ":/opt/homebrew/bin"
    } else {
        ""
    };
    assert_eq!(
        fx.stub.env_value_seen("PATH"),
        Some(format!("/usr/local/bin:/usr/bin:/bin{homebrew}:{dir}"))
    );
    assert_eq!(
        fx.stub.env_value_seen("TRACKER_TOKEN").as_deref(),
        Some(TOKEN)
    );
    assert_eq!(fx.stub.args_seen(), ["--workspace", "acme"]);
}

#[test]
fn a_declared_proxy_reaches_the_server() {
    let fx = Fx::with(|y| {
        y.replace(
            "    TRACKER_TOKEN: \"{{secret.token}}\"\n",
            "    TRACKER_TOKEN: \"{{secret.token}}\"\n    HTTPS_PROXY: \"{{account.proxy}}\"\n",
        )
    });
    fx.pin();
    let _caller = EnvVar::set("HTTPS_PROXY", "http://caller.proxy.test:1");
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert_eq!(
        fx.stub.env_value_seen("HTTPS_PROXY").as_deref(),
        Some("http://proxy.account.test:3128")
    );
}

#[test]
fn a_shadow_program_on_the_callers_path_is_not_started() {
    let fx = Fx::with(|y| {
        let line = y
            .lines()
            .find(|l| l.starts_with("  command: "))
            .unwrap()
            .to_string();
        y.replace(&line, "  command: stub-mcp.sh")
    });
    // Approved with the stub's directory on the person's PATH: the record
    // names the stub's absolute path.
    fx.pin_edited(|p| p.command = "stub-mcp.sh".to_string());
    let shadow = fx.base().join("shadow");
    std::fs::create_dir_all(&shadow).unwrap();
    let marker = fx.base().join("shadow-ran");
    write_script(
        &shadow.join("stub-mcp.sh"),
        &format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
    );
    let path = format!(
        "{}:{}",
        shadow.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _path = EnvVar::set("PATH", path);
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert!(!marker.exists(), "the shadow program started");
    assert_eq!(fx.stub.starts().len(), 1);
}

#[test]
fn a_changed_program_file_is_refused() {
    let fx = Fx::new();
    fx.pin();
    let mut bytes = std::fs::read(&fx.program).unwrap();
    bytes.push(b'\n');
    common::write_sync(&fx.program, &String::from_utf8(bytes).unwrap());
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    let msg = error_message(&v);
    assert!(msg.starts_with("mcp_server_changed:"), "{msg}");
    assert!(msg.contains("the program file changed"), "{msg}");
    fx.assert_not_started();
    assert!(fx.connector_calls().is_empty());
}

#[test]
fn a_changed_definition_refuses_the_old_pin() {
    let fx = Fx::new();
    // Pinned under the tree before an `mcp.args` change and a new tree
    // approval: the run snapshots the new tree.
    fx.pin_edited(|p| p.connector_digest = "sha256:tree-before".to_string());
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    let msg = error_message(&v);
    assert!(msg.starts_with("mcp_server_changed:"), "{msg}");
    assert!(msg.contains("the connector definition changed"), "{msg}");
    fx.assert_not_started();
}

#[test]
fn an_unpinned_server_is_refused() {
    let fx = Fx::new();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    assert!(error_message(&v).starts_with("mcp_server_unpinned:"), "{v}");
    fx.assert_not_started();
}

#[test]
fn a_run_step_with_a_redirected_config_dir_is_refused() {
    let fx = Fx::new();
    fx.pin();
    let other = fx.base().join("default-cfg");
    std::fs::create_dir_all(&other).unwrap();
    let _step = EnvVar::set("APB_RUN_ID", "r1");
    let _default = EnvVar::set("APB_TEST_DEFAULT_CONFIG_DIR", &other);
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    assert!(
        error_message(&v).starts_with("mcp_config_dir_redirected:"),
        "{v}"
    );
    fx.assert_not_started();
}

/// The refusal texts of each kind of pin difference, in order: an
/// unpinned server, a changed tree, a changed account, a program gone, a
/// changed program, an unpinned tool and a changed tool.
fn refusal_texts() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut take = |what: &str, fx: &Fx, f: &dyn Fn(&Fx) -> (Value, bool)| {
        let (v, ok) = f(fx);
        assert!(!ok, "{what}: {v}");
        out.push((what.to_string(), error_message(&v)));
    };
    {
        let fx = Fx::new();
        take("server_unpinned", &fx, &|fx| fx.search());
        fx.pin_edited(|p| p.connector_digest = "sha256:other-tree".into());
        take("tree", &fx, &|fx| fx.search());
        fx.pin_edited(|p| p.account_digest = "sha256:other-account".into());
        take("account", &fx, &|fx| fx.search());
        fx.pin_edited(|p| p.program = fx.base().join("gone.sh").to_string_lossy().into());
        take("program_gone", &fx, &|fx| fx.search());
        fx.pin_edited(|p| p.program_digest = "sha256:other-program".into());
        take("program", &fx, &|fx| fx.search());
        fx.pin_edited(|p| {
            p.tools.remove("search_issues");
        });
        take("tool_unpinned", &fx, &|fx| fx.search());
        fx.pin_edited(|p| {
            p.tools.get_mut("search_issues").unwrap().digest = "sha256:other-tool".into();
        });
        take("tool_changed", &fx, &|fx| fx.search());
    }
    out
}

#[test]
fn no_refusal_names_the_approve_command() {
    for (what, text) in refusal_texts() {
        assert!(!text.contains("apb connector approve"), "{what}: {text}");
        assert!(text.contains("docs/CONNECTORS.md"), "{what}: {text}");
    }
}

#[test]
fn no_refusal_carries_a_digest() {
    let texts = refusal_texts();
    let expect = [
        (
            "server_unpinned",
            "mcp_server_unpinned:",
            "is not approved for account `work`",
        ),
        (
            "tree",
            "mcp_server_changed:",
            "the connector definition changed",
        ),
        ("account", "mcp_server_changed:", "the account changed"),
        (
            "program_gone",
            "mcp_server_changed:",
            "the program file is gone",
        ),
        ("program", "mcp_server_changed:", "the program file changed"),
        (
            "tool_unpinned",
            "mcp_tool_unpinned:",
            "tool `search_issues`",
        ),
        ("tool_changed", "mcp_tool_changed:", "tool `search_issues`"),
    ];
    assert_eq!(texts.len(), expect.len());
    for ((what, text), (want, prefix, names)) in texts.iter().zip(expect) {
        assert_eq!(what, want);
        assert!(text.starts_with(prefix), "{what}: {text}");
        assert!(text.contains(names), "{what}: {text}");
        assert!(!holds_a_digest(text), "{what}: {text}");
    }
}

#[test]
fn a_changed_execution_member_refuses_the_next_call() {
    let fx = Fx::new();
    fx.pin();
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    let mut changed = tools();
    changed[1]["execution"] = json!({ "taskSupport": "optional" });
    fx.stub.set_tools(&changed).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_tool_changed:"), "{v}");
    assert_eq!(fx.stub.calls().len(), 1);
}

/// The trigger of review pass 3 through the whole call path: the server
/// answers the listing twice in one write, first with a byte order mark
/// and `search_issues` changed in its unknown `execution` member (rmcp
/// would strip the mark and take this one), then with the pinned tools.
/// The tap refuses the first line, so the call is refused before any
/// `tools/call`: no pin is matched against a list rmcp did not read.
#[test]
fn a_byte_order_mark_answer_never_matches_a_pin() {
    let fx = Fx::new();
    fx.pin();
    let mut changed = tools();
    changed[1]["execution"] = json!({ "taskSupport": "required" });
    let first = json!({
        "jsonrpc": "2.0", "id": "@ID@",
        "result": { "tools": changed, "resultType": "complete", "ttlMs": 0, "cacheScope": "private" }
    })
    .to_string()
    .replace("\"@ID@\"", "@ID@");
    let mut before = b"\xEF\xBB\xBF".to_vec();
    before.extend_from_slice(first.as_bytes());
    before.push(b'\n');
    fx.stub.set_list_before(&before).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    let msg = error_message(&v);
    assert!(msg.contains("mcp_message_invalid:"), "{v}");
    assert!(fx.stub.calls().is_empty(), "{:?}", fx.stub.calls());
    assert_eq!(fx.intents(), 0);
}

/// Every file under `dir`, read as bytes.
fn all_bytes_under(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                out.extend_from_slice(&b);
            }
        }
    }
    out
}

/// A server that logs to its stdout: log text and blank lines around the
/// listing's answer and the call's answer are dropped unread, the call
/// succeeds, its note carries the count only, and no file of the run, nor
/// the result, holds the text of a dropped line.
#[test]
fn log_lines_on_stdout_are_dropped_and_only_counted() {
    let fx = Fx::new();
    fx.pin();
    fx.stub
        .set_list_before("server ready LOGSECRET-1\n\n   \n")
        .unwrap();
    fx.stub
        .set_call_prefix(b"[info] LOGSECRET-2 handling call\n")
        .unwrap();
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    let text = v.to_string();
    assert!(!text.contains("LOGSECRET"), "{text}");
    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("4 line(s)"), "{v}");
    assert_eq!(fx.stub.calls().len(), 1);
    let stored = all_bytes_under(&fx.run);
    assert!(
        !String::from_utf8_lossy(&stored).contains("LOGSECRET"),
        "a dropped line reached the run directory"
    );
}

/// A failed call keeps the count in its error and no dropped text.
#[test]
fn a_dropped_line_never_reaches_an_error() {
    let fx = Fx::new();
    fx.pin();
    fx.stub.set_list_before("LOGSECRET-3 boot\n").unwrap();
    fx.set_call(StubCall::ExitAfterRead);
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    let text = v.to_string();
    assert!(!text.contains("LOGSECRET"), "{text}");
    assert!(error_message(&v).contains("1 line(s)"), "{v}");
    assert!(!String::from_utf8_lossy(&all_bytes_under(&fx.run)).contains("LOGSECRET"));
}

/// A log line the server writes right after its answer, in a write of its
/// own, is counted too: the count is read once the session closed. On a
/// failed call the session closes right after the answer.
#[test]
fn a_line_written_after_the_answer_is_counted() {
    let fx = Fx::new();
    fx.pin();
    fx.stub.set_call_suffix(b"LOGSECRET-4 done").unwrap();
    fx.set_call(StubCall::Error {
        code: -32000,
        message: "busy".to_string(),
    });
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert!(!v.to_string().contains("LOGSECRET"), "{v}");
    assert!(error_message(&v).contains("1 line(s)"), "{v}");
    assert!(!String::from_utf8_lossy(&all_bytes_under(&fx.run)).contains("LOGSECRET"));
}

#[test]
fn the_server_runs_in_a_fresh_private_directory() {
    let fx = Fx::new();
    fx.pin();
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    let cwd = fx.stub.cwd_seen().expect("a cwd");
    assert!(
        !cwd.starts_with(&fx.root),
        "never the project root: {cwd:?}"
    );
    assert!(
        cwd.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("apb-mcp-")),
        "{cwd:?}"
    );
    assert!(fx.stub.cwd_entries_seen().is_empty(), "empty at the start");
    assert!(!cwd.exists(), "removed after the call: {cwd:?}");
}

#[test]
fn the_server_leads_its_own_process_group() {
    let fx = Fx::with(|y| {
        y.replace(
            "  transport: stdio\n",
            "  transport: stdio\n  protocol: modern\n",
        )
        .replace(
            "    description: Search issues by text\n",
            "    description: Search issues by text\n    timeout_sec: 4\n",
        )
    });
    fx.pin();
    fx.set_call(StubCall::Hang);
    std::thread::scope(|s| {
        let call = s.spawn(|| fx.search());
        let until = Instant::now() + Duration::from_secs(10);
        while fx.stub.calls().is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let pid = fx.stub.last_pid().expect("the server started");
        assert!(StubServer::is_alive(pid), "the server still runs");
        assert_eq!(StubServer::process_group(pid), Some(pid));
        assert_ne!(
            StubServer::process_group(pid),
            StubServer::process_group(std::process::id()),
        );
        let _ = call.join();
    });
}

/// A `{{cmd:...}}` helper that, while the secrets resolve (after the pin
/// check hashed the program), puts a script that leaves `marker` behind
/// at the program's path, then prints the token.
fn swapping_account(fx: &Fx, marker: &Path) -> apb_engine::manifest::ManifestAccount {
    let swap = fx.base().join("swap.sh");
    write_script(
        &swap,
        &format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
    );
    let helper = fx.base().join("swapping-helper.sh");
    write_script(
        &helper,
        &format!(
            "#!/bin/sh\nmv '{}' '{}'\nprintf '{TOKEN}\\n'\n",
            swap.display(),
            fx.program.display()
        ),
    );
    let mut a = fx.account();
    a.fields.insert(
        "token".to_string(),
        format!("{{{{cmd:{}}}}}", helper.display()),
    );
    a.env.clear();
    a.cmd = BTreeMap::from([("token".to_string(), helper.to_string_lossy().to_string())]);
    a
}

#[test]
fn a_program_swapped_while_the_secrets_resolve_never_runs_with_them() {
    let fx = Fx::new();
    fx.pin();
    let marker = fx.base().join("swapped-program-ran");
    fx.seed_run(swapping_account(&fx, &marker), None);
    let (v, ok) = fx.search();
    assert!(!marker.exists(), "the swapped-in program ran: {v}");
    if cfg!(target_os = "linux") {
        // Started from the descriptor the pin check hashed.
        assert!(ok, "{v}");
        assert_eq!(fx.stub.starts().len(), 1);
        assert_eq!(
            fx.stub.env_value_seen("TRACKER_TOKEN").as_deref(),
            Some(TOKEN)
        );
    } else {
        // The path, hashed again right before the spawn, no longer holds
        // the approved bytes.
        assert!(!ok, "{v}");
        assert!(error_message(&v).starts_with("mcp_server_changed:"), "{v}");
        fx.assert_not_started();
    }
}

#[test]
fn a_program_turned_into_a_link_is_refused() {
    let fx = Fx::new();
    fx.pin();
    // A link to identical bytes: a link is never followed.
    let copy = fx.base().join("stub-copy.sh");
    std::fs::copy(&fx.program, &copy).unwrap();
    std::fs::remove_file(&fx.program).unwrap();
    std::os::unix::fs::symlink(&copy, &fx.program).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    let msg = error_message(&v);
    assert!(msg.starts_with("mcp_server_changed:"), "{msg}");
    assert!(msg.contains("not a regular file"), "{msg}");
    fx.assert_not_started();
}

#[test]
fn a_changed_command_is_refused() {
    // The definition's `command` now names another program, and the
    // connector was approved again: the run snapshots the new tree, while
    // the account's pins still bind the old tree and the old program.
    let other = tempfile::tempdir().unwrap();
    let other_stub = StubServer::write(other.path(), StubEra::Modern).unwrap();
    let other_program = std::fs::canonicalize(other_stub.program()).unwrap();
    let fx = Fx::with(|y| {
        let line = y
            .lines()
            .find(|l| l.starts_with("  command: "))
            .unwrap()
            .to_string();
        y.replace(&line, &format!("  command: {}", other_program.display()))
    });
    fx.pin_edited(|p| p.connector_digest = "sha256:tree-before-the-command-change".into());
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    let msg = error_message(&v);
    assert!(msg.starts_with("mcp_server_changed:"), "{msg}");
    assert!(msg.contains("the connector definition changed"), "{msg}");
    fx.assert_not_started();
    assert!(
        other_stub.starts().is_empty(),
        "the new command never started"
    );
}

/// An eval case keeps the config dir it was handed (CLAUDE.md, the launch
/// policy). A live MCP call from its run steps is refused by the redirect
/// rule even with the pins in that config dir: a step never reads pins a
/// redirected config dir could hold. Such a case gives its MCP calls a
/// double (`connectors:`) instead.
#[test]
fn an_eval_case_with_its_own_config_dir_calls_no_server_live() {
    let fx = Fx::new();
    fx.pin();
    let machine = fx.base().join("machine-cfg");
    std::fs::create_dir_all(&machine).unwrap();
    let _step = EnvVar::set("APB_RUN_ID", "r1");
    // The eval case's config dir (with the pins) is not the machine's.
    let _default = EnvVar::set("APB_TEST_DEFAULT_CONFIG_DIR", &machine);
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert!(
        error_message(&v).starts_with("mcp_config_dir_redirected:"),
        "{v}"
    );
    fx.assert_not_started();
}

#[test]
fn a_changed_pin_runs_no_secret_helper() {
    type Edit = fn(&mut ServerPin);
    let cases: [(&str, &str, Edit); 4] = [
        ("program", "mcp_server_changed:", |p| {
            p.program_digest = "sha256:other".into()
        }),
        ("tree", "mcp_server_changed:", |p| {
            p.connector_digest = "sha256:other".into()
        }),
        ("account", "mcp_server_changed:", |p| {
            p.account_digest = "sha256:other".into()
        }),
        ("tool", "mcp_tool_unpinned:", |p| {
            p.tools.remove("search_issues");
        }),
    ];
    for (what, prefix, edit) in cases {
        let fx = Fx::new();
        fx.seed_run(fx.helper_account(), None);
        fx.pin_edited(edit);
        let (v, ok) = fx.search();
        assert!(!ok, "{what}: {v}");
        assert!(error_message(&v).starts_with(prefix), "{what}: {v}");
        assert!(!fx.helper_marker().exists(), "{what}: the helper ran");
        fx.assert_not_started();
    }
}

#[test]
fn a_dry_run_starts_nothing_and_names_env_only() {
    let fx = Fx::new();
    fx.pin();
    let mut req = fx.request("search_issues", json!({ "query": "bug" }));
    req.dry_run = true;
    let (v, ok) = fx.call_with(req, None);
    assert!(ok, "{v}");
    assert_eq!(v["dry_run"], json!(true), "{v}");
    assert_eq!(v["program"], json!(fx.program.to_string_lossy()), "{v}");
    assert_eq!(v["args"], json!(["--workspace", "acme"]), "{v}");
    assert_eq!(v["env"], json!(["TRACKER_TOKEN"]), "{v}");
    assert_eq!(v["tool"], json!("search_issues"), "{v}");
    assert_eq!(v["arguments"], json!({ "query": "bug" }), "{v}");
    assert!(!v.to_string().contains(TOKEN), "{v}");
    fx.assert_not_started();
    assert!(fx.connector_calls().is_empty());
}

#[test]
fn each_call_starts_its_own_server() {
    let fx = Fx::new();
    fx.pin();
    assert!(fx.search().1);
    assert!(fx.search().1);
    assert_eq!(fx.stub.starts().len(), 2);
    assert_eq!(fx.stub.calls().len(), 2);
}

#[test]
fn arguments_that_are_not_an_object_are_refused() {
    let fx = Fx::new();
    fx.pin();
    for function in ["search_issues", "whoami", "create_comment"] {
        for args in [json!("x"), json!(1), json!([1]), Value::Null] {
            for dry_run in [false, true] {
                let mut req = fx.request(function, args.clone());
                req.dry_run = dry_run;
                let (v, ok) = fx.call_with(req, Some("c1"));
                assert!(!ok, "{function} {args} {dry_run}: {v}");
                assert_eq!(error_code(&v), "invalid_args", "{v}");
            }
        }
    }
    fx.assert_not_started();
    assert_eq!(fx.intents(), 0);
    assert!(fx.connector_calls().is_empty());
}

/// Arguments the line tap could not parse inside the request line (the
/// review's 126 nested arrays) or longer than the client takes are
/// refused at the edge, `invalid_args`, before any secret resolves or
/// server starts; nothing is recorded and no call is journaled.
#[test]
fn arguments_too_deep_or_too_long_are_refused_at_the_edge() {
    let fx = Fx::new();
    fx.pin();
    let mut deep = json!(0);
    for _ in 0..126 {
        deep = json!([deep]);
    }
    let long = "a".repeat(apb_mcp_client::MAX_ARGUMENT_BYTES);
    for (args, name) in [
        (json!({ "q": deep }), "mcp_arguments_too_deep:"),
        (json!({ "q": long }), "mcp_arguments_too_large:"),
    ] {
        for function in ["search_issues", "create_comment"] {
            for dry_run in [false, true] {
                let mut req = fx.request(function, args.clone());
                req.dry_run = dry_run;
                let (v, ok) = fx.call_with(req, Some("c1"));
                assert!(!ok, "{function} {dry_run}");
                assert_eq!(error_code(&v), "invalid_args", "{v}");
                assert!(error_message(&v).starts_with(name), "{v}");
            }
        }
    }
    fx.assert_not_started();
    assert_eq!(fx.intents(), 0);
    assert!(fx.connector_calls().is_empty());
}

#[test]
fn a_pin_of_another_project_account_is_refused() {
    let fx = Fx::new();
    // Two project roots, each with its own `work` account.
    let digest_in = |root: &Path, workspace: &str| {
        let path = root
            .join(".apb/connector-config")
            .join(format!("{CONNECTOR}.yaml"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                "accounts:\n  - name: {ACCOUNT}\n    default: true\n    workspace: {workspace}\n    token: \"{{{{env.{TOKEN_VAR}}}}}\"\n"
            ),
        )
        .unwrap();
        let accounts = config::load_merged(root, CONNECTOR).unwrap();
        config::account_digest(accounts.iter().find(|a| a.name == ACCOUNT).unwrap())
    };
    let first = fx.base().join("first");
    let first_digest = digest_in(&first, "first-ws");
    let second_digest = digest_in(&fx.root, "acme");
    assert_ne!(first_digest, second_digest);
    let mut account = fx.account();
    account.digest = second_digest;
    fx.seed_run(account, None);
    fx.pin_edited(|p| p.account_digest = first_digest);
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    let msg = error_message(&v);
    assert!(msg.starts_with("mcp_server_changed:"), "{msg}");
    assert!(msg.contains("the account changed"), "{msg}");
    assert!(msg.contains("account `work`"), "{msg}");
    fx.assert_not_started();
}

#[test]
fn a_reflected_secret_in_a_call_listing_is_refused() {
    let fx = Fx::new();
    fx.pin();
    let mut reflected = tools();
    reflected[1]["description"] = json!(format!("Search with {TOKEN}"));
    fx.stub.set_tools(&reflected).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    assert!(
        error_message(&v).starts_with("mcp_tools_reflect_a_secret:"),
        "{v}"
    );
    assert!(!v.to_string().contains(TOKEN), "{v}");
    assert!(fx.stub.calls().is_empty(), "a tools/call was made");
    assert!(fx.connector_calls().is_empty());
}

#[test]
fn an_unpinned_tool_is_refused_before_the_call() {
    let fx = Fx::new();
    fx.pin_edited(|p| {
        p.tools.remove("search_issues");
    });
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    assert!(error_message(&v).starts_with("mcp_tool_unpinned:"), "{v}");
    fx.assert_not_started();
    assert!(fx.stub.calls().is_empty());
}

#[test]
fn a_description_changed_between_two_calls_is_refused() {
    let fx = Fx::new();
    fx.pin();
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    let mut longer = tools();
    longer[1]["description"] = json!("Search issues by text quickly");
    fx.stub.set_tools(&longer).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    assert!(error_message(&v).starts_with("mcp_tool_changed:"), "{v}");
    assert_eq!(fx.stub.calls().len(), 1);
}

#[test]
fn a_reapproved_tool_goes_out_again() {
    let fx = Fx::new();
    fx.pin();
    let mut longer = tools();
    longer[1]["description"] = json!("Search issues by text quickly");
    fx.stub.set_tools(&longer).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    PinStore::record(
        CONNECTOR,
        ACCOUNT,
        server_pin(TREE, ACCOUNT_DIGEST, &fx.program, &longer),
    )
    .unwrap();
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
    assert_eq!(fx.stub.calls().len(), 1);
}

#[test]
fn a_missing_tool_is_refused() {
    let fx = Fx::new();
    fx.pin();
    let fewer: Vec<Value> = tools()
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["name"] != "search_issues")
        .cloned()
        .collect();
    fx.stub.set_tools(&json!(fewer)).unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "config", "{v}");
    assert!(error_message(&v).starts_with("mcp_tool_missing:"), "{v}");
    assert!(fx.stub.calls().is_empty());
}

#[test]
fn an_unparsable_pin_store_refuses_the_call() {
    let fx = Fx::new();
    std::fs::write(fx.cfg.join("mcp-tools.json"), "not json").unwrap();
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "config", "{v}");
    assert!(error_message(&v).contains("mcp-tools.json"), "{v}");
    fx.assert_not_started();
    assert_eq!(
        std::fs::read_to_string(fx.cfg.join("mcp-tools.json")).unwrap(),
        "not json"
    );
}

#[test]
fn annotations_never_change_what_a_call_may_do() {
    let fx = Fx::new();
    fx.pin();
    // A write whose tool says it is read-only still goes through the ledger.
    let (v, ok) = fx.call_with(
        fx.request("create_comment", json!({ "issue": "1", "body": "hello" })),
        Some("c1"),
    );
    assert!(ok, "{v}");
    assert_eq!(fx.intents(), 1);
    let calls = fx.stub.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].contains("apb-intent"),
        "the marker rides: {}",
        calls[0]
    );
    // A read whose tool says it is destructive needs no idempotency.
    let (v, ok) = fx.call("whoami", json!({}));
    assert!(ok, "{v}");
    assert_eq!(fx.intents(), 1);
}

#[test]
fn the_journal_carries_the_tool_digest() {
    let fx = Fx::new();
    fx.pin();
    assert!(fx.search().1);
    let want = tool_digest(&tool_of(&tools(), "search_issues"));
    match &fx.connector_calls()[..] {
        [EventPayload::ConnectorCall { tool_digest, .. }] => {
            assert_eq!(tool_digest.as_deref(), Some(want.as_str()));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_journal_url_is_the_endpoint() {
    let fx = Fx::new();
    fx.pin();
    assert!(fx.search().1);
    match &fx.connector_calls()[..] {
        [
            EventPayload::ConnectorCall {
                url,
                outcome,
                response_complete,
                ..
            },
        ] => {
            assert_eq!(url, "mcp+stdio://tracker-mcp/search_issues");
            assert_eq!(outcome, "ok");
            assert_eq!(*response_complete, Some(true));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn max_calls_counts_mcp_calls() {
    let fx = Fx::new();
    fx.seed_run(fx.account(), Some(1));
    fx.pin();
    assert!(fx.search().1);
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "permission", "{v}");
    assert!(error_message(&v).contains("max_calls"), "{v}");
    assert_eq!(fx.stub.starts().len(), 1);
}

/// Writes `bytes` into a FIFO the stub reads, opened without blocking
/// for up to 10 s, so a FIFO nobody reads fails the test instead of
/// hanging it.
#[cfg(unix)]
fn write_fifo(fifo: &Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
        {
            Ok(mut f) => {
                // Blocking again for the write itself: the page is small.
                f.write_all(bytes).unwrap();
                return;
            }
            Err(e) if Instant::now() >= until => panic!("nobody reads the FIFO: {e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The budget is decided again once the server runs: a call whose
/// allowance an earlier call spent while this one's server was starting
/// (held here in its listing) is refused, and its `tools/call` is never
/// sent.
#[cfg(unix)]
#[test]
fn an_allowance_spent_during_the_start_refuses_the_call() {
    let fx = Fx::new();
    fx.seed_run(fx.account(), Some(1));
    fx.pin();
    let journal = fx.run.join("events.jsonl");
    let before = std::fs::read(&journal).unwrap_or_default();
    assert!(fx.search().1, "the first call spends the allowance");
    let spent = std::fs::read(&journal).unwrap();
    // The second call starts on the journal as it was before the first.
    std::fs::write(&journal, &before).unwrap();
    let page = fx.stub.dir.join("tools.0.json");
    std::fs::write(fx.stub.dir.join("tools.pages"), "1").unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&page)
            .status()
            .unwrap()
            .success()
    );
    let listings = || {
        fx.stub
            .methods()
            .iter()
            .filter(|m| *m == "tools/list")
            .count()
    };
    let first = listings();
    let (v, ok) = std::thread::scope(|scope| {
        let second = scope.spawn(|| fx.search());
        let until = Instant::now() + Duration::from_secs(30);
        while listings() == first {
            assert!(
                Instant::now() < until && !second.is_finished(),
                "the second listing request"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // The first call's record lands while the second's server starts.
        std::fs::write(&journal, &spent).unwrap();
        write_fifo(&page, tools().to_string().as_bytes());
        second.join().unwrap()
    });
    assert!(!ok, "{v}");
    assert!(error_message(&v).contains("max_calls"), "{v}");
    assert_eq!(fx.stub.calls().len(), 1, "no second tools/call");
}

#[test]
fn a_refused_pin_check_spends_no_max_calls() {
    let fx = Fx::new();
    fx.seed_run(fx.account(), Some(1));
    let (v, ok) = fx.search();
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_server_unpinned:"), "{v}");
    fx.pin();
    let (v, ok) = fx.search();
    assert!(ok, "{v}");
}

/// The config dir's files and their contents.
fn config_files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
            }
        }
    }
    out
}

#[test]
fn the_healthcheck_checks_the_pins_without_a_call() {
    let fx = Fx::new();
    fx.install_pinned();
    let before = config_files(&fx.cfg);
    let (v, ok) = healthcheck(&fx.root, CONNECTOR, ACCOUNT);
    assert!(ok, "{v}");
    assert_eq!(v["mcp"]["tools_checked"], json!(ALL_FUNCTIONS.len()), "{v}");
    assert!(
        v["mcp"]["era"].as_str().unwrap().starts_with("modern"),
        "{v}"
    );
    assert_eq!(fx.stub.starts().len(), 1);
    let methods = fx.stub.methods();
    assert!(methods.iter().any(|m| m == "tools/list"), "{methods:?}");
    assert!(!methods.iter().any(|m| m == "tools/call"), "{methods:?}");
    assert!(fx.stub.calls().is_empty());
    assert_eq!(config_files(&fx.cfg), before, "the config dir changed");
}

#[test]
fn the_healthcheck_refuses_a_changed_tool() {
    let fx = Fx::new();
    fx.install_pinned();
    let mut changed = tools();
    changed[2]["description"] = json!("Recent comments of one issue, newest first");
    fx.stub.set_tools(&changed).unwrap();
    let (v, ok) = healthcheck(&fx.root, CONNECTOR, ACCOUNT);
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_tool_changed:"), "{v}");
    assert!(error_message(&v).contains("list_comments"), "{v}");
    assert!(!fx.stub.methods().iter().any(|m| m == "tools/call"));
}

#[test]
fn the_healthcheck_of_an_unpinned_account_starts_nothing() {
    let fx = Fx::new();
    let helper = fx.write_helper();
    fx.install(&format!("{{{{cmd:{}}}}}", helper.display()));
    let (v, ok) = healthcheck(&fx.root, CONNECTOR, ACCOUNT);
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_server_unpinned:"), "{v}");
    assert!(!fx.helper_marker().exists(), "the helper ran");
    fx.assert_not_started();
}

#[test]
fn the_healthcheck_of_a_connector_without_one_is_refused_as_on_main() {
    let fx = Fx::with(|y| y.replace("healthcheck: whoami\n", ""));
    fx.install_pinned();
    let (v, ok) = healthcheck(&fx.root, CONNECTOR, ACCOUNT);
    assert!(!ok, "{v}");
    assert!(error_message(&v).contains("declares no healthcheck"), "{v}");
    fx.assert_not_started();
}

#[test]
fn a_playground_call_of_an_mcp_function_is_refused_by_name() {
    let fx = Fx::new();
    let helper = fx.write_helper();
    let (tree, account) = fx.install(&format!("{{{{cmd:{}}}}}", helper.display()));
    PinStore::record(
        CONNECTOR,
        ACCOUNT,
        server_pin(&tree, &account, &fx.program, &tools()),
    )
    .unwrap();
    let (v, ok) = play_call(
        &fx.root,
        CONNECTOR,
        Some(ACCOUNT),
        "search_issues",
        &json!({ "query": "bug" }),
        false,
        false,
    );
    assert!(!ok, "{v}");
    assert!(
        error_message(&v).starts_with("mcp_live_call_unsupported:"),
        "{v}"
    );
    assert!(!fx.helper_marker().exists(), "the helper ran");
    fx.assert_not_started();
}

#[test]
fn a_playground_dry_run_of_an_mcp_function_renders() {
    let fx = Fx::new();
    fx.install_pinned();
    let (v, ok) = play_call(
        &fx.root,
        CONNECTOR,
        Some(ACCOUNT),
        "search_issues",
        &json!({ "query": "bug" }),
        true,
        false,
    );
    assert!(ok, "{v}");
    assert_eq!(v["dry_run"], json!(true), "{v}");
    assert_eq!(v["tool"], json!("search_issues"), "{v}");
    assert_eq!(v["args"], json!(["--workspace", "acme"]), "{v}");
    fx.assert_not_started();
}

/// An eval run's doubles answer an mcp function by the existing mechanism:
/// no server starts and no tool is verified (design D5 "Eval doubles").
#[test]
fn an_mcp_function_is_answered_by_its_double() {
    use apb_core::connector::resolve::resolve_playbook;
    use apb_core::eval_doubles::Doubles;
    use apb_core::schema::Playbook;
    use apb_engine::connector::run::snapshot_connectors;
    use apb_engine::eval::doubles::{create_capture, read_calls};
    use apb_engine::run_config::{EvalRunSettings, RunConfig, write_run_config};

    let fx = Fx::new();
    let (tree, account) = fx.install(&format!("{{{{env.{TOKEN_VAR}}}}}"));
    PinStore::record(
        CONNECTOR,
        ACCOUNT,
        server_pin(&tree, &account, &fx.program, &tools()),
    )
    .unwrap();
    let playbook = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: agent_task, prompt: hi, profile: x, connectors: [{ name: tracker-mcp }] }\nedges: []\n";
    let pb = Playbook::from_yaml(playbook).unwrap();
    let out = resolve_playbook(&fx.root, &pb).unwrap();
    let mut conns = BTreeMap::new();
    let mut accts = BTreeMap::new();
    for (name, r) in &out.connectors {
        conns.insert(name.clone(), r.loaded.digest.clone());
        for a in &r.accounts {
            accts.insert(format!("{name}/{}", a.name), config::account_digest(a));
        }
    }
    let run = fx.root.join(".apb/runs/doubled");
    std::fs::create_dir_all(&run).unwrap();
    let (connectors, grants) =
        snapshot_connectors(&fx.root, &run, &pb, &conns, &accts, true).unwrap();
    manifest::write(
        &run,
        &RunExecutionManifest {
            connectors,
            connector_grants: grants,
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(run.join("playbook.yaml"), playbook).unwrap();
    let doubles: Doubles = yaml_serde::from_str(
        "tracker-mcp:\n  create_comment:\n    - answer: { status: 200, body: { text: commented } }\n",
    )
    .unwrap();
    write_run_config(
        &run,
        &RunConfig {
            eval: Some(EvalRunSettings {
                doubles: Some(doubles),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let journal = |payload: EventPayload| {
        EventLog::open(&run)
            .or_else(|_| EventLog::create(&run))
            .unwrap()
            .append(payload)
            .unwrap();
    };
    journal(
        serde_json::from_value(json!({ "type": "run_provenance", "eval_doubles": true })).unwrap(),
    );
    create_capture(&run).unwrap();
    journal(EventPayload::NodeStarted {
        node: NODE.into(),
        attempt: 1,
    });
    let token = apb_core::connector::marker::new_nonce();
    journal(EventPayload::AttemptStarted {
        item: None,
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
        cloud: None,
    });
    let claim = AttemptClaim {
        item: None,
        attempt: Some(1),
        token: Some(token),
        host_task: None,
    };
    let (v, ok) = execute_with(
        CallRequest {
            run_dir: &run,
            root: &fx.root,
            node_id: NODE,
            connector: CONNECTOR,
            function: "create_comment",
            account: None,
            args: json!({ "issue": "1", "body": "hello" }),
            dry_run: false,
            full: false,
        },
        CallContext::node(Some("c1"), claim),
    );
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "text": "commented" }), "{v}");
    let calls = read_calls(&run).unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].function, "create_comment");
    let journaled: Vec<EventPayload> = read_all_lossy_tail(&run)
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .filter(|p| matches!(p, EventPayload::ConnectorCall { .. }))
        .collect();
    match &journaled[..] {
        [
            EventPayload::ConnectorCall {
                tool_digest, url, ..
            },
        ] => {
            assert_eq!(*tool_digest, None);
            assert_eq!(url, "eval-double");
        }
        other => panic!("{other:?}"),
    }
    fx.assert_not_started();
}

/// A profile in the minimal environment (the default): claude is started
/// with `--strict-mcp-config` and no MCP configuration names the granted
/// connector, so the agent never reaches the server itself. Under
/// `environment: full` claude loads the person's own MCP configuration,
/// which this test cannot see; the claim is scoped to the minimal one.
#[test]
fn an_mcp_grant_hands_the_agent_no_mcp_server() {
    use apb_core::registry::init_project;
    use apb_engine::gate::launch::{LaunchContext, LaunchKind};
    use apb_engine::scheduler::{RunOptions, run};

    let fx = Fx::new();
    fx.install_pinned();
    init_project(&fx.root).unwrap();
    let playbook = r#"
schema: 2
id: mcpgrant
name: MCP grant
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: agent_task, prompt: "do", profile: p, connectors: [{ name: tracker-mcp, functions: [search_issues] }] }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: done }
"#;
    let dir = fx.root.join(".apb/playbooks/mcpgrant/1.0.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("playbook.yaml"), playbook).unwrap();
    std::fs::write(fx.root.join(".apb/playbooks/mcpgrant/current"), "1.0.0").unwrap();
    common::seed_profile(&fx.root, "p", "claude", "haiku", &[]);
    let argv_file = fx.base().join("argv.log");
    let agent = fx.base().join("stream-agent.sh");
    write_script(
        &agent,
        &format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\necho '{{\"type\":\"system\",\"subtype\":\"init\",\"tools\":[\"Bash\"],\"mcp_servers\":[]}}'\necho '{{\"type\":\"result\",\"is_error\":false,\"result\":\"done\"}}'\n",
            argv_file.display()
        ),
    );
    std::fs::write(
        fx.cfg.join("config.yaml"),
        format!(
            "agents:\n  claude: {{ program: {}, transport: acp }}\n",
            agent.display()
        ),
    )
    .unwrap();
    let _cmd = EnvVar::remove("APB_AGENT_CMD");
    let wref = apb_core::scope::PlaybookRef {
        origin: apb_core::scope::Origin::Project { workspace_id: None },
        id: "mcpgrant".into(),
        version: None,
    };
    let permit = apb_engine::gate::check_run(
        &fx.root,
        &wref,
        true,
        false,
        &LaunchContext::dashboard(LaunchKind::Start),
    )
    .expect("the gate admits the run");
    let mut opts = RunOptions::default();
    permit.apply(&mut opts);
    let res = run(&fx.root, "mcpgrant", None, opts).expect("the run drives");
    let argv: Vec<String> = std::fs::read_to_string(&argv_file)
        .unwrap_or_else(|e| panic!("the agent ran ({e}): {:?}", res.outcome))
        .lines()
        .map(str::to_string)
        .collect();
    assert!(argv.iter().any(|a| a == "--strict-mcp-config"), "{argv:?}");
    for pair in argv.windows(2) {
        if pair[0] == "--mcp-config" {
            let config = std::fs::read_to_string(&pair[1]).unwrap_or_else(|_| pair[1].clone());
            assert!(!config.contains(CONNECTOR), "{config}");
            assert!(!config.contains("stub-mcp"), "{config}");
        }
    }
    fx.assert_not_started();
}
