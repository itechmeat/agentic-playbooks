//! Context-ful binding validation and grant resolution (spec
//! 2026-07-18-connectors-design, section 5): turns a playbook's structurally
//! valid `ConnectorBinding`s into fully resolved grants against the actual
//! installed connectors and their configured accounts. This is the layer
//! that needs the filesystem (installed connectors, account config) on top
//! of the FS-free structural checks already done by `schema.rs` parsing and
//! validator V23-V26.
//!
//! Each distinct connector a playbook binds is loaded and its accounts
//! merged/validated exactly once (`BTreeMap` keyed by connector name), no
//! matter how many nodes or bindings reference it; per-binding grant
//! expansion then only ever reads from that one resolved connector. Errors
//! naming a connector as a whole (failed to load, config-level account
//! errors) are reported once per connector; errors specific to one node's
//! binding (an allowlisted account or function that does not exist) carry
//! that node's id so the caller can point back at the offending binding.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::schema::{FunctionsAllow, Playbook};

use super::config::{self, Account};
use super::secrets;
use super::store::{self, LoadedConnector};

/// A node's fully resolved grant for one connector binding, ready to be
/// snapshotted into the run manifest: full non-secret account objects (not
/// just names) and an explicit function list (the `read_only` shorthand
/// already expanded against the connector's manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGrant {
    pub connector: String,
    pub accounts: Vec<Account>,
    pub functions: Vec<String>,
    pub max_calls: Option<u32>,
}

/// Everything the policy gate and the engine need about one connector a
/// playbook binds, resolved once regardless of how many nodes reference it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConnector {
    pub loaded: LoadedConnector,
    /// Merged and validated accounts (global + project, spec 4.1).
    pub accounts: Vec<Account>,
    /// Union of env var names referenced by every merged account's secret
    /// fields, sorted and deduped.
    pub required_env: Vec<String>,
}

/// The result of resolving every connector binding in a playbook: the
/// resolved connectors keyed by name, the resolved grants keyed by node id
/// (a node with no connector bindings contributes no entry), and non-fatal
/// warnings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolutionOutput {
    pub connectors: BTreeMap<String, ResolvedConnector>,
    pub grants: BTreeMap<String, Vec<ResolvedGrant>>,
    pub warnings: Vec<String>,
}

/// Resolves every connector binding across `playbook.nodes` (via
/// `NodeKind::connector_bindings`) against the connectors actually installed
/// under `root`'s config, and the accounts actually configured for them.
///
/// Every distinct connector name referenced anywhere in the playbook is
/// loaded and its accounts merged/validated exactly once; per-binding grant
/// expansion (accounts allowlist, functions allowlist/shorthand) then reads
/// from that single resolved connector. All issues are collected rather than
/// stopping at the first, so a caller sees the whole picture in one pass:
/// - a bound connector is not installed, or fails to load
/// - the merged account config for a bound connector fails validation
///   (`config::validate_accounts`)
/// - a binding's `accounts` allowlist names an account not in the merged
///   config for that connector
/// - a binding's `functions` allowlist names a function the connector does
///   not declare
/// - a binding's `functions: read_only` expands to zero functions
///
/// On success, `ResolutionOutput::warnings` carries any non-fatal issues:
/// a deprecated function pulled into a grant, and the project
/// `.apb/secrets.env` / `.gitignore` coverage gap
/// (`secrets::gitignore_gap`). Warnings are only surfaced when resolution
/// otherwise succeeds; a run that fails hard reports errors, not warnings.
pub fn resolve_playbook(root: &Path, playbook: &Playbook) -> Result<ResolutionOutput, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut connectors: BTreeMap<String, ResolvedConnector> = BTreeMap::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();

    let mut names: BTreeSet<String> = BTreeSet::new();
    for node in &playbook.nodes {
        for binding in node.kind.connector_bindings() {
            names.insert(binding.name.clone());
        }
    }

    for name in &names {
        match resolve_connector(root, name) {
            Ok(resolved) => {
                connectors.insert(name.clone(), resolved);
            }
            Err(mut errs) => {
                errors.append(&mut errs);
                failed.insert(name.clone());
            }
        }
    }

    let mut grants: BTreeMap<String, Vec<ResolvedGrant>> = BTreeMap::new();
    for node in &playbook.nodes {
        let bindings = node.kind.connector_bindings();
        if bindings.is_empty() {
            continue;
        }

        let mut node_grants = Vec::new();
        for binding in bindings {
            if failed.contains(&binding.name) {
                // The connector-level error was already recorded once; a
                // binding referencing a broken connector adds nothing new.
                continue;
            }
            let Some(resolved) = connectors.get(&binding.name) else {
                // Unreachable in practice: every binding's name was inserted
                // into `names` above, so it is either in `connectors` or
                // `failed`. Kept as a defensive error rather than a panic.
                errors.push(format!(
                    "node `{}`: connector `{}` was not resolved",
                    node.id, binding.name
                ));
                continue;
            };

            match expand_grant(&node.id, binding, resolved, &mut warnings) {
                Ok(grant) => node_grants.push(grant),
                Err(mut errs) => errors.append(&mut errs),
            }
        }
        grants.insert(node.id.clone(), node_grants);
    }

    if secrets::gitignore_gap(root) {
        warnings.push(".apb/secrets.env exists but is not covered by .gitignore".to_string());
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(ResolutionOutput {
        connectors,
        grants,
        warnings,
    })
}

