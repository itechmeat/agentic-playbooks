//! The mcp connector kind: a connector whose functions call the tools of
//! an MCP server. The connector-level `mcp:` block says how apb reaches the
//! server; a function-level `mcp: { tool }` names the one tool a function
//! calls. Every effect (`read_only`, `irreversible`, `idempotency`) comes
//! from the manifest, never from the server's tool annotations.

pub mod digest;
pub mod pins;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::common::ConnectorError;

/// The connector-level `mcp:` block: how apb reaches the server. One flat
/// struct (serde's `flatten` does not work with `deny_unknown_fields`);
/// `validate_server_shape` enforces which fields go with which transport.
/// mcp-http-and-surfaces adds the http fields `url` and `auth` here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSpec {
    pub transport: McpTransportKind,
    /// Stdio only, required: a literal program, an absolute path or a bare
    /// name. No placeholder. A bare name is resolved once, by the approval
    /// at a person's terminal, to an absolute canonical path that the pin
    /// records; a call never looks it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Stdio only. Each entry may use `{{account.*}}` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Stdio only. Variable name -> template (`{{account.*}}`, `{{secret.*}}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// How the client opens the conversation; `auto` by default.
    #[serde(default, skip_serializing_if = "McpProtocol::is_auto")]
    pub protocol: McpProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    Stdio,
    /// Streamable HTTP. This apb refuses it in `validate_server_shape`.
    Http,
}

impl McpTransportKind {
    /// The transport's name as a manifest writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            McpTransportKind::Stdio => "stdio",
            McpTransportKind::Http => "http",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpProtocol {
    /// Probe with `server/discover`, fall back to `initialize`.
    #[default]
    Auto,
    /// 2026-07-28 or later only: a legacy server is an error.
    Modern,
    /// `initialize` only (skips the probe and its 10 s silence timeout on
    /// a legacy server that ignores unknown methods).
    Legacy,
}

impl McpProtocol {
    pub fn is_auto(&self) -> bool {
        *self == McpProtocol::Auto
    }
}

/// The function-level `mcp:` block: which tool a function calls.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpFunctionSpec {
    /// The tool's `name` as the server lists it (case-sensitive).
    pub tool: String,
}

/// The smallest `timeout_sec` of an mcp function under `protocol: auto`:
/// rmcp's fixed 10 s discover probe plus 5 s.
pub const MCP_AUTO_MIN_TIMEOUT_SEC: u64 = 15;

/// Program file names that run other code than their own file: launchers
/// that fetch or pick a package at each start, and interpreters that run a
/// script their arguments name (V301, and a line of the approval screen).
/// The approval pins the interpreter's or launcher's own file only, never
/// the script or package it runs.
pub const MCP_LAUNCHERS: [&str; 30] = [
    // Launchers: fetch or pick a package at each start.
    "npx", "pnpx", "bunx", "uvx", "pipx", "uv", "npm", "pnpm", "yarn", "docker", "podman", "env",
    // Interpreters and runtimes: run the script their arguments name.
    "node", "nodejs", "deno", "bun", "python", "python3", "sh", "bash", "zsh", "dash", "ksh",
    "fish", "ruby", "perl", "php", "java", "lua", "pwsh",
];

/// The file name of a stdio `command` when it is in `MCP_LAUNCHERS`; a
/// versioned interpreter name (`python3.12`, `ruby3.3`) answers its base.
pub fn launcher_name(command: &str) -> Option<&'static str> {
    let file = std::path::Path::new(command).file_name()?.to_str()?;
    if let Some(l) = MCP_LAUNCHERS.into_iter().find(|l| *l == file) {
        return Some(l);
    }
    MCP_LAUNCHERS
        .into_iter()
        .filter(|l| {
            file.strip_prefix(l).is_some_and(|v| {
                !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            })
        })
        .max_by_key(|l| l.len())
}

/// Environment variables a definition may not set on a server because
/// they change what code a process loads (the dynamic loader, an
/// interpreter's start-up and module paths): exact names. The one list;
/// `is_loader_env` also refuses every name under `LOADER_ENV_PREFIXES`.
pub const LOADER_ENV: [&str; 24] = [
    "NODE_OPTIONS",
    "NODE_PATH",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "PYTHONUSERBASE",
    "PYTHONINSPECT",
    "RUBYOPT",
    "RUBYLIB",
    "GEM_HOME",
    "GEM_PATH",
    "PERL5OPT",
    "PERL5LIB",
    "PERLLIB",
    "PERL5DB",
    "BASH_ENV",
    "ENV",
    "ZDOTDIR",
    "CLASSPATH",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "GCONV_PATH",
    "LUA_PATH",
];

