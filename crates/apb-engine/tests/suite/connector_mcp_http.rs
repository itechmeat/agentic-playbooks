//! The call path of an MCP connector reached over Streamable HTTP (design
//! D7 "The engine", "Settlement of an HTTP write", "The four orders over
//! HTTP"): the endpoint and the token URL are rendered and checked before
//! any secret, the endpoint digest is compared in the one pin comparison,
//! the authorization is the connector-level `auth` (a header, or a bearer
//! minted through an agent with no proxy, refreshed once after a 401 while
//! connecting), and every failure of a write after the hand-over settles
//! `unknown`.
//!
//! The MCP endpoint and the token endpoint are `common::spawn_script`
//! servers on `127.0.0.1`, one scripted answer per request, so a request
//! nobody scripted fails to connect. The pin record is written with
//! `PinStore::record` of a hand-built `ServerPin` whose `program` is the
//! rendered endpoint and `program_digest` its endpoint digest. Every test
//! takes `common::env_lock()`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use apb_core::connector::config;
use apb_core::connector::mcp::digest::{endpoint_digest, pin_digest, tool_digest};
use apb_core::connector::mcp::pins::{PinStore, ServerPin, ToolPin};
use apb_core::connector::store;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::connector::call::{
    AttemptClaim, CallContext, CallRequest, execute_with, healthcheck,
};
use apb_engine::event::{EventPayload, read_all_lossy_tail};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{self, EnvVar, Reply, ScriptedHttpServer, request_json, spawn_script};
use crate::connector_mcp::{error_code, error_message, tool_of, tools, write_script};

const CONNECTOR: &str = "tracker-mcp";
const NODE: &str = "n";
const ACCOUNT: &str = "work";
const TOKEN_VAR: &str = "APB_MCP_HTTP_TEST_TOKEN";
const TOKEN: &str = "mcp-http-secret-5151";
const TREE: &str = "sha256:tree-http";
const ACCOUNT_DIGEST: &str = "sha256:account-http";
const FUNCTIONS: &[&str] = &[
    "whoami",
    "search_issues",
    "list_comments",
    "create_comment",
    "post_note",
];

/// The header auth block.
const HEADER: &str =
    "auth:\n  kind: header\n  header: X-Api-Key\n  value_template: \"{{secret.token}}\"\n";

/// The oauth2_refresh auth block: the token endpoint on `token_host`.
const OAUTH: &str = "auth:\n  kind: oauth2_refresh\n  token_url: \"http://{{account.token_host}}/token\"\n  client_id_template: \"{{account.client_id}}\"\n  refresh_token_template: \"{{secret.token}}\"\n";

/// The connector over http with `auth` and a function timeout of `timeout`
/// seconds.
fn connector_yaml(auth: &str, timeout: u64) -> String {
    format!(
        r#"name: tracker-mcp
version: 0.1.0
healthcheck: whoami
mcp:
  transport: http
  url: "http://127.0.0.1:{{{{account.port}}}}/mcp"
  protocol: modern
{auth}account_fields:
  - {{ name: port, required: true }}
  - {{ name: token_host }}
  - {{ name: client_id }}
  - {{ name: token, required: true, secret: true }}
identity: {{ function: whoami, path: id }}
functions:
  - name: whoami
    description: The account the server acts as
    read_only: true
    timeout_sec: {timeout}
    mcp: {{ tool: whoami }}
  - name: search_issues
    description: Search issues by text
    read_only: true
    timeout_sec: {timeout}
    mcp: {{ tool: search_issues }}
  - name: list_comments
    description: Recent comments of one issue
    read_only: true
    timeout_sec: {timeout}
    mcp: {{ tool: list_comments }}
    args_schema:
      type: object
      properties: {{ issue: {{ type: string }}, since: {{ type: string }} }}
      required: [issue]
  - name: create_comment
    description: Add a comment to an issue
    timeout_sec: {timeout}
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
  - name: post_note
    description: Post a note
    timeout_sec: {timeout}
    mcp: {{ tool: post_note }}
    idempotency: {{ mode: none }}
"#
    )
}

/// The stdio tests' tools plus the `post_note` tool.
fn http_tools() -> Value {
    let mut t = tools();
    t.as_array_mut().unwrap().push(json!({
        "name": "post_note",
        "description": "Post a note",
        "inputSchema": { "type": "object" }
    }));
    t
}

/// One JSON-RPC answer for the request just read, its id echoed.
fn rpc(result: Value) -> Reply {
    Reply::From(Box::new(move |seen: &[String]| {
        let id = request_json(seen.last().unwrap())["id"].clone();
        (
            200,
            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        )
    }))
}

fn discover() -> Reply {
    rpc(json!({
        "resultType": "complete",
        "supportedVersions": ["2026-07-28"],
        "capabilities": { "tools": {} },
        "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "scripted", "version": "1" } },
        "ttlMs": 0,
        "cacheScope": "private"
    }))
}

fn listing_of(tools: Value) -> Reply {
    rpc(json!({ "tools": tools, "resultType": "complete", "ttlMs": 0, "cacheScope": "private" }))
}

fn listing() -> Reply {
    listing_of(http_tools())
}

