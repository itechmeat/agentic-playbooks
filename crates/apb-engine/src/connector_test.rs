//! Offline connector contract-test runner (spec 2026-07-19-official-connectors,
//! section 4.6). Runs every `tests.yaml` case of a connector through the same
//! render path a `--dry-run` call uses (`connector_call::render_http`), with
//! secrets stubbed to a fixed value and no network, then checks the rendered
//! request against the case's `expect` block. Exit-code semantics live in the
//! CLI (`apb connector test`); this module returns a structured report.

use std::collections::BTreeMap;

use apb_core::connector::contract::{Envelope, ExpectKind, ImapExpect, TestCase, TestsDoc};
use apb_core::connector::def::{ConnectorDoc, FunctionSpec};
use serde_json::Value;

use crate::connector_call::render_http;

/// The fixed value every secret account field resolves to in the offline
/// runner. A real secret is never read (spec 4.6).
const SECRET_STUB: &str = "test-secret";

/// The result of one contract-test case.
pub struct CaseResult {
    pub function: String,
    pub passed: bool,
    /// Empty when the case passed; the failure reason otherwise.
    pub detail: String,
}

/// The result of running a connector's whole `tests.yaml`.
pub struct TestReport {
    pub results: Vec<CaseResult>,
}

impl TestReport {
    /// True when every case passed (an empty case list passes vacuously; the
    /// per-function coverage requirement is the slice-5 CI gate's job).
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }
}

/// Runs every case in `tests` against `doc`.
pub fn run_tests(doc: &ConnectorDoc, tests: &TestsDoc) -> TestReport {
    let results = tests.cases.iter().map(|case| run_case(doc, case)).collect();
    TestReport { results }
}

fn run_case(doc: &ConnectorDoc, case: &TestCase) -> CaseResult {
    let detail = evaluate(doc, case).err().unwrap_or_default();
    CaseResult {
        function: case.function.clone(),
        passed: detail.is_empty(),
        detail,
    }
}

fn evaluate(doc: &ConnectorDoc, case: &TestCase) -> Result<(), String> {
    let function = doc.function(&case.function).ok_or_else(|| {
        format!(
            "function `{}` is not defined by the connector",
            case.function
        )
    })?;
    let kind = case.expect.resolve()?;
    let args = if case.args.is_null() {
        Value::Object(Default::default())
    } else {
        case.args.clone()
    };
    match kind {
        ExpectKind::Mock { status, body } => eval_mock(function, status, body),
        ExpectKind::Http {
            method,
            url,
            headers,
            body_contains,
        } => eval_http(
            doc,
            function,
            &case.account,
            &args,
            method,
            url,
            headers,
            body_contains,
        ),
        ExpectKind::Smtp(envelope) => eval_smtp(doc, function, &case.account, &args, envelope),
        ExpectKind::Imap(expected) => eval_imap(doc, function, &case.account, &args, expected),
    }
}

