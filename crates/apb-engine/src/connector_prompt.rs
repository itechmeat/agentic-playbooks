//! Connector instruction block (spec 2026-07-18-connectors-design section 6
//! step 3): the text appended to an agent-task node prompt when the node holds
//! connector grants. It tells the agent exactly which connectors, accounts,
//! and functions it may call and how to call them.
//!
//! Secret isolation (spec 4.3): the block is built only from the run snapshot -
//! the manifest's non-secret account fields and the snapshotted `ConnectorDoc`
//! function metadata. Secret account fields (the ones the manifest records as
//! `env` var names, never values) are excluded, so neither a resolved secret
//! nor an `{{env.*}}` reference can ever reach the prompt.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::connector::ConnectorDoc;

use crate::manifest::{ManifestConnector, ManifestConnectorGrant};

/// Loads the snapshotted `ConnectorDoc` for each manifest connector from
/// `run_dir/connectors/<name>.yaml` (spec 6 step 2 copies them there at run
/// start). Best-effort: a missing or unparsable snapshot is skipped rather
/// than failing prompt assembly - the block then simply omits that
/// connector's function metadata.
pub fn load_snapshot_docs(
    run_dir: &Path,
    connectors: &[ManifestConnector],
) -> BTreeMap<String, ConnectorDoc> {
    let mut docs = BTreeMap::new();
    for c in connectors {
        let path = run_dir.join("connectors").join(format!("{}.yaml", c.name));
        let Ok(yaml) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = ConnectorDoc::from_yaml(&yaml, &c.name) else {
            continue;
        };
        docs.insert(c.name.clone(), doc);
    }
    docs
}

/// Renders the instruction block for a node's `grants`. `connectors` is the
/// manifest's connector list (source of the non-secret account fields);
/// `docs` maps connector name to its snapshotted `ConnectorDoc` (source of
/// function descriptions and args schemas, which the manifest does not carry).
///
/// Returns an empty string when `grants` is empty, so callers can append the
/// result unconditionally.
pub fn instruction_block(
    grants: &[ManifestConnectorGrant],
    connectors: &[ManifestConnector],
    docs: &BTreeMap<String, ConnectorDoc>,
) -> String {
    if grants.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("## Connectors\n\n");
    out.push_str("You may call the external connectors below. Call a function with:\n\n");
    out.push_str(
        "    apb connector call <connector> <function> [--account <name>] --args '<json>'\n\n",
    );
    out.push_str(
        "`--args -` reads the JSON arguments from stdin (use it for large payloads). \
         `--dry-run` previews the request (method, URL, body) without executing it.\n",
    );

    for grant in grants {
        let connector = connectors.iter().find(|c| c.name == grant.connector);
        out.push_str(&format!("\n### {}\n", grant.connector));

        out.push_str("\nAccounts:\n");
        for acct_name in &grant.accounts {
            match connector.and_then(|c| c.accounts.iter().find(|a| &a.name == acct_name)) {
                Some(a) => {
                    let default_mark = if a.default { " (default)" } else { "" };
                    // Non-secret fields only: any field whose key is an
                    // env-backed (secret) field is excluded, so no `{{env.*}}`
                    // reference and no resolved secret ever reaches the prompt.
                    let fields: Vec<String> = a
                        .fields
                        .iter()
                        .filter(|(k, _)| !a.env.contains_key(k.as_str()))
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect();
                    if fields.is_empty() {
                        out.push_str(&format!("- {}{default_mark}\n", a.name));
                    } else {
                        out.push_str(&format!(
                            "- {}{default_mark}: {}\n",
                            a.name,
                            fields.join(", ")
                        ));
                    }
                }
                None => out.push_str(&format!("- {acct_name}\n")),
            }
        }

        out.push_str("\nFunctions:\n");
        let doc = docs.get(&grant.connector);
        for fn_name in &grant.functions {
            match doc.and_then(|d| d.function(fn_name)) {
                Some(f) => {
                    let dep = match &f.deprecated {
                        Some(reason) => format!(" (deprecated: {reason})"),
                        None => String::new(),
                    };
                    out.push_str(&format!("- {}{dep}: {}\n", f.name, f.description));
                    if let Some(schema) = &f.args_schema {
                        out.push_str(&format!("  args: {}\n", compact_json(schema)));
                    }
                }
                None => out.push_str(&format!("- {fn_name}\n")),
            }
        }
    }

    out
}