fn answered(text: &str) -> Reply {
    rpc(json!({ "content": [{ "type": "text", "text": text }], "resultType": "complete" }))
}

fn status(code: u16, body: &str) -> Reply {
    Reply::RespondWith(
        code,
        vec![("Content-Type".into(), "text/plain".into())],
        body.to_string(),
    )
}

/// A token endpoint answer: `access`, and a rotated refresh token.
fn token(access: &str, rotated: Option<&str>) -> Reply {
    let mut body = json!({ "access_token": access, "token_type": "Bearer", "expires_in": 3600 });
    if let Some(r) = rotated {
        body["refresh_token"] = json!(r);
    }
    Reply::Respond(200, body.to_string())
}

/// The methods of the JSON-RPC requests a server saw.
fn methods(server: &ScriptedHttpServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|r| request_json(r)["method"].as_str().unwrap_or("").to_string())
        .collect()
}

fn header_of(raw: &str, name: &str) -> Option<String> {
    raw.split("\r\n\r\n")
        .next()
        .unwrap_or("")
        .lines()
        .find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
}

fn port_of(server: &ScriptedHttpServer) -> String {
    server.base_url.rsplit(':').next().unwrap().to_string()
}

fn host_of(server: &ScriptedHttpServer) -> String {
    server.base_url.trim_start_matches("http://").to_string()
}

/// The pin record of `tools` for `endpoint` (and `token_url`).
fn http_pin(
    tree: &str,
    account: &str,
    endpoint: &str,
    token_url: Option<&str>,
    tools: &Value,
) -> ServerPin {
    let digest = endpoint_digest(endpoint, token_url);
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
    let command = "http://127.0.0.1:{{account.port}}/mcp".to_string();
    ServerPin {
        connector_digest: tree.to_string(),
        account_digest: account.to_string(),
        pin_digest: pin_digest(tree, account, endpoint, &digest, &digests),
        command,
        program: endpoint.to_string(),
        program_digest: digest,
        tools: pins,
        approved_at_ms: 1,
    }
}

