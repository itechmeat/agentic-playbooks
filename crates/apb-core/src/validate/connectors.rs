//! Connector rules: the grants a playbook declares, the function allowlists
//! inside them, and the two inbox rules that need to know what the installed
//! connectors actually look like.

use super::*;

/// V23 (error): a connector binding name, an `accounts` entry, or a
/// `functions` list entry fails its identifier format check. Binding names
/// and account entries are connector/account folder names - hyphen slugs
/// (`crate::profile::validate_profile_name`); `functions` list entries are
/// the connector's snake_case function names
/// (`crate::connector::validate_snake_name`). V24 (error): a node binds the
/// same connector name more than once. V25 (error): an `accounts` or
/// `functions` list entry that is empty or repeated within one binding. V26
/// (error): `max_calls` is 0 (a binding that can never be called).
///
/// V42 (error) covers two cases, both about a binding that cannot do what it
/// says. First, a node grants inbox functions of a connector whose manifest
/// carries no `webhook` block, so no delivery could ever reach the inbox it
/// intends to read. The manifest-internal version of that rule lives in
/// `ConnectorDoc::from_yaml`; V42 catches the case where an installed
/// connector lost its block after a playbook was authored against it. Second,
/// and in practice more often, a node binds a connector that is installed but
/// whose manifest no longer loads at all. That case is not inbox-specific (a
/// node binding the connector purely for HTTP functions is equally broken) and
/// its message says so; it is checked first because a manifest that failed to
/// load carries no function list, so whether the binding reaches an inbox
/// function cannot be determined. See `check_inbox_binding`.
/// V43 (error): a node grants inbox functions of a connector whose webhook
/// block references account fields that a selectable account does not
/// define, so a delivery to that account could never be verified. The
/// accounts it checks are the GLOBAL ones only, matching what a hook URL can
/// address: `/hooks/{connector}/{account}` carries no workspace, so a
/// project-scoped account cannot receive anything and must not be blessed
/// here. `apb connector doctor` reports that case separately.
///
/// Both inbox rules read `ctx.connectors` and are silent when it is empty:
/// a caller with no connector store cannot decide either way, and a false
/// error there would block every playbook on a machine that simply has not
/// installed the connector yet.
pub(crate) fn check_connectors(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    for (site, bindings) in playbook.connector_sites() {
        let site: &str = &site;
        let label = crate::connector::sites::site_label(site);
        let mut seen_connectors = HashSet::new();
        for b in bindings.iter() {
            if !seen_connectors.insert(b.name.as_str()) {
                r.error(
                    "V24",
                    Some(site),
                    format!("{} binds connector `{}` more than once", label, b.name),
                );
            }
            if let Err(msg) = crate::profile::validate_profile_name(&b.name) {
                r.error(
                    "V23",
                    Some(site),
                    format!(
                        "{} connector `{}` has an invalid name: {msg}",
                        label, b.name
                    ),
                );
            }
            if let Some(accounts) = &b.accounts {
                check_connector_list(site, &b.name, "accounts", accounts, r, |item| {
                    crate::profile::validate_profile_name(item)
                });
            }
            if let FunctionsAllow::List(names) = &b.functions {
                check_connector_list(site, &b.name, "functions", names, r, |item| {
                    crate::connector::validate_snake_name(item)
                });
            }
            if b.max_calls == Some(0) {
                r.error(
                    "V26",
                    Some(site),
                    format!("{label} connector `{}` has max_calls 0", b.name),
                );
            }
            check_inbox_binding(site, b, ctx, r);
            check_send_policy(site, b, ctx, r);
        }
    }
}