/// Prefixes of the dynamic loader's variables (`LD_PRELOAD`,
/// `LD_LIBRARY_PATH`, `LD_AUDIT`, `DYLD_INSERT_LIBRARIES` and the rest).
pub const LOADER_ENV_PREFIXES: [&str; 2] = ["LD_", "DYLD_"];

/// Whether `name` changes what code a process loads (`LOADER_ENV`).
pub fn is_loader_env(name: &str) -> bool {
    LOADER_ENV.contains(&name) || LOADER_ENV_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn invalid(msg: String) -> ConnectorError {
    ConnectorError::Invalid(msg)
}

/// Whether `tool` follows the tool naming rule: 1 to 128 characters of
/// `[A-Za-z0-9_.-]`.
fn is_tool_name(tool: &str) -> bool {
    (1..=128).contains(&tool.len())
        && tool
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// What the shape rules of one mcp function read (manifest rule 15.2),
/// taken from the function by `def.rs`.
pub struct FunctionFacts<'a> {
    pub name: &'a str,
    pub spec: &'a McpFunctionSpec,
    /// The first HTTP field the function sets, if any.
    pub http_field: Option<&'static str>,
    pub read_only: bool,
    pub has_idempotency: bool,
}

/// The shape rules of one mcp function (manifest rule 15.2): a tool name
/// that follows the naming rule, no HTTP field, and a declared idempotency
/// on a function that changes something.
pub fn validate_function_shape(f: &FunctionFacts<'_>) -> Result<(), ConnectorError> {
    let name = f.name;
    if !is_tool_name(&f.spec.tool) {
        // Escaped: the value may hold a control character.
        return Err(invalid(format!(
            "mcp function `{name}`: tool name `{}` must be 1 to 128 of A-Z a-z 0-9 _ - .",
            f.spec.tool.escape_debug()
        )));
    }
    if let Some(field) = f.http_field {
        return Err(invalid(format!(
            "mcp function `{name}` must not set `{field}`; it is an HTTP field"
        )));
    }
    if !f.read_only && !f.has_idempotency {
        return Err(invalid(format!(
            "mcp function `{name}` changes something and must declare idempotency (`marker`, `none` or `repeatable`); the protocol has no idempotency key"
        )));
    }
    Ok(())
}

/// Whether `name` is an environment variable name a definition may set:
/// `[A-Z_][A-Z0-9_]*`, not starting with `APB_`.
fn is_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.starts_with("APB_")
}

