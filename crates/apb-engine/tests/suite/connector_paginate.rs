//! End-to-end paging tests (D2): the declared `paginate` block over a
//! scripted server, the Link walk, the token cache across pages, the
//! journaled page count, and the one-deadline budget over every page.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use apb_engine::connector::call::{CallRequest, execute};
use apb_engine::event::{EventPayload, read_all};
use apb_engine::manifest::{
    self, ManifestAccount, ManifestConnector, ManifestConnectorGrant, RunExecutionManifest,
};
use serde_json::{Value, json};

use crate::common::{Reply, spawn_script};

const NODE: &str = "n";
const CONNECTOR: &str = "pager";
const TOKEN_VAR: &str = "APB_PAGER_TEST_TOKEN";
const TOKEN_VALUE: &str = "pager-token-0123456789";

/// A minimal connector with one paginated read and one paginated read
/// under a 2 s timeout, over header auth.
fn pager_yaml() -> String {
    r#"
name: pager
version: 0.1.0
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - { name: api_base, required: true }
  - { name: token, required: true, secret: true }
functions:
  - name: list_things
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/things"
    response_pick: [items]
    paginate:
      items: items
      next: { link_header: true }
      max_pages: 3
  - name: list_big
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/big"
    response_pick: [items]
    paginate:
      items: items
      next: { link_header: true }
      max_pages: 8
  - name: list_slow
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/slow"
    response_pick: [items]
    timeout_sec: 2
    paginate:
      items: items
      next: { link_header: true }
      max_pages: 3
"#
    .to_string()
}

/// The same listing over an `oauth2_refresh` connector, for the token
/// cache test.
fn oauth_yaml() -> String {
    r#"
name: pager
version: 0.1.0
auth:
  kind: oauth2_refresh
  token_url: "{{account.token_url}}"
  client_id_template: "{{account.client_id}}"
  refresh_token_template: "{{secret.refresh_token}}"
account_fields:
  - { name: api_base, required: true }
  - { name: token_url, required: true }
  - { name: client_id, required: true }
  - { name: client_secret, required: true, secret: true }
  - { name: refresh_token, required: true, secret: true }
functions:
  - name: list_things
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/things"
    response_pick: [items]
    paginate:
      items: items
      next: { link_header: true }
      max_pages: 3
"#
    .to_string()
}

struct Rig {
    run: tempfile::TempDir,
    root: tempfile::TempDir,
}

fn rig_with(yaml: &str, base: &str, max_calls: Option<u32>) -> Rig {
    let run = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut m = RunExecutionManifest::default();
    let account = if yaml.contains("oauth2_refresh") {
        ManifestAccount {
            name: "acct1".to_string(),
            default: true,
            fields: BTreeMap::from([
                ("api_base".to_string(), base.to_string()),
                ("token_url".to_string(), format!("{base}/token")),
                ("client_id".to_string(), "client-id-1".to_string()),
                (
                    "client_secret".to_string(),
                    "{{env.APB_PAGER_CS}}".to_string(),
                ),
                (
                    "refresh_token".to_string(),
                    "{{env.APB_PAGER_RT}}".to_string(),
                ),
            ]),
            env: BTreeMap::from([
                ("client_secret".to_string(), "APB_PAGER_CS".to_string()),
                ("refresh_token".to_string(), "APB_PAGER_RT".to_string()),
            ]),
            cmd: BTreeMap::new(),
            digest: "sha256:acct".to_string(),
        }
    } else {
        ManifestAccount {
            name: "acct1".to_string(),
            default: true,
            fields: BTreeMap::from([
                ("api_base".to_string(), base.to_string()),
                ("token".to_string(), format!("{{{{env.{TOKEN_VAR}}}}}")),
            ]),
            env: BTreeMap::from([("token".to_string(), TOKEN_VAR.to_string())]),
            cmd: BTreeMap::new(),
            digest: "sha256:acct".to_string(),
        }
    };
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
            functions: vec![
                "list_things".to_string(),
                "list_slow".to_string(),
                "list_big".to_string(),
            ],
            max_calls,
            on_unknown: None,
        }],
    );
    manifest::write(run.path(), &m).unwrap();
    let cdir = run.path().join("connectors");
    std::fs::create_dir_all(&cdir).unwrap();
    std::fs::write(cdir.join(format!("{CONNECTOR}.yaml")), yaml).unwrap();
    let secrets = if yaml.contains("oauth2_refresh") {
        "APB_PAGER_CS=gocsp-secret-value\nAPB_PAGER_RT=pager-refresh-value\n".to_string()
    } else {
        format!("{TOKEN_VAR}={TOKEN_VALUE}\n")
    };
    let path = root.path().join(".apb/secrets.env");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, secrets).unwrap();
    Rig { run, root }
}

fn rig(base: &str) -> Rig {
    rig_with(&pager_yaml(), base, None)
}

