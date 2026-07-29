//! `apb connector` subcommands (spec 2026-07-18-connectors-design section
//! 10): list/show/call/doctor/env/init over the connector store. `call` is
//! the thin CLI wrapper `apb_engine::connector::call` documents itself as -
//! run context comes only from `APB_RUN_DIR`/`APB_NODE_ID`, both set by the
//! engine adapter when a node actually executes a connector call; everything
//! else here is a read-only or scaffolding convenience for a human or an
//! agent debugging outside a run.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use apb_core::connector::config::{self};
use apb_core::connector::def::ConnectorDoc;
use apb_core::connector::secrets;
use apb_core::connector::store::{self, LoadedConnector};
use apb_core::doctor::{Check, CheckStatus};
use apb_core::trust::{Kind, OriginKind, TrustStore, account_trust_id};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::util::{print_json, print_table};

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
    /// Install an embedded official connector into the global store and record
    /// its trust in the same action; or install any folder on disk with
    /// --from-dir (validated, but no trust recorded - the normal approve flow
    /// applies). Refuses a differing existing target unless --force; a
    /// same-digest reinstall is a no-op.
    Install {
        /// Name of the embedded connector to install; omit only with --from-dir
        name: Option<String>,
        /// Install from this folder instead of the embedded set (no trust)
        #[arg(long)]
        from_dir: Option<PathBuf>,
        /// Overwrite an existing, differing target
        #[arg(long)]
        force: bool,
    },
    /// Run a connector's offline contract tests (tests.yaml). Resolves an
    /// installed connector, an embedded one, or a folder on disk (--dir).
    /// Fully offline; exits non-zero on any failing case.
    Test {
        /// Installed or embedded connector name; omit only with --dir
        name: Option<String>,
        /// Test the connector folder at this path instead
        #[arg(long)]
        dir: Option<PathBuf>,
    },
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
        ConnectorAction::Install {
            name,
            from_dir,
            force,
        } => install_cmd(name, from_dir, force),
        ConnectorAction::Test { name, dir } => test_cmd(name, dir),
    }
}

// --- list -----------------------------------------------------------------

fn list_cmd(root: &Path) -> ExitCode {
    let summaries = store::list();
    let official = apb_core::connector::official::list();

    if summaries.is_empty() {
        println!(
            "no connectors installed (see `apb connector install <name>` or `apb connector init <name>`)"
        );
    } else {
        let trust = TrustStore::load();
        let approved_connector_ids = trust.approved_record_ids(Kind::Connector);
        let mut rows: Vec<Vec<String>> = vec![vec![
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
            rows.push(vec![
                s.name.clone(),
                s.version.clone(),
                trust_state.to_string(),
                accounts_count.to_string(),
            ]);
        }
        print_table(&rows);

        // Version-drift notes: an installed connector whose embedded version
        // differs (a binary upgrade shipped a newer manifest).
        for s in &summaries {
            if let Some(o) = official.iter().find(|o| o.name == s.name)
                && o.version != s.version
            {
                println!(
                    "note: `{}` installed {}, embedded {} (reinstall with --force to upgrade)",
                    s.name, s.version, o.version
                );
            }
        }
    }

    let installed: HashSet<&str> = summaries.iter().map(|s| s.name.as_str()).collect();
    let available: Vec<&apb_core::connector::official::OfficialConnector> = official
        .iter()
        .filter(|o| !installed.contains(o.name.as_str()))
        .collect();
    if !available.is_empty() {
        println!();
        println!("AVAILABLE (embedded, not installed):");
        for o in &available {
            println!("  {}  {}", o.name, o.version);
        }
        println!("install one with `apb connector install <name>`");
    }

    ExitCode::SUCCESS
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

    let req = apb_engine::connector::call::CallRequest {
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
    let (value, ok) = apb_engine::connector::call::execute(req);
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
        .and_then(|_| std::fs::write(target.join("PUBLIC.md"), scaffold_public_md(name)))
        .and_then(|_| std::fs::write(target.join("README.md"), scaffold_readme_md(name)))
        .and_then(|_| std::fs::write(target.join("INSTALL.md"), scaffold_install_md(name)));
    if let Err(e) = write_result {
        eprintln!("connector error: cannot write scaffold: {e}");
        return failed_init(&target);
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
            failed_init(&target)
        }
    }
}

/// Removes the folder `init_cmd` just created, so a failed scaffold does not
/// leave a partial connector behind. Without this the next `init` refuses on
/// "already exists" and the user has to know to delete the folder by hand -
/// a dead end produced by a failure that was not theirs. `dir` was created by
/// this same call (an existing path is refused earlier), so removing it cannot
/// touch a connector someone else installed.
///
/// A cleanup failure only adds a line to the report: the exit code stays the
/// scaffold's own failure, which is what the caller actually needs to know.
fn failed_init(dir: &Path) -> ExitCode {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        eprintln!(
            "connector error: could not clean up {} after the failure: {e}",
            dir.display()
        );
    }
    ExitCode::from(2)
}

