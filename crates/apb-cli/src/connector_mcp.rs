//! The approval of an MCP connector (design D2 "Capturing pins", D5 "The
//! approve output"): a person's act at an interactive terminal and nowhere
//! else. The tree path shows the `mcp:` block with its templates and asks
//! once; the account path asks twice, the program first (what it will be
//! started with, before any secret is resolved) and the tools second
//! (after the confirmed program listed them), then approves the account and
//! records the pins.
//!
//! `approve_cmd` refuses a run step and a caller without a terminal before
//! anything here runs; [`approve_mcp`] takes the typed confirmations
//! through [`Confirm`], so its tests script them.

use std::io::Write;
use std::path::Path;

use apb_core::connector::config;
use apb_core::connector::mcp::pins::{
    BindingDiff, LiveBinding, PinStore, ProgramFact, program_status,
};
use apb_core::connector::mcp::{McpProtocol, McpServerSpec};
use apb_core::connector::store;
use apb_core::step_env::StepCaller;
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use apb_engine::connector::mcp::StartPreview;
use apb_engine::connector::mcp::pin::{self, ConfirmedProgram, ProgramFacts, ToolListing, ToolRow};

use crate::util::sanitize_for_terminal;

/// The refusal of an approval without a person at a terminal. It prints no
/// digest, no program and no tool.
pub(crate) const NEEDS_A_PERSON: &str = "mcp_approve_needs_a_person: approving an MCP connector or its tools is a person's act at an interactive terminal (docs/CONNECTORS.md, MCP servers)";

/// A typed answer that is not the connector name.
const NOT_CONFIRMED: &str = "not confirmed: nothing was approved or pinned";

/// The longest title a confirmation shows.
const TITLE_CHARS: usize = 200;
/// The longest input schema a confirmation shows.
const SCHEMA_CHARS: usize = 2000;
/// The longest description a row line shows.
const ROW_DESCRIPTION_CHARS: usize = 80;
/// The longest description a changed tool shows, pinned and now.
const CHANGED_DESCRIPTION_CHARS: usize = 300;
/// The longest list of changed members a changed tool shows.
const MEMBERS_CHARS: usize = 300;
/// The longest annotations a row shows.
const ANNOTATIONS_CHARS: usize = 500;
/// The longest list of unmapped tool names.
const UNMAPPED_CHARS: usize = 1000;
/// The longest protocol version shown.
const ERA_CHARS: usize = 64;
/// The most characters of server text one confirmation shows over all
/// its lines; past it the rest is left out, so server text can never
/// scroll the digests and the question away.
const SERVER_TEXT_TOTAL: usize = 20_000;

/// The one line of the program confirmation that says what an approved
/// program can do.
pub(crate) const RUNS_AS_YOU: &str = "this program runs as you, unsandboxed: it can read and change what you can, including apb's own files and the secrets of other connectors";

/// The one call a typed confirmation makes: print `lines`, ask `question`
/// and give back the line typed; `None` when nobody can answer.
pub(crate) trait Confirm {
    fn ask(&mut self, lines: &[String], question: &str) -> Option<String>;
}

/// The person's terminal (`consent::ask_typed`).
pub(crate) struct Terminal;

impl Confirm for Terminal {
    fn ask(&mut self, lines: &[String], question: &str) -> Option<String> {
        crate::consent::ask_typed(lines, question)
    }
}

/// Why an MCP approval cannot be taken here, before anything is read or
/// started: a run step, or no interactive terminal.
pub(crate) fn refusal() -> Option<String> {
    use std::io::IsTerminal as _;
    if let Some(why) = StepCaller::from_env().refusal("approving an MCP connector") {
        return Some(why);
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Some(NEEDS_A_PERSON.to_string());
    }
    None
}

/// One value from the definition, the account, the file system or the
/// server, made safe for the terminal.
fn shown(text: &str) -> String {
    sanitize_for_terminal(text, usize::MAX)
}

/// `text` made safe for the terminal, then cut to `max` characters on a
/// character boundary with a marker that says how much was left out.
fn cut_marked(text: &str, max: usize) -> String {
    let clean = shown(text);
    let total = clean.chars().count();
    if total <= max {
        return clean;
    }
    let kept: String = clean.chars().take(max).collect();
    format!("{kept} [cut: {} more characters]", total - max)
}

fn launcher_warning(name: &str) -> String {
    format!(
        "warning: {} is an interpreter or a launcher; this approval covers the {} file only, not the script or package it runs",
        shown(name),
        shown(name)
    )
}

/// The tree confirmation: the digest and the whole `mcp:` block, every
/// `env` entry with its template as written (no account value is read).
pub(crate) fn print_tree(name: &str, digest: &str, spec: &McpServerSpec) -> Vec<String> {
    let command = spec.command.clone().unwrap_or_default();
    let mut lines = vec![
        format!("connector {} digest {digest}", shown(name)),
        format!("mcp: {} {}", spec.transport.as_str(), shown(&command)),
    ];
    lines.extend(spec.args.iter().map(|a| format!("arg: {}", shown(a))));
    lines.extend(
        spec.env
            .iter()
            .map(|(k, v)| format!("env: {}={}", shown(k), shown(v))),
    );
    let protocol = match spec.protocol {
        McpProtocol::Auto => "auto",
        McpProtocol::Modern => "modern",
        McpProtocol::Legacy => "legacy",
    };
    lines.push(format!("protocol: {protocol}"));
    if let Some(l) = apb_core::connector::mcp::launcher_name(&command) {
        lines.push(launcher_warning(l));
    }
    lines
}