/// One run with an http mcp connector snapshot and a config dir that is
/// also the default one.
struct Hx {
    tmp: tempfile::TempDir,
    cfg: PathBuf,
    root: PathBuf,
    run: PathBuf,
    yaml: String,
    _vars: Vec<EnvVar>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Hx {
    fn new(auth: &str) -> Hx {
        Hx::timed(auth, 10)
    }

    fn timed(auth: &str, timeout: u64) -> Hx {
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
        std::fs::write(
            root.join(".apb/secrets.env"),
            format!("{TOKEN_VAR}={TOKEN}\n"),
        )
        .unwrap();
        Hx {
            tmp,
            cfg,
            root,
            run,
            yaml: connector_yaml(auth, timeout),
            _vars: vars,
            _lock: lock,
        }
    }

    fn base(&self) -> PathBuf {
        std::fs::canonicalize(self.tmp.path()).unwrap()
    }

    /// The account fields: the MCP endpoint's port, the token endpoint's
    /// host and the token from the project's secrets.
    fn fields(
        &self,
        mcp: &ScriptedHttpServer,
        token: Option<&ScriptedHttpServer>,
    ) -> BTreeMap<String, String> {
        let mut f = BTreeMap::from([
            ("port".to_string(), port_of(mcp)),
            ("client_id".to_string(), "client-1".to_string()),
            ("token".to_string(), format!("{{{{env.{TOKEN_VAR}}}}}")),
        ]);
        if let Some(t) = token {
            f.insert("token_host".to_string(), host_of(t));
        }
        f
    }

    fn account(&self, fields: BTreeMap<String, String>) -> ManifestAccount {
        ManifestAccount {
            name: ACCOUNT.to_string(),
            default: true,
            fields,
            env: BTreeMap::from([("token".to_string(), TOKEN_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: ACCOUNT_DIGEST.to_string(),
        }
    }

    /// The account with its token from a `{{cmd:...}}` helper that leaves
    /// `helper-ran` behind when it runs.
    fn helper_account(&self, mut fields: BTreeMap<String, String>) -> ManifestAccount {
        let helper = self.base().join("helper.sh");
        write_script(
            &helper,
            &format!(
                "#!/bin/sh\necho ran >> '{}'\nprintf '{TOKEN}\\n'\n",
                self.marker().display()
            ),
        );
        fields.insert(
            "token".to_string(),
            format!("{{{{cmd:{}}}}}", helper.display()),
        );
        ManifestAccount {
            env: BTreeMap::new(),
            cmd: BTreeMap::from([("token".to_string(), helper.to_string_lossy().to_string())]),
            ..self.account(fields)
        }
    }

    fn marker(&self) -> PathBuf {
        self.base().join("helper-ran")
    }

    fn seed(&self, account: ManifestAccount) {
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
                functions: FUNCTIONS.iter().map(|s| s.to_string()).collect(),
                max_calls: None,
                on_unknown: None,
            }],
        );
        let _ = std::fs::remove_file(self.run.join("manifest.yaml"));
        manifest::write(&self.run, &m).unwrap();
        let cdir = self.run.join("connectors");
        std::fs::create_dir_all(&cdir).unwrap();
        std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), &self.yaml).unwrap();
    }

    fn endpoint(mcp: &ScriptedHttpServer) -> String {
        format!("{}/mcp", mcp.base_url)
    }

    fn token_url(token: &ScriptedHttpServer) -> String {
        format!("{}/token", token.base_url)
    }

    /// Seeds the run and pins the server under the snapshot's digests.
    fn pinned(&self, mcp: &ScriptedHttpServer, token: Option<&ScriptedHttpServer>) {
        self.seed(self.account(self.fields(mcp, token)));
        self.pin(mcp, token);
    }

    fn pin(&self, mcp: &ScriptedHttpServer, token: Option<&ScriptedHttpServer>) {
        let tu = token.map(Hx::token_url);
        PinStore::record(
            CONNECTOR,
            ACCOUNT,
            http_pin(
                TREE,
                ACCOUNT_DIGEST,
                &Hx::endpoint(mcp),
                tu.as_deref(),
                &http_tools(),
            ),
        )
        .unwrap();
    }

    fn request(&self, function: &'static str, args: Value) -> CallRequest<'_> {
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

    fn call(&self, function: &'static str, args: Value) -> (Value, bool) {
        execute_with(
            self.request(function, args),
            CallContext::node(None, AttemptClaim::default()),
        )
    }

    fn call_labelled(&self, function: &'static str, args: Value, label: &str) -> (Value, bool) {
        execute_with(
            self.request(function, args),
            CallContext::node(Some(label), AttemptClaim::default()),
        )
    }

    fn search(&self) -> (Value, bool) {
        self.call("search_issues", json!({ "query": "bug" }))
    }

    fn comment(&self) -> (Value, bool) {
        self.call(
            "create_comment",
            json!({ "issue": "42", "body": "Triage." }),
        )
    }

    fn events(&self) -> Vec<EventPayload> {
        read_all_lossy_tail(&self.run)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.payload)
            .collect()
    }

    fn settlements(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|p| match p {
                EventPayload::SendIntentSettled { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect()
    }

    fn call_outcomes(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|p| match p {
                EventPayload::ConnectorCall { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect()
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.run.join("events.jsonl")).unwrap_or_default()
    }

    /// Installs the connector, writes the global account file with
    /// `fields` (plus `token`) and approves both; pins the server for the
    /// installed tree and the merged account.
    fn install(
        &self,
        fields: &[(&str, String)],
        token: &str,
        mcp: &ScriptedHttpServer,
        tok: Option<&ScriptedHttpServer>,
    ) {
        let dir = self.cfg.join("connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connector.yaml"), &self.yaml).unwrap();
        let path = config::global_config_path(CONNECTOR).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body =
            format!("accounts:\n  - name: {ACCOUNT}\n    default: true\n    token: \"{token}\"\n");
        for (k, v) in fields {
            body.push_str(&format!("    {k}: \"{v}\"\n"));
        }
        std::fs::write(path, body).unwrap();
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
        let tu = tok.map(Hx::token_url);
        PinStore::record(
            CONNECTOR,
            ACCOUNT,
            http_pin(
                &loaded.digest,
                &account_digest,
                &Hx::endpoint(mcp),
                tu.as_deref(),
                &http_tools(),
            ),
        )
        .unwrap();
    }
}

#[test]
fn a_header_server_gets_its_header() {
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing(), answered("found")]);
    hx.pinned(&mcp, None);
    let (v, ok) = hx.search();
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "text": "found" }));
    let reqs = mcp.requests();
    assert_eq!(reqs.len(), 3);
    for r in &reqs {
        assert_eq!(header_of(r, "x-api-key").as_deref(), Some(TOKEN));
        assert_eq!(header_of(r, "authorization"), None);
    }
}

#[test]
fn an_oauth_call_mints_and_sends_the_bearer() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-A-1", None)]);
    let mcp = spawn_script(vec![discover(), listing(), answered("found")]);
    hx.pinned(&mcp, Some(&tok));
    let (v, ok) = hx.search();
    assert!(ok, "{v}");
    let t = tok.requests();
    assert_eq!(t.len(), 1);
    assert!(t[0].contains("grant_type=refresh_token"), "{}", t[0]);
    for r in &mcp.requests() {
        assert_eq!(
            header_of(r, "authorization").as_deref(),
            Some("Bearer access-A-1")
        );
    }
}

#[test]
fn a_401_at_connect_refreshes_once() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![
        token("access-A-2", None),
        token("access-B-2", Some("refresh-R-2")),
    ]);
    let mcp = spawn_script(vec![
        status(401, ""),
        discover(),
        listing(),
        answered("found"),
    ]);
    hx.pinned(&mcp, Some(&tok));
    let (v, ok) = hx.search();
    assert!(ok, "{v}");
    assert_eq!(tok.requests().len(), 2, "two token requests");
    let reqs = mcp.requests();
    assert_eq!(
        methods(&mcp),
        vec![
            "server/discover",
            "server/discover",
            "tools/list",
            "tools/call"
        ]
    );
    assert_eq!(
        header_of(&reqs[0], "authorization").as_deref(),
        Some("Bearer access-A-2")
    );
    for r in &reqs[1..] {
        assert_eq!(
            header_of(r, "authorization").as_deref(),
            Some("Bearer access-B-2")
        );
        assert!(!r.contains("access-A-2"));
    }
    assert!(
        v["warnings"].to_string().contains("new refresh token"),
        "the rotation warning: {v}"
    );
}

