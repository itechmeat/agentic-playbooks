//! `apb connector oauth`: a person's terminal runs the loopback consent
//! flow of one connector account, prints the refresh token once, and tells
//! the person where to store it. apb itself stores no token; the command
//! is refused inside a run step and without a terminal.

use std::io::IsTerminal as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use apb_core::connector::config;
use apb_engine::connector::oauth::consent;

/// The command body behind `ConnectorAction::Oauth`.
pub(crate) fn oauth_cmd(
    root: &Path,
    name: &str,
    account: Option<&str>,
    scopes: &[String],
    timeout_sec: u64,
) -> ExitCode {
    if let Some(where_) = apb_core::step_env::StepCaller::from_env().describe() {
        eprintln!(
            "apb connector oauth is refused {where_}: only a person grants access to an account"
        );
        return ExitCode::from(2);
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        eprintln!("apb connector oauth needs a terminal: it prints a secret once, for a person");
        return ExitCode::from(2);
    }
    match run_flow(root, name, account, scopes, timeout_sec) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("connector oauth error: {message}");
            ExitCode::from(1)
        }
    }
}

fn run_flow(
    root: &Path,
    name: &str,
    account: Option<&str>,
    scopes: &[String],
    timeout_sec: u64,
) -> Result<(), String> {
    let plan = consent::plan(root, name, account, scopes).map_err(|e| e.message)?;
    let pending = consent::start(&plan).map_err(|e| format!("the consent listener failed: {e}"))?;
    eprintln!("Open this URL in the browser and approve the access:");
    eprintln!("{}", pending.url);
    eprintln!(
        "Waiting for the provider to call back on {} (timeout {} s)...",
        redirect_of(&pending.url),
        timeout_sec
    );
    let result =
        consent::finish(&plan, pending, Duration::from_secs(timeout_sec)).map_err(|e| e.message)?;
    // The token goes to stdout exactly once, alone on its line.
    println!("{}", result.refresh_token.expose_secret());
    eprintln!(
        "granted scopes: {}",
        result
            .granted
            .as_deref()
            .unwrap_or("(the provider said none)")
    );
    if let Some(hint) = storage_hint(root, name, account, &plan.refresh_field_hint) {
        eprintln!("{hint}");
    }
    eprintln!("apb keeps no copy of this token; store it before closing the terminal.");
    Ok(())
}

/// Where the callback listens, for the instructions line.
fn redirect_of(url: &str) -> String {
    url.split('&')
        .find_map(|p| p.strip_prefix("redirect_uri="))
        .map(|v| v.replace("%3A", ":").replace("%2F", "/"))
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// The storage hint: the env var name when the account's refresh token
/// field is `{{env.VAR}}`, the helper when it is `{{cmd:...}}`. Reads only
/// the reference, never a value.
fn storage_hint(root: &Path, name: &str, account: Option<&str>, field: &str) -> Option<String> {
    let accounts = config::load_merged(root, name).ok()?;
    let chosen = match account {
        Some(want) => accounts.iter().find(|a| a.name == want)?,
        None => accounts.first()?,
    };
    let reference = chosen.fields.get(field)?;
    if let Some(var) = reference
        .strip_prefix("{{env.")
        .and_then(|r| r.strip_suffix("}}"))
    {
        return Some(format!(
            "put the token into {var} in {}/.apb/secrets.env or in the secrets.env of your apb config dir",
            root.display()
        ));
    }
    if reference.starts_with("{{cmd:") {
        return Some(format!(
            "your helper `{field}` prints the token; keep it storing the newest one"
        ));
    }
    None
}

/// The connector's auth kind, as `apb connector show` reports it.
pub(crate) fn auth_kind(doc: &apb_core::connector::def::ConnectorDoc) -> Option<&'static str> {
    doc.auth.as_ref().map(|a| match a {
        apb_core::connector::def::AuthSpec::Header { .. } => "header",
        apb_core::connector::def::AuthSpec::Query { .. } => "query",
        apb_core::connector::def::AuthSpec::Basic { .. } => "basic",
        apb_core::connector::def::AuthSpec::Path { .. } => "path",
        apb_core::connector::def::AuthSpec::Oauth2Refresh { .. } => "oauth2_refresh",
    })
}
