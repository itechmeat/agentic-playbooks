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
use crate::manifest::{DoubledAccount, ManifestAccount, ManifestConnector, ManifestConnectorGrant};

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
///
/// `doubled` marks a doubled eval run: every step runs as for any run, but
/// each connector is recorded with no account in `accounts` and its
/// accounts' names and default flags in `doubled_accounts`, so no binary,
/// of this version or an older one, can resolve an account to call a
/// service through the run's snapshot.
pub fn snapshot_connectors(
    root: &Path,
    run_dir: &Path,
    playbook: &Playbook,
    expected_connectors: &BTreeMap<String, String>,
    expected_accounts: &BTreeMap<String, String>,
    doubled: bool,
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
        if doubled {
            connectors.push(ManifestConnector {
                name: cname.clone(),
                digest: resolved.loaded.digest.clone(),
                accounts: Vec::new(),
                doubled_accounts: resolved
                    .accounts
                    .iter()
                    .map(|a| DoubledAccount {
                        name: a.name.clone(),
                        default: a.default,
                    })
                    .collect(),
            });
            continue;
        }
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
            doubled_accounts: Vec::new(),
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
                on_unknown: g.on_unknown,
            })
            .collect();
        grants.insert(node_id.clone(), mapped);
    }

    Ok((connectors, grants))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A doubled run's snapshot records each connector with no account in
    /// `accounts` and only the names and default flags in
    /// `doubled_accounts`: no field, no variable name, no command, so no
    /// binary can resolve an account from it.
    #[test]
    fn a_doubled_snapshot_records_no_account() {
        apb_core::process_mode::testing::in_plain_child(
            "connector::run::tests::a_doubled_snapshot_records_no_account",
            || {
                let cfg = tempfile::tempdir().unwrap();
                // SAFETY: a child process runs this one test on one thread.
                unsafe { std::env::set_var("APB_CONFIG_DIR", cfg.path()) };
                let src = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/connectors/mock-tracker/connector.yaml");
                let dst = cfg.path().join("connectors/mock-tracker");
                std::fs::create_dir_all(&dst).unwrap();
                std::fs::copy(&src, dst.join("connector.yaml")).unwrap();
                let root = tempfile::tempdir().unwrap();
                let run = tempfile::tempdir().unwrap();
                let accounts = config::project_config_path(root.path(), "mock-tracker");
                std::fs::create_dir_all(accounts.parent().unwrap()).unwrap();
                std::fs::write(
                    &accounts,
                    "accounts:\n  - name: main\n    default: true\n    base_url: https://unused.example\n    token: \"{{env.APB_DOUBLED_TOKEN_VAR}}\"\n",
                )
                .unwrap();
                // A command-sourced secret is a global account's only.
                let global = config::global_config_path("mock-tracker").unwrap();
                std::fs::create_dir_all(global.parent().unwrap()).unwrap();
                std::fs::write(
                    &global,
                    "accounts:\n  - name: other\n    base_url: https://other.example\n    token: \"{{cmd:printf other-secret}}\"\n",
                )
                .unwrap();
                std::fs::write(
                    root.path().join(".apb/secrets.env"),
                    "APB_DOUBLED_TOKEN_VAR=x\n",
                )
                .unwrap();
                let pb = Playbook::from_yaml(
                    "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, profile: x, connectors: [{ name: mock-tracker, functions: read_only }] }\nedges: []\n",
                )
                .unwrap();
                let out = resolve_playbook(root.path(), &pb).unwrap();
                let mut conns = BTreeMap::new();
                let mut accts = BTreeMap::new();
                for (name, r) in &out.connectors {
                    conns.insert(name.clone(), r.loaded.digest.clone());
                    for a in &r.accounts {
                        accts.insert(format!("{name}/{}", a.name), account_digest(a));
                    }
                }
                let (connectors, grants) =
                    snapshot_connectors(root.path(), run.path(), &pb, &conns, &accts, true)
                        .unwrap();
                assert_eq!(connectors.len(), 1);
                let c = &connectors[0];
                assert!(c.accounts.is_empty(), "{c:?}");
                let mut names: Vec<(String, bool)> = c
                    .doubled_accounts
                    .iter()
                    .map(|a| (a.name.clone(), a.default))
                    .collect();
                names.sort();
                assert_eq!(
                    names,
                    [("main".to_string(), true), ("other".to_string(), false)]
                );
                let text = serde_json::to_string(c).unwrap();
                for absent in ["APB_DOUBLED_TOKEN_VAR", "printf", "unused.example", "cmd"] {
                    assert!(!text.contains(absent), "`{absent}` in {text}");
                }
                // Grants keep their account names.
                let mut granted = grants["a"][0].accounts.clone();
                granted.sort();
                assert_eq!(granted, ["main", "other"]);
                // A run that is not doubled records its accounts as on main.
                let (plain, _) =
                    snapshot_connectors(root.path(), run.path(), &pb, &conns, &accts, false)
                        .unwrap();
                assert_eq!(plain[0].accounts.len(), 2);
                assert!(plain[0].doubled_accounts.is_empty());
            },
        );
    }
}
