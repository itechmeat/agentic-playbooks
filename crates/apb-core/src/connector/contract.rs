//! `tests.yaml` schema (spec 2026-07-19-official-connectors, section 4.6): the
//! declarative offline contract tests shipped inside a connector folder. Parsed
//! here as pure data (`deny_unknown_fields` at the document and case level);
//! executed by the engine's offline runner (`apb_engine::connector::contract_test`),
//! which renders each case through the same path a `--dry-run` call uses and
//! checks the rendered request against the `expect` block.
//!
//! `Expectation` is one struct with all optional fields (not an untagged enum)
//! so `deny_unknown_fields` actually applies (serde ignores it inside untagged
//! variants); `resolve` discriminates by shape - `inbox` -> inbox, `imap` ->
//! imap, `envelope` -> smtp, `status`/`body` -> mock, otherwise HTTP
//! (`method` + `url`).
//!
//! Envelope semantics (cross-slice obligation 5): every `Envelope` field is
//! optional so the empty `envelope: {}` form (used by slice 5 for smtp `verify`
//! cases) parses. For a `verify` function an empty envelope asserts only that
//! the connection renders without error; for a `send` function, only the
//! envelope fields that are present are compared (an absent field is not
//! asserted).

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::common::ConnectorError;

/// The whole `tests.yaml` document: an ordered list of cases.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestsDoc {
    #[serde(default)]
    pub cases: Vec<TestCase>,
}

/// One contract-test case: the function to render, fake non-secret account
/// field values, the call args, and the expected rendered result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    pub function: String,
    #[serde(default)]
    pub account: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Value,
    /// Renders the case as a send of this intent: a `key` function gets the
    /// nonce in its header or field, a `marker` function the marker built
    /// from these fields. Without it a case renders as a plain call.
    #[serde(default)]
    pub intent: Option<IntentCase>,
    pub expect: Expectation,
}

/// The intent a case renders: the fields the marker carries and the nonce.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentCase {
    pub run: String,
    pub node: String,
    #[serde(default)]
    pub attempt: Option<u32>,
    pub nonce: String,
}

impl IntentCase {
    /// The marker fields this intent stamps.
    pub fn marker_fields(&self) -> super::marker::MarkerFields {
        super::marker::MarkerFields {
            run: self.run.clone(),
            node: self.node.clone(),
            attempt: self.attempt,
            nonce: self.nonce.clone(),
        }
    }
}

/// A reconciliation a case checks offline: what the identity function and
/// the reconcile listing answered, and whether the case's intent is found
/// among the listed items.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileExpect {
    /// The identity function's raw response.
    pub identity: Value,
    /// The reconcile function's raw response.
    pub response: Value,
    /// Whether the listing said a further page exists.
    #[serde(default)]
    pub next_page: bool,
    /// What the lookup must conclude.
    pub outcome: ReconcileOutcome,
    /// The reference the found item must carry.
    #[serde(default)]
    pub item_ref: Option<String>,
}

/// What a reconciliation case expects the lookup to conclude.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    /// The intent's item is in the listing.
    Found,
    /// The whole listing was read and the item is not in it.
    Absent,
    /// The listing cannot settle it (no item list where the connector says
    /// it is, or more pages exist).
    Inconclusive,
}

/// The expected rendered result. Shape-discriminated by `resolve`: exactly one
/// of the inbox (`inbox`), imap (`imap`), HTTP (`method` + `url`), smtp
/// (`envelope`), or mock (`status` + `body`) shapes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub body_contains: Option<Value>,
    /// Subset match over the DECODED `body_form` pairs of the rendered
    /// request, mirroring `body_contains` (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.1).
    #[serde(default)]
    pub body_form_contains: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub envelope: Option<Envelope>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub imap: Option<ImapExpect>,
    #[serde(default)]
    pub inbox: Option<InboxExpect>,
    #[serde(default)]
    pub reconcile: Option<ReconcileExpect>,
}

/// The smtp envelope a case asserts. Every field is optional (cross-slice
/// obligation 5): an empty `envelope: {}` is a legal verify-case assertion, and
/// a send case compares only the fields that are present.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<Vec<String>>,
    #[serde(default)]
    pub subject: Option<String>,
}

/// The imap expectation a case asserts (wave 2): the op the rendered call
/// must use, the folder it must target (if any), and a subset of `params` the
/// rendered call's params must contain.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapExpect {
    pub op: String,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub params_contains: Option<BTreeMap<String, String>>,
}

