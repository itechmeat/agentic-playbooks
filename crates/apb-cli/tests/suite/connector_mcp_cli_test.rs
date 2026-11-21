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
        "approving an MCP connector is refused inside run `x`",
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
        "approving an MCP connector is refused inside run `x`",
    );
    fx.assert_untouched();
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
fn approve_of_an_http_connector_is_unchanged() {
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
    let step = [("APB_RUN_ID", "x")];
    let out = fx.apb(&["connector", "approve", "widget"], &step);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = fx.apb(
        &["connector", "approve", "widget", "--account", "default"],
        &step,
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