#[test]
fn a_listing_that_echoes_the_refreshed_token_is_refused() {
    for echoed in ["access-B-3", "refresh-R-3"] {
        let hx = Hx::new(OAUTH);
        let tok = spawn_script(vec![
            token("access-A-3", None),
            token("access-B-3", Some("refresh-R-3")),
        ]);
        let mut listed = http_tools();
        listed[0]["description"] = json!(format!("hello {echoed}"));
        let mcp = spawn_script(vec![status(401, ""), discover(), listing_of(listed)]);
        hx.pinned(&mcp, Some(&tok));
        let (v, ok) = hx.call("whoami", json!({}));
        assert!(!ok, "{v}");
        assert!(
            error_message(&v).starts_with("mcp_tools_reflect_a_secret:"),
            "{echoed}: {v}"
        );
        assert!(!methods(&mcp).contains(&"tools/call".to_string()));
        let journal = hx.journal();
        // The messages name the position, never the token itself.
        for (i, secret) in ["access-B-3", "refresh-R-3"].iter().enumerate() {
            assert!(
                !journal.contains(secret),
                "{echoed}: token {i} in the journal"
            );
            assert!(
                !v.to_string().contains(secret),
                "{echoed}: token {i} in the result"
            );
        }
    }
}

/// The marker of the child process a proxy test runs its body in.
const CHILD: &str = "APB_MCP_HTTP_PROXY_CHILD";

#[test]
fn the_token_request_ignores_the_callers_proxy() {
    if let Ok(spec) = std::env::var(CHILD) {
        // The child: the parent's servers are named in the variable; the
        // proxy variables are set on this process only.
        let parts: Vec<&str> = spec.split('|').collect();
        let (mcp_port, tok_host) = (parts[0], parts[1]);
        let hx = Hx::new(OAUTH);
        let fields = BTreeMap::from([
            ("port".to_string(), mcp_port.to_string()),
            ("client_id".to_string(), "client-1".to_string()),
            ("token_host".to_string(), tok_host.to_string()),
            ("token".to_string(), format!("{{{{env.{TOKEN_VAR}}}}}")),
        ]);
        hx.seed(hx.account(fields));
        let endpoint = format!("http://127.0.0.1:{mcp_port}/mcp");
        let token_url = format!("http://{tok_host}/token");
        PinStore::record(
            CONNECTOR,
            ACCOUNT,
            http_pin(
                TREE,
                ACCOUNT_DIGEST,
                &endpoint,
                Some(&token_url),
                &http_tools(),
            ),
        )
        .unwrap();
        let (v, ok) = hx.search();
        assert!(ok, "{v}");
        return;
    }
    let tok = spawn_script(vec![token("access-P-1", None)]);
    let mcp = spawn_script(vec![discover(), listing(), answered("found")]);
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "connector_mcp_http::the_token_request_ignores_the_callers_proxy",
            "--nocapture",
        ])
        .env(CHILD, format!("{}|{}", port_of(&mcp), host_of(&tok)))
        .env("HTTPS_PROXY", &proxy_url)
        .env("HTTP_PROXY", &proxy_url)
        .env("https_proxy", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .status()
        .unwrap();
    assert!(status.success(), "the child's body passed");
    assert_eq!(
        tok.requests().len(),
        1,
        "the token endpoint saw its request"
    );
    assert!(proxy.accept().is_err(), "the proxy saw no connection");
}

#[test]
fn a_401_on_the_call_settles_unknown() {
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing(), status(401, "")]);
    hx.pinned(&mcp, None);
    let args = json!({ "issue": "42", "body": "note" });
    let (v, ok) = hx.call_labelled("post_note", args.clone(), "note-1");
    assert!(!ok, "{v}");
    assert_eq!(hx.settlements(), vec!["unknown".to_string()]);
    assert_eq!(hx.call_outcomes(), vec!["tool_error".to_string()]);
    let (again, ok) = hx.call_labelled("post_note", args, "note-1");
    assert!(!ok);
    assert_eq!(error_code(&again), "unknown_effect", "{again}");
    assert_eq!(
        methods(&mcp).iter().filter(|m| *m == "tools/call").count(),
        1
    );
}

