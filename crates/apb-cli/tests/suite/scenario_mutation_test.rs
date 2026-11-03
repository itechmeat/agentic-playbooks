//! A3: run-only mistakes in the fix-loop fixture are caught by its quick
//! scenarios, each by a check with its own name, while an edit that changes
//! nothing the run does passes all three; review route mistakes are the
//! validator's warnings V144 and V145.

use crate::scenario_fixture::{Project, failed_checks};

/// One mistake: the fixture text it replaces, and the scenario and check
/// that must catch it.
struct Mutation {
    name: &'static str,
    from: &'static str,
    to: &'static str,
    scenario: &'static str,
    check: &'static str,
}

const MUTATIONS: &[Mutation] = &[
    Mutation {
        name: "the loop runs on a green verdict",
        from: "field: verdict, equals: red }",
        to: "field: verdict, equals: green }",
        scenario: "green",
        check: "visits[work]",
    },
    Mutation {
        name: "the agent gets no retry",
        from: "max_retries: 1, retry_delay",
        to: "max_retries: 0, retry_delay",
        scenario: "green",
        check: "run.outcome",
    },
    Mutation {
        name: "the approval routes on the wrong decision",
        from: "{ type: review_status, equals: approve }",
        to: "{ type: review_status, equals: reject }",
        scenario: "green",
        check: "finish",
    },
    Mutation {
        name: "the lint routes are reversed",
        from: "{ type: node_status, node: lint, equals: success } }\n  - { from: lint, to: lintfix, condition: { type: node_status, node: lint, equals: failure } }",
        to: "{ type: node_status, node: lint, equals: failure } }\n  - { from: lint, to: lintfix, condition: { type: node_status, node: lint, equals: success } }",
        scenario: "green",
        check: "joins.dead[summary]",
    },
    Mutation {
        name: "the summary goes on with the first input",
        from: "{ from: docs, to: summary }",
        to: "{ from: docs, to: summary, join: any }",
        scenario: "green",
        check: "visits[lintfix]",
    },
    Mutation {
        name: "the summary reads a field the child never writes",
        from: "prompt: \"Summarize {{nodes.work.output}}\"",
        to: "prompt: \"Summarize {{nodes.docs.output.pr}}\"",
        scenario: "green",
        check: "templates_empty",
    },
    Mutation {
        name: "the judge reads a node that has not run yet",
        from: "state: { out: \"{{nodes.gate.output}}\" }",
        to: "state: { out: \"{{nodes.lint.output}}\" }",
        scenario: "reject",
        check: "templates_empty",
    },
];

const SCENARIOS: [&str; 3] = ["green", "reject", "red"];

#[test]
fn each_run_only_mistake_fails_a_named_check() {
    let p = Project::new(&["fixloop", "kid"]);
    for s in SCENARIOS {
        let (code, r) = p.play("fixloop", "quick", s, &[]);
        assert_eq!(code, 0, "unmutated {s}: {:?}", failed_checks(&r));
    }
    for m in MUTATIONS {
        let p = Project::new(&["fixloop", "kid"]);
        p.edit_playbook("fixloop", m.from, m.to);
        let (code, r) = p.play("fixloop", "quick", m.scenario, &[]);
        let failed = failed_checks(&r);
        assert!(
            code != 0 && failed.iter().any(|c| c == m.check),
            "{}: {} exit {code} failed {failed:?}, expected {}",
            m.name,
            m.scenario,
            m.check
        );
    }
}

/// An edit that changes nothing a run does passes every scenario: the
/// checks are about the run, not the text.
#[test]
fn an_edit_that_changes_no_run_passes_every_scenario() {
    let p = Project::new(&["fixloop", "kid"]);
    p.edit_playbook(
        "fixloop",
        "description: An agent fixes",
        "description: An agent repairs",
    );
    p.edit_playbook(
        "fixloop",
        "equals: red }, max_traversals: 2",
        "equals: red }, max_traversals: 1",
    );
    for s in SCENARIOS {
        let (code, r) = p.play("fixloop", "quick", s, &[]);
        assert_eq!(code, 0, "{s}: {:?}", failed_checks(&r));
    }
}

/// Review route mistakes are caught before any run: a `review_status`
/// value that is not an option is V144, an option no edge carries is V145.
#[test]
fn review_route_mistakes_are_validator_warnings() {
    for (from, to, code) in [
        (
            "{ type: review_status, equals: approve }",
            "{ type: review_status, equals: approved }",
            "V144",
        ),
        (
            "  - { id: rejected, from: approve, to: failed, fallback: true }\n",
            "",
            "V145",
        ),
    ] {
        let p = Project::new(&["fixloop", "kid"]);
        p.edit_playbook("fixloop", from, to);
        let out = p.apb(&["validate", "fixloop"]);
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(
            text.lines().any(|l| l.contains(&format!("warning {code}"))),
            "{code} not reported:\n{text}"
        );
    }
}