/// V69 (error): a binding sets `on_unknown`, but none of the functions it
/// grants declares `idempotency: none` or `key`, the only modes the policy
/// applies to, so the setting would never take effect. V84 (error):
/// `on_unknown: resend` on a binding that grants a function flagged
/// `irreversible`, where a duplicate cannot be taken back.
///
/// Both read `ctx.connectors` and are silent when the connector is unknown
/// there or its manifest no longer loads (V42 reports that).
fn check_send_policy(
    site: &str,
    b: &crate::schema::ConnectorBinding,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    use crate::connector::idempotency::IdempotencyMode;
    use crate::schema::OnUnknown;
    let Some(policy) = b.on_unknown else {
        return;
    };
    let Some(facts) = ctx.connectors.get(&b.name) else {
        return;
    };
    if facts.load_error.is_some() {
        return;
    }
    let label = crate::connector::sites::site_label(site);
    let applies: Vec<String> = facts
        .idempotency
        .iter()
        .filter(|(_, mode)| matches!(mode, IdempotencyMode::None | IdempotencyMode::Key))
        .map(|(name, _)| name.clone())
        .collect();
    if super::decisions::granted(b, &applies).is_empty() {
        r.error(
            "V69",
            Some(site),
            format!(
                "{label} connector `{}` sets on_unknown, but none of the functions it grants declares idempotency `none` or `key`: a function with no idempotency declaration is not tracked at all, and a `marker` send is never resent automatically",
                b.name
            ),
        );
    }
    if policy == OnUnknown::Resend {
        let irreversible = super::decisions::granted(b, &facts.irreversible_functions);
        if !irreversible.is_empty() {
            r.error(
                "V84",
                Some(site),
                format!(
                    "{label} connector `{}` sets on_unknown: resend but grants irreversible function(s) {}, where a duplicate cannot be taken back",
                    b.name,
                    irreversible
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
    }
}

/// V42 and V43 for one binding, plus the unloadable-manifest case they share.
/// Does nothing unless the connector is known and either failed to load or the
/// binding actually reaches at least one of its inbox functions.
fn check_inbox_binding(
    node_id: &str,
    b: &crate::schema::ConnectorBinding,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    let label = crate::connector::sites::site_label(node_id);
    let Some(facts) = ctx.connectors.get(&b.name) else {
        return;
    };
    // An installed connector whose manifest no longer loads cannot be relied on
    // for anything, inbox or otherwise. This deliberately runs BEFORE the
    // inbox-reach check below, because a manifest that failed to load carries
    // no function list: there is no way to tell whether the binding would have
    // reached an inbox function, so deferring the check would simply lose it.
    // The manifest-internal rule in `ConnectorDoc::from_yaml` rejects an inbox
    // function that has lost its `webhook` block, which makes the whole
    // manifest stop loading; without preserving that as a fact the connector
    // would vanish from the map and V42 would stay silent on the very case it
    // exists for.
    //
    // The message is therefore connector-generic rather than inbox-specific: a
    // node binding this connector purely for HTTP functions is equally broken,
    // and telling it the inbox is unreliable would be a false explanation.
    if let Some(err) = &facts.load_error {
        r.error(
            "V42",
            Some(node_id),
            format!(
                "{label} binds connector `{}`, which is installed but its manifest no longer loads, so none of its functions can be relied on: {err}",
                b.name
            ),
        );
        return;
    }
    if facts.inbox_functions.is_empty() && !facts.has_webhook {
        return;
    }
    // Which inbox functions this binding actually reaches. An explicit list
    // is intersected with the manifest; `read_only` and `all` reach every
    // inbox function the connector declares, because both expand at run
    // start over the manifest itself.
    let granted: Vec<&String> = match &b.functions {
        FunctionsAllow::List(names) => facts
            .inbox_functions
            .iter()
            .filter(|f| names.contains(f))
            .collect(),
        _ => facts.inbox_functions.iter().collect(),
    };
    if granted.is_empty() {
        return;
    }

    if !facts.has_webhook {
        r.error(
            "V42",
            Some(node_id),
            format!(
                "{label} grants inbox function(s) {} of connector `{}`, which declares no webhook block, so no event can ever be delivered to that inbox",
                granted
                    .iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                b.name
            ),
        );
        return;
    }

    // Which accounts a call could select: the explicit allowlist, or every
    // configured account when the binding names none.
    let selectable: Vec<&String> = match &b.accounts {
        Some(list) => facts.accounts.keys().filter(|a| list.contains(a)).collect(),
        None => facts.accounts.keys().collect(),
    };
    for account in selectable {
        let Some(defined) = facts.accounts.get(account) else {
            continue;
        };
        let missing: Vec<&str> = facts
            .webhook_secret_fields
            .iter()
            .filter(|f| !defined.contains(f.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            r.error(
                "V43",
                Some(node_id),
                format!(
                    "{label} grants inbox functions of connector `{}` on account `{account}`, whose webhook block references account field(s) {} that the account does not define, so a delivery could not be verified",
                    b.name,
                    missing.join(", ")
                ),
            );
        }
    }
}

/// Checks one `accounts`/`functions` list of a connector binding: every
/// entry must be non-empty, unique within the list (V25), and pass its
/// identifier format check (V23). `field` names the offending list in the
/// message (`accounts` or `functions`).
pub(crate) fn check_connector_list(
    node_id: &str,
    connector: &str,
    field: &str,
    items: &[String],
    r: &mut ValidationReport,
    validate: impl Fn(&str) -> Result<(), String>,
) {
    let label = crate::connector::sites::site_label(node_id);
    let mut seen = HashSet::new();
    for item in items {
        if item.is_empty() {
            r.error(
                "V25",
                Some(node_id),
                format!("{label} connector `{connector}` has an empty {field} entry"),
            );
            continue;
        }
        if !seen.insert(item.as_str()) {
            r.error(
                "V25",
                Some(node_id),
                format!("{label} connector `{connector}` has duplicate {field} entry `{item}`"),
            );
        }
        if let Err(msg) = validate(item) {
            r.error(
                "V23",
                Some(node_id),
                format!("{label} connector `{connector}` {field} entry `{item}` is invalid: {msg}"),
            );
        }
    }
}
