//! Secret resolution for a call: which account fields are secret, where their
//! values come from, and how a failing `{{cmd:...}}` reference is reported.
//! Values live in this process only, inside the outgoing auth block.

use super::*;

/// The non-secret account fields: every field whose key is NOT a secret field
/// (env-backed or command-backed). Secret fields hold a raw `{{env.VAR}}` /
/// `{{cmd:...}}` reference in the manifest and must never reach the render
/// context's `account` map.
pub(crate) fn non_secret_fields(account: &ManifestAccount) -> BTreeMap<String, String> {
    account
        .fields
        .iter()
        .filter(|(k, _)| {
            !account.env.contains_key(k.as_str()) && !account.cmd.contains_key(k.as_str())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The resolved secrets map (field name -> value) plus the redaction pairs
/// (resolved value, redaction label) the response body is scrubbed against.
/// The label is the ENV var name for env-sourced secrets and `cmd:<field>`
/// for command-sourced ones.
pub(crate) type ResolvedSecrets = (BTreeMap<String, String>, Vec<(String, String)>);

/// Resolves every secret field to its value: env-ref fields via the secrets
/// resolution chain, cmd-ref fields by executing the command (spec 4.1).
/// Returns the secrets map keyed by FIELD name (for the render context) and
/// the redaction pairs (resolved value, redaction label). A var that resolves
/// nowhere, or a command that fails, is a Config error naming the field.
pub(crate) fn resolve_secrets(
    root: &Path,
    account: &ManifestAccount,
) -> Result<ResolvedSecrets, CallError> {
    let mut secrets = BTreeMap::new();
    let mut redactions = Vec::new();
    for (field, var) in &account.env {
        let value = secrets::resolve_var(root, var).ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("secret env var `{var}` (account field `{field}`) is not set"),
            )
        })?;
        // Empty secrets would redact every empty run in the body; skip them.
        if !value.is_empty() {
            redactions.push((value.clone(), var.clone()));
        }
        secrets.insert(field.clone(), value);
    }
    for (field, cmdline) in &account.cmd {
        let value = secrets::resolve_cmd(cmdline, secrets::CMD_SECRET_TIMEOUT)
            .map_err(|e| cmd_secret_error(&account.name, field, e))?;
        // resolve_cmd rejects empty output, so the value is always non-empty
        // and safe to register for redaction. The label carries no secret.
        redactions.push((value.clone(), format!("cmd:{field}")));
        secrets.insert(field.clone(), value);
    }
    Ok((secrets, redactions))
}

/// Maps a `CmdSecretError` to a `config` call error naming the account and
/// field and, where the helper produced one, a trimmed stderr excerpt. The
/// resolved secret is never part of any variant, so nothing sensitive can
/// reach this message.
pub(crate) fn cmd_secret_error(
    account: &str,
    field: &str,
    err: secrets::CmdSecretError,
) -> CallError {
    use secrets::CmdSecretError as E;
    let detail = match err {
        E::Parse(m) => format!("command reference is not valid: {m}"),
        E::Spawn(m) => format!("command could not start: {m}"),
        E::Timeout => "command timed out after 10s".to_string(),
        E::NonZero { code, stderr } => {
            let code = code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string());
            if stderr.is_empty() {
                format!("command exited with status {code}")
            } else {
                format!("command exited with status {code}: {stderr}")
            }
        }
        E::Empty { stderr } => {
            if stderr.is_empty() {
                "command produced no output".to_string()
            } else {
                format!("command produced no output: {stderr}")
            }
        }
    };
    CallError::new(
        CallErrorCode::Config,
        format!("secret for account `{account}` field `{field}`: {detail}"),
    )
}
