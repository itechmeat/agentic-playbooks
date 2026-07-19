//! `apb connector` subcommands (spec 2026-07-18-connectors-design section
//! 10): list/show/call/doctor/env/init over the connector store. `call` is
//! the thin CLI wrapper `apb_engine::connector_call` documents itself as -
//! run context comes only from `APB_RUN_DIR`/`APB_NODE_ID`, both set by the
//! engine adapter when a node actually executes a connector call; everything
//! else here is a read-only or scaffolding convenience for a human or an
//! agent debugging outside a run.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use apb_core::connector::config::{self};
use apb_core::connector::secrets;
use apb_core::connector::store::{self, LoadedConnector};
use apb_core::doctor::{Check, CheckStatus};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::util::print_json;

#[derive(Subcommand)]
pub(crate) enum ConnectorAction {
    /// List installed connectors, their trust and config status
    List,
    /// Show one connector's manifest summary and account status
    Show { name: String },
    /// Call a connector function - the agent-facing call channel (also
    /// usable by a human for debugging). Requires a run context: this
    /// process must be spawned by the engine with `APB_RUN_DIR` and
    /// `APB_NODE_ID` set.
    Call {
        name: String,
        function: String,
        /// Explicit account name; omit to use the single or default granted
        /// account
        #[arg(long)]
        account: Option<String>,
        /// Call arguments as a JSON document, or "-" to read them from
        /// stdin; defaults to "{}"
        #[arg(long)]
        args: Option<String>,
        /// Render the call (URL/body) without executing it or resolving
        /// secrets
        #[arg(long)]
        dry_run: bool,
        /// Return the complete response body, skipping the function's
        /// response_pick projection (spec 4.5 debugging escape)
        #[arg(long)]
        full: bool,
    },
    /// Approve trust for a connector or one of its accounts. With no flag,
    /// approves the connector's current tree digest; with --account, approves
    /// that account's current non-secret-field digest. A deliberate user
    /// action: connector and account trust guard secret egress and are never
    /// bypassed by a run's acknowledge_untrusted (spec 7).
    Approve {
        name: String,
        /// Approve this account's digest instead of the connector digest
        #[arg(long)]
        account: Option<String>,
    },
    /// Check every installed connector: manifest, config, env resolution,
    /// trust status, healthcheck declaration
    Doctor,
    /// Print the env var names configured accounts need that do not
    /// currently resolve, as ready-to-paste `KEY=` lines. Names only, never
    /// values.
    Env {
        name: Option<String>,
        /// Append the missing `KEY=` template lines to
        /// `<root>/.apb/secrets.env` (creating it 0600 when absent),
        /// preserving existing content and never duplicating a key already in
        /// the file, then ensure `.gitignore` covers it. Values stay empty for
        /// the user to fill in. Without this flag, the lines only print.
        #[arg(long)]
        write: bool,
    },
    /// Scaffold a new connector folder from the embedded template
    Init { name: String },
}

pub(crate) fn connector_cmd(root: &Path, action: ConnectorAction) -> ExitCode {
    match action {
        ConnectorAction::List => list_cmd(root),
        ConnectorAction::Show { name } => show_cmd(root, &name),
        ConnectorAction::Call {
            name,
            function,
            account,
            args,
            dry_run,
            full,
        } => call_cmd(root, &name, &function, account, args, dry_run, full),
        ConnectorAction::Approve { name, account } => approve_cmd(root, &name, account.as_deref()),
        ConnectorAction::Doctor => doctor_cmd(root),
        ConnectorAction::Env { name, write } => env_cmd(root, name, write),
        ConnectorAction::Init { name } => init_cmd(&name),
    }
}

// --- list -----------------------------------------------------------------

fn list_cmd(root: &Path) -> ExitCode {
    let summaries = store::list();
    if summaries.is_empty() {
        println!("no connectors installed (see `apb connector init <name>`)");
        return ExitCode::SUCCESS;
    }

    let trust = TrustStore::load();
    let approved_connector_ids = trust.approved_record_ids(Kind::Connector);

    let mut rows: Vec<[String; 4]> = vec![[
        "NAME".to_string(),
        "VERSION".to_string(),
        "TRUST".to_string(),
        "ACCOUNTS".to_string(),
    ]];
    for s in &summaries {
        let trust_state = match store::load(&s.name) {
            Ok(loaded) => {
                if trust.is_approved(&loaded.digest) {
                    "approved"
                } else if approved_connector_ids.iter().any(|id| id == &s.name) {
                    "changed"
                } else {
                    "unapproved"
                }
            }
            Err(_) => "invalid",
        };
        let accounts_count = config::load_merged(root, &s.name)
            .map(|a| a.len())
            .unwrap_or(0);
        rows.push([
            s.name.clone(),
            s.version.clone(),
            trust_state.to_string(),
            accounts_count.to_string(),
        ]);
    }
    print_table(&rows);
    ExitCode::SUCCESS
}