/// The shape rules of a connector's `mcp:` block (manifest rule 15.3.4).
/// `mcp_timeouts` are the name and `timeout_sec` of each mcp function of
/// the connector, for the timeout rule under `protocol: auto`.
pub fn validate_server_shape(
    _connector: &str,
    spec: &McpServerSpec,
    mcp_timeouts: &[(&str, u64)],
) -> Result<(), ConnectorError> {
    let transport = spec.transport.as_str();
    if spec.transport == McpTransportKind::Http {
        return Err(invalid(format!(
            "mcp transport `{transport}` is not supported by this apb"
        )));
    }
    let Some(command) = &spec.command else {
        return Err(invalid(format!(
            "mcp transport `{transport}` requires `command`"
        )));
    };
    // The control character check comes first, so no message below
    // prints a value that holds one.
    let controlled = |s: &str| s.chars().any(char::is_control);
    if controlled(command) {
        return Err(invalid(
            "mcp `command` holds a control character".to_string(),
        ));
    }
    let path = std::path::Path::new(command);
    if command.trim().is_empty()
        || command.contains("{{")
        || (command.contains('/') && !path.is_absolute())
    {
        return Err(invalid(format!(
            "mcp command `{command}` must be an absolute path or a bare program name"
        )));
    }
    if let Some(i) = spec.args.iter().position(|a| controlled(a)) {
        return Err(invalid(format!(
            "mcp `args[{i}]` holds a control character"
        )));
    }
    for (name, value) in &spec.env {
        if controlled(name) || controlled(value) {
            return Err(invalid(format!(
                "mcp `env.{}` holds a control character",
                name.chars().filter(|c| !c.is_control()).collect::<String>()
            )));
        }
        if !is_env_name(name) {
            return Err(invalid(format!(
                "mcp env name `{name}` must be upper case letters, digits and _, and must not start with APB_"
            )));
        }
        if is_loader_env(name) {
            return Err(invalid(format!(
                "mcp_env_loads_code: mcp env name `{name}` changes what code the server loads, which the approval of its program does not cover; it cannot be set"
            )));
        }
    }
    if spec.protocol.is_auto()
        && let Some((name, timeout_sec)) = mcp_timeouts
            .iter()
            .find(|(_, t)| *t < MCP_AUTO_MIN_TIMEOUT_SEC)
    {
        return Err(invalid(format!(
            "mcp_timeout_too_small: function `{name}` has timeout_sec {timeout_sec}, and with protocol auto a server that ignores `server/discover` is only asked `initialize` after a fixed 10 s probe; set timeout_sec to {MCP_AUTO_MIN_TIMEOUT_SEC} or more, or protocol to modern or legacy"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::def::ConnectorDoc;
    use crate::connector::resolve::ConnectorFacts;

    /// The example of the design: a stdio server, reads and one write.
    const EXAMPLE: &str = r#"name: tracker-mcp
version: 0.1.0
mcp:
  transport: stdio
  command: tracker-mcp-server
  args: ["--workspace", "{{account.workspace}}"]
  env:
    TRACKER_TOKEN: "{{secret.token}}"
account_fields:
  - { name: workspace, required: true }
  - { name: token, required: true, secret: true }
identity: { function: whoami, path: id }
functions:
  - name: whoami
    description: The account the server acts as
    read_only: true
    mcp: { tool: whoami }
  - name: search_issues
    description: Search issues by text
    read_only: true
    mcp: { tool: search_issues }
    args_schema:
      type: object
      properties: { query: { type: string } }
      required: [query]
  - name: list_comments
    description: Recent comments of one issue
    read_only: true
    mcp: { tool: list_comments }
    args_schema:
      type: object
      properties: { issue: { type: string }, since: { type: string } }
      required: [issue]
  - name: create_comment
    description: Add a comment to an issue
    mcp: { tool: create_comment }
    args_schema:
      type: object
      properties: { issue: { type: string }, body: { type: string } }
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
"#;

    /// The marker declaration of `create_comment`, for replacing it.
    const MARKER: &str = r#"    idempotency:
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
"#;

    fn load(yaml: &str) -> Result<ConnectorDoc, ConnectorError> {
        ConnectorDoc::from_yaml(yaml, "tracker-mcp")
    }

    /// The message of a refused manifest.
    fn refused(yaml: &str) -> String {
        match load(yaml) {
            Ok(_) => panic!("the manifest loaded"),
            Err(ConnectorError::Invalid(msg)) => msg,
            Err(e) => panic!("not a rule refusal: {e}"),
        }
    }

    fn with(old: &str, new: &str) -> String {
        assert!(EXAMPLE.contains(old), "the example has no `{old}`");
        EXAMPLE.replacen(old, new, 1)
    }

    #[test]
    fn a_stdio_connector_loads() {
        let doc = load(EXAMPLE).unwrap();
        let mcp = doc.mcp.as_ref().expect("the mcp block");
        assert_eq!(mcp.transport, McpTransportKind::Stdio);
        assert_eq!(mcp.command.as_deref(), Some("tracker-mcp-server"));
        assert_eq!(mcp.protocol, McpProtocol::Auto);
        let f = doc.function("search_issues").unwrap();
        assert!(f.is_mcp());
        assert_eq!(f.mcp.as_ref().unwrap().tool, "search_issues");
    }

    #[test]
    fn an_mcp_function_with_an_http_field_is_refused() {
        let msg = refused(&with(
            "    mcp: { tool: search_issues }\n",
            "    mcp: { tool: search_issues }\n    headers: { X-Trace: \"1\" }\n",
        ));
        assert_eq!(
            msg,
            "mcp function `search_issues` must not set `headers`; it is an HTTP field"
        );
    }

    #[test]
    fn a_tool_name_outside_the_naming_rule_is_refused() {
        let msg = refused(&with("mcp: { tool: whoami }", "mcp: { tool: \"who ami\" }"));
        assert_eq!(
            msg,
            "mcp function `whoami`: tool name `who ami` must be 1 to 128 of A-Z a-z 0-9 _ - ."
        );
    }

    #[test]
    fn a_relative_command_is_refused() {
        let msg = refused(&with("command: tracker-mcp-server", "command: ./server"));
        assert_eq!(
            msg,
            "mcp command `./server` must be an absolute path or a bare program name"
        );
        assert!(
            load(&with(
                "command: tracker-mcp-server",
                "command: /opt/t/server"
            ))
            .is_ok()
        );
    }

    #[test]
    fn a_command_with_a_placeholder_is_refused() {
        let yaml = with(
            "command: tracker-mcp-server",
            "command: \"{{account.workspace}}\"",
        );
        assert!(load(&yaml).is_err());
    }

    #[test]
    fn a_stdio_block_without_a_command_is_refused() {
        let msg = refused(&with("  command: tracker-mcp-server\n", ""));
        assert_eq!(msg, "mcp transport `stdio` requires `command`");
    }

    #[test]
    fn an_env_name_starting_with_apb_is_refused() {
        let msg = refused(&with("TRACKER_TOKEN:", "APB_TOKEN:"));
        assert_eq!(
            msg,
            "mcp env name `APB_TOKEN` must be upper case letters, digits and _, and must not start with APB_"
        );
        assert!(load(&with("TRACKER_TOKEN:", "tracker_token:")).is_err());
    }

    #[test]
    fn a_control_character_in_args_is_refused() {
        let msg = refused(&with("\"--workspace\"", "\"--work\\u001b[2Jspace\""));
        assert_eq!(msg, "mcp `args[0]` holds a control character");
    }

    #[test]
    fn a_secret_in_args_is_refused() {
        assert!(load(&with("{{account.workspace}}\"]", "{{secret.token}}\"]")).is_err());
    }

    #[test]
    fn a_secret_in_env_is_allowed() {
        let doc = load(EXAMPLE).unwrap();
        assert_eq!(
            doc.mcp
                .unwrap()
                .env
                .get("TRACKER_TOKEN")
                .map(String::as_str),
            Some("{{secret.token}}")
        );
    }

    #[test]
    fn args_take_account_fields_only() {
        assert!(load(&with("{{account.workspace}}\"]", "{{args.query}}\"]")).is_err());
        assert!(load(&with("{{account.workspace}}\"]", "{{account.nope}}\"]")).is_err());
        // `{{args.*}}` is refused in the env too: the arguments go to the tool.
        assert!(load(&with("\"{{secret.token}}\"", "\"{{args.query}}\"")).is_err());
        assert!(load(EXAMPLE).is_ok());
    }

    /// A connector template never names a run param: a secret param stays
    /// out of the server's start whatever the playbook declares, and V340
    /// covers every string of the playbook side (the node's arguments).
    #[test]
    fn a_param_placeholder_is_refused_in_args_and_env() {
        for doc in [
            with("{{account.workspace}}\"]", "{{params.token}}\"]"),
            with("\"{{secret.token}}\"", "\"{{params.token}}\""),
        ] {
            let msg = refused(&doc);
            assert!(
                msg.contains("unknown placeholder namespace `params`"),
                "{msg}"
            );
        }
    }

    #[test]
    fn a_write_without_idempotency_is_refused() {
        let msg = refused(&with(MARKER, ""));
        assert_eq!(
            msg,
            "mcp function `create_comment` changes something and must declare idempotency (`marker`, `none` or `repeatable`); the protocol has no idempotency key"
        );
    }

    #[test]
    fn key_is_refused_on_mcp() {
        let msg = refused(&with(
            MARKER,
            "    idempotency: { mode: key, header: Idempotency-Key, window_sec: 60 }\n",
        ));
        assert_eq!(
            msg,
            "mcp function `create_comment` cannot declare idempotency mode `key`: the protocol has no idempotency key; use `marker` or `none`"
        );
    }

    #[test]
    fn marker_and_none_are_accepted_on_mcp() {
        assert!(load(EXAMPLE).is_ok(), "marker");
        load(&with(MARKER, "    idempotency: { mode: none }\n")).expect("none");
        load(&with(MARKER, "    idempotency: { mode: repeatable }\n")).expect("repeatable");
    }

    #[test]
    fn a_marker_reconcile_function_must_be_a_read_only_mcp_function() {
        // A read-only mock is not an mcp function.
        let mock = with(
            "    mcp: { tool: list_comments }\n",
            "    mock: { status: 200, body: { comments: [] } }\n",
        );
        assert!(load(&mock).is_err());
        // A write is not read-only.
        let write = with(
            "    description: Recent comments of one issue\n    read_only: true\n",
            "    description: Recent comments of one issue\n    idempotency: { mode: none }\n",
        );
        assert!(load(&write).is_err());
    }

    #[test]
    fn an_mcp_identity_function_is_accepted() {
        let doc = load(EXAMPLE).unwrap();
        assert_eq!(doc.identity.unwrap().function, "whoami");
    }

    #[test]
    fn an_mcp_block_without_mcp_functions_is_refused() {
        let yaml = "name: tracker-mcp\nversion: 0.1.0\nmcp: { transport: stdio, command: srv }\nfunctions:\n  - name: f\n    description: d\n    read_only: true\n    mock: { status: 200, body: {} }\n";
        assert_eq!(
            refused(yaml),
            "connector `tracker-mcp` declares an `mcp:` block but no mcp function"
        );
    }

    #[test]
    fn mcp_functions_without_a_block_are_refused() {
        let yaml = "name: tracker-mcp\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    read_only: true\n    mcp: { tool: f }\n  - name: g\n    description: d\n    read_only: true\n    mcp: { tool: g }\n";
        assert_eq!(
            refused(yaml),
            "connector `tracker-mcp` declares mcp function(s) f, g but no `mcp:` block"
        );
    }

    #[test]
    fn an_http_function_next_to_an_mcp_block_is_refused() {
        let yaml = with(
            "functions:\n",
            "functions:\n  - name: ping\n    description: d\n    read_only: true\n    method: GET\n    url: https://example.test/ping\n",
        );
        assert_eq!(
            refused(&yaml),
            "connector `tracker-mcp` has an `mcp:` block, so its functions are mcp or mock functions only; `ping` is not"
        );
    }

    #[test]
    fn auth_and_webhook_are_refused_with_an_mcp_block() {
        let auth = with(
            "account_fields:\n",
            "auth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n",
        );
        assert_eq!(
            refused(&auth),
            "connector `tracker-mcp` has an `mcp:` block and must not set `auth`"
        );
        let webhook = with(
            "account_fields:\n",
            "webhook:\n  dedupe_path: id\n  signature:\n    scheme: hmac_sha256_hex\n    header: X-Sig\n    secret: \"{{secret.token}}\"\naccount_fields:\n",
        )
        .replacen(
            "functions:\n",
            "functions:\n  - name: events\n    description: d\n    read_only: true\n    inbox: { op: read }\n",
            1,
        );
        assert_eq!(
            refused(&webhook),
            "connector `tracker-mcp` has an `mcp:` block and must not set `webhook`"
        );
    }

    #[test]
    fn two_writes_mapping_one_tool_are_refused() {
        let yaml = with(
            "functions:\n",
            "functions:\n  - name: post_note\n    description: d\n    mcp: { tool: create_comment }\n    idempotency: { mode: none }\n",
        );
        assert_eq!(
            refused(&yaml),
            "functions `post_note` and `create_comment` map the same tool `create_comment`; only read-only functions may share a tool"
        );
    }

    #[test]
    fn two_reads_mapping_one_tool_are_accepted() {
        let yaml = with(
            "functions:\n",
            "functions:\n  - name: find_bugs\n    description: d\n    read_only: true\n    mcp: { tool: search_issues }\n",
        );
        load(&yaml).unwrap();
    }

    #[test]
    fn a_timeout_below_the_probe_is_refused_under_auto() {
        let yaml = with(
            "    mcp: { tool: search_issues }\n",
            "    mcp: { tool: search_issues }\n    timeout_sec: 10\n",
        );
        let msg = refused(&yaml);
        assert!(msg.starts_with("mcp_timeout_too_small:"), "{msg}");
        assert_eq!(
            msg,
            "mcp_timeout_too_small: function `search_issues` has timeout_sec 10, and with protocol auto a server that ignores `server/discover` is only asked `initialize` after a fixed 10 s probe; set timeout_sec to 15 or more, or protocol to modern or legacy"
        );
        let at_min = with(
            "    mcp: { tool: search_issues }\n",
            "    mcp: { tool: search_issues }\n    timeout_sec: 15\n",
        );
        load(&at_min).unwrap();
    }

    #[test]
    fn a_short_timeout_is_accepted_under_modern() {
        for protocol in ["modern", "legacy"] {
            let yaml = with(
                "    mcp: { tool: search_issues }\n",
                "    mcp: { tool: search_issues }\n    timeout_sec: 2\n",
            )
            .replacen(
                "  transport: stdio\n",
                &format!("  transport: stdio\n  protocol: {protocol}\n"),
                1,
            );
            let doc = load(&yaml).unwrap();
            assert!(!doc.mcp.unwrap().protocol.is_auto());
        }
    }

    #[test]
    fn launcher_name_knows_the_launchers() {
        for l in MCP_LAUNCHERS {
            assert_eq!(launcher_name(l), Some(l));
        }
        assert_eq!(launcher_name("/usr/local/bin/npx"), Some("npx"));
        assert_eq!(launcher_name("/usr/bin/docker"), Some("docker"));
        assert_eq!(launcher_name("tracker-mcp-server"), None);
        assert_eq!(launcher_name("uvicorn"), None);
        assert_eq!(launcher_name("/opt/npx/server"), None);
        for i in [
            "node", "python3", "sh", "bash", "ruby", "perl", "bun", "env",
        ] {
            assert_eq!(launcher_name(&format!("/usr/bin/{i}")), Some(i));
        }
        assert_eq!(launcher_name("/usr/bin/python3.12"), Some("python3"));
        assert_eq!(launcher_name("/usr/bin/pythonista"), None);
        assert_eq!(launcher_name("/usr/bin/shell-server"), None);
    }

    #[test]
    fn the_loader_list_is_refused_in_mcp_env_and_nothing_else_is() {
        let named = |name: &str| {
            with(
                "    TRACKER_TOKEN: \"{{secret.token}}\"\n",
                &format!("    TRACKER_TOKEN: \"{{{{secret.token}}}}\"\n    {name}: x\n"),
            )
        };
        let mut refused_names: Vec<String> = LOADER_ENV.iter().map(|n| n.to_string()).collect();
        refused_names.extend(
            [
                "LD_PRELOAD",
                "LD_LIBRARY_PATH",
                "LD_AUDIT",
                "DYLD_INSERT_LIBRARIES",
                "DYLD_LIBRARY_PATH",
            ]
            .map(str::to_string),
        );
        for name in &refused_names {
            assert!(is_loader_env(name), "{name}");
            let msg = refused(&named(name));
            assert!(msg.starts_with("mcp_env_loads_code:"), "{name}: {msg}");
            assert!(msg.contains(name.as_str()), "{msg}");
        }
        for name in [
            "TRACKER_URL",
            "NODE_ENV",
            "PYTHONUNBUFFERED",
            "LANG",
            "HTTPS_PROXY",
        ] {
            assert!(!is_loader_env(name), "{name}");
            load(&named(name)).unwrap();
        }
    }

    #[test]
    fn install_errors_never_print_a_control_character() {
        let yaml = EXAMPLE.replace("command: tracker-mcp-server", "command: \"a/\\e[2J\"");
        let msg = refused(&yaml);
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
        let yaml = EXAMPLE.replace("mcp: { tool: whoami }", "mcp: { tool: \"who\\e[2Jami\" }");
        let msg = refused(&yaml);
        assert!(!msg.chars().any(char::is_control), "{msg:?}");
        assert!(msg.contains("tool name"), "{msg}");
    }

    #[test]
    fn http_transport_is_refused_by_this_apb() {
        let yaml = "name: tracker-mcp\nversion: 0.1.0\nmcp:\n  transport: http\nfunctions:\n  - name: f\n    description: d\n    read_only: true\n    mcp: { tool: f }\n";
        assert_eq!(
            refused(yaml),
            "mcp transport `http` is not supported by this apb"
        );
    }

    #[test]
    fn mcp_functions_count_as_untrusted_output() {
        let doc = load(EXAMPLE).unwrap();
        for f in &doc.functions {
            assert!(!f.untrusted_output, "nobody marked `{}`", f.name);
            assert!(f.is_untrusted_output(), "`{}`", f.name);
        }
        let facts = ConnectorFacts::of_manifest(&doc);
        assert_eq!(
            facts.untrusted_output_functions,
            ["whoami", "search_issues", "list_comments", "create_comment"]
        );
    }

    #[test]
    fn mcp_functions_never_carry_notices() {
        let yaml = with(
            "    mcp: { tool: create_comment }\n",
            "    mcp: { tool: create_comment }\n    people: { text: body, to: issue }\n",
        );
        let doc = load(&yaml).unwrap();
        let f = doc.function("create_comment").unwrap();
        assert!(f.people.is_some() && f.send_once().is_some());
        assert!(!f.carries_notices());
    }
}
