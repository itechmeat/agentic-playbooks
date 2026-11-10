//! The `content_types` response guard end to end (D5): a 2xx whose media
//! type matches no entry fails `service` before the body is read, on the
//! live path and through the offline contract runner alike.

use std::collections::BTreeMap;

use apb_engine::connector::call::{CallRequest, execute};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{self, spawn_script};

const NODE: &str = "n";
const CONNECTOR: &str = "fdr";
const TOKEN_VAR: &str = "APB_FDR_TEST_TOKEN";
const TOKEN_VALUE: &str = "fdr-token-0123456789";

fn connector_yaml() -> String {
    r#"
name: fdr
version: 0.1.0
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - { name: api_base, required: true }
  - { name: token, required: true, secret: true }
functions:
  - name: read_text_file
    description: d
    read_only: true
    untrusted_output: true
    method: GET
    url: "{{account.api_base}}/files/{{args.id}}"
    args_schema: { type: object, properties: { id: { type: string } }, required: [id] }
    query:
      alt: media
      supportsAllDrives: "true"
    content_types: ["text/", "application/json", "application/xml", "application/yaml", "application/x-yaml"]
"#
    .trim_start()
    .to_string()
}

struct Rig {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
}

fn rig(base: &str) -> Rig {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![ManifestAccount {
            name: "acct1".to_string(),
            default: true,
            fields: BTreeMap::from([
                ("api_base".to_string(), base.to_string()),
                ("token".to_string(), format!("{{{{env.{TOKEN_VAR}}}}}")),
            ]),
            env: BTreeMap::from([("token".to_string(), TOKEN_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: "sha256:acct".to_string(),
        }],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["acct1".to_string()],
            functions: vec!["read_text_file".to_string()],
            max_calls: None,
            on_unknown: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), connector_yaml()).unwrap();
    let path = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{TOKEN_VAR}={TOKEN_VALUE}\n")).unwrap();
    Rig { run, root }
}

impl Rig {
    fn call(&self, args: Value) -> (Value, bool) {
        execute(CallRequest {
            run_dir: self.run.path(),
            root: self.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function: "read_text_file",
            account: None,
            args,
            dry_run: false,
            full: false,
        })
    }
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

fn message(value: &Value) -> &str {
    value["error"]["message"].as_str().unwrap_or("")
}

/// A 2 MiB body: the guard must fail BEFORE the body is read, so the
/// error is `service`, never the cap's `incomplete_response`.
fn big_body() -> String {
    "x".repeat(2 * 1024 * 1024)
}

#[test]
fn a_binary_answer_fails_service_before_the_body_is_read() {
    let server = spawn_script(vec![common::Reply::RespondWith(
        200,
        vec![("Content-Type".to_string(), "image/png".to_string())],
        big_body(),
    )]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call(json!({ "id": "f1" }));
    assert!(!ok);
    assert_eq!(code(&value), "service", "{value}");
    assert!(message(&value).contains("image/png"), "{}", message(&value));
    assert!(
        !message(&value).contains("incomplete"),
        "{}",
        message(&value)
    );
}

#[test]
fn a_mismatched_type_fails_with_the_allowlist() {
    let server = spawn_script(vec![common::Reply::RespondWith(
        200,
        vec![(
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        )],
        "raw".to_string(),
    )]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call(json!({ "id": "f2" }));
    assert!(!ok);
    assert_eq!(code(&value), "service");
    assert!(
        message(&value).contains("application/octet-stream"),
        "{}",
        message(&value)
    );
}

#[test]
fn json_and_text_answers_pass() {
    let server = spawn_script(vec![
        common::Reply::Respond(200, json!({ "note": "hi" }).to_string()),
        common::Reply::RespondWith(
            200,
            vec![("Content-Type".to_string(), "text/markdown".to_string())],
            "# doc".to_string(),
        ),
    ]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call(json!({ "id": "f1" }));
    assert!(ok, "{value}");
    assert_eq!(value["body"]["note"], json!("hi"));
    let (value, ok) = rig.call(json!({ "id": "f1" }));
    assert!(ok, "{value}");
    assert_eq!(value["body"], json!("# doc"));
}