/// One inline event a case seeds into the fixture inbox before the op runs
/// (spec 2026-08-16-webhook-ingest-design). Seeded in list order, so the
/// first entry becomes seq 1.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxSeed {
    pub provider_id: String,
    #[serde(default)]
    pub body: Value,
}

/// The inbox expectation a case asserts (spec
/// 2026-08-16-webhook-ingest-design): the op the rendered call must use, the
/// events the case seeds, the cursor position before the op, and whichever
/// parts of the returned envelope the case chooses to pin. Every assertion
/// field is optional, following the `envelope` precedent: an absent field is
/// not asserted, so a case can check only the part it cares about.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxExpect {
    pub op: String,
    /// Events present in the inbox before the op, oldest first.
    #[serde(default)]
    pub seed: Vec<InboxSeed>,
    /// The consumer's cursor before the op. Zero (the default) means the
    /// consumer has acknowledged nothing.
    #[serde(default)]
    pub acked: u64,
    /// Expected `events[].seq`, in order, for an `op: read` case.
    #[serde(default)]
    pub events: Option<Vec<u64>>,
    /// Expected `cursor` for an `op: read` case.
    #[serde(default)]
    pub cursor: Option<u64>,
    /// Expected `acked_up_to` for an `op: ack` case.
    #[serde(default)]
    pub acked_up_to: Option<u64>,
    /// Expected `pending` for an `op: peek_depth` case.
    #[serde(default)]
    pub pending: Option<u64>,
}