// --- test -----------------------------------------------------------------

fn test_cmd(name: Option<String>, dir: Option<PathBuf>) -> ExitCode {
    let (doc, tests) = match load_test_target(name.as_deref(), dir.as_deref()) {
        Ok(pair) => pair,
        Err(msg) => {
            eprintln!("connector error: {msg}");
            return ExitCode::from(2);
        }
    };
    let report = apb_engine::connector::contract_test::run_tests(&doc, &tests);
    for r in &report.results {
        if r.passed {
            println!("[pass] {}", r.function);
        } else {
            println!("[fail] {}: {}", r.function, r.detail);
        }
    }
    if report.all_passed() {
        println!("{} case(s) passed", report.results.len());
        ExitCode::SUCCESS
    } else {
        let failed = report.results.iter().filter(|r| !r.passed).count();
        eprintln!("connector test: {failed} case(s) failed");
        ExitCode::from(1)
    }
}

/// Resolves the `(ConnectorDoc, TestsDoc)` pair for `apb connector test` from,
/// in precedence order: a folder (--dir), an installed connector, then an
/// embedded one.
fn load_test_target(
    name: Option<&str>,
    dir: Option<&Path>,
) -> Result<(ConnectorDoc, apb_core::connector::contract::TestsDoc), String> {
    use apb_core::connector::contract::TestsDoc;

    if let Some(dir) = dir {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("cannot derive a connector name from {}", dir.display()))?;
        let yaml = std::fs::read_to_string(dir.join("connector.yaml"))
            .map_err(|e| format!("cannot read {}/connector.yaml: {e}", dir.display()))?;
        let doc = ConnectorDoc::from_yaml(&yaml, dir_name).map_err(|e| e.to_string())?;
        let tests_raw = std::fs::read_to_string(dir.join("tests.yaml"))
            .map_err(|e| format!("cannot read {}/tests.yaml: {e}", dir.display()))?;
        let tests = TestsDoc::from_yaml(&tests_raw).map_err(|e| e.to_string())?;
        return Ok((doc, tests));
    }

    let name = name.ok_or_else(|| "provide a connector name or --dir <path>".to_string())?;

    // Installed connector first.
    if let Ok(loaded) = store::load(name) {
        let tests_raw = std::fs::read_to_string(loaded.dir.join("tests.yaml"))
            .map_err(|e| format!("connector `{name}` has no readable tests.yaml: {e}"))?;
        let tests = TestsDoc::from_yaml(&tests_raw).map_err(|e| e.to_string())?;
        return Ok((loaded.doc, tests));
    }

    // Otherwise an embedded connector.
    let official = apb_core::connector::official::get(name)
        .ok_or_else(|| format!("`{name}` is neither installed nor an embedded connector"))?;
    let yaml = official
        .files
        .get("connector.yaml")
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| format!("embedded `{name}` has no readable connector.yaml"))?;
    let doc = ConnectorDoc::from_yaml(yaml, name).map_err(|e| e.to_string())?;
    let tests_raw = official
        .files
        .get("tests.yaml")
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or_else(|| format!("embedded `{name}` has no tests.yaml"))?;
    let tests = TestsDoc::from_yaml(tests_raw).map_err(|e| e.to_string())?;
    Ok((doc, tests))
}

