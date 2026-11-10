//! Engine tests for the official `google-drive` connector (design D5).
//! They drive the real, checked-in `connectors/google-drive/connector.yaml`
//! through `connector::call::execute` against one scripted server that
//! plays both Google's token endpoint (`{base}/token`) and the Drive API
//! (`api_base = {base}`), fully offline:
//!
//! - `export_text` mints a bearer token from the account's refresh token,
//!   sends it to the export URL, and returns the exported text whole as
//!   the result body;
//! - `read_text_file` refuses an `application/octet-stream` answer with a
//!   `service` error naming the type, before the body is read (the
//!   `content_types` guard), even when the body is over the read cap;
//! - `search_files` follows `nextPageToken` to the next page, carrying it
//!   as `pageToken`, and merges both pages' files under the pick;
//! - a token that grants only metadata access lists metadata but cannot
//!   read content (the manifest's two scope lists).

use std::collections::BTreeMap;
use std::path::Path;

use apb_engine::connector::call::{CallRequest, execute};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{Reply, spawn_script};

const NODE: &str = "n";
const CONNECTOR: &str = "google-drive";
const CLIENT_SECRET: &str = "gdrive-client-secret-0123456789";
const REFRESH: &str = "gdrive-refresh-token-0123456789";
const ACCESS: &str = "gdrive-access-token-0123456789";
const SECRET_VAR_CS: &str = "APB_GDRIVE_TEST_CLIENT_SECRET";
const SECRET_VAR_RT: &str = "APB_GDRIVE_TEST_REFRESH_TOKEN";
const DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
const DRIVE_METADATA_READONLY: &str = "https://www.googleapis.com/auth/drive.metadata.readonly";
const FUNCTIONS: [&str; 5] = [
    "verify",
    "search_files",
    "get_file_metadata",
    "export_text",
    "read_text_file",
];
/// The fixed `fields` selection `search_files` sends, as rendered.
const SEARCH_FIELDS: &str =
    "fields=nextPageToken,files(id,name,mimeType,modifiedTime,size,webViewLink)";

/// The real, checked-in connector manifest. Loading it (rather than an
/// inline copy) means a drift between this test and the shipped
/// `connector.yaml` is impossible.
fn drive_yaml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/google-drive/connector.yaml")
        .canonicalize()
        .expect("repository connectors/google-drive/connector.yaml must exist");
    std::fs::read_to_string(path).unwrap()
}

/// A seeded run.
struct Rig {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
}

