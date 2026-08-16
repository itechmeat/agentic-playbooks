//! The `inbox` contract-test kind, driven over the `echo-hooks` fixture
//! connector. Fully offline: the runner seeds the case's inline events in
//! memory and asserts against the same envelope builders a live call uses,
//! so no store, no config dir and no process env are involved.

use apb_core::connector::contract::TestsDoc;
use apb_core::connector::def::ConnectorDoc;
use apb_engine::connector::contract_test::run_tests;

const CONNECTOR: &str = include_str!("../fixtures/connectors/echo-hooks/connector.yaml");
const TESTS: &str = include_str!("../fixtures/connectors/echo-hooks/tests.yaml");

fn fixture() -> (ConnectorDoc, TestsDoc) {
    (
        ConnectorDoc::from_yaml(CONNECTOR, "echo-hooks").unwrap(),
        TestsDoc::from_yaml(TESTS).unwrap(),
    )
}

#[test]
fn the_echo_hooks_fixture_passes_its_own_contract_tests() {
    let (doc, tests) = fixture();
    let report = run_tests(&doc, &tests);
    let failures: Vec<String> = report
        .results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| format!("{}: {}", r.function, r.detail))
        .collect();
    assert!(failures.is_empty(), "cases failed: {failures:?}");
    assert_eq!(report.results.len(), 6);
    assert!(report.all_passed());
}

#[test]
fn a_wrong_event_list_fails_the_case() {
    let (doc, _) = fixture();
    let tests = TestsDoc::from_yaml(
        "cases:\n  - function: inbox_read\n    expect:\n      inbox:\n        op: read\n        seed:\n          - { provider_id: e1, body: {} }\n        events: [1, 2]\n",
    )
    .unwrap();
    let report = run_tests(&doc, &tests);
    assert!(!report.all_passed());
    assert!(
        report.results[0].detail.contains("events"),
        "the failure must name what mismatched: {}",
        report.results[0].detail
    );
}

#[test]
fn a_case_whose_op_disagrees_with_the_manifest_fails() {
    let (doc, _) = fixture();
    let tests = TestsDoc::from_yaml(
        "cases:\n  - function: inbox_read\n    expect:\n      inbox:\n        op: ack\n        acked_up_to: 0\n",
    )
    .unwrap();
    let report = run_tests(&doc, &tests);
    assert!(!report.all_passed());
    assert!(
        report.results[0].detail.contains("op"),
        "was: {}",
        report.results[0].detail
    );
}

#[test]
fn an_inbox_case_against_a_non_inbox_function_fails() {
    let doc = ConnectorDoc::from_yaml(
        "name: x\nversion: 0.1.0\nfunctions:\n  - name: ping\n    description: d\n    mock: { status: 200, body: {} }\n",
        "x",
    )
    .unwrap();
    let tests =
        TestsDoc::from_yaml("cases:\n  - function: ping\n    expect:\n      inbox: { op: read }\n")
            .unwrap();
    let report = run_tests(&doc, &tests);
    assert!(!report.all_passed());
    assert!(
        report.results[0].detail.contains("inbox"),
        "was: {}",
        report.results[0].detail
    );
}

#[test]
fn bad_case_args_surface_as_a_render_failure_rather_than_a_panic() {
    let (doc, _) = fixture();
    let tests = TestsDoc::from_yaml(
        "cases:\n  - function: inbox_ack\n    args: {}\n    expect:\n      inbox:\n        op: ack\n        acked_up_to: 0\n",
    )
    .unwrap();
    let report = run_tests(&doc, &tests);
    assert!(!report.all_passed());
    assert!(
        report.results[0].detail.contains("up_to_seq"),
        "the missing argument must be named: {}",
        report.results[0].detail
    );
}
