//! `apb connector approve` of an MCP connector without a person (design D2
//! "Capturing pins"): every integration test runs without a terminal, so
//! both the tree and the account path are refused before anything is read,
//! resolved or started, and print nothing a later run could supply. The
//! approvals with a person are the unit tests of `connector_mcp.rs`, which
//! script the confirmations.
//!
//! The setup (a config dir, the connector installed, an account whose token
//! comes from a marker-writing helper, the stub server) is shared with the
//! listing tests of a later change.

use std::path::PathBuf;

use apb_mcp_client::testing::{StubEra, StubServer};

const CONNECTOR: &str = "tracker-mcp";
const ACCOUNT: &str = "work";

pub(crate) struct Fx {
    _tmp: tempfile::TempDir,
    pub(crate) dir: PathBuf,
    pub(crate) stub: StubServer,
}

pub(crate) fn connector_yaml(command: &str, args: &str) -> String {
    // Quoted: a path holding ` #` or `: ` stays one YAML string.
    let command = serde_json::to_string(command).unwrap();
    format!(
        "name: tracker-mcp\nversion: 0.1.0\nmcp:\n  transport: stdio\n  command: {command}\n  args: {args}\n  env:\n    TRACKER_TOKEN: \"{{{{secret.token}}}}\"\naccount_fields:\n  - {{ name: workspace, required: true }}\n  - {{ name: token, required: true, secret: true }}\nfunctions:\n  - name: whoami\n    description: Who\n    read_only: true\n    mcp: {{ tool: whoami }}\n"
    )
}

