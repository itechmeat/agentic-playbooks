//! Task 10: connector and grant snapshot structures in the run manifest.
//! The manifest is write-once and read back verbatim (spec 3.6); connectors
//! and grants must roundtrip the same way profiles already do, and an old
//! manifest (written before this change, without the new keys) must still
//! parse with empty connector fields.

use std::collections::BTreeMap;

use apb_engine::manifest::{
    ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest, read, write,
};

fn sample_account() -> ManifestAccount {
    let mut fields = BTreeMap::new();
    fields.insert("workspace".to_string(), "acme".to_string());
    let mut env = BTreeMap::new();
    env.insert("token".to_string(), "SLACK_ACME_TOKEN".to_string());
    ManifestAccount {
        name: "acme".to_string(),
        default: true,
        fields,
        env,
        cmd: BTreeMap::new(),
        digest: "sha256:account".to_string(),
    }
}

fn sample_connector() -> ManifestConnector {
    ManifestConnector {
        name: "slack".to_string(),
        digest: "sha256:connector".to_string(),
        accounts: vec![sample_account()],
    }
}

fn sample_grant() -> ManifestConnectorGrant {
    ManifestConnectorGrant {
        connector: "slack".to_string(),
        accounts: vec!["acme".to_string()],
        functions: vec!["post_message".to_string()],
        max_calls: Some(5),
    }
}

#[test]
fn connector_and_grants_roundtrip_through_manifest() {
    let dir = tempfile::tempdir().unwrap();

    let mut manifest = RunExecutionManifest::default();
    manifest.connectors.push(sample_connector());
    manifest
        .connector_grants
        .insert("t1".to_string(), vec![sample_grant()]);

    write(dir.path(), &manifest).unwrap();
    let read_back = read(dir.path()).unwrap().expect("manifest written");

    assert_eq!(read_back.connectors, manifest.connectors);
    assert_eq!(read_back.connector_grants, manifest.connector_grants);
    assert_eq!(
        read_back.connector("slack"),
        Some(&sample_connector()),
        "connector lookup by name"
    );
    assert_eq!(read_back.connector("missing"), None);
    assert_eq!(read_back.grants_for("t1"), &[sample_grant()]);
    assert_eq!(
        read_back.grants_for("no-such-node"),
        &[] as &[ManifestConnectorGrant],
        "node with no grants returns empty slice"
    );
}

#[test]
fn connector_only_manifest_is_not_empty() {
    let mut manifest = RunExecutionManifest::default();
    assert!(manifest.is_empty(), "no profiles, no connectors: empty");
    manifest.connectors.push(sample_connector());
    assert!(
        !manifest.is_empty(),
        "a connector-only manifest must still write"
    );
}

// Pre-change manifest shape (profiles + node_bindings only, no connector
// keys). Written by hand to simulate a manifest produced before this task -
// it must still parse, with connectors/connector_grants defaulting empty.
const OLD_MANIFEST_YAML: &str = r#"profiles:
  - scope: project
    name: arch
    profile_digest: sha256:aaa
    bundle_digest: sha256:bbb
    soul: You are an architect.
    soul_requirement: any
    skills: []
    chain: []
    ephemeral: false
node_bindings:
  t1: project/arch
"#;

#[test]
fn old_manifest_without_connector_keys_reads_with_empty_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.yaml"), OLD_MANIFEST_YAML).unwrap();

    let manifest = read(dir.path()).unwrap().expect("manifest exists");
    assert_eq!(manifest.profiles.len(), 1);
    assert_eq!(manifest.node_bindings.get("t1").unwrap(), "project/arch");
    assert!(manifest.connectors.is_empty());
    assert!(manifest.connector_grants.is_empty());
    assert_eq!(manifest.grants_for("t1"), &[] as &[ManifestConnectorGrant]);
}
