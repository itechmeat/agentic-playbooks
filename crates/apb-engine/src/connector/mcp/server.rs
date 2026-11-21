//! The stdio server of a call (design D4 "The server's environment"): the
//! pinned program file, its rendered arguments and an environment from a
//! fixed source, never from the calling process. Its working directory is
//! a fresh empty private directory the client makes and removes.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::config::AccountEntry;
use apb_core::connector::config;
use apb_core::connector::def::ConnectorDoc;
use apb_core::connector::mcp::McpServerSpec;
use apb_core::connector::template::{Namespace, RenderCtx, placeholders, render_raw};
use apb_core::secret::Secret;
use apb_mcp_client::{EnvValue, ProgramFile, StdioServer};
use serde_json::Value;

use crate::connector::result::{CallError, CallErrorCode};

/// What the server would be started with, rendered from the definition
/// and the account's non-secret fields only, for a person's confirmation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartPreview {
    /// Each `args` entry, rendered.
    pub args: Vec<String>,
    /// Each declared `env` override, its secret parts shown as
    /// `[secret <field> from <source>]`.
    pub env: Vec<(String, String)>,
    /// Every non-secret field of the account.
    pub fields: Vec<(String, String)>,
    /// Each command-sourced secret field with the command line its
    /// resolution will run, extracted and not run.
    pub secret_commands: Vec<(String, String)>,
}