/// Prints rows as a whitespace-aligned table (first row is the header).
fn print_table(rows: &[[String; 4]]) {
    let mut widths = [0usize; 4];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(&format!("{:<width$}", cell, width = widths[i]));
        }
        println!("{}", line.trim_end());
    }
}

// --- show -------------------------------------------------------------

fn show_cmd(root: &Path, name: &str) -> ExitCode {
    let loaded = match store::load(name) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("connector error: {e}");
            return ExitCode::from(2);
        }
    };
    let accounts = match config::load_merged(root, name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("connector error: {e}");
            return ExitCode::from(2);
        }
    };

    let functions: Vec<Value> = loaded
        .doc
        .functions
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "description": f.description,
                "read_only": f.read_only,
                "deprecated": f.deprecated,
            })
        })
        .collect();

    let account_fields: Vec<Value> = loaded
        .doc
        .account_fields
        .iter()
        .map(|f| json!({ "name": f.name, "required": f.required, "secret": f.secret }))
        .collect();

    let secret_fields = loaded.doc.secret_fields();
    let accounts_json: Vec<Value> = accounts
        .iter()
        .map(|a| {
            let env_refs = config::env_refs(&loaded.doc, a);
            let env: Vec<Value> = env_refs
                .iter()
                .map(|(field, var)| {
                    json!({
                        "field": field,
                        "var": var,
                        "set": secrets::resolve_var(root, var).is_some(),
                    })
                })
                .collect();
            // Non-secret fields only: a secret field's config value is the
            // raw `{{env.VAR}}` reference, not the value itself, but `show`
            // must never print anything secret-shaped even by proxy.
            let fields: serde_json::Map<String, Value> = a
                .fields
                .iter()
                .filter(|(k, _)| !secret_fields.iter().any(|s| s == *k))
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            json!({
                "name": a.name,
                "default": a.default,
                "fields": Value::Object(fields),
                "env": env,
            })
        })
        .collect();

    let out = json!({
        "name": loaded.name,
        "version": loaded.doc.version,
        "healthcheck": loaded.doc.healthcheck,
        "functions": functions,
        "account_fields": account_fields,
        "accounts": accounts_json,
    });
    print_json(&out);
    ExitCode::SUCCESS
}

// --- approve ----------------------------------------------------------

/// Approves the connector's current tree digest, or with `account` the current
/// non-secret-field digest of that account (spec 7). Prints the concrete fields
/// approved for an account so the user sees exactly what they trusted. Secret
/// fields carry only their raw `{{env.VAR}}` reference in the config, never the
/// value, so printing every field is safe.
fn approve_cmd(root: &Path, name: &str, account: Option<&str>) -> ExitCode {
    let loaded = match store::load(name) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("connector error: {e}");
            return ExitCode::from(2);
        }
    };
    match account {
        None => {
            let mut trust = TrustStore::load();
            if let Err(e) = trust.approve_kind(
                &loaded.digest,
                name,
                Kind::Connector,
                OriginKind::LocallyApproved,
            ) {
                eprintln!("connector error: cannot record approval: {e}");
                return ExitCode::from(2);
            }
            println!("approved connector `{name}` (digest {})", loaded.digest);
            ExitCode::SUCCESS
        }
        Some(acct_name) => {
            let accounts = match config::load_merged(root, name) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("connector error: {e}");
                    return ExitCode::from(2);
                }
            };
            let Some(account) = accounts.iter().find(|a| a.name == acct_name) else {
                eprintln!("connector error: account `{acct_name}` not configured for `{name}`");
                return ExitCode::from(2);
            };
            let digest = config::account_digest(account);
            let id = account_trust_id(name, acct_name);
            let mut trust = TrustStore::load();
            if let Err(e) = trust.approve_kind(
                &digest,
                &id,
                Kind::ConnectorAccount,
                OriginKind::LocallyApproved,
            ) {
                eprintln!("connector error: cannot record approval: {e}");
                return ExitCode::from(2);
            }
            // Show the non-secret identity approved (name, default, fields).
            let fields = serde_json::Map::from_iter(
                account.fields.iter().map(|(k, v)| (k.clone(), json!(v))),
            );
            print_json(&json!({
                "approved": id,
                "digest": digest,
                "default": account.default,
                "fields": Value::Object(fields),
            }));
            ExitCode::SUCCESS
        }
    }
}