/// Compact one-line JSON for an args schema. A serialization failure (never
/// expected for a `serde_json::Value`) degrades to an empty object rather than
/// panicking in a prompt-assembly path.
fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestAccount;

    fn sample_connector() -> ManifestConnector {
        ManifestConnector {
            name: "mock-tracker".to_string(),
            digest: "sha256:c".to_string(),
            accounts: vec![ManifestAccount {
                name: "acct1".to_string(),
                default: true,
                fields: BTreeMap::from([
                    ("base_url".to_string(), "https://example.com".to_string()),
                    ("token".to_string(), "{{env.MOCK_TOKEN}}".to_string()),
                ]),
                env: BTreeMap::from([("token".to_string(), "MOCK_TOKEN".to_string())]),
                digest: "sha256:a".to_string(),
            }],
        }
    }

    fn sample_docs() -> BTreeMap<String, ConnectorDoc> {
        let doc = ConnectorDoc::from_yaml(
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
    args_schema: { type: object, properties: { q: { type: string } } }
  - name: create_item
    description: Create an item
    deprecated: use create_item_v2
    method: POST
    url: "{{account.base_url}}/items"
    body: "{{args}}"
  - name: delete_item
    description: Delete an item
    method: POST
    url: "{{account.base_url}}/delete"
    body: "{{args}}"
"#,
            "mock-tracker",
        )
        .unwrap();
        BTreeMap::from([("mock-tracker".to_string(), doc)])
    }

    fn grant(functions: &[&str]) -> ManifestConnectorGrant {
        ManifestConnectorGrant {
            connector: "mock-tracker".to_string(),
            accounts: vec!["acct1".to_string()],
            functions: functions.iter().map(|s| s.to_string()).collect(),
            max_calls: None,
        }
    }

    #[test]
    fn empty_grants_render_nothing() {
        let out = instruction_block(&[], &[sample_connector()], &sample_docs());
        assert_eq!(out, "");
    }

    #[test]
    fn lists_only_granted_functions() {
        let grants = vec![grant(&["list_items", "create_item"])];
        let out = instruction_block(&grants, &[sample_connector()], &sample_docs());
        assert!(
            out.contains("list_items"),
            "granted function missing: {out}"
        );
        assert!(
            out.contains("create_item"),
            "granted function missing: {out}"
        );
        assert!(
            !out.contains("delete_item"),
            "ungranted function must not appear: {out}"
        );
    }

    #[test]
    fn includes_account_name_and_non_secret_fields() {
        let grants = vec![grant(&["list_items"])];
        let out = instruction_block(&grants, &[sample_connector()], &sample_docs());
        assert!(out.contains("acct1"), "account name missing: {out}");
        assert!(out.contains("(default)"), "default marker missing: {out}");
        assert!(
            out.contains("base_url=https://example.com"),
            "non-secret field missing: {out}"
        );
    }

    #[test]
    fn never_leaks_env_ref_or_secret_var_name() {
        let grants = vec![grant(&["list_items", "create_item"])];
        let out = instruction_block(&grants, &[sample_connector()], &sample_docs());
        assert!(
            !out.contains("{{env"),
            "an {{{{env}}}} reference leaked into the prompt: {out}"
        );
        assert!(
            !out.contains("MOCK_TOKEN"),
            "a secret env var name leaked into the prompt: {out}"
        );
    }

    #[test]
    fn marks_deprecated_functions_with_reason() {
        let grants = vec![grant(&["create_item"])];
        let out = instruction_block(&grants, &[sample_connector()], &sample_docs());
        assert!(
            out.contains("(deprecated: use create_item_v2)"),
            "deprecated marker/reason missing: {out}"
        );
    }

    #[test]
    fn includes_compact_args_schema_and_call_syntax() {
        let grants = vec![grant(&["list_items"])];
        let out = instruction_block(&grants, &[sample_connector()], &sample_docs());
        assert!(
            out.contains("apb connector call <connector> <function>"),
            "call syntax line missing: {out}"
        );
        assert!(out.contains("--dry-run"), "dry-run note missing: {out}");
        assert!(
            out.contains(r#"args: {"#) && out.contains(r#""type":"object""#),
            "compact args schema missing: {out}"
        );
    }
}