#[test]
fn every_failure_of_an_http_write_after_the_hand_over_settles_unknown() {
    let cut_stream = Reply::RespondWith(
        200,
        vec![("Content-Type".into(), "text/event-stream".into())],
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n\n".into(),
    );
    let rpc_error = Reply::From(Box::new(|seen: &[String]| {
        let id = request_json(seen.last().unwrap())["id"].clone();
        (
            500,
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "bad" } })
                .to_string(),
        )
    }));
    let (tx, held) = std::sync::mpsc::channel::<()>();
    let (release_tx, release) = std::sync::mpsc::channel::<()>();
    let mut release_tx = Some(release_tx);
    let deadline = Reply::Held {
        body: String::new(),
        arrived: tx,
        release,
    };
    let rows: Vec<(&str, Reply, &str, u64)> = vec![
        ("401", status(401, ""), "tool_error", 10),
        ("403", status(403, ""), "tool_error", 10),
        ("500 json-rpc", rpc_error, "tool_error", 10),
        ("500 plain", status(500, "oops"), "network", 10),
        ("cut stream", cut_stream, "network", 10),
        ("deadline", deadline, "timeout", 2),
    ];
    for (name, reply, outcome, timeout) in rows {
        let hx = Hx::timed(HEADER, timeout);
        let mcp = spawn_script(vec![discover(), listing(), reply]);
        hx.pinned(&mcp, None);
        let (v, ok) = hx.comment();
        assert!(!ok, "{name}: {v}");
        assert_eq!(hx.settlements(), vec!["unknown".to_string()], "{name}: {v}");
        assert_eq!(hx.call_outcomes(), vec![outcome.to_string()], "{name}: {v}");
        if name == "deadline" {
            // The held answer is let go, so the server's thread ends now.
            drop(release_tx.take());
        }
    }
    assert!(held.try_recv().is_ok(), "the deadline row's call arrived");
}

#[test]
fn a_changed_account_url_sends_nothing() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-C-1", None)]);
    let mcp = spawn_script(vec![discover(), listing(), answered("x")]);
    let other = spawn_script(vec![discover(), listing(), answered("x")]);
    hx.pin(&mcp, Some(&tok));
    // The account now names another server than the one approved.
    hx.seed(hx.account(hx.fields(&other, Some(&tok))));
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_server_changed:"), "{v}");
    assert!(error_message(&v).contains("endpoint changed"), "{v}");
    assert!(tok.requests().is_empty() && mcp.requests().is_empty() && other.requests().is_empty());
}

#[test]
fn a_url_with_a_query_is_refused() {
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing(), answered("x")]);
    hx.pin(&mcp, None);
    let mut fields = hx.fields(&mcp, None);
    fields.insert("port".into(), format!("{}/mcp?tenant=1", port_of(&mcp)));
    hx.seed(hx.account(fields));
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "config", "{v}");
    assert!(error_message(&v).contains("query"), "{v}");
    assert!(mcp.requests().is_empty());
}

#[test]
fn a_refused_endpoint_starts_no_secret_helper() {
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing(), answered("x")]);
    let mut fields = hx.fields(&mcp, None);
    fields.insert("port".into(), format!("{}/mcp?x=1", port_of(&mcp)));
    hx.seed(hx.helper_account(fields.clone()));
    hx.pin(&mcp, None);
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "config", "{v}");
    // `list_approved` over the installed connector, the same refusal.
    let helper = hx.base().join("helper.sh");
    hx.install(
        &[("port", format!("{}/mcp?x=1", port_of(&mcp)))],
        &format!("{{{{cmd:{}}}}}", helper.display()),
        &mcp,
        None,
    );
    let listed = apb_engine::connector::mcp::pin::list_approved(
        &hx.root,
        CONNECTOR,
        Some(ACCOUNT),
        apb_engine::decision_channel::DecisionChannel::Terminal,
        Instant::now() + Duration::from_secs(10),
    );
    match listed {
        Err(e) => assert_eq!(e.code.as_str(), "config", "{}", e.message),
        Ok(_) => panic!("the listing was not refused"),
    }
    assert!(!hx.marker().exists(), "the helper never ran");
    assert!(mcp.requests().is_empty());
}

#[test]
fn a_refused_token_url_starts_no_secret_helper() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-D-1", None)]);
    let mcp = spawn_script(vec![discover(), listing(), answered("x")]);
    let mut fields = hx.fields(&mcp, Some(&tok));
    // A plaintext token URL on the name `localhost`.
    fields.insert("token_host".into(), format!("localhost:{}", port_of(&tok)));
    hx.seed(hx.helper_account(fields));
    hx.pin(&mcp, Some(&tok));
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert_eq!(error_code(&v), "config", "{v}");
    assert!(error_message(&v).contains("token_url"), "{v}");
    let helper = hx.base().join("helper.sh");
    hx.install(
        &[
            ("port", port_of(&mcp)),
            ("client_id", "client-1".into()),
            ("token_host", format!("localhost:{}", port_of(&tok))),
        ],
        &format!("{{{{cmd:{}}}}}", helper.display()),
        &mcp,
        Some(&tok),
    );
    let listed = apb_engine::connector::mcp::pin::list_approved(
        &hx.root,
        CONNECTOR,
        Some(ACCOUNT),
        apb_engine::decision_channel::DecisionChannel::Terminal,
        Instant::now() + Duration::from_secs(10),
    );
    match listed {
        Err(e) => assert_eq!(e.code.as_str(), "config", "{}", e.message),
        Ok(_) => panic!("the listing was not refused"),
    }
    assert!(!hx.marker().exists(), "the helper never ran");
    assert!(mcp.requests().is_empty() && tok.requests().is_empty());
}

#[test]
fn a_changed_execution_member_refuses_the_next_http_call() {
    let hx = Hx::new(HEADER);
    let mut changed = http_tools();
    changed[1]["execution"] = json!({ "taskSupport": "optional" });
    let mcp = spawn_script(vec![
        discover(),
        listing(),
        answered("first"),
        discover(),
        listing_of(changed),
    ]);
    hx.pinned(&mcp, None);
    let (v, ok) = hx.search();
    assert!(ok, "{v}");
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_tool_changed:"), "{v}");
    let m = methods(&mcp);
    assert_eq!(m.iter().filter(|m| *m == "tools/call").count(), 1, "{m:?}");
    assert_eq!(m.last().map(String::as_str), Some("tools/list"));
}