// --- install --------------------------------------------------------------

fn install_cmd(name: Option<String>, from_dir: Option<PathBuf>, force: bool) -> ExitCode {
    match from_dir {
        Some(dir) => install_from_dir(&dir, force),
        None => match name {
            Some(n) => install_embedded(&n, force),
            None => {
                eprintln!("connector error: provide a connector name or --from-dir <path>");
                ExitCode::from(2)
            }
        },
    }
}

/// Installs an embedded official connector through the shared core routine
/// (`apb_core::connector::install::install_official`), which the HTTP API calls
/// too. This function only renders that structured outcome as the CLI's lines
/// and exit code; every staging, digest, force and trust decision lives in core.
fn install_embedded(name: &str, force: bool) -> ExitCode {
    let report = match apb_core::connector::install::install_official(name, force) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("connector error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(warning) = &report.trust_warning {
        eprintln!("connector warning: installed `{name}` but could not record trust: {warning}");
    }
    if report.no_op {
        println!(
            "connector `{name}` already installed at version {} (no changes)",
            report.version
        );
    } else {
        println!(
            "installed connector `{name}` version {} and recorded its trust",
            report.version
        );
    }
    ExitCode::SUCCESS
}

/// Installs a folder on disk with no trust recorded (the explicit local dev
/// loop). `--force` is accepted for CLI uniformity but has no effect: --from-dir
/// overwrites freely because it seeds no trust, and the run/probe trust gate
/// still catches an unapproved digest.
fn install_from_dir(src: &Path, _force: bool) -> ExitCode {
    let Some(name) = src.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
        eprintln!(
            "connector error: cannot derive a connector name from {}",
            src.display()
        );
        return ExitCode::from(2);
    };
    if let Err(e) = apb_core::profile::validate_profile_name(&name) {
        eprintln!("connector error: folder name `{name}` is not a valid connector name: {e}");
        return ExitCode::from(2);
    }
    // Validate: the manifest must parse and match the folder name.
    let yaml = match std::fs::read_to_string(src.join("connector.yaml")) {
        Ok(y) => y,
        Err(e) => {
            eprintln!(
                "connector error: cannot read {}/connector.yaml: {e}",
                src.display()
            );
            return ExitCode::from(2);
        }
    };
    if let Err(e) = ConnectorDoc::from_yaml(&yaml, &name) {
        eprintln!("connector error: {} does not validate: {e}", src.display());
        return ExitCode::from(2);
    }
    let Some(base) = store::connectors_dir() else {
        eprintln!("connector error: no config directory available");
        return ExitCode::from(2);
    };
    let target = base.join(&name);
    let staging = base.join(format!(".{name}.install-tmp"));
    let _ = std::fs::remove_dir_all(&staging);
    // TOCTOU-safe copy + digest of the source folder into staging.
    let limits = apb_core::content::TreeLimits::default();
    if let Err(e) = apb_core::content::snapshot_tree(src, &staging, &limits) {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("connector error: cannot copy {}: {e}", src.display());
        return ExitCode::from(2);
    }
    // --from-dir is an explicit local dev action and records no trust, so it
    // overwrites freely (the run/probe gate still catches an unapproved digest).
    if target.exists()
        && let Err(e) = std::fs::remove_dir_all(&target)
    {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!("connector error: cannot replace {}: {e}", target.display());
        return ExitCode::from(2);
    }
    if let Err(e) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        eprintln!(
            "connector error: cannot install into {}: {e}",
            target.display()
        );
        return ExitCode::from(2);
    }
    println!(
        "installed connector `{name}` from {} (trust not recorded; approve it before use)",
        src.display()
    );
    ExitCode::SUCCESS
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
    let display_name = display_name_of(name);
    format!(
        "---\ndisplay_name: {display_name}\nsummary: Scaffolded connector - edit connector.yaml and this file.\n---\n# {display_name}\n\nDescribe what this connector does and how to configure an account here.\n"
    )
}

