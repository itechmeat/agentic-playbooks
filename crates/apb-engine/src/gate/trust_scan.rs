//! The trust facts of one playbook, collected without refusing: every bound
//! connector and account with its digest and whether it is approved, the
//! secret env vars that do not resolve, and every profile bundle with its
//! digest. The run gate turns a scan into its refusals (in a fixed order);
//! the fit report shows the whole scan.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use apb_core::connector::config::account_digest;
use apb_core::connector::resolve::resolve_playbook;
use apb_core::connector::secrets::missing_vars;
use apb_core::profile::QualifiedProfileRef;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::schema::Playbook;
use apb_core::trust::{TrustStore, account_trust_id};
use serde_json::{Value, json};

/// The connector side of a playbook's trust.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConnectorScan {
    /// `connector name -> tree digest`, for every bound connector.
    pub connectors: BTreeMap<String, String>,
    /// `"connector/account" -> account digest`, for every merged account.
    pub accounts: BTreeMap<String, String>,
    /// Required secret env vars that do not resolve.
    pub missing_env: Vec<String>,
    /// Connectors whose digest is not approved.
    pub untrusted_connectors: Vec<String>,
    /// Accounts whose digest is not approved, with their non-secret display
    /// in `account_fields`.
    pub unapproved_accounts: Vec<String>,
    pub account_fields: serde_json::Map<String, Value>,
    /// Connectors bound with no configured account (`connector`), and the
    /// consent-time warning for each.
    pub no_account: Vec<String>,
    pub warnings: Vec<String>,
}

/// Scans the connectors `playbook` binds. `Err` holds the resolution errors
/// when a bound connector does not resolve.
pub(crate) fn scan_connectors(
    root: &Path,
    playbook: &Playbook,
) -> Result<ConnectorScan, Vec<String>> {
    let mut scan = ConnectorScan::default();
    if !playbook.binds_connectors() {
        return Ok(scan);
    }
    let resolution = resolve_playbook(root, playbook)?;

    let required: BTreeSet<String> = resolution
        .connectors
        .values()
        .flat_map(|r| r.required_env.iter().cloned())
        .collect();
    scan.missing_env = missing_vars(root, &required.into_iter().collect::<Vec<_>>());

    let store = TrustStore::load();
    for (name, resolved) in &resolution.connectors {
        let digest = resolved.loaded.digest.clone();
        if !store.is_approved(&digest) {
            scan.untrusted_connectors.push(name.clone());
        }
        scan.connectors.insert(name.clone(), digest);

        // A bound connector with zero accounts passes the gate (a
        // configuration gap, not the secret-egress problem trust guards), but
        // every node bound to it would fail at call time: a consent-time
        // warning naming the nodes and the functions they call.
        if resolved.accounts.is_empty() {
            scan.no_account.push(name.clone());
            scan.warnings
                .push(no_account_warning(name, &resolution.grants));
        }

        for account in &resolved.accounts {
            let id = account_trust_id(name, &account.name);
            let adigest = account_digest(account);
            if !store.is_approved(&adigest) && !scan.unapproved_accounts.contains(&id) {
                scan.unapproved_accounts.push(id.clone());
                scan.account_fields
                    .insert(id.clone(), account_display(&resolved.loaded.doc, account));
            }
            scan.accounts.insert(id, adigest);
        }
    }
    Ok(scan)
}

fn no_account_warning(
    name: &str,
    grants: &BTreeMap<String, Vec<apb_core::connector::resolve::ResolvedGrant>>,
) -> String {
    let mut node_ids: BTreeSet<String> = BTreeSet::new();
    let mut functions: BTreeSet<String> = BTreeSet::new();
    for (node_id, grants) in grants {
        for grant in grants.iter().filter(|g| g.connector == name) {
            node_ids.insert(node_id.clone());
            functions.extend(grant.functions.iter().cloned());
        }
    }
    if node_ids.is_empty() {
        // Defensive: a bound connector should always have a grant, but prefer
        // the connector-only wording over empty brackets.
        format!(
            "connector `{name}` is bound but has no configured accounts; nodes that call it will fail until an account is added"
        )
    } else {
        let nodes = node_ids.into_iter().collect::<Vec<_>>().join(", ");
        let funcs = functions.into_iter().collect::<Vec<_>>().join(", ");
        format!(
            "connector `{name}` is bound but has no configured accounts; nodes [{nodes}] call functions [{funcs}] and will fail until an account is added"
        )
    }
}

/// Non-secret display of an account for an approval prompt (spec 7: the user
/// sees the concrete fields they approve). Every value is safe: a secret-marked
/// field holds only its raw `{{env.VAR}}` or `{{cmd:...}}` reference in the
/// config, never the resolved secret, so the whole `fields` map plus the
/// `default` flag can be shown. This mirrors exactly what the account digest
/// pins. `cmd` names each secret read from a command, with the command line:
/// approving the account authorizes apb to run it.
fn account_display(
    doc: &apb_core::connector::def::ConnectorDoc,
    account: &apb_core::connector::config::Account,
) -> Value {
    let fields: serde_json::Map<String, Value> = account
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    json!({
        "default": account.default,
        "fields": Value::Object(fields),
        "cmd": apb_core::connector::config::cmd_refs(doc, account),
    })
}

/// The profile side of a playbook's trust.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProfileScan {
    /// `<scope>/<name> -> bundle digest`, for every reference.
    pub verified: BTreeMap<String, String>,
    /// The keys whose bundle digest is not approved, in reference order.
    pub untrusted: Vec<String>,
}

/// Computes the bundle of every profile in `refs`, resolved for a playbook
/// of `origin`. The refusal (`profile_unresolved`) when one does not
/// resolve.
pub(crate) fn scan_profile_bundles(
    root: &Path,
    refs: &[QualifiedProfileRef],
    origin: PlaybookOrigin,
) -> Result<ProfileScan, Value> {
    let mut scan = ProfileScan::default();
    if refs.is_empty() {
        return Ok(scan);
    }
    let store = TrustStore::load();
    for r in refs {
        match profile_store::compute_bundle(root, origin, r) {
            Ok((loaded, _pairs, bundle)) => {
                let key = format!("{}/{}", profile_store::scope_str(loaded.scope), loaded.name);
                if !store.is_approved(&bundle) && !scan.untrusted.contains(&key) {
                    scan.untrusted.push(key.clone());
                }
                scan.verified.insert(key, bundle);
            }
            Err(e) => {
                return Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() }));
            }
        }
    }
    Ok(scan)
}
