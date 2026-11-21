//! The pins of an MCP connector account as a person approves them (design
//! D2 "Capturing pins"), the listing limits every path shares, and the
//! healthcheck that checks the pins without a tool call (design D4 "Live
//! calls outside a run").

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use apb_core::connector::mcp::pins::ToolPin;
use apb_core::secret::Redaction;
use serde::Serialize;
use serde_json::{Value, json};

use crate::connector::result::{CallError, CallErrorCode};

use super::redact::server_name;
pub use super::{
    MCP_LIST_MAX_BYTES, MCP_LIST_MAX_PAGES, MCP_LIST_MAX_TOOLS, MCP_PIN_CHECK_TIMEOUT,
};
use super::{program_file, redact_tree, reflected_secret, reflects_secret};

/// One mapped tool as the server lists it now, against its pin.
#[derive(Debug, Clone, Serialize)]
pub struct ToolRow {
    pub function: String,
    pub tool: String,
    /// `None` when the server does not list the tool.
    pub digest: Option<String>,
    /// `pinned`, `unpinned`, `changed` or `missing`.
    pub status: &'static str,
    pub title: Option<String>,
    pub description: Option<String>,
    /// For a `changed` row: the pinned description (display only).
    pub pinned_description: Option<String>,
    /// For a `changed` row: the top-level members whose digest differs
    /// from the pinned one, or that one side lacks (`ToolPin.members`).
    pub members: Vec<String>,
    /// The server's annotations, shown and never used.
    pub annotations: Option<Value>,
    pub input_schema: Option<Value>,
}

/// The program a listing started.
#[derive(Debug, Clone, Serialize)]
pub struct ProgramFacts {
    pub command: String,
    /// Absolute canonical path.
    pub program: String,
    pub program_digest: String,
    /// `MCP_LAUNCHERS` name when it is one.
    pub launcher: Option<String>,
    /// `same`, `new` (no record yet) or `changed` against the record.
    pub status: &'static str,
}

/// What a live listing found: one row per function of the connector that
/// maps a tool, then the server's tools no function maps (`unmapped`, by
/// name only, never callable), and the pin digest the rows would record.
#[derive(Debug, Clone, Serialize)]
pub struct ToolListing {
    pub connector: String,
    pub account: String,
    /// The approved tree digest the listing ran under; the record binds it.
    pub connector_digest: String,
    /// `config::account_digest` of the merged account; the record binds it.
    pub account_digest: String,
    /// `modern <version>` or `legacy <version>`, the version one of the
    /// client's supported constants, never a string the server sent.
    pub era: String,
    /// Set when the server started for this listing could not be shown
    /// ended (its reap not confirmed, or its process group not shown
    /// empty): plain words for the person, shown before the second
    /// confirmation.
    pub cleanup: Option<String>,
    pub program: ProgramFacts,
    pub rows: Vec<ToolRow>,
    pub unmapped: Vec<String>,
    /// `digest::pin_digest` over the tree digest, the account digest, the
    /// program and every listed mapped tool.
    pub pin_digest: String,
    /// The pins `record` writes, prepared from the raw tool objects before
    /// the person's second confirmation. Private: never serialized or
    /// printed, and `record` writes exactly this map.
    #[serde(skip)]
    pins: BTreeMap<String, ToolPin>,
}

/// Resolves `command` for an approval: an absolute `command` as is, a bare
/// name through `path` (the approving person's `PATH`, the first entry
/// whose file exists and is executable), then `std::fs::canonicalize` and
/// the sha256 of the file's bytes, streamed. Answers the path and its
/// digest.
pub fn resolve_program(
    command: &str,
    path: &std::ffi::OsStr,
) -> Result<(String, String), CallError> {
    let not_found = || {
        CallError::new(
            CallErrorCode::Config,
            format!("mcp command `{command}` was not found"),
        )
    };
    let given = Path::new(command);
    let found = if given.is_absolute() {
        given.to_path_buf()
    } else if command.contains('/') {
        return Err(not_found());
    } else {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(command))
            .find(|p| {
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && apb_core::content::is_executable(&m))
            })
            .ok_or_else(not_found)?
    };
    let canonical = std::fs::canonicalize(&found).map_err(|_| not_found())?;
    let digest = program_file(&canonical)
        .map(|p| p.digest().to_string())
        .ok_or_else(not_found)?;
    // Stored and opened again by this text: a path that is not UTF-8
    // cannot be kept without changing it, so it is refused here.
    let canonical = canonical.to_str().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!(
                "mcp command `{command}` resolves to a path that is not UTF-8; apb cannot pin it"
            ),
        )
    })?;
    Ok((canonical.to_string(), digest))
}

/// What the first confirmation of an approval showed and the person
/// confirmed.
pub struct ConfirmedProgram {
    pub connector_digest: String,
    pub account_digest: String,
    pub program: String,
    pub program_digest: String,
}

/// The longest description a listing keeps, cut after the redaction.
const DESCRIPTION_CHARS: usize = 300;

