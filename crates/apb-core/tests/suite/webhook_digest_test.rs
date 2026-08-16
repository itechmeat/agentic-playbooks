//! The whole-folder tree digest covers the webhook block, so editing the
//! signature header, the prefix, or the secret reference drops the
//! connector's recorded trust. This is the property that stops a shared
//! config from silently weakening verification.

use apb_core::content::{TreeLimits, tree_digest};

const BASE: &str = r#"name: echo-hooks
version: 0.1.0
webhook:
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
account_fields:
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending inbound events
    read_only: true
    response_pick: [events, cursor]
    inbox:
      op: read
"#;

fn digest_of(yaml: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("echo-hooks");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("connector.yaml"), yaml).unwrap();
    // Sanity: the manifest under test must actually parse, or the digest
    // would be comparing two files apb would never load.
    apb_core::connector::def::ConnectorDoc::from_yaml(yaml, "echo-hooks").unwrap();
    tree_digest(&folder, &TreeLimits::default()).unwrap()
}

#[test]
fn editing_the_webhook_block_changes_the_connector_digest() {
    let base = digest_of(BASE);
    assert_eq!(
        base,
        digest_of(BASE),
        "the digest is stable for identical content"
    );

    let moved_header = BASE.replace("X-Hub-Signature-256", "X-Attacker-Signature");
    assert_ne!(base, digest_of(&moved_header), "the header name is covered");

    let dropped_prefix = BASE.replace("    prefix: \"sha256=\"\n", "");
    assert_ne!(base, digest_of(&dropped_prefix), "the prefix is covered");

    let swapped_secret = BASE.replace("app_secret", "other_secret");
    assert_ne!(
        base,
        digest_of(&swapped_secret),
        "the secret reference is covered"
    );
}