/// The resolved, shape-typed view of an `Expectation`, borrowed from it.
/// `reconcile` wins the discrimination: such a case checks a listing, not a
/// rendered request.
pub enum ExpectKind<'a> {
    Http {
        method: &'a str,
        url: &'a str,
        headers: Option<&'a BTreeMap<String, String>>,
        body_contains: Option<&'a Value>,
        body_form_contains: Option<&'a BTreeMap<String, String>>,
    },
    Smtp(&'a Envelope),
    Mock {
        status: u16,
        body: &'a Value,
    },
    Imap(&'a ImapExpect),
    Inbox(&'a InboxExpect),
    Reconcile(&'a ReconcileExpect),
}

impl Expectation {
    /// Discriminates the expectation shape. `imap` -> imap; `envelope` ->
    /// smtp; `status` or `body` -> mock; otherwise HTTP (which requires
    /// `method` and `url`). An incomplete shape is an error naming what is
    /// missing.
    pub fn resolve(&self) -> Result<ExpectKind<'_>, String> {
        if let Some(reconcile) = &self.reconcile {
            return Ok(ExpectKind::Reconcile(reconcile));
        }
        if let Some(inbox) = &self.inbox {
            return Ok(ExpectKind::Inbox(inbox));
        }
        if let Some(imap) = &self.imap {
            return Ok(ExpectKind::Imap(imap));
        }
        if let Some(env) = &self.envelope {
            return Ok(ExpectKind::Smtp(env));
        }
        if self.status.is_some() || self.body.is_some() {
            let status = self
                .status
                .ok_or_else(|| "mock expectation needs a `status`".to_string())?;
            let body = self
                .body
                .as_ref()
                .ok_or_else(|| "mock expectation needs a `body`".to_string())?;
            return Ok(ExpectKind::Mock { status, body });
        }
        let method = self.method.as_deref().ok_or_else(|| {
            "expectation must be inbox (`inbox`), imap (`imap`), http (`method` + `url`), smtp (`envelope`), or mock (`status` + `body`)".to_string()
        })?;
        let url = self
            .url
            .as_deref()
            .ok_or_else(|| "http expectation needs a `url`".to_string())?;
        Ok(ExpectKind::Http {
            method,
            url,
            headers: self.headers.as_ref(),
            body_contains: self.body_contains.as_ref(),
            body_form_contains: self.body_form_contains.as_ref(),
        })
    }
}

impl TestsDoc {
    /// Parses a `tests.yaml` document.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConnectorError> {
        yaml_serde::from_str(yaml).map_err(|e| ConnectorError::Yaml(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_smtp_and_mock_cases() {
        let yaml = r#"
cases:
  - function: create_issue
    account: { api_base: https://api.github.com }
    args: { owner: acme, repo: site, title: "Broken build" }
    expect:
      method: POST
      url: https://api.github.com/repos/acme/site/issues
      body_contains: { title: "Broken build" }
  - function: send_email
    account: { host: smtp.example.com, port: "587", from_email: a@b.c }
    args: { to: x@y.z, subject: Hi, body_text: Test }
    expect:
      envelope: { from: a@b.c, to: [x@y.z], subject: Hi }
  - function: ping
    account: {}
    args: {}
    expect:
      status: 200
      body: { ok: true }
"#;
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert_eq!(doc.cases.len(), 3);
        assert!(matches!(
            doc.cases[0].expect.resolve().unwrap(),
            ExpectKind::Http { .. }
        ));
        assert!(matches!(
            doc.cases[1].expect.resolve().unwrap(),
            ExpectKind::Smtp(_)
        ));
        assert!(matches!(
            doc.cases[2].expect.resolve().unwrap(),
            ExpectKind::Mock { .. }
        ));
    }

    #[test]
    fn empty_envelope_parses_and_resolves_to_smtp() {
        // Cross-slice obligation 5: `envelope: {}` (a verify case) is legal and
        // resolves to the smtp shape with every field absent.
        let yaml = "cases:\n  - function: verify\n    expect:\n      envelope: {}\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        match doc.cases[0].expect.resolve().unwrap() {
            ExpectKind::Smtp(env) => {
                assert!(env.from.is_none());
                assert!(env.to.is_none());
                assert!(env.subject.is_none());
            }
            _ => panic!("empty envelope must resolve to smtp"),
        }
    }

    #[test]
    fn intent_and_reconcile_cases_parse() {
        let yaml = r#"
cases:
  - function: comment_issue
    args: { owner: a, repo: b, number: 1, body: hi }
    intent: { run: r1, node: triage, attempt: 2, nonce: n1 }
    expect:
      reconcile:
        identity: { login: bot }
        response: []
        outcome: absent
"#;
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        let case = &doc.cases[0];
        assert_eq!(
            case.intent.as_ref().unwrap().marker_fields().attempt,
            Some(2)
        );
        match case.expect.resolve().unwrap() {
            ExpectKind::Reconcile(r) => {
                assert!(r.outcome == ReconcileOutcome::Absent && !r.next_page)
            }
            _ => panic!("a reconcile expectation must resolve to reconcile"),
        }
        let bad = "cases:\n  - function: f\n    intent: { run: r, node: n, nonce: x, bogus: 1 }\n    expect: { status: 200, body: {} }\n";
        assert!(TestsDoc::from_yaml(bad).is_err());
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let yaml = "cases: []\nbogus: 1\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    #[test]
    fn unknown_case_key_is_rejected() {
        let yaml = "cases:\n  - function: f\n    bogus: 1\n    expect: { status: 200, body: {} }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    #[test]
    fn unknown_envelope_key_is_rejected() {
        let yaml = "cases:\n  - function: f\n    expect:\n      envelope: { bogus: 1 }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    #[test]
    fn args_default_to_null_and_account_to_empty() {
        let yaml = "cases:\n  - function: ping\n    expect: { status: 200, body: {} }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert!(doc.cases[0].account.is_empty());
        assert!(doc.cases[0].args.is_null());
    }

    #[test]
    fn mock_expectation_missing_status_is_a_resolve_error() {
        let yaml = "cases:\n  - function: ping\n    expect: { body: { ok: true } }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert!(doc.cases[0].expect.resolve().is_err());
    }

    // -- imap expectation (wave 2) --

    #[test]
    fn imap_expect_parses_and_resolves() {
        let yaml = "cases:\n  - function: search_inbox\n    expect:\n      imap:\n        op: search\n        folder: INBOX\n        params_contains: { limit: \"20\" }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        match doc.cases[0].expect.resolve().unwrap() {
            ExpectKind::Imap(imap) => {
                assert_eq!(imap.op, "search");
                assert_eq!(imap.folder.as_deref(), Some("INBOX"));
                assert_eq!(
                    imap.params_contains.as_ref().unwrap().get("limit"),
                    Some(&"20".to_string())
                );
            }
            _ => panic!("imap expectation must resolve to imap"),
        }
    }

    // -- body_form_contains (spec 2026-07-22-official-connectors-wave-3, 4.1) --

    #[test]
    fn body_form_contains_parses_and_resolves_to_http() {
        let yaml = "cases:\n  - function: send_message\n    account: { base_url: https://chat.example.com }\n    args: { to: general, content: hi }\n    expect:\n      method: POST\n      url: https://chat.example.com/messages\n      body_form_contains: { to: general, content: hi }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        match doc.cases[0].expect.resolve().unwrap() {
            ExpectKind::Http {
                body_form_contains, ..
            } => {
                let subset = body_form_contains.expect("body_form_contains must resolve");
                assert_eq!(subset.get("to"), Some(&"general".to_string()));
                assert_eq!(subset.get("content"), Some(&"hi".to_string()));
            }
            _ => panic!("body_form_contains expectation must resolve to http"),
        }
    }

    #[test]
    fn unknown_expectation_key_still_rejected() {
        let yaml = "cases:\n  - function: f\n    expect:\n      method: POST\n      url: https://x\n      body_form_bogus: { k: v }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    #[test]
    fn imap_expect_unknown_key_rejected() {
        let yaml = "cases:\n  - function: f\n    expect:\n      imap: { op: x, bogus: 1 }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }

    // -- inbox expectation (spec 2026-08-16-webhook-ingest-design) --

    #[test]
    fn inbox_expect_parses_and_resolves() {
        let yaml = r#"
cases:
  - function: inbox_read
    args: { consumer: worker }
    expect:
      inbox:
        op: read
        seed:
          - { provider_id: m1, body: { n: 1 } }
          - { provider_id: m2, body: { n: 2 } }
        events: [1, 2]
        cursor: 0
"#;
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        match doc.cases[0].expect.resolve().unwrap() {
            ExpectKind::Inbox(inbox) => {
                assert_eq!(inbox.op, "read");
                assert_eq!(inbox.seed.len(), 2);
                assert_eq!(inbox.seed[0].provider_id, "m1");
                assert_eq!(inbox.seed[1].body["n"], 2);
                assert_eq!(inbox.events.as_deref(), Some(&[1u64, 2u64][..]));
                assert_eq!(inbox.cursor, Some(0));
                assert_eq!(
                    inbox.acked, 0,
                    "the pre-op cursor defaults to nothing acked"
                );
            }
            _ => panic!("an inbox expectation must resolve to inbox"),
        }
    }

    #[test]
    fn inbox_ack_and_depth_expectations_resolve() {
        let yaml = r#"
cases:
  - function: inbox_ack
    args: { up_to_seq: 2 }
    expect:
      inbox:
        op: ack
        seed:
          - { provider_id: m1, body: {} }
          - { provider_id: m2, body: {} }
        acked_up_to: 2
  - function: inbox_depth
    expect:
      inbox:
        op: peek_depth
        seed:
          - { provider_id: m1, body: {} }
        acked: 0
        pending: 1
"#;
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        match doc.cases[0].expect.resolve().unwrap() {
            ExpectKind::Inbox(inbox) => assert_eq!(inbox.acked_up_to, Some(2)),
            _ => panic!("ack case must resolve to inbox"),
        }
        match doc.cases[1].expect.resolve().unwrap() {
            ExpectKind::Inbox(inbox) => {
                assert_eq!(inbox.pending, Some(1));
                assert!(inbox.events.is_none(), "a depth case asserts no event list");
            }
            _ => panic!("depth case must resolve to inbox"),
        }
    }

    #[test]
    fn inbox_expect_unknown_key_rejected() {
        let yaml = "cases:\n  - function: f\n    expect:\n      inbox: { op: read, bogus: 1 }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
        let seed = "cases:\n  - function: f\n    expect:\n      inbox:\n        op: read\n        seed:\n          - { provider_id: m1, body: {}, bogus: 1 }\n";
        assert!(TestsDoc::from_yaml(seed).is_err());
    }

    #[test]
    fn inbox_wins_the_shape_discrimination() {
        // `Expectation` has no `deny_unknown_fields`-style mutual exclusion
        // between `inbox` and `imap` (both are plain sibling `Option`
        // fields), so a case co-declaring both parses. `resolve` checks
        // `inbox` first, so it wins over the co-declared `imap` block; this
        // pins that ordering as deliberate and documented rather than
        // incidental.
        let yaml = "cases:\n  - function: f\n    expect:\n      inbox: { op: read }\n      imap: { op: search }\n";
        let doc = TestsDoc::from_yaml(yaml).unwrap();
        assert!(matches!(
            doc.cases[0].expect.resolve().unwrap(),
            ExpectKind::Inbox(_)
        ));
    }
}