/// What the server would be started with, rendered from the definition
/// and the account's non-secret fields only: each `args` entry, and each
/// `env` entry with every `{{secret.<field>}}` replaced by
/// `[secret <field> from <source>]`. Resolves no secret and runs no
/// helper. Also carries `config::cmd_refs(doc, account)`: each
/// command-sourced secret field with the command line its resolution will
/// run, extracted without executing it. Takes the whole definition, since
/// which fields are secret is the definition's `account_fields`.
pub fn preview_start(
    doc: &ConnectorDoc,
    account: &config::Account,
) -> Result<StartPreview, CallError> {
    let Some(spec) = doc.mcp.as_ref() else {
        return Ok(StartPreview::default());
    };
    let secret_fields = doc.secret_fields();
    let env_refs = config::env_refs(doc, account);
    let cmd_refs = config::cmd_refs(doc, account);
    let fields: BTreeMap<String, String> = account
        .fields
        .iter()
        .filter(|(k, _)| !secret_fields.contains(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let masked: BTreeMap<String, Secret> = secret_fields
        .iter()
        .map(|f| {
            let source = match (env_refs.get(f), cmd_refs.contains_key(f)) {
                (Some(var), _) => format!("env {var}"),
                (None, true) => "cmd".to_string(),
                (None, false) => "account".to_string(),
            };
            (
                f.clone(),
                Secret::new(format!("[secret {f} from {source}]")),
            )
        })
        .collect();
    let ctx = RenderCtx {
        account: &fields,
        args: &Value::Null,
        secrets: &masked,
    };
    let env = spec
        .env
        .iter()
        .map(|(name, template)| {
            render_raw(template, &ctx)
                .map(|v| (name.clone(), v))
                .map_err(|e| config_error(name, &e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StartPreview {
        args: render_args(spec, &fields)?,
        env,
        fields: fields.into_iter().collect(),
        secret_commands: cmd_refs.into_iter().collect(),
    })
}

/// The `StdioServer` of a pinned program file, a spec and an account: the
/// program, the `args` rendered with the account's fields, and the fixed
/// environment of design D4 with the declared `env` on top. Nothing of the
/// calling process passes.
pub(crate) fn stdio_server(
    program: ProgramFile,
    spec: &McpServerSpec,
    account_fields: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, Secret>,
    home: &AccountEntry,
) -> Result<StdioServer, CallError> {
    let args = render_args(spec, account_fields)?;
    let mut env = fixed_env(program.path(), home);
    let ctx = RenderCtx {
        account: account_fields,
        args: &Value::Null,
        secrets,
    };
    for (name, template) in &spec.env {
        let holds_secret = placeholders(template)
            .map_err(|e| config_error(name, &e.to_string()))?
            .iter()
            .any(|(ns, _)| *ns == Namespace::Secret);
        let value = render_raw(template, &ctx).map_err(|e| config_error(name, &e.to_string()))?;
        let value = if holds_secret {
            EnvValue::Secret(Secret::new(value))
        } else {
            EnvValue::Plain(value)
        };
        // A declared name overrides a fixed one.
        env.retain(|(n, _)| n != name);
        env.push((name.clone(), value));
    }
    Ok(StdioServer { program, args, env })
}

/// The system directories of the server's `PATH`, in order (macOS adds
/// `/opt/homebrew/bin` after them).
const SYSTEM_PATH: [&str; 3] = ["/usr/local/bin", "/usr/bin", "/bin"];

/// The server's `PATH`: the system directories first, then on macOS
/// `/opt/homebrew/bin`, then the program's own directory last (when it is
/// not one of them), so a name the server looks up never resolves first
/// in the program's directory. A directory that cannot be one `PATH`
/// entry (it holds a `:` or is not UTF-8) is left out rather than split
/// into entries nobody approved.
pub(crate) fn server_path(program: &Path) -> String {
    let mut dirs: Vec<String> = SYSTEM_PATH.iter().map(|d| d.to_string()).collect();
    if cfg!(target_os = "macos") {
        dirs.push("/opt/homebrew/bin".to_string());
    }
    if let Some(dir) = program.parent().and_then(Path::to_str).map(str::to_string)
        && !dir.is_empty()
        && !dir.contains(':')
        && !dirs.contains(&dir)
    {
        dirs.push(dir);
    }
    dirs.join(":")
}

/// The fixed rows of the server's environment: the account database's
/// home and name, the XDG base directories under that home, and the
/// `PATH` of `server_path`.
fn fixed_env(program: &Path, home: &AccountEntry) -> Vec<(String, EnvValue)> {
    let h = home.home.display().to_string();
    let plain = |name: &str, value: String| (name.to_string(), EnvValue::Plain(value));
    vec![
        plain("HOME", h.clone()),
        plain("USER", home.name.clone()),
        plain("LOGNAME", home.name.clone()),
        plain("XDG_CONFIG_HOME", format!("{h}/.config")),
        plain("XDG_DATA_HOME", format!("{h}/.local/share")),
        plain("XDG_STATE_HOME", format!("{h}/.local/state")),
        plain("XDG_CACHE_HOME", format!("{h}/.cache")),
        plain("PATH", server_path(program)),
    ]
}

/// Each `args` entry rendered with the account's non-secret fields (an
/// `args` template takes `{{account.*}}` only).
pub(crate) fn render_args(
    spec: &McpServerSpec,
    account_fields: &BTreeMap<String, String>,
) -> Result<Vec<String>, CallError> {
    let none = BTreeMap::new();
    let ctx = RenderCtx {
        account: account_fields,
        args: &Value::Null,
        secrets: &none,
    };
    spec.args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            render_raw(a, &ctx).map_err(|e| config_error(&format!("args[{i}]"), &e.to_string()))
        })
        .collect()
}

fn config_error(field: &str, why: &str) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!("mcp `{field}` could not be rendered: {why}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_puts_the_programs_own_directory_last() {
        let path = server_path(Path::new("/home/u/project/bin/server"));
        let dirs: Vec<&str> = path.split(':').collect();
        assert_eq!(&dirs[..3], &SYSTEM_PATH[..]);
        assert_eq!(dirs.last(), Some(&"/home/u/project/bin"));
        assert_eq!(
            dirs.contains(&"/opt/homebrew/bin"),
            cfg!(target_os = "macos")
        );
        let system = server_path(Path::new("/usr/bin/server"));
        assert_eq!(
            system.split(':').filter(|d| *d == "/usr/bin").count(),
            1,
            "no directory twice: {system}"
        );
    }

    #[test]
    fn a_program_directory_with_a_colon_is_left_out_of_the_path() {
        let path = server_path(Path::new("/opt/vendor:tools/server"));
        let system = server_path(Path::new("/usr/bin/server"));
        assert_eq!(path, system, "only the system directories: {path}");
        assert!(
            !path.contains("vendor") && !path.contains("tools"),
            "{path}"
        );
    }
}
