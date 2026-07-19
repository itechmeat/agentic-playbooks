//! Account configuration: global and project account files for a connector,
//! their merge semantics, validation against the connector's declared
//! `account_fields`, and the canonical non-secret digest used for trust
//! pinning (spec 2026-07-18-connectors-design, section 4).
//!
//! Both files are non-secret and safe to commit and share: a `secret: true`
//! field never holds the secret value itself, only an `{{env.VAR}}`
//! reference resolved at call time (Task 5's secrets module).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::common::ConnectorError;
use super::def::ConnectorDoc;
use super::secrets::{parse_cmd_ref, parse_env_ref};

/// One configured account for a connector. `name` is a hyphen slug
/// (user-facing, validated with `crate::profile::validate_profile_name`,
/// like a profile name), `fields` holds the connector's declared account
/// field values keyed by their snake_case name. `#[serde(flatten)]` on
/// `fields` means `Account` cannot also carry `deny_unknown_fields` (serde
/// limitation); unknown field keys are instead reported by
/// `validate_accounts`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Account {
    pub name: String,
    #[serde(default)]
    pub default: bool,
    #[serde(flatten)]
    pub fields: BTreeMap<String, String>,
}

/// The content of one `connector-config/<name>.yaml` file: an ordered list
/// of accounts. File order matters for `load_merged`'s additivity rule.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountsFile {
    #[serde(default)]
    pub accounts: Vec<Account>,
}

/// Path to the global account config file for connector `name`:
/// `<config_dir>/connector-config/<name>.yaml`. `None` in a no-config
/// environment, mirroring `crate::config::config_dir`; `load_merged` treats
/// that as an absent (empty) file rather than an error.
pub fn global_config_path(name: &str) -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join("connector-config").join(format!("{name}.yaml")))
}

/// Path to the project account config file for connector `name`:
/// `<root>/.apb/connector-config/<name>.yaml`.
pub fn project_config_path(root: &Path, name: &str) -> PathBuf {
    root.join(".apb/connector-config")
        .join(format!("{name}.yaml"))
}

/// Reads and parses one account file. A missing file is empty (the
/// config-less / not-yet-configured path remains functional); an existing
/// file that cannot be read or does not parse is a hard `ConnectorError` -
/// never silently swallowed.
fn load_file(path: &Path) -> Result<Vec<Account>, ConnectorError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let file: AccountsFile =
                serde_yaml_ng::from_str(&raw).map_err(|e| ConnectorError::Yaml(e.to_string()))?;
            Ok(file.accounts)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(ConnectorError::Io(e)),
    }
}

/// Loads and merges the global and project account files for connector
/// `name` (spec 4.1): a project account with the same `name` replaces the
/// global one entirely; all others are additive. Resulting order:
/// global-only accounts (file order), then project accounts (file order).
///
/// Cross-scope default resolution (spec 4.1: "a project default wins over
/// a global one"): if any project account has `default: true`, the
/// `default` flag is cleared on every account that came from the global
/// file (same-named globals are already gone via the replace-by-name
/// step above, so this only touches global-only survivors). If the
/// project has no default at all, global defaults pass through
/// unchanged. This makes "at most one default in the merged list" hold
/// by construction whenever the violation spans two scopes; more than one
/// default within a single scope (two project accounts, or two global
/// accounts, both `default: true`) is left for `validate_accounts` to
/// reject, since clearing flags can't fix an ambiguity that scope itself
/// created.
pub fn load_merged(root: &Path, name: &str) -> Result<Vec<Account>, ConnectorError> {
    let global = match global_config_path(name) {
        Some(path) => load_file(&path)?,
        None => Vec::new(),
    };
    let project = load_file(&project_config_path(root, name))?;

    let project_names: std::collections::HashSet<&str> =
        project.iter().map(|a| a.name.as_str()).collect();
    let project_has_default = project.iter().any(|a| a.default);

    let mut merged: Vec<Account> = global
        .into_iter()
        .filter(|a| !project_names.contains(a.name.as_str()))
        .map(|mut a| {
            if project_has_default {
                a.default = false;
            }
            a
        })
        .collect();
    merged.extend(project);
    Ok(merged)
}