#[test]
fn the_token_never_reaches_the_journal_or_the_result() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-T-9", Some("refresh-T-9"))]);
    let echo = Reply::From(Box::new(|_| {
        (
            500,
            "access-T-9 refresh-T-9 mcp-http-secret-5151".to_string(),
        )
    }));
    let mcp = spawn_script(vec![discover(), listing(), echo]);
    hx.pinned(&mcp, Some(&tok));
    let (v, ok) = hx.comment();
    assert!(!ok, "{v}");
    let shown = v.to_string();
    let journal = hx.journal();
    for (i, secret) in ["access-T-9", "refresh-T-9", TOKEN].iter().enumerate() {
        assert!(!shown.contains(secret), "token {i} in the result");
        assert!(!journal.contains(secret), "token {i} in the journal");
    }
}

#[test]
fn a_listing_that_reflects_the_access_token_is_refused() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-R-5", None)]);
    let mut listed = http_tools();
    listed[1]["description"] = json!("token access-R-5");
    let mcp = spawn_script(vec![discover(), listing_of(listed)]);
    hx.pinned(&mcp, Some(&tok));
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert!(
        error_message(&v).starts_with("mcp_tools_reflect_a_secret:"),
        "{v}"
    );
    assert!(!v.to_string().contains("access-R-5"));
}

#[test]
fn the_endpoint_is_journaled_without_secrets() {
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing(), answered("found")]);
    hx.pinned(&mcp, None);
    let (v, ok) = hx.search();
    assert!(ok, "{v}");
    let call = hx
        .events()
        .into_iter()
        .find_map(|p| match p {
            EventPayload::ConnectorCall {
                url, tool_digest, ..
            } => Some((url, tool_digest)),
            _ => None,
        })
        .expect("a connector_call");
    assert_eq!(
        call.0,
        format!("mcp+http://127.0.0.1:{}/mcp#search_issues", port_of(&mcp))
    );
    assert_eq!(
        call.1.as_deref(),
        Some(tool_digest(&tool_of(&http_tools(), "search_issues")).as_str())
    );
    assert!(!hx.journal().contains(TOKEN));
}

#[test]
fn a_dry_run_of_an_http_function_sends_nothing() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![]);
    let mcp = spawn_script(vec![]);
    hx.pinned(&mcp, Some(&tok));
    let mut req = hx.request("search_issues", json!({ "query": "bug" }));
    req.dry_run = true;
    let (v, ok) = execute_with(req, CallContext::node(None, AttemptClaim::default()));
    assert!(ok, "{v}");
    assert_eq!(v["transport"], "http");
    assert_eq!(v["url"], json!(Hx::endpoint(&mcp)));
    assert_eq!(v["auth"], "oauth2_refresh");
    assert_eq!(v["tool"], "search_issues");
    assert!(tok.requests().is_empty() && mcp.requests().is_empty());
}

#[test]
fn a_duplicated_tool_member_never_matches_a_pin() {
    let hx = Hx::new(HEADER);
    let dup = Reply::From(Box::new(|seen: &[String]| {
        let id = request_json(seen.last().unwrap())["id"].clone();
        (
            200,
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"tools":[{{"name":"search_issues","description":"Search issues by text","inputSchema":{{"type":"object","properties":{{"query":{{"type":"string"}}}}}},"execution":{{"taskSupport":"optional"}},"execution":{{"taskSupport":"forbidden"}}}}]}}}}"#
            ),
        )
    }));
    let mcp = spawn_script(vec![discover(), dup]);
    hx.pinned(&mcp, None);
    let (v, ok) = hx.comment();
    assert!(!ok, "{v}");
    assert!(error_message(&v).contains("mcp_message_invalid"), "{v}");
    assert!(!methods(&mcp).contains(&"tools/call".to_string()));
    assert!(
        hx.settlements().is_empty()
            && hx
                .events()
                .iter()
                .all(|p| !matches!(p, EventPayload::SendIntentRecorded { .. }))
    );
}

#[test]
fn a_connection_refused_on_the_call_settles_unknown() {
    let hx = Hx::new(HEADER);
    // The listener closes before the listing is answered: the call's
    // connection is refused.
    let mcp = spawn_script(vec![discover(), Reply::ThenClose(Box::new(listing()))]);
    hx.pinned(&mcp, None);
    let (v, ok) = hx.comment();
    assert!(!ok, "{v}");
    assert_eq!(hx.settlements(), vec!["unknown".to_string()], "{v}");
    assert_eq!(hx.call_outcomes(), vec!["network".to_string()], "{v}");
}