/// The program confirmation, before any secret is resolved: the program,
/// its digest, every rendered argument and `env` override (secret parts by
/// source only), every non-secret field of the account and the command
/// line of every command-sourced secret, not run.
pub(crate) fn print_program(
    facts: &ProgramFacts,
    binding: &str,
    preview: &StartPreview,
) -> Vec<String> {
    let mut lines = vec![
        shown(binding),
        format!(
            "program: {} -> {}",
            shown(&facts.command),
            shown(&facts.program)
        ),
        format!(
            "program digest: {} ({})",
            facts.program_digest, facts.status
        ),
    ];
    if let Some(l) = &facts.launcher {
        lines.push(launcher_warning(l));
    }
    lines.push(format!("warning: {RUNS_AS_YOU}"));
    lines.extend(preview.args.iter().map(|a| format!("arg: {}", shown(a))));
    lines.extend(
        preview
            .env
            .iter()
            .map(|(k, v)| format!("env: {}={}", shown(k), shown(v))),
    );
    lines.extend(
        preview
            .fields
            .iter()
            .map(|(k, v)| format!("field: {}={}", shown(k), shown(v))),
    );
    lines.extend(
        preview
            .secret_commands
            .iter()
            .map(|(k, v)| format!("secret command: {}: {}", shown(k), shown(v))),
    );
    lines
}

/// The lines of one confirmation that carry server text, within
/// `SERVER_TEXT_TOTAL` characters over all of them.
struct ServerLines {
    lines: Vec<String>,
    used: usize,
    left_out: usize,
}

impl ServerLines {
    /// A mapped tool's row: its identity (function, tool, status, digest)
    /// is always shown, whatever the budget, so no tool is pinned unseen;
    /// its description joins the line only within the budget.
    fn push_row(&mut self, identity: String, description: String) {
        let len = description.chars().count();
        if len > 0 && self.left_out == 0 && self.used + len <= SERVER_TEXT_TOTAL {
            self.used += len;
            self.lines.push(format!("{identity}  {description}"));
        } else {
            if len > 0 {
                self.left_out += 1;
            }
            self.lines.push(identity);
        }
    }

    fn push(&mut self, line: String) {
        let len = line.chars().count();
        if self.left_out > 0 || self.used + len > SERVER_TEXT_TOTAL {
            self.left_out += 1;
            return;
        }
        self.used += len;
        self.lines.push(line);
    }
}

/// The server-text lines of the tools confirmation: one row per mapped
/// tool (its identity always shown), then the changed descriptions,
/// titles, input schemas, annotations and unmapped names within
/// `SERVER_TEXT_TOTAL`.
fn server_lines(rows: &[ToolRow], unmapped: &[String]) -> ServerLines {
    let mut server = ServerLines {
        lines: Vec::new(),
        used: 0,
        left_out: 0,
    };
    for row in rows {
        server.push_row(
            format!(
                "{}  {}  {}  {}",
                shown(&row.function),
                shown(&row.tool),
                row.status,
                row.digest.as_deref().unwrap_or("-"),
            ),
            sanitize_for_terminal(
                row.description.as_deref().unwrap_or_default(),
                ROW_DESCRIPTION_CHARS,
            ),
        );
        if row.status == "changed" {
            server.push(format!(
                "pinned: {}",
                cut_marked(
                    row.pinned_description.as_deref().unwrap_or_default(),
                    CHANGED_DESCRIPTION_CHARS
                )
            ));
            server.push(format!(
                "now: {}",
                cut_marked(
                    row.description.as_deref().unwrap_or_default(),
                    CHANGED_DESCRIPTION_CHARS
                )
            ));
            server.push(format!(
                "changed members: {}",
                cut_marked(&row.members.join(", "), MEMBERS_CHARS)
            ));
        }
        if let Some(title) = &row.title {
            server.push(format!("title: {}", cut_marked(title, TITLE_CHARS)));
        }
        if let Some(schema) = &row.input_schema {
            server.push(format!(
                "input schema: {}",
                cut_marked(&schema.to_string(), SCHEMA_CHARS)
            ));
        }
        if let Some(annotations) = &row.annotations {
            server.push(format!(
                "annotations (shown, not used): {}",
                cut_marked(&annotations.to_string(), ANNOTATIONS_CHARS)
            ));
        }
    }
    if !unmapped.is_empty() {
        server.push(format!(
            "unmapped: {}",
            cut_marked(&unmapped.join(", "), UNMAPPED_CHARS)
        ));
    }
    server
}

/// The tools confirmation: the program, one line per mapped tool with its
/// status and full digest, the side-by-side description of a changed tool,
/// titles, input schemas and annotations, the unmapped names and the
/// missing tools, each field of server text cut to its own bound and all
/// of them to `SERVER_TEXT_TOTAL`; then, right above the question, the
/// program, the count of each status and the pin digest again. Server text
/// is already redacted by the listing and is made safe for the terminal
/// here.
pub(crate) fn print_pins(listing: &ToolListing) -> Vec<String> {
    let p = &listing.program;
    let program_line = format!("program digest: {} ({})", p.program_digest, p.status);
    let mut lines = vec![
        format!(
            "connector {} account {} ({})",
            shown(&listing.connector),
            shown(&listing.account),
            cut_marked(&listing.era, ERA_CHARS)
        ),
        format!("program: {} -> {}", shown(&p.command), shown(&p.program)),
        program_line.clone(),
    ];
    if let Some(l) = &p.launcher {
        lines.push(launcher_warning(l));
    }
    let mut server = server_lines(&listing.rows, &listing.unmapped);
    lines.append(&mut server.lines);
    if server.left_out > 0 {
        lines.push(format!(
            "[{} more lines of server text left out: past the screen's limit of {SERVER_TEXT_TOTAL} characters]",
            server.left_out
        ));
    }
    for row in listing.rows.iter().filter(|r| r.status == "missing") {
        lines.push(format!(
            "warning: tool {} of {} is not listed by the server; its function cannot be called",
            shown(&row.tool),
            shown(&listing.connector)
        ));
    }
    // The listing's own server, when it could not be shown ended, right
    // above the summary and the question.
    if let Some(note) = &listing.cleanup {
        lines.push(format!("warning: {}", shown(note)));
    }
    // The summary right above the question.
    let count = |status: &str| listing.rows.iter().filter(|r| r.status == status).count();
    lines.push(program_line);
    lines.push(format!(
        "tools: {} pinned, {} unpinned, {} changed, {} missing",
        count("pinned"),
        count("unpinned"),
        count("changed"),
        count("missing")
    ));
    lines.push(format!("pin digest: {}", listing.pin_digest));
    lines
}