/// Title-cases a connector slug for the scaffolded documents (`my-tracker` ->
/// `My Tracker`).
fn display_name_of(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The human setup page every connector carries (docs/CONNECTORS.md, "README.md
/// and INSTALL.md"). Scaffolded as a filled-in outline rather than an empty
/// heading list, so the author edits real sentences instead of inventing the
/// structure: the agent-install prompt at the top is the part that makes the
/// convention work and is easy to leave out when starting from a blank file.
///
/// Prose is written one paragraph per line - never hard-wrapped at a width.
fn scaffold_readme_md(name: &str) -> String {
    let display_name = display_name_of(name);
    format!(
        r#"# {name}: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, creating a credential, and approving trust. An agent can do all of it for you and will only stop to ask for the credential, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `{name}` connector, then read `connectors/{name}/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the credential when you get there.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck. What you get back is either a working account or a specific error.

## What you will be asked for

Describe the credential {display_name} needs and exactly where in its interface a person creates one, including any scopes or permissions to select. Say what the credential can reach, especially when the service offers no way to narrow it.

The credential is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

List what the functions cover, then what they deliberately do not. Name the functions that are not read-only or not easily undone, so a reader knows which ones deserve a narrow grant and a `max_calls` cap.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
"#
    )
}

/// The agent runbook every connector carries (docs/CONNECTORS.md, "README.md and
/// INSTALL.md"). Scaffolded with the steps that are identical for every
/// connector already written out, and service-specific spots marked, so an
/// author fills in hosts, scopes and failure modes rather than re-deriving the
/// install, trust and healthcheck sequence.
///
/// Prose is written one paragraph per line - never hard-wrapped at a width.
fn scaffold_install_md(name: &str) -> String {
    format!(
        r#"# {name}: installation instructions for an agent

You are setting up the apb `{name}` connector. Work through the steps in order. The only thing you need from the user is a credential, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the credential back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `{name}` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install {name} --from-dir <path-to-this-folder>
```

This is a scaffolded connector, not one embedded in the `apb` binary, so it installs with `--from-dir` rather than by name alone. That path records no trust automatically; step 5 approves it explicitly. It is local either way: no network call, nothing published.

If it refuses because a differing `{name}` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: settle the service settings and the credential

Fill this in for the service: the concrete value of every non-secret account field (base URLs and their required suffixes, ids, identities), where in the service's interface the credential is created, and which scopes or permissions to select for the functions the playbooks will actually use. State plainly when the service offers no scope selection, since that cannot be narrowed later.

Do not invent a hostname or an id. Ask the user to read it off their own interface when you are unsure.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/{name}.yaml`: an identity the user wants available from every project.
- project, `<project>/.apb/connector-config/{name}.yaml`: an identity that belongs to this project.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: default
    default: true
    # non-secret fields here
    token: "{{{{env.{env_key}}}}}"    # example only: rename `token` (here and in connector.yaml) to whatever this service calls its credential, e.g. password or api_key
```

A secret field must hold exactly one reference, either `{{{{env.VAR}}}}` or `{{{{cmd:<command>}}}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the credential

```sh
apb connector env {name} --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the credential. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the credential can be revoked and reissued at the service if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve {name} --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check them before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `{name}` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real service

The live probe is this connector's declared healthcheck. Name it here and say what it does and does not touch.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -s -X POST "http://127.0.0.1:7321/api/connectors/{name}/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

List the failures this service actually produces and what each one points at, so a reader is not left guessing which layer broke. The ones every connector shares:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created and in which scope;
- which file holds the credential and which key;
- the healthcheck result;
- anything the setup still needs from them;
- which functions are irreversible enough to deserve a narrow grant.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
"#,
        env_key = name.to_uppercase().replace('-', "_") + "_TOKEN"
    )
}