#[test]
fn an_endpoint_digest_that_differs_refuses_the_call() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-E-1", None)]);
    let mcp = spawn_script(vec![discover(), listing(), answered("x")]);
    hx.seed(hx.account(hx.fields(&mcp, Some(&tok))));
    let mut pin = http_pin(
        TREE,
        ACCOUNT_DIGEST,
        &Hx::endpoint(&mcp),
        Some(&Hx::token_url(&tok)),
        &http_tools(),
    );
    pin.program_digest = endpoint_digest("https://other.example/mcp", None);
    PinStore::record(CONNECTOR, ACCOUNT, pin).unwrap();
    let (v, ok) = hx.search();
    assert!(!ok, "{v}");
    assert!(error_message(&v).starts_with("mcp_server_changed:"), "{v}");
    assert!(
        error_message(&v).contains("the server endpoint changed"),
        "{v}"
    );
    assert!(tok.requests().is_empty() && mcp.requests().is_empty());
}

#[test]
fn the_healthcheck_of_an_http_connector_checks_the_pins() {
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing()]);
    hx.install(
        &[("port", port_of(&mcp))],
        &format!("{{{{env.{TOKEN_VAR}}}}}"),
        &mcp,
        None,
    );
    let (v, ok) = healthcheck(
        &hx.root,
        CONNECTOR,
        ACCOUNT,
        apb_engine::decision_channel::DecisionChannel::Terminal,
    );
    assert!(ok, "{v}");
    assert_eq!(v["mcp"]["tools_checked"], json!(FUNCTIONS.len()), "{v}");
    assert_eq!(methods(&mcp), vec!["server/discover", "tools/list"]);
}

#[test]
fn an_http_check_opens_no_program_file() {
    // The record's `program` is the endpoint URL, which names no file:
    // the check reads the endpoint digest, never a file, and the call is
    // answered.
    let hx = Hx::new(HEADER);
    let mcp = spawn_script(vec![discover(), listing(), answered("found")]);
    hx.pinned(&mcp, None);
    assert!(!std::path::Path::new(&Hx::endpoint(&mcp)).exists());
    let (v, ok) = hx.search();
    assert!(ok, "{v}");
    assert_eq!(v["body"], json!({ "text": "found" }));
}

#[test]
fn a_listing_of_a_rotating_oauth_account_carries_the_rotation_warning() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-L-1", Some("refresh-L-2"))]);
    let mcp = spawn_script(vec![discover(), listing()]);
    hx.install(
        &[
            ("port", port_of(&mcp)),
            ("client_id", "client-1".into()),
            ("token_host", host_of(&tok)),
        ],
        &format!("{{{{env.{TOKEN_VAR}}}}}"),
        &mcp,
        Some(&tok),
    );
    let listing = apb_engine::connector::mcp::pin::list_approved(
        &hx.root,
        CONNECTOR,
        Some(ACCOUNT),
        apb_engine::decision_channel::DecisionChannel::Terminal,
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap_or_else(|e| panic!("{}", e.message));
    let note = listing.cleanup.clone().unwrap_or_default();
    assert!(note.contains("new refresh token"), "{note}");
    assert!(!note.contains("refresh-L-2") && !note.contains("access-L-1"));
    assert_eq!(methods(&mcp), vec!["server/discover", "tools/list"]);
}

/// Installs the rotating oauth account of the rotation tests: a token
/// endpoint that answers a new refresh token, and a server that lists.
fn rotating(hx: &Hx, tok: &ScriptedHttpServer, mcp: &ScriptedHttpServer) {
    hx.install(
        &[
            ("port", port_of(mcp)),
            ("client_id", "client-1".into()),
            ("token_host", host_of(tok)),
        ],
        &format!("{{{{env.{TOKEN_VAR}}}}}"),
        mcp,
        Some(tok),
    );
}

#[test]
fn an_approval_listing_of_a_rotating_oauth_account_carries_the_rotation_warning() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-A-1", Some("refresh-A-2"))]);
    let mcp = spawn_script(vec![discover(), listing()]);
    rotating(&hx, &tok, &mcp);
    // What the person confirmed: the record's binding, as the approval's
    // first confirmation shows it.
    let record = PinStore::load()
        .unwrap()
        .server(CONNECTOR, ACCOUNT)
        .cloned()
        .unwrap();
    let confirmed = apb_engine::connector::mcp::pin::ConfirmedProgram {
        connector_digest: record.connector_digest,
        account_digest: record.account_digest,
        program: record.program,
        program_digest: record.program_digest,
    };
    let listing =
        apb_engine::connector::mcp::pin::list_live(&hx.root, CONNECTOR, ACCOUNT, &confirmed)
            .unwrap_or_else(|e| panic!("{}", e.message));
    let note = listing.cleanup.clone().unwrap_or_default();
    assert!(note.contains("new refresh token"), "{note}");
    assert!(!note.contains("refresh-A-2") && !note.contains("access-A-1"));
    assert_eq!(methods(&mcp), vec!["server/discover", "tools/list"]);
}

#[test]
fn a_healthcheck_of_a_rotating_oauth_account_carries_the_rotation_warning() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-H-1", Some("refresh-H-2"))]);
    let mcp = spawn_script(vec![discover(), listing()]);
    rotating(&hx, &tok, &mcp);
    let (v, ok) = healthcheck(
        &hx.root,
        CONNECTOR,
        ACCOUNT,
        apb_engine::decision_channel::DecisionChannel::Terminal,
    );
    assert!(ok, "{v}");
    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("new refresh token"), "{v}");
    assert!(!warnings.contains("refresh-H-2") && !warnings.contains("access-H-1"));
    assert_eq!(methods(&mcp), vec!["server/discover", "tools/list"]);
}