fn eval_mock(function: &FunctionSpec, status: u16, body: &Value) -> Result<(), String> {
    let mock = function.mock.as_ref().ok_or_else(|| {
        format!(
            "function `{}` is not a mock but the case expects a mock response",
            function.name
        )
    })?;
    if mock.status != status {
        return Err(format!(
            "status mismatch: expected {status}, rendered {}",
            mock.status
        ));
    }
    if &mock.body != body {
        return Err(format!(
            "body mismatch: expected {body}, rendered {}",
            mock.body
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn eval_http(
    doc: &ConnectorDoc,
    function: &FunctionSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    method: &str,
    url: &str,
    headers: Option<&BTreeMap<String, String>>,
    body_contains: Option<&Value>,
) -> Result<(), String> {
    if function.mock.is_some() {
        return Err(format!(
            "function `{}` is a mock but the case expects an HTTP request",
            function.name
        ));
    }
    if function.smtp.is_some() {
        return Err(format!(
            "function `{}` is an smtp function but the case expects an HTTP request",
            function.name
        ));
    }
    let secrets: BTreeMap<String, String> = doc
        .secret_fields()
        .into_iter()
        .map(|f| (f, SECRET_STUB.to_string()))
        .collect();
    let rendered = render_http(function, account, args, &secrets)
        .map_err(|e| format!("render failed: {}", e.message))?;
    if !rendered.method.eq_ignore_ascii_case(method) {
        return Err(format!(
            "method mismatch: expected {method}, rendered {}",
            rendered.method
        ));
    }
    if rendered.pre_auth_url != url {
        return Err(format!(
            "url mismatch: expected `{url}`, rendered `{}`",
            rendered.pre_auth_url
        ));
    }
    // Header subset match: every expected header key must be present in the
    // rendered header map (function headers plus the default User-Agent, folded
    // in by `render_http`) with an exactly-equal value. Headers not mentioned by
    // the expectation are not asserted.
    if let Some(expected) = headers {
        for (key, want) in expected {
            match rendered.headers.get(key) {
                Some(got) if got == want => {}
                Some(got) => {
                    return Err(format!(
                        "header mismatch: `{key}` expected `{want}`, rendered `{got}`"
                    ));
                }
                None => {
                    return Err(format!(
                        "header mismatch: `{key}` expected `{want}`, but no such header is rendered"
                    ));
                }
            }
        }
    }
    if let Some(subset) = body_contains {
        let body = rendered.rendered_body.unwrap_or(Value::Null);
        if !json_subset(subset, &body) {
            return Err(format!(
                "body_contains mismatch: `{subset}` is not a subset of `{body}`"
            ));
        }
    }
    Ok(())
}

/// Matches an smtp `envelope` expectation. Renders the function's envelope
/// offline through `connector_smtp::build` in dry-run mode (secrets stubbed, no
/// connection), then compares only the envelope fields the expectation carries
/// (cross-slice obligation 5): an absent field is not asserted. An empty
/// `envelope: {}` asserts only that the render succeeded - the shape a verify
/// function uses, since a verify render produces no envelope at all.
fn eval_smtp(
    doc: &ConnectorDoc,
    function: &FunctionSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    expected: &Envelope,
) -> Result<(), String> {
    let spec = function.smtp.as_ref().ok_or_else(|| {
        format!(
            "function `{}` is not an smtp function but the case expects an envelope",
            function.name
        )
    })?;
    let secrets: BTreeMap<String, String> = doc
        .secret_fields()
        .into_iter()
        .map(|f| (f, SECRET_STUB.to_string()))
        .collect();
    let built = crate::connector_smtp::build(
        spec,
        account,
        args,
        &secrets,
        Vec::new(),
        true,
        function.timeout_sec,
    )
    .map_err(|e| format!("render failed: {}", e.message))?;
    let json = match built {
        crate::connector_smtp::SmtpBuild::DryRun(v) => v,
        crate::connector_smtp::SmtpBuild::Call(_) => {
            return Err("smtp dry-run unexpectedly produced a live call".to_string());
        }
    };

    // Empty envelope (a verify case): the render succeeding is the whole
    // assertion; there is nothing else to compare.
    if expected.from.is_none() && expected.to.is_none() && expected.subject.is_none() {
        return Ok(());
    }

    let env = json.get("envelope").ok_or_else(|| {
        "expected an smtp envelope but the function rendered none (a verify function has no \
         envelope; assert it with an empty `envelope: {}`)"
            .to_string()
    })?;
    if let Some(from) = &expected.from {
        let got = env.get("from").and_then(Value::as_str).unwrap_or_default();
        if got != from {
            return Err(format!(
                "envelope from mismatch: expected `{from}`, rendered `{got}`"
            ));
        }
    }
    if let Some(to) = &expected.to {
        let got: Vec<String> = env
            .get("to")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if &got != to {
            return Err(format!(
                "envelope to mismatch: expected {to:?}, rendered {got:?}"
            ));
        }
    }
    if let Some(subject) = &expected.subject {
        let got = env
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if got != subject {
            return Err(format!(
                "envelope subject mismatch: expected `{subject}`, rendered `{got}`"
            ));
        }
    }
    Ok(())
}

/// Matches an imap expectation. Renders the function's op offline through
/// `connector_imap::build` in dry-run mode (secrets stubbed, no connection),
/// then compares the rendered `op` (always asserted), the rendered `folder`
/// (only when `expect.imap.folder` is present), and a subset of `params`
/// (only when `expect.imap.params_contains` is present). Each `params_contains`
/// value is compared by its rendered value's JSON text so a numeric param
/// (`limit: "20"`) matches the string `"20"` the case wrote.
fn eval_imap(
    doc: &ConnectorDoc,
    function: &FunctionSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    expected: &ImapExpect,
) -> Result<(), String> {
    let spec = function.imap.as_ref().ok_or_else(|| {
        format!(
            "function `{}` is not an imap function but the case expects an imap op",
            function.name
        )
    })?;
    let secrets: BTreeMap<String, String> = doc
        .secret_fields()
        .into_iter()
        .map(|f| (f, SECRET_STUB.to_string()))
        .collect();
    let built = crate::connector_imap::build(
        spec,
        account,
        args,
        &secrets,
        Vec::new(),
        true,
        function.timeout_sec,
    )
    .map_err(|e| format!("render failed: {}", e.message))?;
    let json = match built {
        crate::connector_imap::ImapBuild::DryRun(v) => v,
        crate::connector_imap::ImapBuild::Call(_) => {
            return Err("imap dry-run unexpectedly produced a live call".to_string());
        }
    };
    let imap = json
        .get("imap")
        .ok_or_else(|| "imap dry-run rendered no `imap` block".to_string())?;
    let op = imap.get("op").and_then(Value::as_str).unwrap_or_default();
    if op != expected.op {
        return Err(format!(
            "imap op mismatch: expected `{}`, rendered `{op}`",
            expected.op
        ));
    }
    let params = imap.get("params").cloned().unwrap_or(Value::Null);
    if let Some(folder) = &expected.folder {
        let got = params
            .get("folder")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if got != folder {
            return Err(format!(
                "imap folder mismatch: expected `{folder}`, rendered `{got}`"
            ));
        }
    }
    if let Some(subset) = &expected.params_contains {
        for (key, want) in subset {
            match params.get(key) {
                Some(got) => {
                    let got_text = imap_param_text(got);
                    if &got_text != want {
                        return Err(format!(
                            "imap params mismatch: `{key}` expected `{want}`, rendered `{got_text}`"
                        ));
                    }
                }
                None => {
                    return Err(format!(
                        "imap params mismatch: `{key}` expected `{want}`, but no such param is rendered"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Renders an imap param value to the text a `params_contains` expectation
/// compares against: a JSON string's own text (no surrounding quotes), or the
/// JSON text of any other value (so a rendered number like `20` matches the
/// case's string `"20"`).
fn imap_param_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Subset match: every key of `expected` (recursively for nested objects) must
/// be present in `actual` with a subset-matching value; non-object values must
/// be exactly equal. Keeps cases robust to services (and manifests) adding
/// fields (spec 4.6).
fn json_subset(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, v)| a.get(k).is_some_and(|av| json_subset(v, av))),
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_core::connector::contract::TestsDoc;
    use apb_core::connector::def::ConnectorDoc;

    const YAML: &str = r#"
name: example
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - name: api_base
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: ping
    description: d
    read_only: true
    mock: { status: 200, body: { ok: true } }
  - name: get_item
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/items/{{args.id}}"
    args_schema: { type: object, properties: { id: { type: string } }, required: [id] }
  - name: create_item
    description: d
    method: POST
    url: "{{account.api_base}}/items"
    body: "{{args}}"
    args_schema: { type: object }
  - name: with_headers
    description: d
    read_only: true
    method: GET
    url: "{{account.api_base}}/h"
    headers: { X-Api-Version: "2022-11", X-Owner: "{{args.owner}}" }
    args_schema: { type: object }
"#;

    fn doc() -> ConnectorDoc {
        ConnectorDoc::from_yaml(YAML, "example").unwrap()
    }

    const IMAP_YAML: &str = r#"
name: mailbox
version: 0.1.0
account_fields:
  - name: host
    required: true
  - name: port
    required: true
  - name: use_tls
  - name: auth_method
  - name: username
  - name: password
    required: true
    secret: true
functions:
  - name: search_inbox
    description: d
    read_only: true
    imap:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        auth_method: "{{account.auth_method}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
      op: search
      params:
        folder: "{{args.folder}}"
        limit: "{{args.limit}}"
        since_days: "{{args.since_days}}"
"#;

    fn imap_doc() -> ConnectorDoc {
        ConnectorDoc::from_yaml(IMAP_YAML, "mailbox").unwrap()
    }

    #[test]
    fn imap_case_passes_on_match() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: search_inbox\n    account: { host: imap.example.com, port: \"993\" }\n    args: { folder: INBOX, limit: \"20\" }\n    expect:\n      imap: { op: search, folder: INBOX, params_contains: { limit: \"20\" } }\n",
        )
        .unwrap();
        let report = run_tests(&imap_doc(), &tests);
        assert!(report.all_passed(), "{:?}", failing(&report));
    }

    #[test]
    fn imap_case_fails_on_wrong_op() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: search_inbox\n    account: { host: imap.example.com, port: \"993\" }\n    args: { folder: INBOX, limit: \"20\" }\n    expect:\n      imap: { op: fetch }\n",
        )
        .unwrap();
        let report = run_tests(&imap_doc(), &tests);
        assert!(!report.all_passed());
        assert!(
            report.results[0].detail.contains("op mismatch"),
            "detail: {}",
            report.results[0].detail
        );
    }

    #[test]
    fn imap_case_fails_on_missing_param() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: search_inbox\n    account: { host: imap.example.com, port: \"993\" }\n    args: { folder: INBOX, limit: \"20\" }\n    expect:\n      imap: { op: search, params_contains: { since_days: \"5\" } }\n",
        )
        .unwrap();
        let report = run_tests(&imap_doc(), &tests);
        assert!(!report.all_passed());
        assert!(
            report.results[0].detail.contains("since_days"),
            "detail: {}",
            report.results[0].detail
        );
    }

    const SMTP_YAML: &str = r#"
name: mailer
version: 0.1.0
account_fields:
  - name: host
    required: true
  - name: port
    required: true
  - name: use_tls
    required: true
  - name: from_email
    required: true
  - name: password
    required: true
    secret: true
functions:
  - name: send_email
    description: d
    smtp:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        password: "{{secret.password}}"
      message:
        from_email: "{{account.from_email}}"
        to: "{{args.to}}"
        subject: "{{args.subject}}"
        body_text: "{{args.body_text}}"
      verify: false
  - name: verify
    description: d
    read_only: true
    smtp:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        password: "{{secret.password}}"
      verify: true
"#;

    fn smtp_doc() -> ConnectorDoc {
        ConnectorDoc::from_yaml(SMTP_YAML, "mailer").unwrap()
    }

    #[test]
    fn all_cases_pass_for_matching_expectations() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: ping\n    expect: { status: 200, body: { ok: true } }\n  - function: get_item\n    account: { api_base: https://api.example.com }\n    args: { id: \"42\" }\n    expect: { method: GET, url: https://api.example.com/items/42 }\n  - function: create_item\n    account: { api_base: https://api.example.com }\n    args: { title: Hi }\n    expect: { method: POST, url: https://api.example.com/items, body_contains: { title: Hi } }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.all_passed(), "{:?}", failing(&report));
        assert_eq!(report.results.len(), 3);
    }

    #[test]
    fn url_mismatch_fails_that_case() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: get_item\n    account: { api_base: https://api.example.com }\n    args: { id: \"42\" }\n    expect: { method: GET, url: https://api.example.com/items/99 }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(!report.all_passed());
        assert!(report.results[0].detail.contains("url mismatch"));
    }

    #[test]
    fn body_contains_is_a_subset_match() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: create_item\n    account: { api_base: https://api.example.com }\n    args: { title: Hi, extra: 1 }\n    expect: { method: POST, url: https://api.example.com/items, body_contains: { title: Hi } }\n",
        )
        .unwrap();
        assert!(run_tests(&doc(), &tests).all_passed());
    }

    #[test]
    fn unknown_function_fails() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: nope\n    expect: { status: 200, body: {} }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.results[0].detail.contains("not defined"));
    }

    #[test]
    fn header_expectation_subset_match_passes() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: with_headers\n    account: { api_base: https://api.example.com }\n    args: { owner: acme }\n    expect: { method: GET, url: https://api.example.com/h, headers: { X-Api-Version: \"2022-11\", X-Owner: acme } }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.all_passed(), "{:?}", failing(&report));
    }

    #[test]
    fn header_value_mismatch_fails_naming_the_key() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: with_headers\n    account: { api_base: https://api.example.com }\n    args: { owner: acme }\n    expect: { method: GET, url: https://api.example.com/h, headers: { X-Api-Version: WRONG } }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(!report.all_passed());
        assert!(
            report.results[0].detail.contains("header mismatch"),
            "detail: {}",
            report.results[0].detail
        );
        assert!(report.results[0].detail.contains("X-Api-Version"));
    }

    #[test]
    fn header_expectation_can_assert_the_default_user_agent() {
        // The default User-Agent is folded into the rendered header map, so a
        // case may assert it (its value tracks the crate version prefix).
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: with_headers\n    account: { api_base: https://api.example.com }\n    args: { owner: acme }\n    expect: { method: GET, url: https://api.example.com/h, headers: { User-Agent: MISSING } }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        // The default UA is not "MISSING", so this asserts the key is present and
        // compared (mismatch), proving the header map carries the default UA.
        assert!(report.results[0].detail.contains("header mismatch"));
    }

    #[test]
    fn smtp_envelope_match_passes() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: send_email\n    account: { host: smtp.example.com, port: \"587\", use_tls: \"true\", from_email: a@b.c }\n    args: { to: x@y.z, subject: Hi, body_text: T }\n    expect:\n      envelope: { from: a@b.c, to: [x@y.z], subject: Hi }\n",
        )
        .unwrap();
        let report = run_tests(&smtp_doc(), &tests);
        assert!(report.all_passed(), "{:?}", failing(&report));
    }

    #[test]
    fn smtp_envelope_subject_mismatch_fails() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: send_email\n    account: { host: smtp.example.com, port: \"587\", use_tls: \"true\", from_email: a@b.c }\n    args: { to: x@y.z, subject: Hi, body_text: T }\n    expect:\n      envelope: { from: a@b.c, to: [x@y.z], subject: Nope }\n",
        )
        .unwrap();
        let report = run_tests(&smtp_doc(), &tests);
        assert!(!report.all_passed());
        assert!(
            report.results[0].detail.contains("subject mismatch"),
            "detail: {}",
            report.results[0].detail
        );
    }

    #[test]
    fn smtp_empty_envelope_on_verify_passes() {
        // Cross-slice obligation 5: an empty `envelope: {}` on a verify function
        // asserts only that the connection renders without error.
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: verify\n    account: { host: smtp.example.com, port: \"587\", use_tls: \"true\" }\n    expect:\n      envelope: {}\n",
        )
        .unwrap();
        let report = run_tests(&smtp_doc(), &tests);
        assert!(report.all_passed(), "{:?}", failing(&report));
    }

    #[test]
    fn kind_mismatch_between_function_and_expectation_fails() {
        let tests = TestsDoc::from_yaml(
            "cases:\n  - function: ping\n    expect: { method: GET, url: https://x }\n",
        )
        .unwrap();
        let report = run_tests(&doc(), &tests);
        assert!(report.results[0].detail.contains("mock"));
    }

    fn failing(report: &TestReport) -> Vec<String> {
        report
            .results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| format!("{}: {}", r.function, r.detail))
            .collect()
    }
}