/// Validates a merged account list against the connector's declared
/// `account_fields`. Collects every issue rather than stopping at the
/// first (so a single call reports the full picture); each message names
/// the offending account and/or field. Checks:
/// 1. account name passes `crate::profile::validate_profile_name`;
/// 2. duplicate account names within the list;
/// 3. field keys not declared in `account_fields` (unknown field);
/// 4. required fields missing from an account;
/// 5. a `secret: true` field whose value is not exactly one
///    `{{env.VAR}}` or `{{cmd:...}}` reference (a literal secret value);
/// 6. more than one `default: true` account in the given list. When `accounts`
///    is the output of `load_merged`, cross-scope doubles are already
///    resolved there (project default wins), so this only fires for a
///    same-scope double: two project accounts, or two global-only
///    accounts, both `default: true`.
pub fn validate_accounts(doc: &ConnectorDoc, accounts: &[Account]) -> Vec<String> {
    let mut errors = Vec::new();

    let declared: BTreeMap<&str, bool> = doc
        .account_fields
        .iter()
        .map(|f| (f.name.as_str(), f.secret))
        .collect();

    let mut seen_names = std::collections::HashSet::new();
    let mut default_names = Vec::new();

    for account in accounts {
        if let Err(e) = crate::profile::validate_profile_name(&account.name) {
            errors.push(format!("account `{}`: {e}", account.name));
        }
        if !seen_names.insert(account.name.as_str()) {
            errors.push(format!("duplicate account name `{}`", account.name));
        }
        if account.default {
            default_names.push(account.name.as_str());
        }

        for (key, value) in &account.fields {
            match declared.get(key.as_str()) {
                None => errors.push(format!(
                    "account `{}` sets unknown field `{key}`",
                    account.name
                )),
                Some(true) => {
                    if parse_env_ref(value).is_none() && parse_cmd_ref(value).is_none() {
                        errors.push(format!(
                            "account `{}` field `{key}` must be exactly one `{{{{env.VAR}}}}` or `{{{{cmd:...}}}}` reference, not a literal value",
                            account.name
                        ));
                    }
                }
                Some(false) => {}
            }
        }

        for field in &doc.account_fields {
            if field.required && !account.fields.contains_key(field.name.as_str()) {
                errors.push(format!(
                    "account `{}` is missing required field `{}`",
                    account.name, field.name
                ));
            }
        }
    }

    if default_names.len() > 1 {
        errors.push(format!(
            "more than one default account in the merged list: {}",
            default_names.join(", ")
        ));
    }

    errors
}

/// Writes the length (u64 LE) followed by the bytes - an unambiguous field
/// separator. Local copy of `content::lp` (private there, so copied rather
/// than made public just for this).
fn lp(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

/// Canonical digest of the non-secret identity of an account (spec 4.1):
/// sha256 over the domain tag `apb-account-v1\0`, the account name, the
/// `default` flag, and every field sorted by key (`fields` is a
/// `BTreeMap`, so iteration order is already sorted). Secret fields
/// participate with their RAW config value - the `{{env.VAR}}` or
/// `{{cmd:...}}` reference string, never the resolved secret - so switching
/// a secret's source (renaming the env var, or swapping env for a command)
/// is a change the trust store sees and the user re-approves.
pub fn account_digest(account: &Account) -> String {
    let mut h = Sha256::new();
    h.update(b"apb-account-v1\0");
    lp(&mut h, account.name.as_bytes());
    lp(&mut h, if account.default { b"1" } else { b"0" });
    for (key, value) in &account.fields {
        lp(&mut h, key.as_bytes());
        lp(&mut h, value.as_bytes());
    }
    format!("sha256:{}", crate::content::hex_lower(&h.finalize()))
}

/// The env var names an account's secret fields reference, keyed by field
/// name. Only fields declared `secret: true` in `doc` are considered;
/// a field whose value is not a valid `{{env.VAR}}` reference is skipped
/// here (rejecting it is `validate_accounts`'s job, not this lookup's).
pub fn env_refs(doc: &ConnectorDoc, account: &Account) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in doc.secret_fields() {
        if let Some(value) = account.fields.get(&name)
            && let Some(var) = parse_env_ref(value)
        {
            out.insert(name, var);
        }
    }
    out
}

/// The shell command line each secret field sources its value from, keyed by
/// field name. Only fields declared `secret: true` in `doc` whose value is a
/// valid `{{cmd:...}}` reference are included; env-ref and invalid fields are
/// skipped here (rejecting an invalid value is `validate_accounts`'s job).
/// This never executes the command; it only extracts the reference text.
pub fn cmd_refs(doc: &ConnectorDoc, account: &Account) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in doc.secret_fields() {
        if let Some(value) = account.fields.get(&name)
            && let Some(cmd) = parse_cmd_ref(value)
        {
            out.insert(name, cmd);
        }
    }
    out
}