/// The listing a person runs twice in one process (the dashboard's): the
/// second takes the cached access token, and the refresh token the first
/// mint rotated is still registered, so a listing that reflects it is
/// refused exactly as the first was. One token request proves the hit.
#[test]
fn a_cached_token_refuses_a_listing_that_reflects_the_rotated_refresh() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-K-1", Some("refresh-K-2"))]);
    let mut listed = http_tools();
    listed[0]["description"] = json!("hello refresh-K-2");
    let mcp = spawn_script(vec![
        discover(),
        listing_of(listed.clone()),
        discover(),
        listing_of(listed),
    ]);
    rotating(&hx, &tok, &mcp);
    for round in ["minted", "cached"] {
        let err = apb_engine::connector::mcp::pin::list_approved(
            &hx.root,
            CONNECTOR,
            Some(ACCOUNT),
            apb_engine::decision_channel::DecisionChannel::Terminal,
            Instant::now() + Duration::from_secs(10),
        )
        .map(|_| ())
        .unwrap_err();
        assert!(
            err.message.starts_with("mcp_tools_reflect_a_secret:"),
            "{round}: {}",
            err.message
        );
        assert!(!err.message.contains("refresh-K-2"), "{round}");
        assert!(
            err.warnings.iter().any(|w| w.contains("new refresh token")),
            "{round}: {:?}",
            err.warnings
        );
    }
    assert_eq!(tok.requests().len(), 1, "the second listing hit the cache");
}

/// A call answer that echoes the rotated refresh token is redacted on the
/// call that minted and again on a call that takes the cached token, and
/// both carry the rotation warning.
#[test]
fn a_cached_token_redacts_the_rotated_refresh_in_a_call_answer() {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token("access-Q-1", Some("refresh-Q-2"))]);
    let mcp = spawn_script(vec![
        discover(),
        listing(),
        answered("seen refresh-Q-2"),
        discover(),
        listing(),
        answered("seen refresh-Q-2"),
    ]);
    hx.pinned(&mcp, Some(&tok));
    for round in ["minted", "cached"] {
        let (v, ok) = hx.search();
        assert!(ok, "{round}: {v}");
        let text = v["body"]["text"].as_str().unwrap_or_default().to_string();
        assert!(
            text.contains("[redacted:oauth:refresh_token]"),
            "{round}: {v}"
        );
        assert!(!v.to_string().contains("refresh-Q-2"), "{round}: {v}");
        assert!(
            v["warnings"].to_string().contains("new refresh token"),
            "{round}: {v}"
        );
    }
    assert!(!hx.journal().contains("refresh-Q-2"));
    assert_eq!(tok.requests().len(), 1, "the second call hit the cache");
}

/// Calls `search_issues` against a server scripted with `replies` after a
/// mint that rotated the refresh token, and answers the refused result.
fn refused_after_a_rotating_mint(tag: &str, replies: Vec<Reply>) -> Value {
    let hx = Hx::new(OAUTH);
    let tok = spawn_script(vec![token(
        &format!("access-{tag}-1"),
        Some(&format!("refresh-{tag}-2")),
    )]);
    let mcp = spawn_script(replies);
    hx.pinned(&mcp, Some(&tok));
    let (v, ok) = hx.search();
    assert!(!ok, "{tag}: {v}");
    assert!(!methods(&mcp).contains(&"tools/call".to_string()), "{tag}");
    assert!(
        v["warnings"].to_string().contains("new refresh token"),
        "{tag}: the rotation warning on the refusal: {v}"
    );
    for (i, secret) in [format!("access-{tag}-1"), format!("refresh-{tag}-2")]
        .iter()
        .enumerate()
    {
        assert!(
            !v.to_string().contains(secret),
            "{tag}: token {i} in the result"
        );
        assert!(!hx.journal().contains(secret), "{tag}: token {i} journaled");
    }
    v
}

#[test]
fn a_failed_discovery_after_a_rotating_mint_carries_the_warning() {
    let v = refused_after_a_rotating_mint("D", vec![status(500, "down")]);
    assert!(!error_code(&v).is_empty(), "{v}");
}

#[test]
fn a_changed_pinned_tool_after_a_rotating_mint_carries_the_warning() {
    let mut listed = http_tools();
    let at = listed
        .as_array()
        .unwrap()
        .iter()
        .position(|t| t["name"] == "search_issues")
        .unwrap();
    listed[at]["description"] = json!("Search issues, changed");
    let v = refused_after_a_rotating_mint("G", vec![discover(), listing_of(listed)]);
    assert!(error_message(&v).starts_with("mcp_tool_changed:"), "{v}");
}

#[test]
fn a_reflected_secret_after_a_rotating_mint_carries_the_warning() {
    let mut listed = http_tools();
    listed[1]["description"] = json!("token access-F-1");
    let v = refused_after_a_rotating_mint("F", vec![discover(), listing_of(listed)]);
    assert!(
        error_message(&v).starts_with("mcp_tools_reflect_a_secret:"),
        "{v}"
    );
}
