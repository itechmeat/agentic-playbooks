//! Choosing which configured account a call runs as, and saying clearly why
//! no choice could be made. Acceptance is bound to the grant: an account the
//! binding did not grant is never selectable, whatever the snapshot holds.

use super::*;

/// Picks the account for a live probe/playground call: an explicit name
/// must match one of the LIVE configured accounts; with none given, the
/// single configured account is used, else the one flagged `default`, else
/// no selection (ambiguous, reported by the caller via
/// `account_selection_error`). Mirrors the CLI pipeline's `select_account`
/// defaulting rule, minus the grant list (a live call has no grants).
pub(crate) fn select_live_account<'a>(
    accounts: &'a [config::Account],
    account: Option<&str>,
) -> Option<&'a config::Account> {
    if let Some(explicit) = account {
        return accounts.iter().find(|a| a.name == explicit);
    }
    if let [only] = accounts {
        return Some(only);
    }
    let defaults: Vec<&config::Account> = accounts.iter().filter(|a| a.default).collect();
    if let [only] = defaults.as_slice() {
        return Some(only);
    }
    None
}

pub(crate) fn account_selection_error(
    name: &str,
    account: Option<&str>,
    accounts: &[config::Account],
) -> CallError {
    if let Some(explicit) = account {
        return CallError::new(
            CallErrorCode::Config,
            format!("connector `{name}` has no account `{explicit}`"),
        );
    }
    let choices: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    CallError::new(
        CallErrorCode::Config,
        format!(
            "connector `{name}` has several accounts and no single default; specify an account (choices: {})",
            choices.join(", ")
        ),
    )
}

/// Names that are informative context for an account-selection error message
/// (finding 12 of issue 45), never a source of truth for what is granted.
///
/// Prefers grant allowlist names that exist in the connector snapshot (so the
/// listed names are ones an operator could plausibly grant). When the grant
/// allowlist is empty but the snapshot still has accounts, falls back to
/// every snapshotted account name so the error can say what accounts exist
/// on the connector even though none of them are granted - otherwise the
/// message is uninformative about how to fix the binding. Callers MUST NOT
/// use this list to decide what `--account` values are accepted or what gets
/// auto-selected: that decision is `granted_account_names` alone.
pub(crate) fn selectable_account_names<'a>(
    grant: &'a ManifestConnectorGrant,
    mconn: &'a ManifestConnector,
) -> Vec<&'a str> {
    let from_grant: Vec<&str> = grant
        .accounts
        .iter()
        .filter(|name| mconn.accounts.iter().any(|a| a.name == **name))
        .map(|s| s.as_str())
        .collect();
    if !from_grant.is_empty() {
        return from_grant;
    }
    if !grant.accounts.is_empty() {
        // Grant names missing from the snapshot: still surface them so the
        // operator can see what the grant recorded.
        return grant.accounts.iter().map(|s| s.as_str()).collect();
    }
    mconn.accounts.iter().map(|a| a.name.as_str()).collect()
}

/// Names actually granted to this node, the sole basis for `--account`
/// acceptance and auto-selection. An empty grant allowlist means no account
/// is accepted and nothing is auto-selected, regardless of what the
/// connector snapshot has configured.
pub(crate) fn granted_account_names(grant: &ManifestConnectorGrant) -> Vec<&str> {
    grant.accounts.iter().map(|s| s.as_str()).collect()
}

/// Formats a `choices:`-style account name list for an error message.
pub(crate) fn format_account_choices(names: &[&str]) -> String {
    names.join(", ")
}

/// Account selection (spec 6 step 4): an explicit `--account` must be granted;
/// with none given, the single granted account is used, else the granted
/// account flagged `default` in the connector snapshot, else a Config error
/// listing the choices. An empty grant allowlist grants no account at all:
/// it is never widened to the connector snapshot's accounts, since that would
/// let a deny-all binding (`accounts: []`) call the connector with any
/// configured account.
pub(crate) fn select_account(
    req: &CallRequest,
    grant: &ManifestConnectorGrant,
    mconn: &ManifestConnector,
) -> Result<String, CallError> {
    let granted = granted_account_names(grant);

    if let Some(explicit) = req.account {
        if granted.contains(&explicit) {
            return Ok(explicit.to_string());
        }
        return Err(CallError::new(
            CallErrorCode::Permission,
            format!(
                "node `{}` is not granted account `{explicit}` on connector `{}`",
                req.node_id, req.connector
            ),
        ));
    }

    if let [only] = granted.as_slice() {
        return Ok((*only).to_string());
    }

    let defaults: Vec<&str> = granted
        .iter()
        .copied()
        .filter(|name| mconn.accounts.iter().any(|a| a.name == *name && a.default))
        .collect();
    if let [only] = defaults.as_slice() {
        return Ok((*only).to_string());
    }

    if granted.is_empty() {
        let snapshot_names = selectable_account_names(grant, mconn);
        return Err(CallError::new(
            CallErrorCode::Config,
            format!(
                "connector `{}` binding grants no accounts; configured accounts: {}",
                req.connector,
                format_account_choices(&snapshot_names)
            ),
        ));
    }

    Err(CallError::new(
        CallErrorCode::Config,
        format!(
            "connector `{}` has several granted accounts and no single default; pass --account (choices: {})",
            req.connector,
            format_account_choices(&granted)
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_account_choices_joins_names() {
        assert_eq!(
            format_account_choices(&["work", "personal"]),
            "work, personal"
        );
        assert_eq!(format_account_choices(&[]), "");
    }
}
