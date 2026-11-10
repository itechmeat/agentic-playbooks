//! V73 over the checked-in `google-calendar` manifest: the calendar writes
//! that mail people (`create_event`, `update_event`, `delete_event`) are
//! flagged `irreversible`, so a node granted one of them after a
//! `human_review` gate keeps a person at that gate, and `auto_decide` there
//! is refused. The facts come from the manifest apb ships, read through
//! `ConnectorFacts::of_manifest`, so a manifest that drops the flag (or
//! declares `update_event` repeatable) fails here.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::connector::ConnectorDoc;
use apb_core::connector::idempotency::IdempotencyMode;
use apb_core::connector::resolve::ConnectorFacts;
use apb_core::schema::Playbook;
use apb_core::validate::{Severity, ValidationContext, validate};

const CONNECTOR: &str = "google-calendar";

/// The checked-in manifest, parsed as the official set parses it.
fn calendar_doc() -> ConnectorDoc {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../connectors/google-calendar/connector.yaml");
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    ConnectorDoc::from_yaml(&yaml, CONNECTOR).expect("the google-calendar manifest loads")
}

fn calendar_facts() -> ConnectorFacts {
    ConnectorFacts::of_manifest(&calendar_doc())
}

fn with_calendar() -> ValidationContext {
    ValidationContext {
        connectors: BTreeMap::from([(CONNECTOR.to_string(), calendar_facts())]),
        ..ValidationContext::default()
    }
}

/// `start -> draft -> gate -> schedule -> done`: `gate` a review with
/// `auto_decide`, `schedule` an agent step granted `functions` of the
/// calendar (`None`: the binding names no `functions`, which grants all of
/// them). No node is named like a merge, push, deploy or publish step, so
/// only the connector facts can refuse the automatic decision.
fn playbook(functions: Option<&str>) -> String {
    let grant = functions
        .map(|f| format!(", functions: {f}"))
        .unwrap_or_default();
    format!(
        "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: draft, type: agent_task, prompt: x, profile: m }}\n  - {{ id: gate, type: human_review, options: [approve, needs_changes], auto_decide: {{ allow: [needs_changes] }} }}\n  - {{ id: schedule, type: agent_task, prompt: y, profile: m, connectors: [{{ name: {CONNECTOR}{grant}, untrusted: true }}] }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: draft }}\n  - {{ from: draft, to: gate }}\n  - {{ from: gate, to: schedule }}\n  - {{ from: schedule, to: done }}\n"
    )
}

/// The V73 issues of `yaml` under `ctx`.
fn v73(yaml: &str, ctx: &ValidationContext) -> Vec<(Severity, String)> {
    let p = Playbook::from_yaml(yaml).unwrap();
    validate(&p, ctx)
        .issues
        .into_iter()
        .filter(|i| i.code == "V73")
        .map(|i| (i.severity, i.message))
        .collect()
}

#[test]
fn the_calendar_writes_are_irreversible_and_update_is_not_repeatable() {
    let facts = calendar_facts();
    let writes = ["create_event", "update_event", "delete_event"];
    assert_eq!(facts.irreversible_functions, writes, "{facts:?}");
    assert_eq!(facts.write_functions, writes, "{facts:?}");
    assert_eq!(
        facts.idempotency.get("create_event"),
        Some(&IdempotencyMode::Key)
    );
    // A repeat of an update can mail every guest again, so it is tracked
    // and an unknown outcome waits for a person: never `repeatable`.
    assert_eq!(
        facts.idempotency.get("update_event"),
        Some(&IdempotencyMode::None)
    );
    assert_eq!(
        facts.idempotency.get("delete_event"),
        Some(&IdempotencyMode::None)
    );
    assert!(facts.repeatable_functions.is_empty(), "{facts:?}");
}

#[test]
fn a_create_event_grant_after_an_auto_decide_gate_gets_v73() {
    let got = v73(&playbook(Some("[create_event]")), &with_calendar());
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, Severity::Error, "{got:?}");
    assert!(
        got[0].1.contains(
            "`schedule` after the gate may call irreversible google-calendar function(s) `create_event`"
        ),
        "{}",
        got[0].1
    );
}

#[test]
fn an_update_event_grant_after_an_auto_decide_gate_gets_v73() {
    let got = v73(&playbook(Some("[update_event]")), &with_calendar());
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, Severity::Error, "{got:?}");
    assert!(
        got[0].1.contains(
            "`schedule` after the gate may call irreversible google-calendar function(s) `update_event`"
        ),
        "{}",
        got[0].1
    );
}

#[test]
fn every_calendar_write_counts_and_reads_do_not() {
    // A binding without `functions` reaches the three writes, named in
    // manifest order.
    let got = v73(&playbook(None), &with_calendar());
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, Severity::Error);
    assert!(
        got[0]
            .1
            .contains("`create_event`, `update_event`, `delete_event`"),
        "{}",
        got[0].1
    );
    let got = v73(&playbook(Some("[delete_event]")), &with_calendar());
    assert_eq!(got.first().map(|g| g.0), Some(Severity::Error), "{got:?}");
    // Reads, and a `read_only` grant, reach no write: the gate may decide.
    for reads in [
        "read_only",
        "[verify, list_calendars, list_events, get_event, query_free_busy]",
    ] {
        assert!(
            v73(&playbook(Some(reads)), &with_calendar()).is_empty(),
            "{reads}"
        );
    }
    // Without the installed connector's facts nothing is known.
    assert!(
        v73(
            &playbook(Some("[create_event]")),
            &ValidationContext::default()
        )
        .is_empty()
    );
    // The explicit override keeps the gate automatic.
    let overridden = playbook(Some("[create_event]")).replace(
        "allow: [needs_changes] }",
        "allow: [needs_changes] }, auto_decide_ok: true",
    );
    assert_ne!(overridden, playbook(Some("[create_event]")));
    assert!(v73(&overridden, &with_calendar()).is_empty());
}