/// The run manifest granting every function to `NODE` on one account whose
/// token endpoint and API base both live on the scripted server at `base`,
/// the snapshot of the checked-in manifest, and the project secrets file.
///
/// The in-process token cache is keyed by the token URL, the client id and
/// the refresh token, so every test passes its own `client_id`: a port the
/// OS hands out again can then never serve a token another test minted.
fn rig(base: &str, client_id: &str) -> Rig {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let account = ManifestAccount {
        name: "acct1".to_string(),
        default: true,
        fields: BTreeMap::from([
            ("api_base".to_string(), base.to_string()),
            ("token_url".to_string(), format!("{base}/token")),
            ("client_id".to_string(), client_id.to_string()),
            (
                "client_secret".to_string(),
                format!("{{{{env.{SECRET_VAR_CS}}}}}"),
            ),
            (
                "refresh_token".to_string(),
                format!("{{{{env.{SECRET_VAR_RT}}}}}"),
            ),
        ]),
        env: BTreeMap::from([
            ("client_secret".to_string(), SECRET_VAR_CS.to_string()),
            ("refresh_token".to_string(), SECRET_VAR_RT.to_string()),
        ]),
        cmd: BTreeMap::new(),
        digest: "sha256:acct1".to_string(),
    };
    let mut m = RunExecutionManifest::default();
    m.connectors.push(ManifestConnector {
        name: CONNECTOR.to_string(),
        digest: "sha256:test".to_string(),
        accounts: vec![account],
    });
    m.connector_grants.insert(
        NODE.to_string(),
        vec![ManifestConnectorGrant {
            connector: CONNECTOR.to_string(),
            accounts: vec!["acct1".to_string()],
            functions: FUNCTIONS.iter().map(|f| f.to_string()).collect(),
            max_calls: None,
            on_unknown: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), drive_yaml()).unwrap();
    let secrets = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(secrets.parent().unwrap()).unwrap();
    std::fs::write(
        &secrets,
        format!("{SECRET_VAR_CS}={CLIENT_SECRET}\n{SECRET_VAR_RT}={REFRESH}\n"),
    )
    .unwrap();
    Rig { run, root }
}

impl Rig {
    /// A plain node call.
    fn call(&self, function: &str, args: Value) -> (Value, bool) {
        execute(CallRequest {
            run_dir: self.run.path(),
            root: self.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: None,
            args,
            dry_run: false,
            full: false,
        })
    }

    /// Every `connector_call` event the run journaled.
    fn connector_calls(&self) -> Vec<EventPayload> {
        read_all(self.run.path())
            .map(|events| {
                events
                    .into_iter()
                    .map(|e| e.payload)
                    .filter(|p| matches!(p, EventPayload::ConnectorCall { .. }))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The raw journal text, for the token-leak greps.
    fn journal_text(&self) -> String {
        std::fs::read_to_string(self.run.path().join("events.jsonl")).unwrap_or_default()
    }
}

/// The token endpoint's answer: the access token, granting `scope`.
fn token_ok(scope: &str) -> Reply {
    Reply::Respond(
        200,
        json!({
            "access_token": ACCESS,
            "expires_in": 3599,
            "scope": scope,
            "token_type": "Bearer",
        })
        .to_string(),
    )
}

/// The captured request line (`GET /files HTTP/1.1`).
fn request_line(raw: &str) -> &str {
    raw.lines().next().unwrap_or("")
}

/// The request target of a captured request (path and query).
fn request_path(raw: &str) -> &str {
    request_line(raw).split(' ').nth(1).unwrap_or("")
}

fn request_method(raw: &str) -> &str {
    request_line(raw).split(' ').next().unwrap_or("")
}

/// A request header value, case-insensitive.
fn request_header(raw: &str, name: &str) -> Option<String> {
    raw.lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
}

/// One value of a captured form body (the token request); the test values
/// hold no reserved characters, so no decoding is needed.
fn form_value(raw: &str, key: &str) -> Option<String> {
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

fn message(value: &Value) -> &str {
    value["error"]["message"].as_str().unwrap_or("")
}

#[test]
fn export_text_returns_the_scripted_text() {
    let text = "# Quarterly plan\n\nShip the beta in March.\n";
    let server = spawn_script(vec![
        token_ok(DRIVE_READONLY),
        Reply::RespondWith(
            200,
            vec![(
                "Content-Type".to_string(),
                "text/markdown; charset=utf-8".to_string(),
            )],
            text.to_string(),
        ),
    ]);
    let rig = rig(&server.base_url, "gdrive-client-export");
    let (value, ok) = rig.call(
        "export_text",
        json!({ "file_id": "doc-1", "mime_type": "text/markdown" }),
    );
    assert!(ok, "{value}");
    assert_eq!(value["body"], json!(text), "{value}");
    // A whole document: no projection applies to it.
    assert!(value.get("picked").is_none(), "{value}");

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    // The checked-in auth block mints from the account's refresh token.
    assert_eq!(request_method(&requests[0]), "POST");
    assert_eq!(request_path(&requests[0]), "/token");
    assert_eq!(
        form_value(&requests[0], "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(
        form_value(&requests[0], "refresh_token").as_deref(),
        Some(REFRESH)
    );
    assert_eq!(
        form_value(&requests[0], "client_id").as_deref(),
        Some("gdrive-client-export")
    );
    assert_eq!(
        form_value(&requests[0], "client_secret").as_deref(),
        Some(CLIENT_SECRET)
    );
    // The export request carries the minted token and the format.
    assert_eq!(request_method(&requests[1]), "GET");
    assert_eq!(
        request_path(&requests[1]),
        "/files/doc-1/export?mimeType=text%2Fmarkdown"
    );
    assert_eq!(
        request_header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS}").as_str())
    );

    // One journaled call, and no credential in the journal.
    assert_eq!(rig.connector_calls().len(), 1);
    let journal = rig.journal_text();
    for secret in [ACCESS, REFRESH, CLIENT_SECRET] {
        assert!(!journal.contains(secret), "the journal holds a credential");
    }
}

#[test]
fn read_text_file_refuses_an_octet_stream_answer() {
    // Over the 1 MiB read cap: a guard that read the body first would fail
    // `incomplete_response` instead of naming the type.
    let body = "\u{1}blob-bytes\u{2}".repeat(192 * 1024);
    let server = spawn_script(vec![
        token_ok(DRIVE_READONLY),
        Reply::RespondWith(
            200,
            vec![(
                "Content-Type".to_string(),
                "application/octet-stream".to_string(),
            )],
            body,
        ),
    ]);
    let rig = rig(&server.base_url, "gdrive-client-octet");
    let (value, ok) = rig.call("read_text_file", json!({ "file_id": "bin-1" }));
    assert!(!ok, "{value}");
    assert_eq!(code(&value), "service", "{value}");
    let msg = message(&value);
    assert!(msg.contains("application/octet-stream"), "{msg}");
    assert!(msg.contains("read_text_file"), "{msg}");
    assert!(!msg.contains("incomplete"), "{msg}");
    // Nothing of the answer reaches the result.
    assert!(value.get("body").is_none(), "{value}");
    assert!(!value.to_string().contains("blob-bytes"), "{value}");

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        request_path(&requests[1]),
        "/files/bin-1?alt=media&supportsAllDrives=true"
    );
    assert_eq!(
        request_header(&requests[1], "Authorization").as_deref(),
        Some(format!("Bearer {ACCESS}").as_str())
    );
}

#[test]
fn search_files_follows_the_token() {
    let page_one = json!({
        "nextPageToken": "tok-2",
        "files": [
            { "id": "f1", "name": "plan.txt", "mimeType": "text/plain", "size": "12",
              "thumbnailLink": "https://example.com/t1" },
            { "id": "f2", "name": "Plan", "mimeType": "application/vnd.google-apps.document" }
        ]
    });
    let page_two = json!({
        "files": [
            { "id": "f3", "name": "plan.csv", "mimeType": "text/csv", "size": "40" }
        ]
    });
    let server = spawn_script(vec![
        token_ok(DRIVE_READONLY),
        Reply::Respond(200, page_one.to_string()),
        Reply::Respond(200, page_two.to_string()),
    ]);
    let rig = rig(&server.base_url, "gdrive-client-search");
    let (value, ok) = rig.call(
        "search_files",
        json!({ "q": "name contains 'plan' and trashed = false", "page_size": 2 }),
    );
    assert!(ok, "{value}");
    assert_eq!(value["pages"], 2, "{value}");
    assert_eq!(value["more"], false, "{value}");
    assert_eq!(value["picked"], true, "{value}");
    let files = value["body"]["files"]
        .as_array()
        .expect("a merged file list");
    let ids: Vec<&str> = files.iter().filter_map(|f| f["id"].as_str()).collect();
    assert_eq!(ids, ["f1", "f2", "f3"], "{value}");
    // The pick keeps the listed fields only, and the last page carried no
    // token, so the merged answer has none.
    assert!(files[0].get("thumbnailLink").is_none(), "{value}");
    assert_eq!(files[0]["size"], "12");
    assert!(value["body"].get("nextPageToken").is_none(), "{value}");

    // One token request, then the two pages with the same bearer token;
    // the second page re-renders the first request with `pageToken`.
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert_eq!(request_path(&requests[0]), "/token");
    let query =
        "q=name%20contains%20%27plan%27%20and%20trashed%20%3D%20false&supportsAllDrives=true";
    assert_eq!(
        request_path(&requests[1]),
        format!("/files?{SEARCH_FIELDS}&includeItemsFromAllDrives=true&pageSize=2&{query}")
    );
    assert_eq!(
        request_path(&requests[2]),
        format!(
            "/files?{SEARCH_FIELDS}&includeItemsFromAllDrives=true&pageSize=2&pageToken=tok-2&{query}"
        )
    );
    for page in &requests[1..] {
        assert_eq!(
            request_header(page, "Authorization").as_deref(),
            Some(format!("Bearer {ACCESS}").as_str())
        );
    }

    // One call in the journal, two pages.
    let calls = rig.connector_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    match &calls[0] {
        EventPayload::ConnectorCall {
            pages,
            outcome,
            http_status,
            ..
        } => {
            assert_eq!(*pages, Some(2));
            assert_eq!(outcome, "ok");
            assert_eq!(*http_status, Some(200));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn a_metadata_only_token_lists_but_cannot_read_content() {
    let server = spawn_script(vec![
        token_ok(DRIVE_METADATA_READONLY),
        Reply::Respond(
            200,
            json!({
                "id": "doc-1",
                "name": "Plan",
                "mimeType": "application/vnd.google-apps.document",
                "owners": [ { "emailAddress": "ana@example.com", "displayName": "Ana" } ]
            })
            .to_string(),
        ),
    ]);
    let rig = rig(&server.base_url, "gdrive-client-metadata");

    // Content needs `drive`, `drive.readonly` or `drive.file`: the scope
    // check refuses before any Drive request, and journals no call.
    let (value, ok) = rig.call(
        "export_text",
        json!({ "file_id": "doc-1", "mime_type": "text/plain" }),
    );
    assert!(!ok, "{value}");
    assert_eq!(code(&value), "auth", "{value}");
    let msg = message(&value);
    assert!(msg.contains(DRIVE_READONLY), "{msg}");
    assert!(msg.contains("apb connector oauth google-drive"), "{msg}");
    assert_eq!(server.requests().len(), 1, "only the token request");
    assert!(rig.connector_calls().is_empty());

    // The same token, cached in this process, reads metadata.
    let (value, ok) = rig.call("get_file_metadata", json!({ "file_id": "doc-1" }));
    assert!(ok, "{value}");
    assert_eq!(value["body"]["name"], "Plan", "{value}");
    assert_eq!(
        value["body"]["owners"],
        json!([{ "emailAddress": "ana@example.com" }]),
        "{value}"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        request_path(&requests[1]),
        "/files/doc-1?fields=id,name,mimeType,modifiedTime,size,webViewLink,description,owners(emailAddress)&supportsAllDrives=true"
    );
}
