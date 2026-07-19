//! `tests.yaml` schema (spec 2026-07-19-official-connectors, section 4.6): the
//! declarative offline contract tests shipped inside a connector folder. Parsed
//! here as pure data (`deny_unknown_fields` at the document and case level);
//! executed by the engine's offline runner (`apb_engine::connector_test`),
//! which renders each case through the same path a `--dry-run` call uses and
//! checks the rendered request against the `expect` block.
//!
//! `Expectation` is one struct with all optional fields (not an untagged enum)
//! so `deny_unknown_fields` actually applies (serde ignores it inside untagged
//! variants); `resolve` discriminates by shape - `imap` -> imap, `envelope` ->
//! smtp, `status`/`body` -> mock, otherwise HTTP (`method` + `url`).
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
    pub expect: Expectation,
}

/// The expected rendered result. Shape-discriminated by `resolve`: exactly one
/// of the imap (`imap`), HTTP (`method` + `url`), smtp (`envelope`), or mock
/// (`status` + `body`) shapes.
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
    #[serde(default)]
    pub envelope: Option<Envelope>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub body: Option<Value>,
    #[serde(default)]
    pub imap: Option<ImapExpect>,
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

/// The resolved, shape-typed view of an `Expectation`, borrowed from it.
pub enum ExpectKind<'a> {
    Http {
        method: &'a str,
        url: &'a str,
        headers: Option<&'a BTreeMap<String, String>>,
        body_contains: Option<&'a Value>,
    },
    Smtp(&'a Envelope),
    Mock {
        status: u16,
        body: &'a Value,
    },
    Imap(&'a ImapExpect),
}

impl Expectation {
    /// Discriminates the expectation shape. `imap` -> imap; `envelope` ->
    /// smtp; `status` or `body` -> mock; otherwise HTTP (which requires
    /// `method` and `url`). An incomplete shape is an error naming what is
    /// missing.
    pub fn resolve(&self) -> Result<ExpectKind<'_>, String> {
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
            "expectation must be imap (`imap`), http (`method` + `url`), smtp (`envelope`), or mock (`status` + `body`)".to_string()
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
        })
    }
}

impl TestsDoc {
    /// Parses a `tests.yaml` document.
    pub fn from_yaml(yaml: &str) -> Result<Self, ConnectorError> {
        serde_yaml_ng::from_str(yaml).map_err(|e| ConnectorError::Yaml(e.to_string()))
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

    #[test]
    fn imap_expect_unknown_key_rejected() {
        let yaml = "cases:\n  - function: f\n    expect:\n      imap: { op: x, bogus: 1 }\n";
        assert!(TestsDoc::from_yaml(yaml).is_err());
    }
}