/// Lists the tools of the confirmed program for a person's approval
/// (design D2 "Capturing pins"): the tree approved, the tree and account
/// digests and the program file compared with what the person confirmed,
/// all before any secret; then the secrets, the start under the fixed
/// `MCP_PIN_CHECK_TIMEOUT`, the listing, the reflected-secret walk, and the
/// rows against the pin store. Writes nothing.
pub fn list_live(
    root: &Path,
    connector: &str,
    account: &str,
    confirmed: &ConfirmedProgram,
) -> Result<ToolListing, CallError> {
    use crate::connector::call::auth::{non_secret_fields, resolve_secrets};
    use crate::connector::call::target::LiveTarget;
    let loaded = apb_core::connector::store::load(connector).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}`: {e}"),
        )
    })?;
    let first = loaded
        .doc
        .functions
        .iter()
        .find(|f| f.is_mcp())
        .map(|f| f.name.clone())
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("connector `{connector}` maps no MCP tool"),
            )
        })?;
    let live = LiveTarget::load(root, connector, Some(account), &first)?;
    let pins = live.pins();
    if !apb_core::trust::TrustStore::load().is_approved(&pins.connector) {
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!("connector `{connector}` is not approved; nothing was started"),
        ));
    }
    if pins.connector != confirmed.connector_digest {
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "mcp_approve_definition_changed: the definition of `{connector}` changed after it was confirmed; nothing was started"
            ),
        ));
    }
    if pins.account != confirmed.account_digest {
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "mcp_approve_account_changed: account `{account}` of `{connector}` changed after it was confirmed; nothing was started"
            ),
        ));
    }
    // The one open of the program file: hashed from its descriptor here,
    // started from that descriptor (or its path checked again) below.
    let changed = || {
        CallError::new(
            CallErrorCode::Permission,
            format!(
                "mcp_server_changed: the server of `{connector}` ({}) is not the one confirmed for account `{account}` (the program file changed); nothing was started",
                confirmed.program
            ),
        )
    };
    let program = program_file(Path::new(&confirmed.program))
        .filter(|p| p.digest() == confirmed.program_digest)
        .ok_or_else(changed)?;
    let target = live.into_target()?;
    let spec = target
        .doc
        .mcp
        .clone()
        .ok_or_else(|| CallError::new(CallErrorCode::Config, "mcp: no `mcp:` block"))?;
    let home = apb_core::config::account_entry().ok_or_else(super::no_home)?;
    // The record the rows are compared with, read before any secret or
    // start: a store that does not read starts nothing.
    let store = apb_core::connector::mcp::pins::PinStore::load()
        .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
    let deadline = Instant::now() + MCP_PIN_CHECK_TIMEOUT;
    let fields = non_secret_fields(&target.maccount);
    let (secrets, redactions) = resolve_secrets(root, &target.maccount, deadline)?;
    let opened = super::open_program(
        super::Start {
            program,
            spec: &spec,
            account_fields: &fields,
            home: &home,
            deadline,
        },
        &secrets,
        &redactions,
    )
    .map_err(|e| match e {
        super::OpenError::Call(e) => super::redacted(e, &redactions),
        super::OpenError::ProgramChanged => changed(),
    })?;
    let ended = opened.session.close();
    let cleanup = cleanup_note(account, &ended);
    let super::Opened { tools, era, .. } = opened;
    if tools.iter().any(|t| reflects_secret(t, &redactions)) {
        return Err(super::with_end(
            reflected_secret(connector, account),
            &ended,
        ));
    }
    let record = store.server(connector, account);
    Ok(build_listing(
        Listed {
            doc: &target.doc,
            connector,
            account,
            confirmed,
            era,
            cleanup,
            tools,
        },
        record,
        &redactions,
    ))
}

/// The plain words a listing shows when its server could not be shown
/// ended: `None` for a server reaped with its group seen empty.
fn cleanup_note(account: &str, ended: &apb_mcp_client::Ended) -> Option<String> {
    super::end_warning(ended)?;
    Some(format!(
        "a process started for this listing may still be running with the secrets of account `{account}` (the server could not be shown ended); check for it before you confirm"
    ))
}

/// What a listing read before its rows are built.
struct Listed<'a> {
    doc: &'a apb_core::connector::def::ConnectorDoc,
    connector: &'a str,
    account: &'a str,
    confirmed: &'a ConfirmedProgram,
    era: String,
    cleanup: Option<String>,
    tools: Vec<Value>,
}

/// The rows, the unmapped names and the prepared pins of a listing whose
/// tools hold no secret. The raw objects end here.
fn build_listing(
    listed: Listed<'_>,
    record: Option<&apb_core::connector::mcp::pins::ServerPin>,
    redactions: &[Redaction],
) -> ToolListing {
    use apb_core::connector::mcp::digest::{pin_digest, tool_digest};
    let Listed {
        doc,
        connector,
        account,
        confirmed,
        era,
        cleanup,
        tools,
    } = listed;
    let named = |name: &str| {
        tools
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
    };
    let mut rows = Vec::new();
    let mut pins = BTreeMap::new();
    for function in doc.functions.iter().filter(|f| f.is_mcp()) {
        let tool = function
            .mcp
            .as_ref()
            .map(|m| m.tool.clone())
            .unwrap_or_default();
        let Some(raw) = named(&tool) else {
            rows.push(ToolRow {
                function: function.name.clone(),
                tool,
                digest: None,
                status: "missing",
                title: None,
                description: None,
                pinned_description: None,
                members: Vec::new(),
                annotations: None,
                input_schema: None,
            });
            continue;
        };
        let digest = tool_digest(raw);
        // Member names are the server's: redacted whole and bounded, on
        // the side compared and the side stored alike.
        let members: BTreeMap<String, String> = member_digests(raw)
            .into_iter()
            .map(|(k, v)| (server_name(&k, redactions), v))
            .collect();
        let description = raw
            .get("description")
            .and_then(Value::as_str)
            .map(|d| display_description(d, redactions));
        let pinned = record.and_then(|r| r.tools.get(&tool));
        let status = row_status(record, confirmed, &tool, &digest);
        let changed: Vec<String> = match (status, pinned) {
            ("changed", Some(p)) => changed_members(&p.members, &members)
                .into_iter()
                .map(|m| server_name(&m, redactions))
                .collect(),
            _ => Vec::new(),
        };
        rows.push(ToolRow {
            function: function.name.clone(),
            tool: tool.clone(),
            digest: Some(digest.clone()),
            status,
            title: raw
                .get("title")
                .and_then(Value::as_str)
                .map(|t| server_name(t, redactions)),
            description: description.clone(),
            pinned_description: (status == "changed")
                .then(|| pinned.map(|p| Redaction::apply_all(&p.description, redactions)))
                .flatten(),
            members: changed,
            annotations: raw
                .get("annotations")
                .cloned()
                .map(|v| redact_tree(v, redactions).0),
            input_schema: raw
                .get("inputSchema")
                .cloned()
                .map(|v| redact_tree(v, redactions).0),
        });
        pins.entry(tool).or_insert_with(|| ToolPin {
            digest,
            description: description.unwrap_or_default(),
            members,
        });
    }
    let mapped: Vec<&str> = doc
        .functions
        .iter()
        .filter_map(|f| f.mcp.as_ref().map(|m| m.tool.as_str()))
        .collect();
    let unmapped = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .filter(|n| !mapped.contains(n))
        .map(|n| server_name(n, redactions))
        .collect();
    let digests: BTreeMap<String, String> = pins
        .iter()
        .map(|(k, v)| (k.clone(), v.digest.clone()))
        .collect();
    let command = doc
        .mcp
        .as_ref()
        .and_then(|m| m.command.clone())
        .unwrap_or_default();
    let program_status = apb_core::connector::mcp::pins::program_status(
        record,
        &confirmed.program,
        &confirmed.program_digest,
    );
    ToolListing {
        connector: connector.to_string(),
        account: account.to_string(),
        connector_digest: confirmed.connector_digest.clone(),
        account_digest: confirmed.account_digest.clone(),
        era,
        cleanup,
        program: ProgramFacts {
            launcher: apb_core::connector::mcp::launcher_name(&command).map(str::to_string),
            command,
            program: confirmed.program.clone(),
            program_digest: confirmed.program_digest.clone(),
            status: program_status,
        },
        rows,
        unmapped,
        pin_digest: pin_digest(
            &confirmed.connector_digest,
            &confirmed.account_digest,
            &confirmed.program,
            &confirmed.program_digest,
            &digests,
        ),
        pins,
    }
}

/// A row's status against the record, through `ServerPin::compare`: a
/// record bound to another tree, account or program pins nothing.
fn row_status(
    record: Option<&apb_core::connector::mcp::pins::ServerPin>,
    confirmed: &ConfirmedProgram,
    tool: &str,
    digest: &str,
) -> &'static str {
    use apb_core::connector::mcp::pins::{BindingDiff, LiveBinding, ProgramFact};
    let Some(record) = record else {
        return "unpinned";
    };
    let live = LiveBinding {
        connector_digest: &confirmed.connector_digest,
        account_digest: &confirmed.account_digest,
        program: ProgramFact::Digest(&confirmed.program_digest),
        tool,
        tool_digest: Some(digest),
    };
    match record.compare(&live) {
        Ok(()) => "pinned",
        Err(BindingDiff::Tool { .. }) => "changed",
        Err(_) => "unpinned",
    }
}

/// Each top-level member of a raw tool definition (minus
/// `UNPINNED_MEMBERS`) -> `sha256:<hex>` of its canonical JSON.
fn member_digests(raw: &Value) -> BTreeMap<String, String> {
    use apb_core::connector::mcp::digest::{UNPINNED_MEMBERS, canonical_json};
    raw.as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, _)| !UNPINNED_MEMBERS.contains(&k.as_str()))
                .map(|(k, v)| {
                    (
                        k.clone(),
                        apb_core::content::sha256_hex(canonical_json(v).as_bytes()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The members whose digest differs, or that one side lacks.
fn changed_members(
    pinned: &BTreeMap<String, String>,
    now: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut names: Vec<String> = pinned
        .keys()
        .chain(now.keys())
        .filter(|k| pinned.get(*k) != now.get(*k))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// A description as a listing keeps it: redacted, then cut to
/// `DESCRIPTION_CHARS` characters on a character boundary, never before
/// the redaction.
fn display_description(text: &str, redactions: &[Redaction]) -> String {
    Redaction::apply_all(text, redactions)
        .chars()
        .take(DESCRIPTION_CHARS)
        .collect()
}

/// Writes the record of `listing` (`PinStore::record`): its tree and
/// account digests, the program, the prepared pins (every tool that is not
/// `missing`) and the pin digest. Called only after the person's
/// confirmation.
pub fn record(listing: &ToolListing) -> Result<(), CallError> {
    use apb_core::connector::mcp::pins::{PinStore, ServerPin};
    PinStore::record(
        &listing.connector,
        &listing.account,
        ServerPin {
            connector_digest: listing.connector_digest.clone(),
            account_digest: listing.account_digest.clone(),
            command: listing.program.command.clone(),
            program: listing.program.program.clone(),
            program_digest: listing.program.program_digest.clone(),
            tools: listing.pins.clone(),
            pin_digest: listing.pin_digest.clone(),
            approved_at_ms: apb_core::clock::now_ms(),
        },
    )
    .map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))
}

/// Starts the pinned server of `connector`/`account`, lists its tools,
/// compares the whole binding of every mcp function's tool and closes the
/// session. Makes no `tools/call`, journals nothing, writes nothing. The
/// same `{ "ok": bool, .. }` shape `healthcheck` returns.
pub(crate) fn healthcheck(root: &Path, connector: &str, account: &str) -> (Value, bool) {
    match check_live(root, connector, account) {
        Ok(value) => (value, true),
        Err(e) => (e.to_json(), false),
    }
}

/// The healthcheck's steps (design D4 "The four orders", list 3).
fn check_live(root: &Path, connector: &str, account: &str) -> Result<Value, CallError> {
    use crate::connector::call::LiveTarget;
    use crate::connector::call::auth::{non_secret_fields, resolve_secrets};
    let loaded = apb_core::connector::store::load(connector).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}`: {e}"),
        )
    })?;
    let hc = loaded.doc.healthcheck.clone().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("connector `{connector}` declares no healthcheck"),
        )
    })?;
    let live = LiveTarget::load(root, connector, Some(account), &hc)?;
    live.require_trust()?;
    let pins = live.pins();
    let deadline = Instant::now() + Duration::from_secs(live.function().timeout_sec);
    let target = live.into_target()?;
    let fields = non_secret_fields(&target.maccount);
    let tools: Vec<(String, String)> = target
        .doc
        .functions
        .iter()
        .filter_map(|f| f.mcp.as_ref().map(|m| (f.name.clone(), m.tool.clone())))
        .collect();
    let input = super::McpConnectInput {
        doc: &target.doc,
        connector_digest: &pins.connector,
        account_digest: &pins.account,
        function: &target.function,
        connector,
        account_name: &target.account,
        account_fields: &fields,
        full: true,
        deadline,
    };
    let mut checked = super::check_tools(&input, tools.clone())?;
    let (secrets, redactions) = resolve_secrets(root, &target.maccount, deadline)?;
    let opened = super::open(&mut checked, &secrets, &redactions)
        .map_err(|e| super::redacted(e, &redactions))?;
    for (function, tool) in &tools {
        if let Err(e) = opened.verify(&checked, function, tool, &redactions) {
            return Err(super::redacted(opened.fail(e), &redactions));
        }
    }
    let era = opened.era.clone();
    let mut value = json!({
        "ok": true,
        "mcp": { "era": era, "tools_checked": tools.len() },
    });
    if let Some(w) = opened.session.close_noted() {
        value["warnings"] = json!([w]);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::connector::config;
    use apb_core::connector::mcp::pins::PinStore;
    use apb_core::connector::store;
    use apb_core::secret::Secret;
    use apb_core::trust::{Kind, OriginKind, TrustStore};
    use apb_mcp_client::testing::{StubEra, StubServer};
    use std::path::PathBuf;

    const CONNECTOR: &str = "tracker-mcp";
    const ACCOUNT: &str = "work";
    const TOKEN_VAR: &str = "APB_PIN_TEST_TOKEN";
    const TOKEN: &str = "pin-token-value-7731";

    /// Runs `body` alone in a child process: the tests set the config dir
    /// and the account's variables process-wide.
    fn child(name: &str, body: impl FnOnce()) {
        apb_core::process_mode::testing::in_plain_child(
            &format!("connector::mcp::pin::tests::{name}"),
            body,
        );
    }

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
  - name: list_comments
    description: Recent comments of one issue
    read_only: true
    mcp: {{ tool: list_comments }}
  - name: create_comment
    description: Add a comment to an issue
    mcp: {{ tool: create_comment }}
    idempotency: {{ mode: none }}
"#
        )
    }

    /// The stub's tools: three mapped ones (`list_comments` is not listed)
    /// and one no function maps.
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

        /// A config dir with the connector (`edit` of the default one)
        /// installed and its tree approved, a project account and a stub.
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
            fx.write_account(&format!("{{{{env.{TOKEN_VAR}}}}}"), "acme");
            fx
        }

        /// Installs `yaml` as the connector and approves its tree.
        fn install(&self, yaml: &str) {
            let dir = self.cfg.join("connectors").join(CONNECTOR);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("connector.yaml"), yaml).unwrap();
            let loaded = store::load(CONNECTOR).unwrap();
            TrustStore::load()
                .approve_kind(
                    &loaded.digest,
                    CONNECTOR,
                    Kind::Connector,
                    OriginKind::LocallyApproved,
                )
                .unwrap();
        }

        /// The account file: a helper-sourced token lives in the global
        /// account file (a project account may not read a command).
        fn write_account(&self, token: &str, workspace: &str) {
            let project = self
                .root
                .join(".apb/connector-config")
                .join(format!("{CONNECTOR}.yaml"));
            let global = config::global_config_path(CONNECTOR).unwrap();
            let _ = std::fs::remove_file(&project);
            let _ = std::fs::remove_file(&global);
            let path = if token.contains("{{cmd:") {
                global
            } else {
                project
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "accounts:\n  - name: {ACCOUNT}\n    default: true\n    workspace: {workspace}\n    token: \"{token}\"\n"
                ),
            )
            .unwrap();
        }

        /// The account's token from a helper that leaves `helper-ran`.
        fn use_helper(&self) {
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
            self.write_account(&format!("{{{{cmd:{}}}}}", helper.display()), "acme");
        }

        fn marker(&self) -> PathBuf {
            self.base.join("helper-ran")
        }

        fn account(&self) -> config::Account {
            config::load_merged(&self.root, CONNECTOR)
                .unwrap()
                .into_iter()
                .find(|a| a.name == ACCOUNT)
                .unwrap()
        }

        /// What the first confirmation shows, as a person confirms it.
        fn confirmed(&self) -> ConfirmedProgram {
            let (program, program_digest) =
                resolve_program(&self.program.to_string_lossy(), std::ffi::OsStr::new("")).unwrap();
            ConfirmedProgram {
                connector_digest: store::load(CONNECTOR).unwrap().digest,
                account_digest: config::account_digest(&self.account()),
                program,
                program_digest,
            }
        }

        fn list(&self) -> Result<ToolListing, CallError> {
            list_live(&self.root, CONNECTOR, ACCOUNT, &self.confirmed())
        }

        fn starts(&self) -> usize {
            self.stub.starts().len()
        }

        fn config_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
            fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
                for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(&self.cfg, &mut out);
            out
        }
    }

    fn row<'a>(listing: &'a ToolListing, function: &str) -> &'a ToolRow {
        listing
            .rows
            .iter()
            .find(|r| r.function == function)
            .unwrap_or_else(|| panic!("no row {function}"))
    }

    fn token_redaction() -> Vec<Redaction> {
        vec![Redaction::new(Secret::new(TOKEN), "token")]
    }

    #[test]
    fn resolve_program_finds_a_bare_name_on_the_given_path() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let stub = StubServer::write(&base.join("bin"), StubEra::Modern).unwrap();
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        // A file of that name that is not executable is passed over.
        std::fs::write(empty.join("stub-mcp.sh"), "not a program").unwrap();
        let path = std::env::join_paths([empty, stub.dir.clone()]).unwrap();
        let (program, digest) = resolve_program("stub-mcp.sh", &path).unwrap();
        assert_eq!(
            PathBuf::from(&program),
            std::fs::canonicalize(stub.program()).unwrap()
        );
        let bytes = std::fs::read(stub.program()).unwrap();
        assert_eq!(digest, apb_core::content::sha256_hex(&bytes));
        let err = resolve_program("no-such-program", &path).unwrap_err();
        assert!(err.message.contains("was not found"), "{}", err.message);
    }

    #[test]
    fn resolve_program_canonicalizes_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let stub = StubServer::write(&base.join("bin"), StubEra::Modern).unwrap();
        let link = base.join("link-to-stub");
        std::os::unix::fs::symlink(stub.program(), &link).unwrap();
        let (program, _) =
            resolve_program(&link.to_string_lossy(), std::ffi::OsStr::new("")).unwrap();
        assert_eq!(
            PathBuf::from(program),
            std::fs::canonicalize(stub.program()).unwrap()
        );
    }

    #[test]
    fn list_live_marks_each_row() {
        child("list_live_marks_each_row", || {
            let fx = Fx::new();
            record(&fx.list().unwrap()).unwrap();
            // The pin of `create_comment` is dropped, `search_issues` gets a
            // longer description.
            let mut pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            pin.tools.remove("create_comment");
            PinStore::record(CONNECTOR, ACCOUNT, pin).unwrap();
            let mut tools = tools();
            tools[1]["description"] = json!("Search issues by text, now longer");
            fx.stub.set_tools(&tools).unwrap();
            let listing = fx.list().unwrap();
            assert_eq!(row(&listing, "whoami").status, "pinned");
            let changed = row(&listing, "search_issues");
            assert_eq!(changed.status, "changed");
            assert_eq!(changed.members, vec!["description".to_string()]);
            assert_eq!(
                changed.pinned_description.as_deref(),
                Some("Search issues by text")
            );
            assert_eq!(
                changed.description.as_deref(),
                Some("Search issues by text, now longer")
            );
            assert_eq!(row(&listing, "create_comment").status, "unpinned");
            let missing = row(&listing, "list_comments");
            assert_eq!(missing.status, "missing");
            assert_eq!(missing.digest, None);
            assert_eq!(listing.unmapped, vec!["admin_tool".to_string()]);
            assert_eq!(listing.program.status, "same");
            assert!(listing.era.starts_with("modern"), "{}", listing.era);
        });
    }

    #[test]
    fn list_live_needs_the_connector_approved_only() {
        child("list_live_needs_the_connector_approved_only", || {
            let fx = Fx::new();
            // The account was never approved: the listing still runs.
            let listing = fx.list().unwrap();
            assert_eq!(row(&listing, "whoami").status, "unpinned");
            // A tree nobody approved starts nothing.
            let dir = fx.cfg.join("connectors").join(CONNECTOR);
            std::fs::write(
                dir.join("connector.yaml"),
                connector_yaml(&fx.program.to_string_lossy())
                    .replace("Search issues by text", "Search issues"),
            )
            .unwrap();
            let confirmed = fx.confirmed();
            let before = fx.starts();
            let err = list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap_err();
            assert!(err.message.contains("is not approved"), "{}", err.message);
            assert_eq!(fx.starts(), before);
        });
    }

    #[test]
    fn list_live_writes_nothing() {
        child("list_live_writes_nothing", || {
            let fx = Fx::new();
            let before = fx.config_files();
            fx.list().unwrap();
            assert_eq!(fx.config_files(), before);
        });
    }

    #[test]
    fn list_live_binds_the_tree_digest() {
        child("list_live_binds_the_tree_digest", || {
            let fx = Fx::new();
            let listing = fx.list().unwrap();
            assert_eq!(
                listing.connector_digest,
                store::load(CONNECTOR).unwrap().digest
            );
            record(&listing).unwrap();
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert_eq!(pin.connector_digest, listing.connector_digest);
        });
    }

    #[test]
    fn list_live_binds_the_account_digest() {
        child("list_live_binds_the_account_digest", || {
            let fx = Fx::new();
            let listing = fx.list().unwrap();
            assert_eq!(
                listing.account_digest,
                config::account_digest(&fx.account())
            );
            record(&listing).unwrap();
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert_eq!(pin.account_digest, listing.account_digest);
        });
    }

    #[test]
    fn record_writes_the_listing_whole() {
        child("record_writes_the_listing_whole", || {
            let fx = Fx::new();
            let listing = fx.list().unwrap();
            record(&listing).unwrap();
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert_eq!(pin.program, listing.program.program);
            assert_eq!(pin.program_digest, listing.program.program_digest);
            assert_eq!(pin.command, fx.program.to_string_lossy());
            assert_eq!(pin.pin_digest, listing.pin_digest);
            let pinned: Vec<&String> = pin.tools.keys().collect();
            // Every listed mapped tool, never the missing one or an unmapped one.
            assert_eq!(pinned, vec!["create_comment", "search_issues", "whoami"]);
            for r in listing.rows.iter().filter(|r| r.status != "missing") {
                assert_eq!(Some(&pin.tools[&r.tool].digest), r.digest.as_ref());
            }
        });
    }

    #[test]
    fn a_reflected_secret_refuses_the_listing_before_hashing() {
        child(
            "a_reflected_secret_refuses_the_listing_before_hashing",
            || {
                let fx = Fx::new();
                let reflect: &[fn(&mut Value)] = &[
                    |t| t[3]["name"] = json!(format!("tool-{TOKEN}")),
                    |t| t[0]["description"] = json!(format!("uses {TOKEN}")),
                    |t| t[1]["annotations"] = json!({ "title": TOKEN }),
                    |t| t[1]["inputSchema"]["properties"] = json!({ TOKEN: { "type": "string" } }),
                    |t| {
                        t[2]["inputSchema"]["properties"] =
                            json!({ "body": { "examples": [{ "deep": TOKEN }] } })
                    },
                    |t| t[3]["description"] = json!(TOKEN),
                ];
                let before = fx.config_files();
                for edit in reflect {
                    let mut tools = tools();
                    edit(&mut tools);
                    fx.stub.set_tools(&tools).unwrap();
                    let err = fx.list().unwrap_err();
                    assert!(
                        err.message.starts_with("mcp_tools_reflect_a_secret:"),
                        "{}",
                        err.message
                    );
                    for name in [
                        "whoami",
                        "search_issues",
                        "create_comment",
                        "admin_tool",
                        TOKEN,
                    ] {
                        assert!(!err.message.contains(name), "{}", err.message);
                    }
                    assert!(!err.message.contains("sha256:"), "{}", err.message);
                }
                assert_eq!(fx.config_files(), before);
            },
        );
    }

    #[test]
    fn the_display_walk_rewrites_member_names() {
        let (shown, changed) = redact_tree(
            json!({ "properties": { TOKEN: { "description": format!("a {TOKEN}") } }, "n": 1 }),
            &token_redaction(),
        );
        assert!(changed);
        let text = shown.to_string();
        assert!(!text.contains(TOKEN), "{text}");
        assert!(text.contains("[redacted:token]"), "{text}");
        assert_eq!(shown["n"], json!(1));
    }

    #[test]
    fn a_cleanup_note_names_every_end_not_shown() {
        use apb_mcp_client::{Ended, GroupEnd};
        for ended in [
            Ended::Unconfirmed,
            Ended::Reaped {
                killed: true,
                group: GroupEnd::NotShownEmpty,
            },
        ] {
            let note = cleanup_note(ACCOUNT, &ended).expect("a note");
            assert!(
                note.contains("may still be running with the secrets of account `work`"),
                "{note}"
            );
        }
        for ended in [
            Ended::NotStarted,
            Ended::Reaped {
                killed: false,
                group: GroupEnd::Empty,
            },
        ] {
            assert_eq!(cleanup_note(ACCOUNT, &ended), None);
        }
    }

    #[test]
    fn a_server_name_is_cut_after_the_redaction() {
        use super::super::redact::SERVER_NAME_CHARS;
        let text = format!(
            "{}{TOKEN}{}",
            "a".repeat(SERVER_NAME_CHARS - 5),
            "b".repeat(50)
        );
        let shown = server_name(&text, &token_redaction());
        assert!(!shown.contains(&TOKEN[..5]), "{shown}");
        assert!(shown.contains("[cut: "), "{shown}");
        assert_eq!(server_name("short", &token_redaction()), "short");
    }

    /// Titles, unmapped names and member names the server sends are
    /// bounded in the listing itself, not only on the terminal.
    #[test]
    fn a_listing_bounds_every_server_name() {
        child("a_listing_bounds_every_server_name", || {
            use super::super::redact::SERVER_NAME_CHARS;
            let fx = Fx::new();
            let long = "t".repeat(5 * SERVER_NAME_CHARS);
            let mut tools = tools();
            tools[0]["title"] = json!(long);
            tools[0][long.as_str()] = json!(1);
            tools
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": long, "inputSchema": { "type": "object" } }));
            fx.stub.set_tools(&tools).unwrap();
            let listing = fx.list().unwrap();
            assert_eq!(listing.cleanup, None);
            let bound = SERVER_NAME_CHARS + 40;
            let title = row(&listing, "whoami").title.clone().unwrap();
            assert!(title.chars().count() < bound, "{}", title.len());
            assert!(listing.unmapped.iter().all(|n| n.chars().count() < bound));
            assert!(
                listing
                    .pins
                    .values()
                    .all(|p| p.members.keys().all(|k| k.chars().count() < bound)),
                "a member name was kept whole"
            );
        });
    }

    #[test]
    fn a_description_is_cut_after_the_redaction() {
        let text = format!("{}{TOKEN}", "a".repeat(DESCRIPTION_CHARS - 5));
        let shown = display_description(&text, &token_redaction());
        assert_eq!(shown.chars().count(), DESCRIPTION_CHARS);
        assert!(!shown.contains(&TOKEN[..5]), "{shown}");
        assert!(shown.ends_with("[reda"), "{shown}");
    }

    #[test]
    fn list_live_runs_only_after_the_program_confirmation() {
        child("list_live_runs_only_after_the_program_confirmation", || {
            let fx = Fx::new();
            fx.use_helper();
            // What the first confirmation shows resolves no secret and
            // starts nothing.
            let confirmed = fx.confirmed();
            let doc = store::load(CONNECTOR).unwrap().doc;
            crate::connector::mcp::preview_start(&doc, &fx.account()).unwrap();
            assert!(!fx.marker().exists());
            assert_eq!(fx.starts(), 0);
            list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap();
            assert!(fx.marker().exists());
            assert_eq!(fx.starts(), 1);
        });
    }

    #[test]
    fn list_live_with_an_unreadable_pin_store_starts_nothing() {
        child(
            "list_live_with_an_unreadable_pin_store_starts_nothing",
            || {
                let fx = Fx::new();
                fx.use_helper();
                let confirmed = fx.confirmed();
                std::fs::write(fx.cfg.join("mcp-tools.json"), "not json").unwrap();
                list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap_err();
                assert!(!fx.marker().exists(), "no secret helper ran");
                assert_eq!(fx.starts(), 0, "no server started");
            },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_program_refuses_a_path_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let odd = base.join(std::ffi::OsStr::from_bytes(b"bin-\xff"));
        let stub = StubServer::write(&odd, StubEra::Modern).unwrap();
        let link = base.join("link-to-stub");
        std::os::unix::fs::symlink(stub.program(), &link).unwrap();
        let err = resolve_program(&link.to_string_lossy(), std::ffi::OsStr::new("")).unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
        assert!(err.message.contains("not UTF-8"), "{}", err.message);
    }

    #[test]
    fn list_live_refuses_an_account_changed_since_confirmation() {
        child(
            "list_live_refuses_an_account_changed_since_confirmation",
            || {
                let fx = Fx::new();
                fx.use_helper();
                let confirmed = fx.confirmed();
                let helper = fx.base.join("helper.sh");
                fx.write_account(&format!("{{{{cmd:{}}}}}", helper.display()), "other");
                let err = list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap_err();
                assert!(
                    err.message.starts_with("mcp_approve_account_changed:"),
                    "{}",
                    err.message
                );
                assert!(!fx.marker().exists());
                assert_eq!(fx.starts(), 0);
            },
        );
    }

    #[test]
    fn list_live_refuses_a_tree_changed_since_confirmation() {
        child(
            "list_live_refuses_a_tree_changed_since_confirmation",
            || {
                let fx = Fx::new();
                fx.use_helper();
                let confirmed = fx.confirmed();
                // Another definition, approved too: only the comparison refuses.
                fx.install(
                    &connector_yaml(&fx.program.to_string_lossy())
                        .replace("Search issues by text", "Search issues"),
                );
                let err = list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap_err();
                assert!(
                    err.message.starts_with("mcp_approve_definition_changed:"),
                    "{}",
                    err.message
                );
                assert!(!fx.marker().exists());
                assert_eq!(fx.starts(), 0);
            },
        );
    }

    #[test]
    fn list_live_uses_the_fixed_timeout() {
        child("list_live_uses_the_fixed_timeout", || {
            let fx = Fx::with(|y| {
                y.replace(
                    "  transport: stdio\n",
                    "  transport: stdio\n  protocol: modern\n",
                )
                .replace(
                    "    description: The account the server acts as\n",
                    "    description: The account the server acts as\n    timeout_sec: 2\n",
                )
            });
            // A program that takes three seconds before it serves: a bound
            // of two seconds would fail the listing.
            let slow = fx.base.join("slow");
            std::fs::create_dir_all(&slow).unwrap();
            let wrapper = slow.join("slow-mcp.sh");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nsleep 3\nexec '{}' \"$@\"\n",
                    fx.program.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            let (program, program_digest) =
                resolve_program(&wrapper.to_string_lossy(), std::ffi::OsStr::new("")).unwrap();
            let confirmed = ConfirmedProgram {
                program,
                program_digest,
                ..fx.confirmed()
            };
            let listing = list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap();
            assert_eq!(row(&listing, "whoami").status, "unpinned");
        });
    }

    #[test]
    fn record_writes_the_prepared_pins() {
        child("record_writes_the_prepared_pins", || {
            let fx = Fx::new();
            let listing = fx.list().unwrap();
            let prepared = listing.pins.clone();
            // The server changes after the listing: the record still holds
            // what the person saw.
            let mut changed = tools();
            changed[0]["description"] = json!("changed after the listing");
            fx.stub.set_tools(&changed).unwrap();
            record(&listing).unwrap();
            let pin = PinStore::load()
                .unwrap()
                .server(CONNECTOR, ACCOUNT)
                .cloned()
                .unwrap();
            assert_eq!(pin.tools, prepared);
            assert_eq!(pin.tools["whoami"].members, member_digests(&tools()[0]));
            assert_eq!(
                pin.tools["whoami"].description,
                "The account the server acts as"
            );
        });
    }

    #[test]
    fn list_live_takes_only_the_confirmed_program() {
        child("list_live_takes_only_the_confirmed_program", || {
            let fx = Fx::new();
            fx.use_helper();
            let other = fx.base.join("other.sh");
            std::fs::write(&other, "#!/bin/sh\nexit 0\n").unwrap();
            let wrong_digest = ConfirmedProgram {
                program_digest: format!("sha256:{}", "0".repeat(64)),
                ..fx.confirmed()
            };
            let wrong_path = ConfirmedProgram {
                program: other.to_string_lossy().to_string(),
                ..fx.confirmed()
            };
            for confirmed in [wrong_digest, wrong_path] {
                let err = list_live(&fx.root, CONNECTOR, ACCOUNT, &confirmed).unwrap_err();
                assert!(
                    err.message.starts_with("mcp_server_changed:"),
                    "{}",
                    err.message
                );
            }
            assert!(!fx.marker().exists());
            assert_eq!(fx.starts(), 0);
        });
    }

    #[test]
    fn preview_start_renders_without_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("helper-ran");
        let yaml = r#"name: tracker-mcp
version: 0.1.0
mcp:
  transport: stdio
  command: /usr/bin/stub
  args: ["--base={{account.base_url}}"]
  env:
    PATH: "{{account.bin_dir}}:/usr/bin"
    HTTPS_PROXY: "{{account.proxy}}"
    TOKEN: "{{secret.token}}"
    PAIR: "{{secret.token}}/{{secret.key}}"
account_fields:
  - { name: base_url }
  - { name: bin_dir }
  - { name: proxy }
  - { name: token, secret: true }
  - { name: key, secret: true }
functions:
  - name: whoami
    description: Who
    read_only: true
    mcp: { tool: whoami }
"#;
        let doc = apb_core::connector::def::ConnectorDoc::from_yaml(yaml, "tracker-mcp").unwrap();
        let helper = format!("sh -c 'echo ran >> {}'", marker.display());
        let account = config::Account {
            name: ACCOUNT.to_string(),
            default: true,
            fields: BTreeMap::from([
                ("base_url".to_string(), "https://tracker.test".to_string()),
                ("bin_dir".to_string(), "/opt/tracker/bin".to_string()),
                ("proxy".to_string(), "http://proxy.test:3128".to_string()),
                ("token".to_string(), format!("{{{{cmd:{helper}}}}}")),
                ("key".to_string(), "{{env.TRACKER_KEY}}".to_string()),
            ]),
        };
        let p = crate::connector::mcp::preview_start(&doc, &account).unwrap();
        assert_eq!(p.args, vec!["--base=https://tracker.test".to_string()]);
        let env: BTreeMap<_, _> = p.env.into_iter().collect();
        assert_eq!(env["PATH"], "/opt/tracker/bin:/usr/bin");
        assert_eq!(env["HTTPS_PROXY"], "http://proxy.test:3128");
        assert_eq!(env["TOKEN"], "[secret token from cmd]");
        assert_eq!(
            env["PAIR"],
            "[secret token from cmd]/[secret key from env TRACKER_KEY]"
        );
        let fields: Vec<&str> = p.fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(fields, vec!["base_url", "bin_dir", "proxy"]);
        assert_eq!(p.secret_commands, vec![("token".to_string(), helper)]);
        assert!(!marker.exists());
    }
}