/// Loads and validates one connector by name: manifest load, merged account
/// config, and `config::validate_accounts`. Returns every issue found (a
/// load failure short-circuits since there is nothing further to check; a
/// validation failure lists every offending account/field).
fn resolve_connector(root: &Path, name: &str) -> Result<ResolvedConnector, Vec<String>> {
    let loaded = store::load(name).map_err(|e| vec![format!("connector `{name}`: {e}")])?;

    let merged =
        config::load_merged(root, name).map_err(|e| vec![format!("connector `{name}`: {e}")])?;

    let acct_errors = config::validate_accounts(&loaded.doc, &merged);
    if !acct_errors.is_empty() {
        return Err(acct_errors
            .into_iter()
            .map(|e| format!("connector `{name}`: {e}"))
            .collect());
    }

    let mut env_set: BTreeSet<String> = BTreeSet::new();
    for account in &merged {
        for var in config::env_refs(&loaded.doc, account).into_values() {
            env_set.insert(var);
        }
    }

    Ok(ResolvedConnector {
        loaded,
        accounts: merged,
        required_env: env_set.into_iter().collect(),
    })
}

/// Expands one binding into a `ResolvedGrant` against its already-resolved
/// connector: the `accounts` allowlist (or every merged account when
/// `None`), and the `functions` grant (`All`, `ReadOnly` via
/// `ConnectorDoc::read_only_functions`, or an explicit `List` checked
/// against the manifest). Appends a deprecation warning for every
/// deprecated function that ends up in the grant. Collects every issue
/// found in this one binding rather than stopping at the first.
fn expand_grant(
    node_id: &str,
    binding: &crate::schema::ConnectorBinding,
    resolved: &ResolvedConnector,
    warnings: &mut Vec<String>,
) -> Result<ResolvedGrant, Vec<String>> {
    let mut errors = Vec::new();

    let accounts: Vec<Account> = match &binding.accounts {
        None => resolved.accounts.clone(),
        Some(names) => {
            let mut out = Vec::new();
            for account_name in names {
                match resolved.accounts.iter().find(|a| &a.name == account_name) {
                    Some(a) => out.push(a.clone()),
                    None => errors.push(format!(
                        "node `{node_id}`: connector `{}` account `{account_name}` not configured",
                        binding.name
                    )),
                }
            }
            out
        }
    };

    let functions: Vec<String> = match &binding.functions {
        FunctionsAllow::All => resolved
            .loaded
            .doc
            .functions
            .iter()
            .map(|f| f.name.clone())
            .collect(),
        FunctionsAllow::ReadOnly => {
            let read_only = resolved.loaded.doc.read_only_functions();
            if read_only.is_empty() {
                errors.push(format!(
                    "node `{node_id}`: connector `{}` functions: read_only has zero read-only functions",
                    binding.name
                ));
            }
            read_only
        }
        FunctionsAllow::List(names) => {
            let mut out = Vec::new();
            for fname in names {
                if resolved.loaded.doc.function(fname).is_some() {
                    out.push(fname.clone());
                } else {
                    errors.push(format!(
                        "node `{node_id}`: connector `{}` function `{fname}` is not declared",
                        binding.name
                    ));
                }
            }
            out
        }
    };

    if !errors.is_empty() {
        return Err(errors);
    }

    for fname in &functions {
        if let Some(f) = resolved.loaded.doc.function(fname)
            && let Some(reason) = &f.deprecated
        {
            warnings.push(format!(
                "node `{node_id}`: connector `{}` function `{fname}` is deprecated: {reason}",
                binding.name
            ));
        }
    }

    Ok(ResolvedGrant {
        connector: binding.name.clone(),
        accounts,
        functions,
        max_calls: binding.max_calls,
    })
}