/// The account marked `default: true`, if any (first match in list order).
/// Callers that must enforce "at most one default" rely on
/// `validate_accounts` catching the multi-default case.
pub fn default_account(accounts: &[Account]) -> Option<&Account> {
    accounts.iter().find(|a| a.default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, yaml: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, yaml).unwrap();
    }

    fn jira_doc() -> ConnectorDoc {
        let yaml = r#"
name: jira
version: 0.1.0
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: ping
    description: d
    mock: { status: 200, body: {} }
"#;
        ConnectorDoc::from_yaml(yaml, "jira").unwrap()
    }

    // --- merge ---------------------------------------------------------

    #[test]
    fn project_account_replaces_global_same_name() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("APB_CONFIG_DIR");
                }
            }
        }
        let _g = EnvGuard;

        write(
            &global_config_path("jira").unwrap(),
            r#"
accounts:
  - name: a
    base_url: https://global-a.example
    token: "{{env.A_TOKEN}}"
  - name: b
    base_url: https://global-b.example
    token: "{{env.B_TOKEN}}"
"#,
        );
        write(
            &project_config_path(root.path(), "jira"),
            r#"
accounts:
  - name: b
    base_url: https://project.example
    token: "{{env.B_TOKEN}}"
  - name: c
    base_url: https://global-c.example
    token: "{{env.C_TOKEN}}"
"#,
        );

        let merged = load_merged(root.path(), "jira").unwrap();
        assert_eq!(
            merged.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(merged[1].fields["base_url"], "https://project.example");
    }

    #[test]
    fn project_default_wins_merged_list_has_one_default() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("APB_CONFIG_DIR");
                }
            }
        }
        let _g = EnvGuard;

        // Global marks "a" as the default. The project overrides "a"
        // entirely (still marked default there) - a single account name
        // never yields two competing default flags, so the merged list
        // ends up with exactly one default: the project's version.
        write(
            &global_config_path("jira").unwrap(),
            r#"
accounts:
  - name: a
    default: true
    base_url: https://global-a.example
    token: "{{env.A_TOKEN}}"
"#,
        );
        write(
            &project_config_path(root.path(), "jira"),
            r#"
accounts:
  - name: a
    default: true
    base_url: https://project-a.example
    token: "{{env.A_TOKEN}}"
"#,
        );

        let merged = load_merged(root.path(), "jira").unwrap();
        assert_eq!(merged.iter().filter(|a| a.default).count(), 1);
        assert_eq!(
            merged.iter().find(|a| a.default).unwrap().fields["base_url"],
            "https://project-a.example"
        );

        let doc = jira_doc();
        assert!(validate_accounts(&doc, &merged).is_empty());
    }

    #[test]
    fn cross_scope_project_default_wins_clears_global_default() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("APB_CONFIG_DIR");
                }
            }
        }
        let _g = EnvGuard;

        // Global marks "a" as default. Project adds an unrelated account
        // "b", also marked default, without overriding "a" by name. Per
        // spec 4.1, the project default wins: "a"'s default flag is
        // cleared in the merged list so at most one default survives.
        write(
            &global_config_path("jira").unwrap(),
            r#"
accounts:
  - name: a
    default: true
    base_url: https://global-a.example
    token: "{{env.A_TOKEN}}"
"#,
        );
        write(
            &project_config_path(root.path(), "jira"),
            r#"
accounts:
  - name: b
    default: true
    base_url: https://project-b.example
    token: "{{env.B_TOKEN}}"
"#,
        );

        let merged = load_merged(root.path(), "jira").unwrap();
        assert_eq!(merged.iter().filter(|a| a.default).count(), 1);
        assert!(!merged.iter().find(|a| a.name == "a").unwrap().default);
        assert!(merged.iter().find(|a| a.name == "b").unwrap().default);

        let doc = jira_doc();
        assert!(validate_accounts(&doc, &merged).is_empty());
    }

    #[test]
    fn cross_scope_global_default_survives_when_project_has_no_default() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("APB_CONFIG_DIR");
                }
            }
        }
        let _g = EnvGuard;

        // Global marks "a" as default. Project adds "b" with no default at
        // all. Since the project has no default account, the global
        // default is left untouched.
        write(
            &global_config_path("jira").unwrap(),
            r#"
accounts:
  - name: a
    default: true
    base_url: https://global-a.example
    token: "{{env.A_TOKEN}}"
"#,
        );
        write(
            &project_config_path(root.path(), "jira"),
            r#"
accounts:
  - name: b
    base_url: https://project-b.example
    token: "{{env.B_TOKEN}}"
"#,
        );

        let merged = load_merged(root.path(), "jira").unwrap();
        assert_eq!(merged.iter().filter(|a| a.default).count(), 1);
        assert!(merged.iter().find(|a| a.name == "a").unwrap().default);
        assert!(!merged.iter().find(|a| a.name == "b").unwrap().default);

        let doc = jira_doc();
        assert!(validate_accounts(&doc, &merged).is_empty());
    }

    #[test]
    fn missing_files_merge_to_empty() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("APB_CONFIG_DIR");
                }
            }
        }
        let _g = EnvGuard;

        let merged = load_merged(root.path(), "jira").unwrap();
        assert!(merged.is_empty());
    }

    #[test]
    fn unparsable_existing_file_is_error() {
        let _lock = crate::env_test_lock();
        let cfg = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("APB_CONFIG_DIR", cfg.path());
        }
        struct EnvGuard;
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("APB_CONFIG_DIR");
                }
            }
        }
        let _g = EnvGuard;

        write(
            &project_config_path(root.path(), "jira"),
            "not: [valid: yaml",
        );
        assert!(load_merged(root.path(), "jira").is_err());
    }

    #[test]
    fn global_config_path_none_without_config_dir() {
        let _lock = crate::env_test_lock();

        struct EnvGuard {
            config_dir: Option<std::ffi::OsString>,
            xdg_config_home: Option<std::ffi::OsString>,
            home: Option<std::ffi::OsString>,
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    match &self.config_dir {
                        Some(v) => std::env::set_var("APB_CONFIG_DIR", v),
                        None => std::env::remove_var("APB_CONFIG_DIR"),
                    }
                    match &self.xdg_config_home {
                        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                        None => std::env::remove_var("XDG_CONFIG_HOME"),
                    }
                    match &self.home {
                        Some(v) => std::env::set_var("HOME", v),
                        None => std::env::remove_var("HOME"),
                    }
                }
            }
        }
        let _g = EnvGuard {
            config_dir: std::env::var_os("APB_CONFIG_DIR"),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
        };

        unsafe {
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
        }
        assert!(global_config_path("jira").is_none());
    }

    // --- digest ----------------------------------------------------------

    fn acct(name: &str, default: bool, fields: &[(&str, &str)]) -> Account {
        Account {
            name: name.to_string(),
            default,
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn digest_is_stable_under_field_insertion_order() {
        let a = acct(
            "x",
            false,
            &[("base_url", "https://a"), ("token", "{{env.T}}")],
        );
        let b = acct(
            "x",
            false,
            &[("token", "{{env.T}}"), ("base_url", "https://a")],
        );
        assert_eq!(account_digest(&a), account_digest(&b));
    }

    #[test]
    fn digest_changes_when_default_flips() {
        let a = acct("x", false, &[("base_url", "https://a")]);
        let b = acct("x", true, &[("base_url", "https://a")]);
        assert_ne!(account_digest(&a), account_digest(&b));
    }

    #[test]
    fn digest_changes_when_field_value_changes() {
        let a = acct("x", false, &[("base_url", "https://a")]);
        let b = acct("x", false, &[("base_url", "https://b")]);
        assert_ne!(account_digest(&a), account_digest(&b));
    }

    // --- validate_accounts ------------------------------------------------

    #[test]
    fn validate_accepts_well_formed_accounts() {
        let doc = jira_doc();
        let accounts = vec![acct(
            "prod",
            true,
            &[("base_url", "https://a"), ("token", "{{env.T}}")],
        )];
        assert!(validate_accounts(&doc, &accounts).is_empty());
    }

    #[test]
    fn validate_catches_literal_secret() {
        let doc = jira_doc();
        let accounts = vec![acct(
            "prod",
            false,
            &[("base_url", "https://a"), ("token", "sk-literal-secret")],
        )];
        let errs = validate_accounts(&doc, &accounts);
        assert!(errs.iter().any(|e| e.contains("token")));
    }

    #[test]
    fn validate_catches_missing_required_field() {
        let doc = jira_doc();
        let accounts = vec![acct("prod", false, &[("token", "{{env.T}}")])];
        let errs = validate_accounts(&doc, &accounts);
        assert!(errs.iter().any(|e| e.contains("base_url")));
    }

    #[test]
    fn validate_catches_unknown_field() {
        let doc = jira_doc();
        let accounts = vec![acct(
            "prod",
            false,
            &[
                ("base_url", "https://a"),
                ("token", "{{env.T}}"),
                ("bogus", "x"),
            ],
        )];
        let errs = validate_accounts(&doc, &accounts);
        assert!(errs.iter().any(|e| e.contains("bogus")));
    }

    #[test]
    fn validate_catches_double_default() {
        let doc = jira_doc();
        let accounts = vec![
            acct(
                "a",
                true,
                &[("base_url", "https://a"), ("token", "{{env.T}}")],
            ),
            acct(
                "b",
                true,
                &[("base_url", "https://b"), ("token", "{{env.T}}")],
            ),
        ];
        let errs = validate_accounts(&doc, &accounts);
        assert!(errs.iter().any(|e| e.contains("default")));
    }

    #[test]
    fn validate_catches_duplicate_name() {
        let doc = jira_doc();
        let accounts = vec![
            acct(
                "a",
                false,
                &[("base_url", "https://a"), ("token", "{{env.T}}")],
            ),
            acct(
                "a",
                false,
                &[("base_url", "https://b"), ("token", "{{env.T}}")],
            ),
        ];
        let errs = validate_accounts(&doc, &accounts);
        assert!(errs.iter().any(|e| e.contains("duplicate")));
    }

    #[test]
    fn validate_catches_invalid_account_name_slug() {
        let doc = jira_doc();
        let accounts = vec![acct(
            "Bad_Name",
            false,
            &[("base_url", "https://a"), ("token", "{{env.T}}")],
        )];
        let errs = validate_accounts(&doc, &accounts);
        assert!(!errs.is_empty());
    }

    // --- env_refs ----------------------------------------------------------

    #[test]
    fn env_refs_extracts_var_from_secret_fields_only() {
        let doc = jira_doc();
        let account = acct(
            "prod",
            false,
            &[("base_url", "https://a"), ("token", "{{env.PROD_TOKEN}}")],
        );
        let refs = env_refs(&doc, &account);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs["token"], "PROD_TOKEN");
    }

    #[test]
    fn env_refs_skips_non_reference_value() {
        let doc = jira_doc();
        let account = acct(
            "prod",
            false,
            &[("base_url", "https://a"), ("token", "literal")],
        );
        assert!(env_refs(&doc, &account).is_empty());
    }

    // --- cmd refs ----------------------------------------------------------

    #[test]
    fn validate_accepts_cmd_ref_secret() {
        let doc = jira_doc();
        let accounts = vec![acct(
            "prod",
            true,
            &[
                ("base_url", "https://a"),
                ("token", "{{cmd:gh auth token}}"),
            ],
        )];
        assert!(validate_accounts(&doc, &accounts).is_empty());
    }

    #[test]
    fn validate_still_rejects_literal_secret_message_names_both_forms() {
        let doc = jira_doc();
        let accounts = vec![acct(
            "prod",
            false,
            &[("base_url", "https://a"), ("token", "sk-literal")],
        )];
        let errs = validate_accounts(&doc, &accounts);
        assert!(
            errs.iter()
                .any(|e| e.contains("token") && e.contains("cmd"))
        );
    }

    #[test]
    fn cmd_refs_extracts_command_from_cmd_secret_only() {
        let doc = jira_doc();
        let account = acct(
            "prod",
            false,
            &[
                ("base_url", "https://a"),
                ("token", "{{cmd:gh auth token}}"),
            ],
        );
        let refs = cmd_refs(&doc, &account);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs["token"], "gh auth token");
        // An env-ref field is not a cmd ref, and vice versa.
        assert!(env_refs(&doc, &account).is_empty());
    }

    #[test]
    fn env_and_cmd_refs_are_disjoint_for_the_same_account() {
        let doc = jira_doc();
        let env_account = acct(
            "e",
            false,
            &[("base_url", "https://a"), ("token", "{{env.T}}")],
        );
        assert_eq!(env_refs(&doc, &env_account).len(), 1);
        assert!(cmd_refs(&doc, &env_account).is_empty());
    }

    #[test]
    fn digest_changes_when_secret_ref_switches_env_to_cmd() {
        // Regression pin: the account digest already covers secret field
        // reference strings, so swapping the source drops account trust.
        let a = acct("x", false, &[("token", "{{env.T}}")]);
        let b = acct("x", false, &[("token", "{{cmd:gh auth token}}")]);
        assert_ne!(account_digest(&a), account_digest(&b));
    }

    // --- default_account -------------------------------------------------

    #[test]
    fn default_account_returns_project_default_when_it_wins() {
        let accounts = vec![
            acct("a", false, &[]),
            acct("b", true, &[]),
            acct("c", false, &[]),
        ];
        let d = default_account(&accounts).unwrap();
        assert_eq!(d.name, "b");
    }

    #[test]
    fn default_account_none_when_no_default() {
        let accounts = vec![acct("a", false, &[]), acct("c", false, &[])];
        assert!(default_account(&accounts).is_none());
    }
}