/// Asks one typed confirmation: `Ok` when the connector name was typed,
/// else the exit code with its line written to `err`.
fn confirmed(
    confirm: &mut dyn Confirm,
    err: &mut dyn Write,
    name: &str,
    lines: &[String],
    question: &str,
) -> Result<(), u8> {
    match confirm.ask(lines, question) {
        None => {
            let _ = writeln!(err, "{NEEDS_A_PERSON}");
            Err(2)
        }
        Some(typed) if typed == name => Ok(()),
        Some(_) => {
            let _ = writeln!(err, "{NOT_CONFIRMED}");
            Err(2)
        }
    }
}

/// An error line made safe for the terminal; the code is 2.
fn failed(err: &mut dyn Write, message: &str) -> u8 {
    let _ = writeln!(err, "connector error: {}", shown(message));
    2
}

/// The approval of MCP connector `name` (its tree with `account: None`,
/// else that account and its tools), after `approve_cmd` refused a run
/// step and a caller without a terminal. Answers the exit code.
pub(crate) fn approve_mcp(
    confirm: &mut dyn Confirm,
    out: &mut dyn Write,
    err: &mut dyn Write,
    root: &Path,
    name: &str,
    account: Option<&str>,
) -> u8 {
    let loaded = match store::load(name) {
        Ok(l) => l,
        Err(e) => return failed(err, &e.to_string()),
    };
    let Some(spec) = loaded.doc.mcp.clone() else {
        return failed(err, &format!("connector `{name}` has no `mcp:` block"));
    };
    match account {
        None => approve_tree(confirm, out, err, root, name, &loaded.digest, &spec),
        Some(acct) => match approve_account(confirm, out, err, root, name, acct, &loaded) {
            Ok(()) => 0,
            Err(code) => code,
        },
    }
}

fn approve_tree(
    confirm: &mut dyn Confirm,
    out: &mut dyn Write,
    err: &mut dyn Write,
    root: &Path,
    name: &str,
    digest: &str,
    spec: &McpServerSpec,
) -> u8 {
    let question = format!(
        "Approve connector `{}` (digest {digest})? Type the connector name to confirm:",
        shown(name)
    );
    if let Err(code) = confirmed(
        confirm,
        err,
        name,
        &print_tree(name, digest, spec),
        &question,
    ) {
        return code;
    }
    if let Err(e) =
        TrustStore::load().approve_kind(digest, name, Kind::Connector, OriginKind::LocallyApproved)
    {
        return failed(err, &format!("cannot record approval: {e}"));
    }
    let _ = writeln!(out, "approved connector {} (digest {digest})", shown(name));
    // A record bound to another tree pins nothing under this one: the one
    // comparison says so (the tree comes first).
    let pins = PinStore::load().ok();
    for account in config::load_merged(root, name).unwrap_or_default() {
        let account_digest = config::account_digest(&account);
        let stale = pins
            .as_ref()
            .and_then(|p| p.server(name, &account.name))
            .is_some_and(|r| {
                matches!(
                    r.compare(&LiveBinding {
                        connector_digest: digest,
                        account_digest: &account_digest,
                        program: ProgramFact::NotRead,
                        tool: "",
                        tool_digest: None,
                    }),
                    Err(BindingDiff::Tree { .. })
                )
            });
        if stale {
            let _ = writeln!(
                out,
                "the MCP tools of its accounts need a new approval: apb connector approve {} --account {}",
                shown(name),
                shown(&account.name)
            );
        }
    }
    0
}