impl Fx {
    /// A project with the connector installed and an account whose token
    /// comes from a helper that leaves `helper-ran` behind.
    pub(crate) fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(dir.join("cfg")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let stub = StubServer::write(&dir.join("stub"), StubEra::Modern).unwrap();
        stub.set_tools(
            &serde_json::json!([{ "name": "whoami", "inputSchema": { "type": "object" } }]),
        )
        .unwrap();
        let fx = Fx {
            _tmp: tmp,
            dir,
            stub,
        };
        let out = fx.apb(&["init"], &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let program = std::fs::canonicalize(fx.stub.program()).unwrap();
        fx.install(&connector_yaml(
            &program.to_string_lossy(),
            "[\"--workspace\", \"{{account.workspace}}\"]",
        ));
        let helper = fx.dir.join("helper.sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\necho ran >> '{}'\nprintf 'secret-value-3301\\n'\n",
                fx.marker().display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let accounts = fx.dir.join("cfg/connector-config");
        std::fs::create_dir_all(&accounts).unwrap();
        std::fs::write(
            accounts.join(format!("{CONNECTOR}.yaml")),
            format!(
                "accounts:\n  - name: {ACCOUNT}\n    default: true\n    workspace: acme\n    token: \"{{{{cmd:{}}}}}\"\n",
                helper.display()
            ),
        )
        .unwrap();
        fx
    }

    pub(crate) fn install(&self, yaml: &str) {
        let dir = self.dir.join("cfg/connectors").join(CONNECTOR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
    }

    pub(crate) fn marker(&self) -> PathBuf {
        self.dir.join("helper-ran")
    }

    pub(crate) fn apb(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = crate::common::apb_std();
        cmd.args(args)
            .current_dir(&self.dir)
            .env("APB_CONFIG_DIR", self.dir.join("cfg"))
            .env("HOME", self.dir.join("home"))
            .env("APB_NO_REGISTRY", "1")
            .stdin(std::process::Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn approve(&self, account: bool, env: &[(&str, &str)]) -> (i32, String) {
        let mut args = vec!["connector", "approve", CONNECTOR];
        if account {
            args.extend(["--account", ACCOUNT]);
        }
        let out = self.apb(&args, env);
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// Nothing was approved, pinned, started or resolved.
    fn assert_untouched(&self) {
        let trust = self.dir.join("cfg/trust.json");
        let trusted = std::fs::read_to_string(&trust).unwrap_or_default();
        assert!(!trusted.contains(CONNECTOR), "{trusted}");
        assert!(!self.dir.join("cfg/mcp-tools.json").exists());
        assert!(!self.stub.dir.join("starts.log").exists());
        assert!(!self.marker().exists());
    }
}

fn holds_a_digest(text: &str) -> bool {
    let mut run = 0;
    for c in text.chars() {
        run = if c.is_ascii_hexdigit() { run + 1 } else { 0 };
        if run >= 16 {
            return true;
        }
    }
    text.contains("sha256:")
}

fn assert_refused(code: i32, text: &str, prefix: &str) {
    assert_eq!(code, 2, "{text}");
    assert!(text.trim_start().starts_with(prefix), "{text}");
    assert!(!holds_a_digest(text), "{text}");
    assert!(
        !text.contains("whoami") && !text.contains("stub-mcp"),
        "{text}"
    );
}

#[test]
fn approve_without_a_terminal_is_refused() {
    let fx = Fx::new();
    let (code, text) = fx.approve(true, &[]);
    assert_refused(code, &text, "mcp_approve_needs_a_person:");
    fx.assert_untouched();
}

#[test]
fn approve_tree_without_a_terminal_is_refused() {
    let fx = Fx::new();
    let (code, text) = fx.approve(false, &[]);
    assert_refused(code, &text, "mcp_approve_needs_a_person:");
    fx.assert_untouched();
}

#[test]
fn a_retry_without_a_terminal_learns_nothing() {
    let fx = Fx::new();
    let first = fx.approve(true, &[]);
    for _ in 0..2 {
        assert_eq!(fx.approve(true, &[]), first);
    }
    let tree = fx.approve(false, &[]);
    assert_eq!(fx.approve(false, &[]), tree);
    for (code, text) in [&first, &tree] {
        assert_refused(*code, text, "mcp_approve_needs_a_person:");
    }
    fx.assert_untouched();
}

#[test]
fn approve_in_a_run_step_is_refused() {
    let fx = Fx::new();
    let (code, text) = fx.approve(true, &[("APB_RUN_ID", "x")]);
    assert_refused(
        code,
        &text,
        "approving a connector is refused inside run `x`",
    );
    fx.assert_untouched();
}

#[test]
fn approve_tree_in_a_run_step_is_refused() {
    let fx = Fx::new();
    let (code, text) = fx.approve(false, &[("APB_RUN_ID", "x")]);
    assert_refused(
        code,
        &text,
        "approving a connector is refused inside run `x`",
    );
    fx.assert_untouched();
}

#[test]
fn approve_of_an_http_mcp_connector_without_a_terminal_is_refused() {
    let fx = Fx::new();
    let mcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tok = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    mcp.set_nonblocking(true).unwrap();
    tok.set_nonblocking(true).unwrap();
    let (mport, tport) = (
        mcp.local_addr().unwrap().port(),
        tok.local_addr().unwrap().port(),
    );
    fx.install(&format!(
        "name: {CONNECTOR}\nversion: 0.1.0\nmcp:\n  transport: http\n  url: \"http://127.0.0.1:{mport}/{{{{account.workspace}}}}\"\n  protocol: modern\nauth:\n  kind: oauth2_refresh\n  token_url: \"http://127.0.0.1:{tport}/token\"\n  client_id_template: client-1\n  refresh_token_template: \"{{{{secret.token}}}}\"\naccount_fields:\n  - {{ name: workspace, required: true }}\n  - {{ name: token, required: true, secret: true }}\nfunctions:\n  - name: whoami\n    description: The account the server acts as\n    read_only: true\n    mcp: {{ tool: whoami }}\n"
    ));
    for env in [&[][..], &[("APB_RUN_ID", "x")][..]] {
        for account in [true, false] {
            let (code, text) = fx.approve(account, env);
            assert_eq!(code, 2, "{text}");
            if env.is_empty() {
                assert!(
                    text.trim_start().starts_with("mcp_approve_needs_a_person:"),
                    "{text}"
                );
            } else {
                assert!(
                    text.contains("approving a connector is refused inside run `x`"),
                    "{text}"
                );
            }
            assert!(!holds_a_digest(&text), "{text}");
        }
    }
    fx.assert_untouched();
    assert!(mcp.accept().is_err(), "the MCP endpoint saw nothing");
    assert!(tok.accept().is_err(), "the token endpoint saw nothing");
}

#[test]
fn an_escape_sequence_in_mcp_args_is_refused_at_install() {
    let fx = Fx::new();
    let program = std::fs::canonicalize(fx.stub.program()).unwrap();
    let src = fx.dir.join("src").join(CONNECTOR);
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("connector.yaml"),
        connector_yaml(&program.to_string_lossy(), "[\"--x=\\e[2J\"]"),
    )
    .unwrap();
    let out = fx.apb(
        &[
            "connector",
            "install",
            "--from-dir",
            &src.to_string_lossy(),
            "--force",
        ],
        &[],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("mcp `args[0]` holds a control character"),
        "{text}"
    );
}

#[test]
fn approve_of_an_http_connector_needs_no_terminal_outside_a_step() {
    let fx = Fx::new();
    let dir = fx.dir.join("cfg/connectors/widget");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        "name: widget\nversion: 0.1.0\naccount_fields:\n  - { name: base_url, required: true }\nfunctions:\n  - name: ping\n    description: p\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/ping\"\n",
    )
    .unwrap();
    let accounts = fx.dir.join(".apb/connector-config");
    std::fs::create_dir_all(&accounts).unwrap();
    std::fs::write(
        accounts.join("widget.yaml"),
        "accounts:\n  - name: default\n    base_url: https://example.com\n",
    )
    .unwrap();
    // Inside a run step every connector kind is refused first, before
    // anything is read (the one-point-oh-fixes step refusal).
    let step = [("APB_RUN_ID", "x")];
    for args in [
        &["connector", "approve", "widget"][..],
        &["connector", "approve", "widget", "--account", "default"][..],
    ] {
        let out = fx.apb(args, &step);
        assert_eq!(out.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("approving a connector is refused inside run `x`"),
            "{stderr}"
        );
    }
    // Outside a run step an HTTP connector still approves without a
    // terminal: the MCP terminal rule does not reach it.
    let out = fx.apb(&["connector", "approve", "widget"], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = fx.apb(
        &["connector", "approve", "widget", "--account", "default"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"approved\": \"widget/default\""),
        "{stdout}"
    );
}

// --- the listing surfaces (mcp-http-and-surfaces design D5) ---------------

/// Runs `body` alone in a child process: the approvals and pins a test
/// writes go through `apb_core`, which reads the config dir from the
/// process environment.
fn in_child(name: &str, body: impl FnOnce()) {
    apb_core::process_mode::testing::in_plain_child(
        &format!("connector_mcp_cli_test::{name}"),
        body,
    );
}

/// The one tool of the stub, with a description.
fn whoami(description: &str) -> serde_json::Value {
    serde_json::json!({ "name": "whoami", "description": description, "inputSchema": { "type": "object" } })
}

impl Fx {
    /// Points this process at the fixture's config dir.
    fn use_config_dir(&self) {
        // SAFETY: a child process runs this one test on one thread.
        unsafe { std::env::set_var("APB_CONFIG_DIR", self.dir.join("cfg")) };
    }

    /// The tree and `account` approved, as an approval records them before
    /// the pins.
    fn approve_trust(&self, account: &str) {
        use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
        self.use_config_dir();
        let tree = apb_core::connector::store::load(CONNECTOR).unwrap().digest;
        let mut trust = TrustStore::load();
        trust
            .approve_kind(
                &tree,
                CONNECTOR,
                Kind::Connector,
                OriginKind::LocallyApproved,
            )
            .unwrap();
        trust
            .approve_kind(
                &self.account_digest(account),
                &account_trust_id(CONNECTOR, account),
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            )
            .unwrap();
    }

    fn account_digest(&self, account: &str) -> String {
        let accounts = apb_core::connector::config::load_merged(&self.dir, CONNECTOR).unwrap();
        apb_core::connector::config::account_digest(
            accounts.iter().find(|a| a.name == account).unwrap(),
        )
    }

    /// The approval of `account` with its pins of `tools`, written as
    /// `pin::record` writes it (no test can pass the terminal confirmation).
    fn pin(&self, account: &str, tools: &serde_json::Value) {
        use apb_core::connector::mcp::digest::{pin_digest, tool_digest};
        use apb_core::connector::mcp::pins::{PinStore, ServerPin, ToolPin};
        self.approve_trust(account);
        let tree = apb_core::connector::store::load(CONNECTOR).unwrap().digest;
        let account_digest = self.account_digest(account);
        let program = std::fs::canonicalize(self.stub.program()).unwrap();
        let program_s = program.to_string_lossy().to_string();
        let program_digest = apb_core::content::sha256_hex(&std::fs::read(&program).unwrap());
        let pins: std::collections::BTreeMap<String, ToolPin> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["name"].as_str().unwrap().to_string(),
                    ToolPin {
                        digest: tool_digest(t),
                        description: t["description"].as_str().unwrap_or_default().to_string(),
                        members: Default::default(),
                    },
                )
            })
            .collect();
        let digests = pins
            .iter()
            .map(|(k, v)| (k.clone(), v.digest.clone()))
            .collect();
        PinStore::record(
            CONNECTOR,
            account,
            ServerPin {
                connector_digest: tree.clone(),
                account_digest: account_digest.clone(),
                command: program_s.clone(),
                pin_digest: pin_digest(
                    &tree,
                    &account_digest,
                    &program_s,
                    &program_digest,
                    &digests,
                ),
                program: program_s,
                program_digest,
                tools: pins,
                approved_at_ms: 1,
            },
        )
        .unwrap();
    }

    /// `apb connector tools` with `extra` arguments: the code, stdout and
    /// stderr.
    fn tools(&self, extra: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let mut args = vec!["connector", "tools", CONNECTOR];
        args.extend_from_slice(extra);
        let out = self.apb(&args, env);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Nothing was pinned, started or resolved (the approvals a test wrote
    /// itself are left alone).
    fn assert_nothing_started(&self) {
        assert!(!self.dir.join("cfg/mcp-tools.json").exists());
        assert!(!self.stub.dir.join("starts.log").exists());
        assert!(!self.marker().exists());
    }
}

#[test]
fn tools_exits_0_when_all_pinned() {
    in_child("tools_exits_0_when_all_pinned", || {
        let fx = Fx::new();
        let tools = serde_json::json!([whoami("Who")]);
        fx.stub.set_tools(&tools).unwrap();
        fx.pin(ACCOUNT, &tools);
        let (code, out, err) = fx.tools(&["--account", ACCOUNT], &[]);
        assert_eq!(code, 0, "{out}{err}");
        assert!(
            out.contains("connector tracker-mcp account work ("),
            "{out}"
        );
        assert!(out.contains("whoami  whoami  pinned"), "{out}");
        assert!(!holds_a_digest(&out), "{out}");
        assert!(!out.contains("apb connector approve"), "{out}");
        assert!(!out.contains("approves changed tools"), "{out}");
        assert_eq!(fx.stub.starts().len(), 1);
    });
}

#[test]
fn tools_lists_each_row_and_exits_3_when_changed() {
    in_child("tools_lists_each_row_and_exits_3_when_changed", || {
        let fx = Fx::new();
        let tools = serde_json::json!([whoami("Who")]);
        fx.stub.set_tools(&tools).unwrap();
        fx.pin(ACCOUNT, &tools);
        fx.stub
            .set_tools(&serde_json::json!([whoami("Who now")]))
            .unwrap();
        let (code, out, err) = fx.tools(&["--account", ACCOUNT], &[]);
        let text = format!("{out}{err}");
        assert_eq!(code, 3, "{text}");
        assert!(out.contains("whoami  whoami  changed"), "{out}");
        assert!(out.contains("pinned: Who"), "{out}");
        assert!(out.contains("now: Who now"), "{out}");
        assert!(
            out.contains("a person approves changed tools again at a terminal (docs/CONNECTORS.md, MCP servers)"),
            "{out}"
        );
        assert!(!text.contains("apb connector approve"), "{text}");
        assert!(!holds_a_digest(&text), "{text}");
    });
}

#[test]
fn tools_json_carries_no_digest() {
    in_child("tools_json_carries_no_digest", || {
        let fx = Fx::new();
        let tools = serde_json::json!([whoami("Who")]);
        fx.stub.set_tools(&tools).unwrap();
        fx.pin(ACCOUNT, &tools);
        let (code, out, err) = fx.tools(&["--account", ACCOUNT, "--json"], &[]);
        assert_eq!(code, 0, "{out}{err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["connector"], CONNECTOR);
        assert_eq!(v["account"], ACCOUNT);
        let rows = v["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["tool"], "whoami");
        assert_eq!(rows[0]["status"], "pinned");
        assert!(!holds_a_digest(&out), "{out}");
    });
}

/// No record of the account: refused before any secret is resolved or
/// anything starts. The tree and the account are trusted here (the record
/// is what is missing), so `assert_untouched`'s trust check does not apply.
#[test]
fn tools_of_an_unapproved_account_starts_nothing() {
    in_child("tools_of_an_unapproved_account_starts_nothing", || {
        let fx = Fx::new();
        fx.approve_trust(ACCOUNT);
        let (code, out, err) = fx.tools(&["--account", ACCOUNT], &[]);
        let text = format!("{out}{err}");
        assert_eq!(code, 2, "{text}");
        assert!(text.contains("mcp_server_unpinned:"), "{text}");
        assert!(!text.contains("apb connector approve"), "{text}");
        fx.assert_nothing_started();
    });
}

#[test]
fn tools_in_a_run_step_is_refused() {
    let fx = Fx::new();
    let (code, out, err) = fx.tools(&["--account", ACCOUNT], &[("APB_RUN_ID", "x")]);
    let text = format!("{out}{err}");
    assert_eq!(code, 2, "{text}");
    assert!(
        text.contains("listing an MCP connector's tools is refused inside run `x`"),
        "{text}"
    );
    fx.assert_untouched();
}

#[test]
fn tools_on_a_connector_without_mcp_exits_2() {
    let fx = Fx::new();
    let dir = fx.dir.join("cfg/connectors/widget");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("connector.yaml"),
        "name: widget\nversion: 0.1.0\naccount_fields:\n  - { name: base_url, required: true }\nfunctions:\n  - name: ping\n    description: p\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/ping\"\n",
    )
    .unwrap();
    let out = fx.apb(&["connector", "tools", "widget"], &[]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(
        text.contains("connector `widget` has no MCP server"),
        "{text}"
    );
    fx.assert_untouched();
}

#[test]
fn doctor_shows_pin_rows() {
    in_child("doctor_shows_pin_rows", || {
        let fx = Fx::new();
        // A second account, never approved.
        let path = fx
            .dir
            .join("cfg/connector-config")
            .join(format!("{CONNECTOR}.yaml"));
        let mut accounts = std::fs::read_to_string(&path).unwrap();
        accounts
            .push_str("  - name: other\n    workspace: beta\n    token: \"{{env.OTHER_TOKEN}}\"\n");
        std::fs::write(&path, accounts).unwrap();
        let tools = serde_json::json!([whoami("Who")]);
        fx.stub.set_tools(&tools).unwrap();
        fx.pin(ACCOUNT, &tools);
        let out = fx.apb(&["connector", "doctor"], &[]);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let line = |account: &str| {
            text.lines()
                .find(|l| l.contains(&format!("connector `{CONNECTOR}`: mcp tools ({account})")))
                .unwrap_or_else(|| panic!("no row for {account}: {text}"))
                .to_string()
        };
        let work = line(ACCOUNT);
        assert!(work.starts_with("[ok]"), "{work}");
        assert!(work.contains("every tool approved"), "{work}");
        let other = line("other");
        assert!(other.starts_with("[warn]"), "{other}");
        assert!(
            other.contains("not approved for this definition and account: whoami"),
            "{other}"
        );
        assert!(!text.contains("apb connector approve"), "{text}");
        assert!(!fx.stub.dir.join("starts.log").exists());
        assert!(!fx.marker().exists());
    });
}

#[test]
fn doctor_starts_no_server() {
    let fx = Fx::new();
    let out = fx.apb(&["connector", "doctor"], &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("connector `{CONNECTOR}`: mcp tools ({ACCOUNT})")),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "[ok]   connector `{CONNECTOR}`: mcp transport: stdio"
        )),
        "{text}"
    );
    fx.assert_untouched();
}

/// A broken account file of an MCP connector hides neither its transport
/// nor a broken pin store.
#[test]
fn doctor_reads_the_pins_when_the_accounts_do_not_load() {
    let fx = Fx::new();
    let path = fx
        .dir
        .join("cfg/connector-config")
        .join(format!("{CONNECTOR}.yaml"));
    std::fs::write(&path, "accounts: [ not: valid: yaml\n").unwrap();
    std::fs::write(fx.dir.join("cfg/mcp-tools.json"), "{ not json").unwrap();
    let out = fx.apb(&["connector", "doctor"], &[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("connector `{CONNECTOR}`: mcp transport: stdio")),
        "{text}"
    );
    assert!(text.contains("[fail] mcp tool pins:"), "{text}");
}