/// Env var names referenced by every installed connector's merged account
/// configs (both global and project scope) - the adapter scrub list (spec
/// 4.3): every one of these must be stripped from a spawned agent's
/// environment regardless of whether the current playbook actually binds
/// that connector, since a prior run's process env could otherwise leak a
/// secret into an unrelated agent. Sorted and deduped.
///
/// Best-effort by design: this feeds a defensive scrub list, not a
/// validation gate, so one broken connector must never stop a run over an
/// unrelated connector's problem. `store::list()` already skips a connector
/// whose `connector.yaml` fails to parse; on top of that, a connector whose
/// merged account config fails to load (an unparsable project or global
/// account file) is silently skipped here too - the run proceeds without
/// that connector's env names in the scrub list rather than failing outright.
pub fn all_referenced_env_names(root: &Path) -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();

    for summary in store::list() {
        let Ok(loaded) = store::load(&summary.name) else {
            continue;
        };
        let Ok(merged) = config::load_merged(root, &summary.name) else {
            continue;
        };
        for account in &merged {
            for var in config::env_refs(&loaded.doc, account).into_values() {
                names.insert(var);
            }
        }
    }

    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a small `mock-tracker`-like connector fixture into `cfg`'s
    /// connectors dir: two functions (one read-only, one deprecated
    /// non-read-only) and one secret account field. `cfg` is assumed to be
    /// the value of `APB_CONFIG_DIR` for the calling test. Kept
    /// self-contained here per the task brief; later engine tests copy the
    /// pattern rather than importing it.
    fn write_fixture_connector(cfg: &Path) {
        let dir = cfg.join("connectors").join("mock-tracker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("connector.yaml"),
            r#"
name: mock-tracker
version: 0.1.0
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: list_items
    description: List items
    read_only: true
    method: GET
    url: "{{account.base_url}}/items"
  - name: create_item
    description: Create an item
    method: POST
    url: "{{account.base_url}}/items"
    body: "{{args}}"
  - name: legacy_item
    description: Legacy write operation
    deprecated: "use create_item instead"
    method: POST
    url: "{{account.base_url}}/legacy"
"#,
        )
        .unwrap();
    }

    fn write_project_account(root: &Path, connector: &str, yaml: &str) {
        let path = config::project_config_path(root, connector);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, yaml).unwrap();
    }

    fn node_playbook(node_yaml: &str) -> Playbook {
        let yaml = format!(
            r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - {{ id: s, type: start }}
  - {node_yaml}
edges: []
"#
        );
        Playbook::from_yaml(&yaml).unwrap()
    }

    struct EnvGuard {
        var: &'static str,
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.var, v),
                    None => std::env::remove_var(self.var),
                }
            }
        }
    }

    fn set_config_dir(cfg: &Path) -> EnvGuard {
        let prior = std::env::var_os("APB_CONFIG_DIR");
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg);
        }
        EnvGuard {
            var: "APB_CONFIG_DIR",
            prior,
        }
    }

    // --- happy path ---------------------------------------------------

    #[test]
    fn happy_path_resolves_grants_with_read_only_shorthand() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.MOCK_TOKEN}}"
"#,
        );

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{ name: mock-tracker, functions: read_only }]",
        );

        let out = resolve_playbook(root.path(), &pb).unwrap();

        let resolved = out.connectors.get("mock-tracker").unwrap();
        assert_eq!(resolved.accounts.len(), 1);
        assert_eq!(resolved.accounts[0].name, "acct1");
        assert_eq!(resolved.required_env, vec!["MOCK_TOKEN".to_string()]);

        let grants = out.grants.get("a").unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].connector, "mock-tracker");
        assert_eq!(grants[0].functions, vec!["list_items".to_string()]);
        assert_eq!(grants[0].accounts, resolved.accounts);
        assert!(grants[0].max_calls.is_none());
    }

    #[test]
    fn node_without_bindings_contributes_empty_grants() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        let out = resolve_playbook(root.path(), &pb).unwrap();
        assert!(out.grants.is_empty());
        assert!(out.connectors.is_empty());
    }

    // --- errors ---------------------------------------------------------

    #[test]
    fn unknown_account_in_allowlist_errors_with_node_id() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.MOCK_TOKEN}}"
"#,
        );

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{ name: mock-tracker, accounts: [missing] }]",
        );

        let errs = resolve_playbook(root.path(), &pb).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("node `a`")
                && e.contains("connector `mock-tracker`")
                && e.contains("account `missing`")
                && e.contains("not configured")),
            "errors: {errs:?}"
        );
    }

    #[test]
    fn unknown_function_in_allowlist_errors_with_node_id() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.MOCK_TOKEN}}"