fn approve_account(
    confirm: &mut dyn Confirm,
    out: &mut dyn Write,
    err: &mut dyn Write,
    root: &Path,
    name: &str,
    acct: &str,
    loaded: &store::LoadedConnector,
) -> Result<(), u8> {
    if !TrustStore::load().is_approved(&loaded.digest) {
        return Err(failed(
            err,
            &format!(
                "connector `{name}` is not approved; approve the connector first, then its account"
            ),
        ));
    }
    let accounts = config::load_merged(root, name).map_err(|e| failed(err, &e.to_string()))?;
    let Some(account) = accounts.iter().find(|a| a.name == acct) else {
        return Err(failed(
            err,
            &format!("account `{acct}` not configured for `{name}`"),
        ));
    };
    let account_digest = config::account_digest(account);
    let command = loaded
        .doc
        .mcp
        .as_ref()
        .and_then(|m| m.command.clone())
        .unwrap_or_default();
    // The person's own terminal is the fixed source at approval.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let (program, program_digest) =
        pin::resolve_program(&command, &path).map_err(|e| failed(err, &e.message))?;
    let preview = apb_engine::connector::mcp::preview_start(&loaded.doc, account)
        .map_err(|e| failed(err, &e.message))?;
    // A store that does not read stops here, before the first question:
    // read as empty, the program would show as new and be started.
    let record = PinStore::load()
        .map_err(|e| failed(err, &e.to_string()))?
        .server(name, acct)
        .cloned();
    let status = program_status(record.as_ref(), &program, &program_digest);
    let facts = ProgramFacts {
        launcher: apb_core::connector::mcp::launcher_name(&command).map(str::to_string),
        command,
        program: program.clone(),
        program_digest: program_digest.clone(),
        status,
    };
    let lines = print_program(
        &facts,
        &format!("connector {name} account {acct}"),
        &preview,
    );
    let question = format!(
        "Start {} with these arguments and this environment, and the secrets of account `{}`? Type the connector name to confirm:",
        shown(&program),
        shown(acct)
    );
    confirmed(confirm, err, name, &lines, &question)?;
    let listing = pin::list_live(
        root,
        name,
        acct,
        &ConfirmedProgram {
            connector_digest: loaded.digest.clone(),
            account_digest: account_digest.clone(),
            program,
            program_digest,
        },
    )
    .map_err(|e| {
        let _ = writeln!(err, "{}", shown(&e.message));
        2u8
    })?;
    let question = format!(
        "Pin these tools for `{}` account `{}`? Type the connector name to confirm:",
        shown(name),
        shown(acct)
    );
    confirmed(confirm, err, name, &print_pins(&listing), &question)?;
    let id = account_trust_id(name, acct);
    TrustStore::load()
        .approve_kind(
            &account_digest,
            &id,
            Kind::ConnectorAccount,
            OriginKind::LocallyApproved,
        )
        .map_err(|e| failed(err, &format!("cannot record approval: {e}")))?;
    // The account approval is already recorded: a pin write that fails
    // leaves it, so the failure says so. That state fails closed (every
    // call and run start stays refused `mcp_tools_unpinned`) and the same
    // approval run again pins the tools.
    pin::record(&listing).map_err(|e| {
        failed(
            err,
            &format!(
                "{}; the approval of account `{}` was recorded but its tools were not pinned: calls and run starts stay refused until this approval is run again",
                e.message,
                shown(acct)
            ),
        )
    })?;
    let pinned: Vec<String> = listing
        .rows
        .iter()
        .filter(|r| r.status != "missing")
        .map(|r| shown(&r.tool))
        .collect();
    let _ = writeln!(out, "approved: {}/{}", shown(name), shown(acct));
    let _ = writeln!(out, "pinned: {}", pinned.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_mcp_client::testing::{StubEra, StubServer};
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::path::PathBuf;

    const CONNECTOR: &str = "tracker-mcp";
    const ACCOUNT: &str = "work";
    const TOKEN_VAR: &str = "APB_CLI_MCP_TOKEN";
    const TOKEN: &str = "cli-token-value-5521";

    /// Runs `body` alone in a child process: the tests set the config dir
    /// and the account's variables process-wide.
    fn child(name: &str, body: impl FnOnce()) {
        apb_core::process_mode::testing::in_plain_child(
            &format!("connector_mcp::tests::{name}"),
            body,
        );
    }

    /// A scripted person: answers each question from a list (`None` once
    /// the list is spent) and records what was printed before it.
    struct Script {
        answers: VecDeque<Option<String>>,
        asked: Vec<(Vec<String>, String)>,
    }

    impl Script {
        fn new(answers: &[Option<&str>]) -> Script {
            Script {
                answers: answers.iter().map(|a| a.map(str::to_string)).collect(),
                asked: Vec::new(),
            }
        }

        fn shown(&self) -> String {
            self.asked
                .iter()
                .flat_map(|(lines, q)| lines.iter().chain(std::iter::once(q)))
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    impl Confirm for Script {
        fn ask(&mut self, lines: &[String], question: &str) -> Option<String> {
            self.asked.push((lines.to_vec(), question.to_string()));
            self.answers.pop_front().flatten()
        }
    }

    const YES: Option<&str> = Some(CONNECTOR);

    fn connector_yaml(command: &str) -> String {
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
  - {{ name: token, required: true, secret: true }}
functions:
  - name: whoami
    description: The account the server acts as
    read_only: true
    mcp: {{ tool: whoami }}
  - name: search_issues
    description: Search issues by text
    read_only: true
    mcp: {{ tool: search_issues }}
  - name: create_comment
    description: Add a comment to an issue
    mcp: {{ tool: create_comment }}
    idempotency: {{ mode: none }}
"#
        )
    }

    fn tools() -> Value {
        json!([
            { "name": "whoami", "description": "The account the server acts as",
              "inputSchema": { "type": "object" }, "annotations": { "readOnlyHint": true } },
            { "name": "search_issues", "title": "Search", "description": "Search issues by text",
              "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } } } },
            { "name": "create_comment", "description": "Add a comment to an issue",
              "inputSchema": { "type": "object" } },
            { "name": "admin_tool", "description": "Not mapped", "inputSchema": { "type": "object" } }
        ])
    }

    struct Fx {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        cfg: PathBuf,
        root: PathBuf,
        stub: StubServer,
        program: PathBuf,
    }

    impl Fx {
        fn new() -> Fx {
            Fx::with(|y| y)
        }

        /// The connector (`edit` of the default) installed and its tree
        /// approved, a project account with the token from the project's
        /// secrets, and a stub server.
        fn with(edit: impl FnOnce(String) -> String) -> Fx {
            let tmp = tempfile::tempdir().unwrap();
            let base = std::fs::canonicalize(tmp.path()).unwrap();
            let cfg = base.join("cfg");
            let root = base.join("root");
            std::fs::create_dir_all(&cfg).unwrap();
            std::fs::create_dir_all(root.join(".apb")).unwrap();
            // SAFETY: a child process runs this one test on one thread.
            unsafe {
                std::env::set_var("APB_CONFIG_DIR", &cfg);
                std::env::set_var("APB_TEST_DEFAULT_CONFIG_DIR", &cfg);
                std::env::remove_var("APB_RUN_ID");
                std::env::remove_var("APB_HOST_TASK");
                std::env::remove_var(TOKEN_VAR);
            }
            let stub = StubServer::write(&base.join("stub"), StubEra::Modern).unwrap();
            stub.set_tools(&tools()).unwrap();
            let program = std::fs::canonicalize(stub.program()).unwrap();
            std::fs::write(
                root.join(".apb/secrets.env"),
                format!("{TOKEN_VAR}={TOKEN}\n"),
            )
            .unwrap();
            let fx = Fx {
                _tmp: tmp,
                base,
                cfg,
                root,
                stub,
                program,
            };
            fx.install(&edit(connector_yaml(&fx.program.to_string_lossy())));
            fx.approve_tree_directly();
            fx.write_account(&format!("{{{{env.{TOKEN_VAR}}}}}"), "workspace: acme\n");
            fx
        }

        fn install(&self, yaml: &str) {
            let dir = self.cfg.join("connectors").join(CONNECTOR);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
        }

        fn tree(&self) -> String {
            store::load(CONNECTOR).unwrap().digest
        }

        fn approve_tree_directly(&self) {
            TrustStore::load()
                .approve_kind(
                    &self.tree(),
                    CONNECTOR,
                    Kind::Connector,
                    OriginKind::LocallyApproved,
                )
                .unwrap();
        }

        /// The account file: a helper-sourced token lives in the global
        /// file (a project account may not read a command). `fields` are
        /// YAML lines of the other fields, four spaces in.
        fn write_account(&self, token: &str, fields: &str) {
            let project = config::project_config_path(&self.root, CONNECTOR);
            let global = config::global_config_path(CONNECTOR).unwrap();
            let _ = std::fs::remove_file(&project);
            let _ = std::fs::remove_file(&global);
            let path = if token.contains("{{cmd:") {
                global
            } else {
                project
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let fields: String = fields.lines().map(|l| format!("    {l}\n")).collect();
            std::fs::write(
                path,
                format!(
                    "accounts:\n  - name: {ACCOUNT}\n    default: true\n{fields}    token: \"{token}\"\n"
                ),
            )
            .unwrap();
        }

        fn helper(&self) -> String {
            let helper = self.base.join("helper.sh");
            std::fs::write(
                &helper,
                format!(
                    "#!/bin/sh\necho ran >> '{}'\nprintf '{TOKEN}\\n'\n",
                    self.marker().display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
            helper.display().to_string()
        }

        /// The account's token from a helper that leaves `helper-ran`.
        fn use_helper(&self, fields: &str) {
            let helper = self.helper();
            self.write_account(&format!("{{{{cmd:{helper}}}}}"), fields);
        }

        fn marker(&self) -> PathBuf {
            self.base.join("helper-ran")
        }

        fn account_digest(&self) -> String {
            let accounts = config::load_merged(&self.root, CONNECTOR).unwrap();
            config::account_digest(accounts.iter().find(|a| a.name == ACCOUNT).unwrap())
        }

        fn account_approved(&self) -> bool {
            TrustStore::load().is_approved(&self.account_digest())
        }

        fn pin_bytes(&self) -> Option<Vec<u8>> {
            std::fs::read(self.cfg.join("mcp-tools.json")).ok()
        }

        fn starts(&self) -> usize {
            self.stub.starts().len()
        }

        fn approve(
            &self,
            script: &mut impl Confirm,
            account: Option<&str>,
        ) -> (u8, String, String) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = approve_mcp(script, &mut out, &mut err, &self.root, CONNECTOR, account);
            (
                code,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }
    }

    #[test]
    fn two_confirmations_pin_the_tools() {
        child("two_confirmations_pin_the_tools", || {
            let fx = Fx::new();
            let mut script = Script::new(&[YES, YES]);
            let (code, out, err) = fx.approve(&mut script, Some(ACCOUNT));
            assert_eq!(code, 0, "{err}");
            assert_eq!(script.asked.len(), 2);
            assert!(
                out.contains(&format!("approved: {CONNECTOR}/{ACCOUNT}")),
                "{out}"
            );
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert_eq!(pin.connector_digest, fx.tree());
            assert_eq!(pin.account_digest, fx.account_digest());
            assert_eq!(PathBuf::from(&pin.program), fx.program);
            assert_eq!(
                pin.program_digest,
                apb_core::content::sha256_hex(&std::fs::read(&fx.program).unwrap())
            );
            let tools: Vec<&String> = pin.tools.keys().collect();
            assert_eq!(tools, vec!["create_comment", "search_issues", "whoami"]);
            assert!(fx.account_approved());
            assert_eq!(fx.starts(), 1);
        });
    }

    #[test]
    fn a_declined_program_confirmation_starts_nothing() {
        child("a_declined_program_confirmation_starts_nothing", || {
            let fx = Fx::new();
            fx.use_helper("workspace: acme");
            let mut script = Script::new(&[Some("no")]);
            let (code, _, err) = fx.approve(&mut script, Some(ACCOUNT));
            assert_eq!(code, 2);
            assert!(err.contains(NOT_CONFIRMED), "{err}");
            assert!(!fx.marker().exists());
            assert_eq!(fx.starts(), 0);
            assert!(fx.pin_bytes().is_none());
            assert!(!fx.account_approved());
        });
    }

    #[test]
    fn an_unreadable_pin_store_stops_before_the_first_question() {
        child(
            "an_unreadable_pin_store_stops_before_the_first_question",
            || {
                let fx = Fx::new();
                fx.use_helper("workspace: acme");
                std::fs::write(fx.cfg.join("mcp-tools.json"), "not json").unwrap();
                let mut script = Script::new(&[YES, YES]);
                let (code, _, _) = fx.approve(&mut script, Some(ACCOUNT));
                assert_eq!(code, 2);
                assert!(script.asked.is_empty(), "no question was asked");
                assert!(!fx.marker().exists());
                assert_eq!(fx.starts(), 0);
                assert!(!fx.account_approved());
            },
        );
    }

    #[test]
    fn a_declined_tool_confirmation_pins_nothing() {
        child("a_declined_tool_confirmation_pins_nothing", || {
            let fx = Fx::new();
            let mut script = Script::new(&[YES, Some("no")]);
            let (code, _, err) = fx.approve(&mut script, Some(ACCOUNT));
            assert_eq!(code, 2);
            assert!(err.contains(NOT_CONFIRMED), "{err}");
            assert_eq!(fx.starts(), 1, "the server was listed");
            assert!(fx.pin_bytes().is_none());
            assert!(!fx.account_approved());
        });
    }

    #[test]
    fn the_program_confirmation_shows_the_rendered_start() {
        child("the_program_confirmation_shows_the_rendered_start", || {
            let fx = Fx::with(|y| {
                y.replace(
                    "  args: [\"--workspace\", \"{{account.workspace}}\"]\n  env:\n    TRACKER_TOKEN: \"{{secret.token}}\"\n",
                    "  args: [\"--base={{account.base_url}}\"]\n  env:\n    PATH: \"{{account.bin_dir}}:/usr/bin\"\n    HTTPS_PROXY: \"{{account.proxy}}\"\n    TOKEN: \"{{secret.token}}\"\n",
                )
                .replace(
                    "  - { name: workspace, required: true }\n",
                    "  - { name: base_url }\n  - { name: bin_dir }\n  - { name: proxy }\n",
                )
            });
            fx.approve_tree_directly();
            fx.use_helper(
                "base_url: https://tracker.test\nbin_dir: /opt/tracker/bin\nproxy: http://proxy.test:3128",
            );
            let mut script = Script::new(&[None]);
            fx.approve(&mut script, Some(ACCOUNT));
            let first = script.asked[0].0.join("\n");
            for want in [
                "arg: --base=https://tracker.test",
                "env: PATH=/opt/tracker/bin:/usr/bin",
                "env: HTTPS_PROXY=http://proxy.test:3128",
                "env: TOKEN=[secret token from cmd]",
                "field: base_url=https://tracker.test",
                "field: bin_dir=/opt/tracker/bin",
                "field: proxy=http://proxy.test:3128",
            ] {
                assert!(first.contains(want), "{want} in {first}");
            }
            assert!(!fx.marker().exists());
            assert_eq!(fx.starts(), 0);
        });
    }

    #[test]
    fn the_program_confirmation_shows_each_secret_command() {
        child("the_program_confirmation_shows_each_secret_command", || {
            let fx = Fx::new();
            fx.use_helper("workspace: acme");
            let helper = fx.base.join("helper.sh").display().to_string();
            let mut script = Script::new(&[None]);
            fx.approve(&mut script, Some(ACCOUNT));
            let first = script.asked[0].0.join("\n");
            assert!(
                first.contains(&format!("secret command: token: {helper}")),
                "{first}"
            );
            assert!(!fx.marker().exists());
        });
    }

    #[test]
    fn the_confirmations_sanitize_definition_and_account_text() {
        child(
            "the_confirmations_sanitize_definition_and_account_text",
            || {
                let fx = Fx::new();
                fx.write_account(
                    &format!("{{{{env.{TOKEN_VAR}}}}}"),
                    "workspace: \"acme\\e[2J\"",
                );
                let mut script = Script::new(&[YES, None]);
                fx.approve(&mut script, Some(ACCOUNT));
                assert_eq!(script.asked.len(), 2, "both confirmations were shown");
                let shown = script.shown();
                assert!(!shown.contains('\u{1b}'), "{shown:?}");
                assert!(shown.contains("acme"), "{shown}");
            },
        );
    }

    #[test]
    fn approve_prints_each_tools_title_and_input_schema() {
        child("approve_prints_each_tools_title_and_input_schema", || {
            let fx = Fx::new();
            let mut list = tools();
            list[1]["title"] = json!(format!("Search \u{1b}[31m{}", "t".repeat(300)));
            list[1]["inputSchema"]["description"] = json!("d".repeat(3000));
            fx.stub.set_tools(&list).unwrap();
            let mut script = Script::new(&[YES, None]);
            fx.approve(&mut script, Some(ACCOUNT));
            let second = &script.asked[1].0;
            let title = second
                .iter()
                .find(|l| l.starts_with("title: Search"))
                .unwrap();
            assert!(title.ends_with("more characters]"), "{title}");
            assert!(!title.contains('\u{1b}'), "{title:?}");
            let schema = second
                .iter()
                .find(|l| l.starts_with("input schema: ") && l.contains("ddd"))
                .unwrap();
            assert!(schema.contains(" [cut: "), "{schema}");
            assert!(schema.chars().count() < 2100, "{}", schema.len());
        });
    }

    #[test]
    fn approve_reports_a_missing_tool_and_pins_the_rest() {
        child("approve_reports_a_missing_tool_and_pins_the_rest", || {
            let fx = Fx::with(|y| {
                y + "  - name: list_comments\n    description: Recent comments\n    read_only: true\n    mcp: { tool: list_comments }\n"
            });
            fx.approve_tree_directly();
            let mut script = Script::new(&[YES, YES]);
            let (code, _, err) = fx.approve(&mut script, Some(ACCOUNT));
            assert_eq!(code, 0, "{err}");
            let second = script.asked[1].0.join("\n");
            assert!(
                second.contains(&format!(
                    "warning: tool list_comments of {CONNECTOR} is not listed by the server"
                )),
                "{second}"
            );
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert!(!pin.tools.contains_key("list_comments"));
            assert_eq!(pin.tools.len(), 3);
        });
    }

    #[test]
    fn approve_warns_for_a_launcher() {
        child("approve_warns_for_a_launcher", || {
            let fx = Fx::new();
            let npx = fx.stub.dir.join("npx");
            std::fs::copy(&fx.program, &npx).unwrap();
            fx.install(&connector_yaml(&npx.to_string_lossy()));
            fx.approve_tree_directly();
            let mut script = Script::new(&[None]);
            fx.approve(&mut script, Some(ACCOUNT));
            let first = script.asked[0].0.join("\n");
            assert!(
                first.contains(
                    "warning: npx is an interpreter or a launcher; this approval covers the npx file only, not the script or package it runs"
                ),
                "{first}"
            );
        });
    }

    #[test]
    fn approve_warns_for_an_interpreter_and_says_the_program_runs_as_you() {
        child(
            "approve_warns_for_an_interpreter_and_says_the_program_runs_as_you",
            || {
                let fx = Fx::new();
                let node = fx.stub.dir.join("node");
                std::fs::copy(&fx.program, &node).unwrap();
                fx.install(&connector_yaml(&node.to_string_lossy()));
                fx.approve_tree_directly();
                let mut script = Script::new(&[None]);
                fx.approve(&mut script, Some(ACCOUNT));
                let first = script.asked[0].0.join("\n");
                assert!(
                    first.contains("warning: node is an interpreter or a launcher; this approval covers the node file only, not the script or package it runs"),
                    "{first}"
                );
                assert_eq!(first.matches(RUNS_AS_YOU).count(), 1, "said once: {first}");
            },
        );
    }

    /// A person who breaks the pin store while the tools confirmation is
    /// open: the account approval is recorded, the pins are not, and the
    /// failure says both and how to recover.
    struct BreakStoreOnSecond {
        script: Script,
        store: PathBuf,
    }

    impl Confirm for BreakStoreOnSecond {
        fn ask(&mut self, lines: &[String], question: &str) -> Option<String> {
            if self.script.asked.len() == 1 {
                std::fs::write(&self.store, "not json").unwrap();
            }
            self.script.ask(lines, question)
        }
    }

    #[test]
    fn a_failed_pin_write_says_the_account_approval_was_kept() {
        child(
            "a_failed_pin_write_says_the_account_approval_was_kept",
            || {
                let fx = Fx::new();
                fx.approve_tree_directly();
                let mut confirm = BreakStoreOnSecond {
                    script: Script::new(&[YES, YES]),
                    store: fx.cfg.join("mcp-tools.json"),
                };
                let (code, _out, err) = fx.approve(&mut confirm, Some(ACCOUNT));
                assert_eq!(code, 2, "{err}");
                assert!(
                    err.contains("was recorded but its tools were not pinned"),
                    "{err}"
                );
                assert!(err.contains("run again"), "{err}");
            },
        );
    }

    /// Large schemas of earlier tools spend the server-text budget; every
    /// later mapped tool still shows its function, tool, status and digest,
    /// so no tool is pinned without its row on the screen.
    #[test]
    fn every_tool_row_is_shown_past_the_text_budget() {
        let rows: Vec<ToolRow> = (0..40)
            .map(|i| ToolRow {
                function: format!("f{i}"),
                tool: format!("t{i}"),
                digest: Some(format!("sha256:{i:064}")),
                status: "unpinned",
                title: None,
                description: Some("d".repeat(100)),
                pinned_description: None,
                members: Vec::new(),
                annotations: None,
                input_schema: Some(json!({ "x": "s".repeat(3_000) })),
            })
            .collect();
        let server = server_lines(&rows, &[]);
        assert!(server.left_out > 0, "the budget was spent");
        for i in 0..40 {
            let identity = format!("f{i}  t{i}  unpinned  sha256:{i:064}");
            assert!(
                server.lines.iter().any(|l| l.starts_with(&identity)),
                "row {i} missing"
            );
        }
        let total: usize = server.lines.iter().map(|l| l.chars().count()).sum();
        assert!(total < SERVER_TEXT_TOTAL + 40 * 120, "{total}");
    }

    #[test]
    fn server_text_cannot_scroll_the_digests_away() {
        child("server_text_cannot_scroll_the_digests_away", || {
            let fx = Fx::new();
            let mut list = tools();
            // Hundreds of kilobytes of annotations, and many unmapped tools.
            list[0]["annotations"] = json!({ "x": "a".repeat(300_000) });
            let mut all = list.as_array().unwrap().clone();
            for i in 0..400 {
                all.push(json!({ "name": format!("extra_tool_{i}_{}", "n".repeat(100)), "inputSchema": { "type": "object" } }));
            }
            fx.stub.set_tools(&Value::Array(all)).unwrap();
            let mut script = Script::new(&[YES, None]);
            fx.approve(&mut script, Some(ACCOUNT));
            let lines = &script.asked[1].0;
            let total: usize = lines.iter().map(|l| l.chars().count()).sum();
            assert!(total < SERVER_TEXT_TOTAL + 5_000, "{total}");
            let n = lines.len();
            assert!(
                lines[n - 3].starts_with("program digest: sha256:"),
                "{lines:?}"
            );
            assert!(lines[n - 2].starts_with("tools: "), "{lines:?}");
            assert!(lines[n - 1].starts_with("pin digest: sha256:"), "{lines:?}");
            assert!(
                lines.iter().any(|l| l.contains("[cut: ")),
                "the annotations were cut"
            );
        });
    }

    /// A listing whose server could not be shown ended says so in plain
    /// words right above the summary and the question.
    #[test]
    fn an_unconfirmed_cleanup_is_shown_before_the_question() {
        child(
            "an_unconfirmed_cleanup_is_shown_before_the_question",
            || {
                let fx = Fx::new();
                let accounts = config::load_merged(&fx.root, CONNECTOR).unwrap();
                let account = accounts.iter().find(|a| a.name == ACCOUNT).unwrap();
                let bytes = std::fs::read(&fx.program).unwrap();
                let mut listing = pin::list_live(
                    &fx.root,
                    CONNECTOR,
                    ACCOUNT,
                    &ConfirmedProgram {
                        connector_digest: fx.tree(),
                        account_digest: config::account_digest(account),
                        program: fx.program.to_string_lossy().into_owned(),
                        program_digest: apb_core::content::sha256_hex(&bytes),
                    },
                )
                .unwrap();
                assert_eq!(listing.cleanup, None, "the stub was reaped");
                let note = "a process started for this listing may still be running with the secrets of account `work`";
                listing.cleanup = Some(note.to_string());
                let lines = print_pins(&listing);
                let n = lines.len();
                assert_eq!(lines[n - 4], format!("warning: {note}"), "{lines:?}");
                assert!(lines[n - 3].starts_with("program digest: "), "{lines:?}");
            },
        );
    }

    #[test]
    fn server_text_is_sanitized() {
        child("server_text_is_sanitized", || {
            let fx = Fx::new();
            let mut list = tools();
            list[0]["description"] = json!("acts \u{1b}[2J as");
            fx.stub.set_tools(&list).unwrap();
            let mut script = Script::new(&[YES, None]);
            fx.approve(&mut script, Some(ACCOUNT));
            let second = script.asked[1].0.join("\n");
            assert!(!second.contains('\u{1b}'), "{second:?}");
            assert!(second.contains("acts"), "{second}");
        });
    }

    #[test]
    fn approve_tree_prints_every_env_template() {
        child("approve_tree_prints_every_env_template", || {
            let fx = Fx::with(|y| {
                y.replace(
                    "    TRACKER_TOKEN: \"{{secret.token}}\"\n",
                    "    TRACKER_TOKEN: \"{{secret.token}}\"\n    HTTPS_PROXY: \"{{account.proxy}}\"\n    PATH: \"{{account.workspace}}:/usr/bin\"\n",
                )
                .replace(
                    "  - { name: workspace, required: true }\n",
                    "  - { name: workspace, required: true }\n  - { name: proxy }\n",
                )
            });
            fx.use_helper("workspace: acme\nproxy: http://proxy.test:3128");
            let mut script = Script::new(&[YES]);
            let (code, out, _) = fx.approve(&mut script, None);
            assert_eq!(code, 0);
            let lines = &script.asked[0].0;
            let env: Vec<&String> = lines.iter().filter(|l| l.starts_with("env: ")).collect();
            assert_eq!(
                env,
                vec![
                    "env: HTTPS_PROXY={{account.proxy}}",
                    "env: PATH={{account.workspace}}:/usr/bin",
                    "env: TRACKER_TOKEN={{secret.token}}",
                ]
            );
            let shown = script.shown();
            assert!(
                !shown.contains("proxy.test") && !shown.contains("acme"),
                "{shown}"
            );
            assert!(
                out.contains(&format!("approved connector {CONNECTOR}")),
                "{out}"
            );
            assert!(!fx.marker().exists());
        });
    }

    #[test]
    fn approve_tree_at_a_terminal_approves_and_pins_nothing() {
        child(
            "approve_tree_at_a_terminal_approves_and_pins_nothing",
            || {
                let fx = Fx::new();
                let mut script = Script::new(&[YES, YES]);
                assert_eq!(fx.approve(&mut script, Some(ACCOUNT)).0, 0);
                let before = fx.pin_bytes().unwrap();
                let starts = fx.starts();
                // Another definition: its tree is approved, nothing is pinned.
                fx.install(
                    &connector_yaml(&fx.program.to_string_lossy())
                        .replace("Search issues by text", "Search issues"),
                );
                let mut script = Script::new(&[YES]);
                let (code, out, _) = fx.approve(&mut script, None);
                assert_eq!(code, 0);
                assert!(TrustStore::load().is_approved(&fx.tree()));
                assert_eq!(fx.pin_bytes().unwrap(), before);
                assert_eq!(fx.starts(), starts);
                assert!(
                out.contains(&format!(
                    "the MCP tools of its accounts need a new approval: apb connector approve {CONNECTOR} --account {ACCOUNT}"
                )),
                "{out}"
            );
            },
        );
    }

    #[test]
    fn an_args_change_needs_the_account_approved_again() {
        child("an_args_change_needs_the_account_approved_again", || {
            use apb_engine::gate::launch::{LaunchContext, LaunchKind};
            let fx = Fx::new();
            assert_eq!(fx.approve(&mut Script::new(&[YES]), None).0, 0);
            assert_eq!(
                fx.approve(&mut Script::new(&[YES, YES]), Some(ACCOUNT)).0,
                0
            );
            // A playbook granting one tool, and its profile.
            apb_core::registry::init_project(&fx.root).unwrap();
            let dir = fx.root.join(".apb/playbooks/mcpgate/1.0.0");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("playbook.yaml"),
                "schema: 2\nid: mcpgate\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: agent_task, prompt: do, profile: p, connectors: [{ name: tracker-mcp, functions: [search_issues] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n",
            )
            .unwrap();
            std::fs::write(fx.root.join(".apb/playbooks/mcpgate/current"), "1.0.0").unwrap();
            let profile = fx.root.join(".apb/profiles/p");
            std::fs::create_dir_all(&profile).unwrap();
            std::fs::write(
                profile.join("profile.yaml"),
                "name: p\ndescription: test\nexecutor:\n  agent: claude\n  model: haiku\n",
            )
            .unwrap();
            std::fs::write(profile.join("SOUL.md"), "").unwrap();
            // The gate's billing half is not under test: the agent's basis is
            // whatever this machine's CLI login says, so the run's agent work
            // is allowed in this config dir.
            std::fs::write(
                fx.cfg.join(apb_core::policy_file::POLICY_FILE),
                "billing:\n  billed: allow\n  unknown_basis: allow\n",
            )
            .unwrap();
            let wref = apb_core::scope::PlaybookRef {
                origin: apb_core::scope::Origin::Project { workspace_id: None },
                id: "mcpgate".into(),
                version: None,
            };
            let gate = || {
                apb_engine::gate::check_run(
                    &fx.root,
                    &wref,
                    true,
                    false,
                    &LaunchContext::dashboard(LaunchKind::Start),
                )
                .map(|_| ())
            };
            gate().expect("admitted while the pins hold");
            // `mcp.args` changes and the new tree is approved.
            fx.install(&connector_yaml(&fx.program.to_string_lossy()).replace(
                "args: [\"--workspace\"",
                "args: [\"--verbose\", \"--workspace\"",
            ));
            assert_eq!(fx.approve(&mut Script::new(&[YES]), None).0, 0);
            let refusal = gate().expect_err("refused under the new tree");
            assert_eq!(refusal["policy"], "mcp_tools_unpinned", "{refusal}");
            // The account approval pins under the new tree.
            assert_eq!(
                fx.approve(&mut Script::new(&[YES, YES]), Some(ACCOUNT)).0,
                0
            );
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert_eq!(pin.connector_digest, fx.tree());
            gate().expect("admitted again");
        });
    }

    #[test]
    fn a_reflected_secret_refuses_the_approval() {
        child("a_reflected_secret_refuses_the_approval", || {
            let fx = Fx::new();
            let edits: &[fn(&mut Value)] = &[
                |t| t[0]["description"] = json!(format!("uses {TOKEN}")),
                |t| t[1]["inputSchema"]["properties"] = json!({ TOKEN: { "type": "string" } }),
                |t| t[3]["name"] = json!(format!("tool-{TOKEN}")),
            ];
            let before = fx.pin_bytes();
            for edit in edits {
                let mut list = tools();
                edit(&mut list);
                fx.stub.set_tools(&list).unwrap();
                let mut script = Script::new(&[YES, YES]);
                let (code, out, err) = fx.approve(&mut script, Some(ACCOUNT));
                assert_eq!(code, 2);
                assert!(err.starts_with("mcp_tools_reflect_a_secret:"), "{err}");
                assert_eq!(script.asked.len(), 1, "no tools confirmation");
                let printed = format!("{}\n{out}\n{err}", script.shown());
                assert!(!printed.contains(TOKEN), "{printed}");
                for tool in ["whoami", "search_issues", "create_comment", "admin_tool"] {
                    assert!(!printed.contains(tool), "{tool} in {printed}");
                }
                assert_eq!(fx.pin_bytes(), before);
                assert!(!fx.account_approved());
            }
        });
    }
}