// --- call -------------------------------------------------------------

fn call_cmd(
    root: &Path,
    name: &str,
    function: &str,
    account: Option<String>,
    args: Option<String>,
    dry_run: bool,
    full: bool,
) -> ExitCode {
    let run_dir = std::env::var("APB_RUN_DIR").ok();
    let node_id = std::env::var("APB_NODE_ID").ok();
    let (run_dir, node_id) = match (run_dir, node_id) {
        (Some(r), Some(n)) => (r, n),
        _ => {
            print_call_result(&call_error_json(
                "config",
                "apb connector call requires a run context: set APB_RUN_DIR (the runs/<id> \
                 directory of the current run) and APB_NODE_ID (the id of the node making the \
                 call). Both are set automatically by the engine when a node executes a \
                 connector call; outside a run, use the connector's healthcheck or `--dry-run` \
                 inside a real run instead.",
            ));
            return ExitCode::FAILURE;
        }
    };

    let args_str = match args.as_deref() {
        None => "{}".to_string(),
        Some("-") => {
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                print_call_result(&call_error_json(
                    "config",
                    &format!("failed to read --args from stdin: {e}"),
                ));
                return ExitCode::FAILURE;
            }
            buf
        }
        Some(s) => s.to_string(),
    };
    let parsed_args: Value = match serde_json::from_str(&args_str) {
        Ok(v) => v,
        Err(e) => {
            print_call_result(&call_error_json(
                "invalid_args",
                &format!("--args is not valid JSON: {e}"),
            ));
            return ExitCode::FAILURE;
        }
    };

    let req = apb_engine::connector_call::CallRequest {
        run_dir: Path::new(&run_dir),
        root,
        node_id: &node_id,
        connector: name,
        function,
        account: account.as_deref(),
        args: parsed_args,
        dry_run,
        full,
    };
    let (value, ok) = apb_engine::connector_call::execute(req);
    print_call_result(&value);
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn call_error_json(code: &str, message: &str) -> Value {
    json!({ "ok": false, "error": { "code": code, "message": message } })
}

/// Prints the single-line JSON document `apb connector call` always emits
/// (spec section 8): compact (not pretty), so downstream tooling can rely on
/// one JSON document per line.
fn print_call_result(value: &Value) {
    println!(
        "{}",
        serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
    );
}

// --- doctor -----------------------------------------------------------