impl Rig {
    fn call(&self, function: &str) -> (Value, bool) {
        execute(CallRequest {
            run_dir: self.run.path(),
            root: self.root.path(),
            node_id: NODE,
            connector: CONNECTOR,
            function,
            account: None,
            args: json!({}),
            dry_run: false,
            full: false,
        })
    }
}

/// One page's JSON body with `n` as its single item, and a `Link` header
/// naming the next page on this server when `next` is set.
fn page(n: u32, next: Option<u32>) -> Reply {
    page_on("things", n, next)
}

/// The same, on another path (the byte-cap test pages live on `/big`).
fn page_on(path: &'static str, n: u32, next: Option<u32>) -> Reply {
    Reply::FromHeaders(Box::new(move |seen: &[String]| {
        let last = seen.last().cloned().unwrap_or_default();
        let host = last
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim()
                    .eq_ignore_ascii_case("host")
                    .then(|| v.trim().to_string())
            })
            .unwrap_or_default();
        let mut headers = Vec::new();
        if let Some(next) = next {
            headers.push((
                "Link".to_string(),
                format!("<http://{host}/{path}?page={next}>; rel=\"next\""),
            ));
        }
        (200, headers, json!({ "items": [n] }).to_string())
    }))
}

fn code(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or("")
}

fn connector_events(rig: &Rig) -> Vec<EventPayload> {
    read_all(rig.run.path())
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .filter(|p| matches!(p, EventPayload::ConnectorCall { .. }))
        .collect()
}

