//! Run-start connector snapshot (spec 2026-07-18-connectors-design, section
//! 6): the engine-side half of the anti-TOCTOU connector pipeline. The policy
//! gate resolves every bound connector once and hands the engine two permit
//! maps verbatim (connector name -> tree digest, `connector/account` ->
//! account digest). At run start `snapshot_connectors` re-resolves the same
//! playbook against the live files, verifies both maps EXACTLY (any drift is a
//! hard error naming the drifted item), checks every required secret env var
//! resolves, copies each used `connector.yaml` into `runs/<id>/connectors/`,
//! and returns the immutable manifest pieces. Everything after start reads the
//! snapshot, never the live connector or account files (only secret VALUES
//! resolve live, at call time).

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::connector::config::{self, account_digest};
use apb_core::connector::resolve::resolve_playbook;
use apb_core::connector::secrets;
use apb_core::schema::Playbook;

use crate::error::EngineError;
use crate::manifest::{ManifestAccount, ManifestConnector, ManifestConnectorGrant};

/// The manifest pieces `snapshot_connectors` returns: the connectors to record
/// under `manifest.connectors`, and the per-node grant map keyed by node id.
type SnapshotOutput = (
    Vec<ManifestConnector>,
    BTreeMap<String, Vec<ManifestConnectorGrant>>,
);

/// Re-resolves the playbook's connectors at run start, verifies BOTH permit
/// maps verbatim, verifies every required env var resolves, copies each used
/// `connector.yaml` into `run_dir/connectors/<name>.yaml`, and returns the
/// manifest connectors + per-node grants.
///
/// `expected_connectors` (name -> tree digest) and `expected_accounts`
/// (`connector/account` -> account digest) come straight from the policy
/// gate's `RunPermit` and are never recomputed here - they are checked against
/// the live resolution and any mismatch, missing, or extra key is a hard
/// `EngineError::Invalid` naming the offending item and the kind of drift.
pub fn snapshot_connectors(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
    expected_connectors: &BTreeMap<String, String>,
    expected_accounts: &BTreeMap<String, String>,
) -> Result<SnapshotOutput, EngineError> {
    // 1. Re-resolve against the live files. Resolution errors (a connector no
    // longer installed, broken account config, an allowlisted account/function
    // that vanished) become one joined Invalid error.
    let output =
        resolve_playbook(root, playbook).map_err(|errs| EngineError::Invalid(errs.join("; ")))?;

    // 2. Verify the connector permit verbatim: exact key set + matching digest.
    // A connector resolved but absent from the permit did not go through the
    // trust check (fail closed); an expected connector not resolved means the
    // run does not actually use it; a digest mismatch means the folder changed
    // between the gate and start.
    for (name, resolved) in &output.connectors {
        match expected_connectors.get(name) {
            Some(exp) if exp == &resolved.loaded.digest => {}
            Some(_) => {
                return Err(EngineError::Invalid(format!(
                    "connector `{name}` changed since the trust check (digest mismatch)"
                )));
            }
            None => {
                return Err(EngineError::Invalid(format!(
                    "connector `{name}` was not covered by the trust check"
                )));
            }
        }
    }
    for name in expected_connectors.keys() {
        if !output.connectors.contains_key(name) {
            return Err(EngineError::Invalid(format!(
                "connector `{name}` was in the trust check but is not used by the run"
            )));
        }
    }

    // 3. Verify the account permit verbatim, keyed `connector/account` against
    // the account digest of each merged account of every USED connector. Same
    // exact-key-set + matching-digest rule as connectors.
    let mut actual_accounts: BTreeMap<String, String> = BTreeMap::new();
    for (cname, resolved) in &output.connectors {
        for account in &resolved.accounts {
            actual_accounts.insert(format!("{cname}/{}", account.name), account_digest(account));
        }
    }
    for (key, digest) in &actual_accounts {
        match expected_accounts.get(key) {
            Some(exp) if exp == digest => {}
            Some(_) => {
                return Err(EngineError::Invalid(format!(
                    "connector account `{key}` changed since the trust check (digest mismatch)"
                )));
            }
            None => {
                return Err(EngineError::Invalid(format!(
                    "connector account `{key}` was not covered by the trust check"
                )));
            }
        }
    }
    for key in expected_accounts.keys() {
        if !actual_accounts.contains_key(key) {
            return Err(EngineError::Invalid(format!(
                "connector account `{key}` was in the trust check but is not configured for the run"
            )));
        }
    }

    // 4. Every secret env var referenced by a used connector's accounts must
    // resolve now (spec 6). A missing one is a hard error naming both the
    // account it belongs to and the variable.
    for (cname, resolved) in &output.connectors {
        for account in &resolved.accounts {
            let vars: Vec<String> = config::env_refs(&resolved.loaded.doc, account)
                .into_values()
                .collect();
            let missing = secrets::missing_vars(root, &vars);
            if let Some(var) = missing.first() {
                return Err(EngineError::Invalid(format!(
                    "connector `{cname}` account `{}` requires env var `{var}` which is not set",
                    account.name
                )));
            }
        }
    }

    // 5. Copy each used connector.yaml into the run snapshot (create the dir;
    // atomic write). The raw string is copied verbatim so the run's view of the
    // connector never depends on the live folder again.
    let dest_dir = run_dir.join("connectors");
    std::fs::create_dir_all(&dest_dir)?;
    for (cname, resolved) in &output.connectors {
        apb_core::fsutil::atomic_write(
            &dest_dir.join(format!("{cname}.yaml")),
            resolved.loaded.yaml.as_bytes(),
        )?;
    }

    // 6. Build the manifest pieces. Accounts carry their fields as-is (a secret
    // field keeps its raw `{{env.VAR}}` / `{{cmd:...}}` reference, never the
    // resolved value), an `env` map of secret-field -> env var name, a `cmd`
    // map of secret-field -> command line, and the account digest.
    let mut connectors = Vec::new();
    for (cname, resolved) in &output.connectors {
        let mut accounts = Vec::new();
        for account in &resolved.accounts {
            accounts.push(ManifestAccount {
                name: account.name.clone(),
                default: account.default,
                fields: account.fields.clone(),
                env: config::env_refs(&resolved.loaded.doc, account),
                cmd: config::cmd_refs(&resolved.loaded.doc, account),
                digest: account_digest(account),
            });
        }
        connectors.push(ManifestConnector {
            name: cname.clone(),
            digest: resolved.loaded.digest.clone(),
            accounts,
        });
    }

    // Grants keyed by node id; accounts recorded by NAME (the full non-secret
    // account object lives once under `connectors`, not duplicated per grant).
    let mut grants: BTreeMap<String, Vec<ManifestConnectorGrant>> = BTreeMap::new();
    for (node_id, node_grants) in &output.grants {
        if node_grants.is_empty() {
            continue;
        }
        let mapped = node_grants
            .iter()
            .map(|g| ManifestConnectorGrant {
                connector: g.connector.clone(),
                accounts: g.accounts.iter().map(|a| a.name.clone()).collect(),
                functions: g.functions.clone(),
                max_calls: g.max_calls,
            })
            .collect();
        grants.insert(node_id.clone(), mapped);
    }

    Ok((connectors, grants))
}