fn doctor_cmd(root: &Path) -> ExitCode {
    let Some(dir) = store::connectors_dir() else {
        println!("no config directory available; nothing to check");
        return ExitCode::SUCCESS;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("no connectors installed (see `apb connector init <name>`)");
        return ExitCode::SUCCESS;
    };

    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();

    if names.is_empty() {
        println!("no connectors installed (see `apb connector init <name>`)");
        return ExitCode::SUCCESS;
    }

    let trust = TrustStore::load();
    let mut checks: Vec<Check> = Vec::new();

    for name in &names {
        let loaded = match store::load(name) {
            Ok(l) => l,
            Err(e) => {
                checks.push(Check {
                    name: format!("connector `{name}`: manifest"),
                    status: CheckStatus::Fail,
                    detail: e.to_string(),
                });
                continue;
            }
        };
        checks.push(Check {
            name: format!("connector `{name}`: manifest"),
            status: CheckStatus::Ok,
            detail: format!("parses; digest {}", loaded.digest),
        });

        let accounts = match config::load_merged(root, name) {
            Ok(a) => a,
            Err(e) => {
                checks.push(Check {
                    name: format!("connector `{name}`: config"),
                    status: CheckStatus::Fail,
                    detail: e.to_string(),
                });
                push_connector_trust_check(&mut checks, &trust, name, &loaded.digest);
                push_healthcheck_check(&mut checks, name, &loaded);
                continue;
            }
        };

        let acct_errors = config::validate_accounts(&loaded.doc, &accounts);
        if acct_errors.is_empty() {
            checks.push(Check {
                name: format!("connector `{name}`: config"),
                status: CheckStatus::Ok,
                detail: format!("{} account(s) configured", accounts.len()),
            });
        } else {
            checks.push(Check {
                name: format!("connector `{name}`: config"),
                status: CheckStatus::Fail,
                detail: acct_errors.join("; "),
            });
        }

        let mut required_env: Vec<String> = accounts
            .iter()
            .flat_map(|a| config::env_refs(&loaded.doc, a).into_values())
            .collect();
        required_env.sort();
        required_env.dedup();
        let missing = secrets::missing_vars(root, &required_env);
        if missing.is_empty() {
            let detail = if required_env.is_empty() {
                "no configured account references a secret env var".to_string()
            } else {
                format!("all {} required env var(s) resolve", required_env.len())
            };
            checks.push(Check {
                name: format!("connector `{name}`: env"),
                status: CheckStatus::Ok,
                detail,
            });
        } else {
            checks.push(Check {
                name: format!("connector `{name}`: env"),
                status: CheckStatus::Fail,
                detail: format!("unresolved: {}", missing.join(", ")),
            });
        }

        push_connector_trust_check(&mut checks, &trust, name, &loaded.digest);
        for account in &accounts {
            let digest = config::account_digest(account);
            let approved = trust.is_approved(&digest);
            checks.push(Check {
                name: format!("connector `{name}` account `{}`: trust", account.name),
                status: if approved {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Warn
                },
                detail: if approved {
                    "approved".to_string()
                } else {
                    format!(
                        "not approved (id `{}`)",
                        account_trust_id(name, &account.name)
                    )
                },
            });
        }

        push_healthcheck_check(&mut checks, name, &loaded);
    }

    let mut has_failure = false;
    for c in &checks {
        let marker = match c.status {
            CheckStatus::Ok => "[ok]  ",
            CheckStatus::Warn => "[warn]",
            CheckStatus::Fail => {
                has_failure = true;
                "[fail]"
            }
        };
        println!("{marker} {}: {}", c.name, c.detail);
    }

    if has_failure {
        eprintln!("connector doctor: found blocking problems");
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Trust status of the connector manifest itself: approved (current digest
/// is trusted), changed (some other digest of this id was approved before -
/// content moved since), or never approved. Never a hard failure: trust is
/// advisory here, not a doctor gate (approval has its own flow).
fn push_connector_trust_check(
    checks: &mut Vec<Check>,
    trust: &TrustStore,
    name: &str,
    digest: &str,
) {
    let approved = trust.is_approved(digest);
    let detail = if approved {
        "approved".to_string()
    } else if trust
        .approved_record_ids(Kind::Connector)
        .iter()
        .any(|id| id == name)
    {
        "changed since last approval".to_string()
    } else {
        "not approved".to_string()
    };
    checks.push(Check {
        name: format!("connector `{name}`: trust"),
        status: if approved {
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        },
        detail,
    });
}

/// Reports whether a healthcheck is declared, without executing it (spec
/// deviation, task 14 report: running a live HTTP healthcheck outside a run
/// needs a synthetic single-account manifest, which lands with the
/// dashboard healthcheck endpoint in Task 16). Never a failure - purely
/// informational.
fn push_healthcheck_check(checks: &mut Vec<Check>, name: &str, loaded: &LoadedConnector) {
    let detail = match &loaded.doc.healthcheck {
        Some(f) => format!("declared ({f}); not executed by doctor (see task 16)"),
        None => "none".to_string(),
    };
    checks.push(Check {
        name: format!("connector `{name}`: healthcheck"),
        status: CheckStatus::Ok,
        detail,
    });
}

// --- env ------------------------------------------------------------------

fn env_cmd(root: &Path, name: Option<String>, write: bool) -> ExitCode {
    let names: Vec<String> = match &name {
        Some(n) => {
            let loaded = match store::load(n) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("connector error: {e}");
                    return ExitCode::from(2);
                }
            };
            let accounts = match config::load_merged(root, n) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("connector error: {e}");
                    return ExitCode::from(2);
                }
            };
            let mut vars: Vec<String> = accounts
                .iter()
                .flat_map(|a| config::env_refs(&loaded.doc, a).into_values())
                .collect();
            vars.sort();
            vars.dedup();
            vars
        }
        None => apb_core::connector::resolve::all_referenced_env_names(root),
    };

    // Missing means unresolved; dedup while preserving order (the `None` path
    // may repeat a var referenced by two accounts).
    let mut missing = secrets::missing_vars(root, &names);
    let mut seen = std::collections::HashSet::new();
    missing.retain(|v| seen.insert(v.clone()));

    if !write {
        for var in &missing {
            println!("{var}=");
        }
        return ExitCode::SUCCESS;
    }

    write_secrets_template(root, &missing)
}