#[test]
fn link_pages_are_followed_and_merged() {
    let server = spawn_script(vec![page(1, Some(2)), page(2, Some(3)), page(3, None)]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call("list_things");
    assert!(ok, "{value}");
    assert_eq!(value["body"]["items"], json!([1, 2, 3]));
    assert_eq!(value["pages"], 3);
    assert_eq!(value["more"], false);
    let requests = server.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(
        requests[0].starts_with("GET /things HTTP/1.1"),
        "{}",
        requests[0]
    );
    assert!(requests[1].starts_with("GET /things?page=2 HTTP/1.1"));
    assert!(requests[2].starts_with("GET /things?page=3 HTTP/1.1"));
}

#[test]
fn a_link_to_another_host_is_not_followed() {
    let server = spawn_script(vec![Reply::RespondWith(
        200,
        vec![(
            "Link".to_string(),
            "<http://other.example/things?page=2>; rel=\"next\"".to_string(),
        )],
        json!({ "items": [1] }).to_string(),
    )]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call("list_things");
    assert!(!ok);
    assert_eq!(code(&value), "service");
    assert!(
        message(&value).contains("another host"),
        "{}",
        message(&value)
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_failed_second_page_fails_the_call() {
    let server = spawn_script(vec![
        page(1, Some(2)),
        Reply::Respond(500, "{}".to_string()),
    ]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call("list_things");
    assert!(!ok);
    assert_eq!(code(&value), "service");
    assert!(
        message(&value).starts_with("page 2 of the listing:"),
        "{}",
        message(&value)
    );
}

#[test]
fn max_calls_counts_a_paged_read_once() {
    let server = spawn_script(vec![page(1, Some(2)), page(2, Some(3)), page(3, None)]);
    let rig = rig_with(&pager_yaml(), &server.base_url, Some(1));
    let (value, ok) = rig.call("list_things");
    assert!(ok, "{value}");
    // The second read is refused: the three pages cost one call.
    let (value, ok) = rig.call("list_things");
    assert!(!ok);
    assert_eq!(code(&value), "permission");
    assert!(message(&value).contains("max_calls"), "{}", message(&value));
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn the_event_records_the_page_count() {
    let server = spawn_script(vec![page(1, Some(2)), page(2, Some(3)), page(3, None)]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call("list_things");
    assert!(ok, "{value}");
    let events = connector_events(&rig);
    assert_eq!(events.len(), 1);
    match &events[0] {
        EventPayload::ConnectorCall { pages, outcome, .. } => {
            assert_eq!(*pages, Some(3));
            assert_eq!(outcome, "ok");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn the_token_cache_mints_once_for_all_pages() {
    let server = spawn_script(vec![
        Reply::Respond(
            200,
            json!({ "access_token": "ya29-pager-token-1", "expires_in": 3600 }).to_string(),
        ),
        page(1, Some(2)),
        page(2, Some(3)),
        page(3, None),
    ]);
    let rig = rig_with(&oauth_yaml(), &server.base_url, None);
    let (value, ok) = rig.call("list_things");
    assert!(ok, "{value}");
    assert_eq!(value["body"]["items"], json!([1, 2, 3]));
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert!(requests[0].starts_with("POST /token HTTP/1.1"));
    assert!(requests[1].starts_with("GET /things HTTP/1.1"));
    assert!(requests[2].starts_with("GET /things?page=2 HTTP/1.1"));
    assert!(requests[3].starts_with("GET /things?page=3 HTTP/1.1"));
}

/// A token the service refused on page one is not sent again: the token
/// the refresh minted carries every later page.
#[test]
fn later_pages_use_the_token_minted_after_a_401() {
    let server = spawn_script(vec![
        Reply::Respond(
            200,
            json!({ "access_token": "ya29-pager-old", "expires_in": 3600 }).to_string(),
        ),
        Reply::Respond(401, "{}".to_string()),
        Reply::Respond(
            200,
            json!({ "access_token": "ya29-pager-new", "expires_in": 3600 }).to_string(),
        ),
        page(1, Some(2)),
        page(2, None),
    ]);
    let rig = rig_with(&oauth_yaml(), &server.base_url, None);
    let (value, ok) = rig.call("list_things");
    assert!(ok, "{value}");
    assert_eq!(value["body"]["items"], json!([1, 2]));
    let requests = server.requests();
    assert_eq!(requests.len(), 5, "{requests:?}");
    assert!(
        requests[1].contains("Bearer ya29-pager-old"),
        "{}",
        requests[1]
    );
    assert!(
        requests[3].contains("Bearer ya29-pager-new"),
        "{}",
        requests[3]
    );
    assert!(
        requests[4].starts_with("GET /things?page=2 HTTP/1.1"),
        "{}",
        requests[4]
    );
    assert!(
        requests[4].contains("Bearer ya29-pager-new"),
        "{}",
        requests[4]
    );
}

#[test]
fn a_failed_page_journals_pages_and_the_last_status() {
    let server = spawn_script(vec![
        page(1, Some(2)),
        Reply::Respond(500, "{}".to_string()),
    ]);
    let rig = rig(&server.base_url);
    let (_value, ok) = rig.call("list_things");
    assert!(!ok);
    let _ = _value;
    let events = connector_events(&rig);
    assert_eq!(events.len(), 1);
    match &events[0] {
        EventPayload::ConnectorCall {
            pages,
            http_status,
            outcome,
            ..
        } => {
            assert_eq!(*pages, Some(2));
            assert_eq!(*http_status, Some(500));
            assert_eq!(outcome, "service");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn real_bytes_count_against_the_total() {
    // Six whitespace-padded pages whose REAL bytes cross the 4 MiB cap
    // exactly on the sixth: the walk reads it whole within the remaining
    // allowance, stops, and says more, instead of following the link.
    const SMALL: usize = 700_000;
    // The allowance of page 6: the cap minus five small pages.
    const LAST: usize = 4 * 1024 * 1024 - 5 * SMALL;
    fn big_page(n: u32, total_len: usize, next: Option<u32>) -> Reply {
        Reply::FromHeaders(Box::new(move |seen: &[String]| {
            let host = seen
                .last()
                .and_then(|raw| {
                    raw.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("host")
                            .then(|| v.trim().to_string())
                    })
                })
                .unwrap_or_default();
            let mut headers = Vec::new();
            if let Some(next) = next {
                headers.push((
                    "Link".to_string(),
                    format!("<http://{host}/big?page={next}>; rel=\"next\""),
                ));
            }
            let prefix = "{\"items\"";
            let rest = format!(": [{n}]}}");
            let spaces = " ".repeat(total_len - prefix.len() - rest.len());
            (200, headers, format!("{prefix}{spaces}{rest}"))
        }))
    }
    let server = spawn_script(vec![
        big_page(1, SMALL, Some(2)),
        big_page(2, SMALL, Some(3)),
        big_page(3, SMALL, Some(4)),
        big_page(4, SMALL, Some(5)),
        big_page(5, SMALL, Some(6)),
        // Page 6 still names a next page; the byte cap stops the walk
        // before it is followed.
        big_page(6, LAST, Some(7)),
    ]);
    let rig = rig(&server.base_url);
    let (value, ok) = rig.call("list_big");
    assert!(
        ok,
        "{}",
        value.to_string().chars().take(200).collect::<String>()
    );
    assert_eq!(value["pages"], 6);
    assert_eq!(value["more"], true);
    assert_eq!(server.requests().len(), 6);
    assert_eq!(value["body"]["items"], json!([1, 2, 3, 4, 5, 6]));
}

#[test]
fn a_stalled_second_page_fails_within_one_deadline() {
    let (arrived_tx, _arrived_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = spawn_script(vec![
        page(1, Some(2)),
        Reply::Held {
            body: json!({ "items": [2] }).to_string(),
            arrived: arrived_tx,
            release: release_rx,
        },
    ]);
    let rig = rig(&server.base_url);
    let started = Instant::now();
    let (value, ok) = rig.call("list_slow");
    let elapsed = started.elapsed();
    assert!(!ok);
    assert_eq!(code(&value), "timeout", "{value}");
    assert!(
        elapsed >= Duration::from_millis(1500) && elapsed <= Duration::from_millis(3500),
        "the stalled second page failed after {elapsed:?}"
    );
    let _ = release_tx.send(());
}

fn message(value: &Value) -> &str {
    value["error"]["message"].as_str().unwrap_or("")
}