"#,
        );

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{ name: mock-tracker, functions: [nope] }]",
        );

        let errs = resolve_playbook(root.path(), &pb).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("node `a`")
                && e.contains("connector `mock-tracker`")
                && e.contains("function `nope`")),
            "errors: {errs:?}"
        );
    }

    #[test]
    fn read_only_with_zero_read_only_functions_errors() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        std::fs::create_dir_all(cfg.path().join("connectors/plain-tracker")).unwrap();
        std::fs::write(
            cfg.path().join("connectors/plain-tracker/connector.yaml"),
            r#"
name: plain-tracker
version: 0.1.0
functions:
  - name: do_thing
    description: does a thing
    method: GET
    url: "https://example.com/thing"
"#,
        )
        .unwrap();
        write_project_account(root.path(), "plain-tracker", "accounts: []\n");

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{ name: plain-tracker, functions: read_only }]",
        );

        let errs = resolve_playbook(root.path(), &pb).unwrap_err();
        assert!(
            errs.iter().any(|e| e.contains("node `a`")
                && e.contains("connector `plain-tracker`")
                && e.contains("read_only")
                && e.contains("zero")),
            "errors: {errs:?}"
        );
    }

    #[test]
    fn connector_not_installed_is_a_connector_level_error() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [nonexistent]",
        );

        let errs = resolve_playbook(root.path(), &pb).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("connector `nonexistent`") && !e.contains("node `")),
            "errors: {errs:?}"
        );
    }

    #[test]
    fn connector_level_account_validation_errors_are_reported_once() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        // Missing the required `token` field.
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
"#,
        );

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [mock-tracker]",
        );

        let errs = resolve_playbook(root.path(), &pb).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("connector `mock-tracker`") && e.contains("token")),
            "errors: {errs:?}"
        );
    }

    // --- warnings ---------------------------------------------------------

    #[test]
    fn deprecated_function_in_grant_produces_warning() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.MOCK_TOKEN}}"
"#,
        );

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [{ name: mock-tracker, functions: [legacy_item] }]",
        );

        let out = resolve_playbook(root.path(), &pb).unwrap();
        assert!(out.warnings.iter().any(|w| {
            w == "node `a`: connector `mock-tracker` function `legacy_item` is deprecated: use create_item instead"
        }), "warnings: {:?}", out.warnings);
    }

    #[test]
    fn gitignore_gap_warning_included_when_true() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.MOCK_TOKEN}}"
"#,
        );
        std::fs::write(
            secrets::project_secrets_path(root.path()),
            "MOCK_TOKEN=shh\n",
        )
        .unwrap();

        let pb = node_playbook(
            "id: a\n    type: agent_task\n    prompt: hi\n    profile: x\n    connectors: [mock-tracker]",
        );

        let out = resolve_playbook(root.path(), &pb).unwrap();
        assert!(
            out.warnings
                .iter()
                .any(|w| w == ".apb/secrets.env exists but is not covered by .gitignore"),
            "warnings: {:?}",
            out.warnings
        );
    }

    // --- all_referenced_env_names ------------------------------------------

    #[test]
    fn all_referenced_env_names_unions_across_connectors_sorted_deduped() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        write_project_account(
            root.path(),
            "mock-tracker",
            r#"
accounts:
  - name: acct1
    base_url: https://example.com
    token: "{{env.SHARED_TOKEN}}"
  - name: acct2
    base_url: https://example2.com
    token: "{{env.MOCK_TOKEN}}"
"#,
        );

        std::fs::create_dir_all(cfg.path().join("connectors/other-tracker")).unwrap();
        std::fs::write(
            cfg.path().join("connectors/other-tracker/connector.yaml"),
            r#"
name: other-tracker
version: 0.1.0
account_fields:
  - name: api_key
    required: true
    secret: true
functions:
  - name: ping
    description: d
    mock: { status: 200, body: {} }
"#,
        )
        .unwrap();
        write_project_account(
            root.path(),
            "other-tracker",
            r#"
accounts:
  - name: acct1
    api_key: "{{env.SHARED_TOKEN}}"
"#,
        );

        let names = all_referenced_env_names(root.path());
        assert_eq!(
            names,
            vec!["MOCK_TOKEN".to_string(), "SHARED_TOKEN".to_string(),]
        );
    }

    #[test]
    fn all_referenced_env_names_skips_connector_with_broken_account_config() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let _guard = set_config_dir(cfg.path());

        write_fixture_connector(cfg.path());
        // Unparsable project account file for mock-tracker: best-effort skip.
        write_project_account(root.path(), "mock-tracker", "not: [valid: yaml");

        assert!(all_referenced_env_names(root.path()).is_empty());
    }

    #[test]
    fn all_referenced_env_names_empty_without_config_dir() {
        let _lock = crate::env_test_lock();
        let root = tempfile::tempdir().unwrap();
        struct FullGuard(Option<std::ffi::OsString>);
        impl Drop for FullGuard {
            fn drop(&mut self) {
                unsafe {
                    match &self.0 {
                        Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                        None => std::env::remove_var("APB_CONFIG_DIR"),
                    }
                }
            }
        }
        let _g = FullGuard(std::env::var_os("APB_CONFIG_DIR"));
        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        assert!(all_referenced_env_names(root.path()).is_empty());
    }
}