/// The `--write` path of `apb connector env`: append the missing `KEY=`
/// template lines to `<root>/.apb/secrets.env` (spec 4.2). Existing content is
/// preserved and a key already present in the file (even with an empty value)
/// is never duplicated; the file is written atomically at mode 0600 and the
/// project `.gitignore` is made to cover it. Values are left empty for the user.
fn write_secrets_template(root: &Path, missing: &[String]) -> ExitCode {
    let path = secrets::project_secrets_path(root);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Keys already in the file, by the part before the first `=` (skip blank
    // and comment lines), so a var already templated is not appended twice.
    let present: std::collections::HashSet<String> = existing
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            line.split_once('=').map(|(k, _)| k.trim().to_string())
        })
        .collect();

    let to_append: Vec<&String> = missing.iter().filter(|v| !present.contains(*v)).collect();

    if !to_append.is_empty() {
        let mut content = existing.clone();
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        for var in &to_append {
            content.push_str(var);
            content.push_str("=\n");
        }
        if let Err(e) = apb_core::fsutil::atomic_write_private(&path, content.as_bytes()) {
            eprintln!("connector error: cannot write {}: {e}", path.display());
            return ExitCode::from(2);
        }
    }

    // Whenever the secrets file exists, make sure the project .gitignore covers
    // it (idempotent - never writes a duplicate line).
    if path.is_file()
        && let Err(e) = secrets::ensure_gitignored(root)
    {
        eprintln!("connector error: cannot update .gitignore: {e}");
        return ExitCode::from(2);
    }

    if to_append.is_empty() {
        println!(
            "nothing to append; {} already lists the required keys",
            path.display()
        );
    } else {
        println!(
            "appended {} template line(s) to {} (fill in the values):",
            to_append.len(),
            path.display()
        );
        for var in &to_append {
            println!("  {var}=");
        }
        println!(".apb/secrets.env is covered by .gitignore");
    }
    ExitCode::SUCCESS
}

// --- init -----------------------------------------------------------------

fn init_cmd(name: &str) -> ExitCode {
    if let Err(e) = apb_core::profile::validate_profile_name(name) {
        eprintln!("connector error: invalid connector name `{name}`: {e}");
        return ExitCode::from(2);
    }
    let Some(base) = store::connectors_dir() else {
        eprintln!("connector error: no config directory available");
        return ExitCode::from(2);
    };
    let target = base.join(name);
    if target.exists() {
        eprintln!(
            "connector error: `{}` already exists; refusing to overwrite",
            target.display()
        );
        return ExitCode::from(2);
    }
    if let Err(e) = std::fs::create_dir_all(&target) {
        eprintln!("connector error: cannot create {}: {e}", target.display());
        return ExitCode::from(2);
    }
    let write_result = std::fs::write(target.join("connector.yaml"), scaffold_yaml(name))
        .and_then(|_| std::fs::write(target.join("PUBLIC.md"), scaffold_public_md(name)));
    if let Err(e) = write_result {
        eprintln!("connector error: cannot write scaffold: {e}");
        return ExitCode::from(2);
    }

    // The scaffold must always pass the same load path a real connector
    // goes through; a defensive re-check here catches a template bug before
    // it ships, rather than a user hitting it on first `doctor`/`show`.
    match store::load(name) {
        Ok(_) => {
            println!("scaffolded connector `{name}` at {}", target.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("connector error: scaffold failed to validate: {e}");
            ExitCode::from(2)
        }
    }
}

fn scaffold_yaml(name: &str) -> String {
    format!(
        r#"name: {name}
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{{{secret.token}}}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: get_item
    description: Fetch one item by id
    read_only: true
    method: GET
    url: "{{{{account.base_url}}}}/items/{{{{args.id}}}}"
    args_schema:
      type: object
      properties:
        id:
          type: string
      required: [id]
  - name: ping
    description: Reachability check (no network call)
    mock:
      status: 200
      body: {{ ok: true }}
"#
    )
}

fn scaffold_public_md(name: &str) -> String {
    let display_name = name
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "---\ndisplay_name: {display_name}\nsummary: Scaffolded connector - edit connector.yaml and this file.\n---\n# {display_name}\n\nDescribe what this connector does and how to configure an account here.\n"
    )
}
