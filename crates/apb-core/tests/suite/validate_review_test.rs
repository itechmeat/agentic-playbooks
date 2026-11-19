//! The routes out of a `human_review` gate: V144 warns on a `review_status`
//! edge no decision can match (a value the review does not offer, or a
//! source that is not a review), V145 on an option no edge carries. Both
//! are warnings: the playbook stays valid and runnable on the options it
//! covers.

use apb_core::schema::Playbook;
use apb_core::validate::{Issue, Severity, ValidationContext, validate};

/// A script `prep`, then a review `gate` with `options` and the edges out
/// of it (`prep_edges` replaces the plain edge from `prep` to `gate`).
fn playbook_with(options: &str, prep_edges: &str, edges: &str) -> Playbook {
    Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: prep, type: script, script: prep.sh, runner: sh, expected_duration: 1m }}\n  - {{ id: gate, type: human_review{options} }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: fix, type: finish, outcome: failure }}\n  - {{ id: stop, type: finish, outcome: failure }}\nedges:\n  - {{ from: start, to: prep }}\n{prep_edges}{edges}"
    ))
    .unwrap()
}

fn playbook(options: &str, edges: &str) -> Playbook {
    // `fix` is reached from `prep` too, so a review may leave it out.
    playbook_with(
        options,
        "  - { from: prep, to: gate }\n  - { from: prep, to: fix }\n",
        edges,
    )
}

/// `prep` reaches every node but `done` itself, so a review may route to
/// one finish only.
const FAN_OUT: &str =
    "  - { from: prep, to: gate }\n  - { from: prep, to: fix }\n  - { from: prep, to: stop }\n";

fn on(option: &str, to: &str) -> String {
    format!(
        "  - {{ from: gate, to: {to}, condition: {{ type: review_status, equals: {option} }} }}\n"
    )
}

fn issues(p: &Playbook, code: &str) -> Vec<Issue> {
    let report = validate(p, &ValidationContext::default());
    assert!(report.is_valid(), "{:?}", report.issues);
    report
        .issues
        .into_iter()
        .filter(|i| i.code == code)
        .collect()
}

#[test]
fn v144_warns_on_a_review_status_value_the_review_does_not_offer() {
    let three = ", options: [approve, needs_changes, reject]";
    let covered = format!(
        "{}{}{}",
        on("approve", "done"),
        on("needs_changes", "fix"),
        on("reject", "stop")
    );
    assert!(issues(&playbook(three, &covered), "V144").is_empty());
    // A typo of an option.
    let typo = covered.replace("equals: needs_changes", "equals: needs_change");
    let got = issues(&playbook(three, &typo), "V144");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].severity, Severity::Warning);
    assert_eq!(got[0].node.as_deref(), Some("gate"));
    assert_eq!(got[0].edges, ["e:gate>fix#1"]);
    assert!(
        got[0].message.contains("`needs_change`"),
        "{}",
        got[0].message
    );
    assert!(got[0].message.contains("approve, needs_changes, reject"));
    // Without `options` the review offers approve and reject only.
    let defaults = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    assert!(issues(&playbook("", &defaults), "V144").is_empty());
    let got = issues(
        &playbook("", &format!("{defaults}{}", on("needs_changes", "fix"))),
        "V144",
    );
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("approve, reject"),
        "{}",
        got[0].message
    );
}

#[test]
fn v144_warns_on_a_review_status_edge_from_a_node_without_a_decision() {
    let prep = "  - { from: prep, to: fix, condition: { type: review_status, equals: approve } }\n  - { from: prep, to: gate, fallback: true }\n";
    let edges = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    let got = issues(&playbook_with("", prep, &edges), "V144");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].node.as_deref(), Some("prep"));
    assert_eq!(got[0].edges, ["e:prep>fix#1"]);
    assert!(got[0].message.contains("script node"), "{}", got[0].message);
}

#[test]
fn v145_warns_on_an_option_no_edge_carries() {
    let three = ", options: [approve, needs_changes, reject]";
    let two = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    let got = issues(&playbook(three, &two), "V145");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].severity, Severity::Warning);
    assert_eq!(got[0].node.as_deref(), Some("gate"));
    assert!(got[0].edges.is_empty());
    assert!(
        got[0].message.contains("`needs_changes`")
            && got[0].message.contains("review_status: needs_changes"),
        "{}",
        got[0].message
    );
    // Default options: `reject` uncovered.
    let got = issues(&playbook_with("", FAN_OUT, &on("approve", "done")), "V145");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].message.contains("`reject`"));
}

#[test]
fn v145_is_quiet_when_a_fallback_or_an_unconditional_edge_carries_the_option() {
    let three = ", options: [approve, needs_changes, reject]";
    let two = format!("{}{}", on("approve", "done"), on("reject", "stop"));
    let fallback = format!("{two}  - {{ from: gate, to: fix, fallback: true }}\n");
    assert!(issues(&playbook(three, &fallback), "V145").is_empty());
    let unconditional = "  - { from: gate, to: done }\n";
    assert!(issues(&playbook_with(three, FAN_OUT, unconditional), "V145").is_empty());
    // One covered option of three warns for the other two.
    let got = issues(
        &playbook_with(three, FAN_OUT, &on("approve", "done")),
        "V145",
    );
    assert_eq!(got.len(), 2, "{got:?}");
}

/// A publish node's consent is a decision too (`publish` or `skip`), so a
/// `review_status` edge on it can match; a value it does not offer cannot.
#[test]
fn v144_takes_a_publish_decision_and_names_a_value_it_does_not_offer() {
    let with = |skip: &str| {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: work, type: script, script: w.sh, runner: sh, expected_duration: 1m }}\n  - {{ id: push, type: publish, from: work, remote: origin, branch: apb/out }}\n  - {{ id: done, type: finish, outcome: success }}\n  - {{ id: held, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: work }}\n  - {{ from: work, to: push }}\n  - {{ from: push, to: done, condition: {{ type: review_status, equals: publish }} }}\n  - {{ from: push, to: held, condition: {{ type: review_status, equals: {skip} }} }}\n"
        ))
        .unwrap()
    };
    let got = issues(&with("skip"), "V144");
    assert!(got.is_empty(), "{got:?}");
    let got = issues(&with("skipped"), "V144");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("`skipped`") && got[0].message.contains("publish, skip"),
        "{}",
        got[0].message
    );
}

/// The options of a review are weighed in the order the playbook declares
/// them, so its warnings come out in that order too.
#[test]
fn v145_names_options_in_declaration_order() {
    let p = playbook_with(
        ", options: [reject, approve, hold]",
        FAN_OUT,
        &on("hold", "done"),
    );
    let got: Vec<String> = issues(&p, "V145").into_iter().map(|i| i.message).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(
        got[0].contains("`reject`") && got[1].contains("`approve`"),
        "{got:?}"
    );
}

/// The issues of one code, without a validity assert: the malformed-form
/// rules are errors, so the playbook they name is not valid.
fn codes(p: &Playbook, code: &str) -> Vec<Issue> {
    validate(p, &ValidationContext::default())
        .issues
        .into_iter()
        .filter(|i| i.code == code)
        .collect()
}

/// A `max_scope` with no limit set is refused (V243).
#[test]
fn v243_max_scope_needs_a_limit() {
    let p = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, max_scope: { withhold: [approve] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    let got = codes(&p, "V243");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].severity, Severity::Error);
    assert_eq!(got[0].node.as_deref(), Some("gate"));
    assert!(
        got[0].message.contains("max_scope sets no limit"),
        "{}",
        got[0].message
    );
}

/// A limit of 0 is refused: a limit is at least 1 (V243).
#[test]
fn v243_a_zero_limit() {
    let p = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, max_scope: { files: 0 } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    let got = codes(&p, "V243");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("max_scope.files is 0"),
        "{}",
        got[0].message
    );
}

/// A withheld option the gate does not offer is refused (V243).
#[test]
fn v243_withhold_names_an_offered_option() {
    let p = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], max_scope: { files: 5, withhold: [ship] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    let got = codes(&p, "V243");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0]
            .message
            .contains("max_scope.withhold entry 1 names an option the gate does not offer"),
        "{}",
        got[0].message
    );
}

/// A `max_scope` without `withhold` on a gate that offers no `approve`
/// is not refused for an option the author never wrote: it names what it
/// withholds instead (V243); with `withhold` given, it is clean.
#[test]
fn v243_the_default_withhold_is_judged_against_the_offered_options() {
    let yaml = |max: &str| {
        format!(
            "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: gate, type: human_review, options: [yes, no], max_scope: {max} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
        )
    };
    let p = Playbook::from_yaml(&yaml("{ files: 5 }")).unwrap();
    let got = codes(&p, "V243");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("max_scope withholds nothing")
            && !got[0].message.contains("which the gate does not offer"),
        "{}",
        got[0].message
    );
    let ok = Playbook::from_yaml(&yaml("{ files: 5, withhold: [yes] }")).unwrap();
    assert!(codes(&ok, "V243").is_empty());
}

/// Withholding every offered option is refused: one stays to decide with
/// (V243).
#[test]
fn v243_not_every_option_withheld() {
    let p = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], max_scope: { files: 5, withhold: [approve, reject] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    let got = codes(&p, "V243");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0]
            .message
            .contains("max_scope withholds every option of the gate"),
        "{}",
        got[0].message
    );
    // The default withhold takes `approve` from a gate that offers only
    // it: nothing would remain either.
    let only = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve], max_scope: { files: 1 } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    let got = codes(&only, "V243");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("withholds every option"),
        "{}",
        got[0].message
    );
    // The default withhold over the default options leaves `reject`.
    let default = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, max_scope: { files: 1 } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(codes(&default, "V243").is_empty());
    // A clean max_scope raises nothing.
    let ok = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], max_scope: { files: 5, withhold: [approve] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(codes(&ok, "V243").is_empty());
}

// --- typed fields on a review gate (people-in-the-loop D3) -------------

/// A gate with fields: `amount` (number), `currency` (enum), `remark`
/// (text), shown above its options.
fn fielded(extra: &str) -> Playbook {
    Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: extract, type: agent_task, prompt: extract, profile: main }}\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    fields:\n      - {{ name: amount, type: number, required: [approve] }}\n      - {{ name: currency, type: enum, options: [EUR, USD] }}\n      - {{ name: remark, type: text, max_len: 200 }}\n{extra}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: extract }}\n  - {{ from: extract, to: gate }}\n  - {{ from: gate, to: done, condition: {{ type: review_status, equals: approve }} }}\n  - {{ from: gate, to: done, condition: {{ type: review_status, equals: reject }} }}\n"
    ))
    .unwrap()
}

fn one_message(p: &Playbook, code: &str) -> String {
    let got = codes(p, code);
    assert_eq!(got.len(), 1, "{code}: {got:?}");
    got[0].message.clone()
}

/// Every V244 problem names its field: names, duplicates, the reserved
/// `decision`, enum shape, `max_len` misuse, a required option the gate
/// does not offer, and a literal default that does not coerce.
#[test]
fn v244_field_names_types_and_defaults() {
    // A name that is not `[a-z][a-z0-9_]*`.
    let bad_name = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: \"9lives\", type: text }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&bad_name, "V244") == "field 1: name must match [a-z][a-z0-9_]*",
        "{}",
        one_message(&bad_name, "V244")
    );

    // A name longer than FIELD_NAME_MAX (32).
    let long = "a".repeat(33);
    let long_name = Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: gate, type: human_review, fields: [{{ name: {long}, type: text }}] }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
    ))
    .unwrap();
    assert!(
        one_message(&long_name, "V244") == "field 1: name is longer than 32 characters",
        "{}",
        one_message(&long_name, "V244")
    );

    // A name used twice, and the reserved `decision`.
    let twice = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: amount, type: number }, { name: amount, type: text }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&twice, "V244").contains("field `amount`: named twice"),
        "{}",
        one_message(&twice, "V244")
    );
    let decision_key = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: decision, type: text }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&decision_key, "V244").contains(
            "field `decision`: `decision` is the key the gate's JSON output names the decision with"
        ),
        "{}",
        one_message(&decision_key, "V244")
    );

    // An enum without options, an enum with a duplicate option, options on
    // a non-enum field.
    let no_options = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: currency, type: enum }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&no_options, "V244").contains("field `currency`: an enum needs options"),
        "{}",
        one_message(&no_options, "V244")
    );
    let dup_options = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: currency, type: enum, options: [EUR, EUR] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&dup_options, "V244") == "field `currency`: option 2 repeats an earlier option",
        "{}",
        one_message(&dup_options, "V244")
    );
    let options_on_text = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: note, type: text, options: [a] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&options_on_text, "V244")
            .contains("field `note`: `options` belongs to an enum field"),
        "{}",
        one_message(&options_on_text, "V244")
    );

    // `max_len` on a non-text field, `max_len` 0, `max_len` above
    // TEXT_MAX_LEN.
    let max_on_number = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: amount, type: number, max_len: 5 }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&max_on_number, "V244")
            .contains("field `amount`: `max_len` belongs to a text field"),
        "{}",
        one_message(&max_on_number, "V244")
    );
    let max_zero = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: note, type: text, max_len: 0 }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&max_zero, "V244").contains("field `note`: `max_len` is 0"),
        "{}",
        one_message(&max_zero, "V244")
    );
    let max_big = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: note, type: text, max_len: 20000 }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&max_big, "V244").contains("field `note`: `max_len` is above 10000"),
        "{}",
        one_message(&max_big, "V244")
    );

    // `required` naming an option the gate does not offer.
    let required_unknown = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve], fields: [{ name: amount, type: number, required: [reject] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&required_unknown, "V244")
            .contains("field `amount`: required entry 1 names an option the gate does not offer"),
        "{}",
        one_message(&required_unknown, "V244")
    );

    // A literal default that does not coerce to the field's type.
    let bad_default = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: amount, type: number, default: ten }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(
        one_message(&bad_default, "V244") == "field `amount`: the literal default is not a number",
        "{}",
        one_message(&bad_default, "V244")
    );

    // More than MAX_FIELDS (20) fields.
    let fields: String = (0..21)
        .map(|i| format!("      - {{ name: f{i}, type: text }}\n"))
        .collect();
    let too_many = Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: gate\n    type: human_review\n    fields:\n{fields}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
    ))
    .unwrap();
    assert!(
        one_message(&too_many, "V244").contains("takes at most 20 fields"),
        "{}",
        one_message(&too_many, "V244")
    );

    // A clean fielded gate raises nothing.
    assert!(codes(&fielded(""), "V244").is_empty());
    // A template default is not checked for coercion.
    let template_default = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, fields: [{ name: amount, type: number, default: \"{{nodes.start.output}}\" }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(codes(&template_default, "V244").is_empty());
}

/// V245: more than MAX_SHOW entries, an empty or over-long label, an
/// empty text.
#[test]
fn v245_show_entries() {
    let long_label = "a".repeat(61);
    let cases = [
        (
            "show:\n      - { label: \"\", text: x }\n",
            "show entry 1: label is empty",
        ),
        (
            &format!("show:\n      - {{ label: {long_label}, text: x }}\n"),
            "show entry 1: label is longer than 60 characters",
        ),
        (
            "show:\n      - { label: L, text: \"\" }\n",
            "show entry 1: text is empty",
        ),
    ];
    for (block, want) in cases {
        let p = Playbook::from_yaml(&format!(
            "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    {block}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
        ))
        .unwrap();
        assert_eq!(one_message(&p, "V245"), want);
    }
    // More than MAX_SHOW (10) entries.
    let entries: String = (0..11)
        .map(|i| format!("      - {{ label: L{i}, text: x }}\n"))
        .collect();
    let p = Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    show:\n{entries}  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
    ))
    .unwrap();
    assert!(
        one_message(&p, "V245").contains("has at most 10 entries"),
        "{}",
        one_message(&p, "V245")
    );
    // A clean show block raises nothing, and its templates resolve.
    let ok = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    show:\n      - { label: Total, text: \"{{nodes.start.output}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(codes(&ok, "V245").is_empty());
    assert!(codes(&ok, "V13").is_empty());
}

/// V246: a field required for an option the engine may choose needs a
/// default on a gate with `auto_decide` or `unattended: { decide }`.
#[test]
fn v246_a_required_field_without_default_on_an_engine_decided_gate() {
    let auto = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], auto_decide: { allow: [approve] }, fields: [{ name: amount, type: number, required: [approve] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    let got = codes(&auto, "V246");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].message.contains("field `amount` is required and has no default, but the gate may be decided without a person (auto_decide or unattended); give it a default or make it optional"),
        "{}",
        got[0].message
    );
    let unattended = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], unattended: { decide: approve }, fields: [{ name: amount, type: number, required: [approve] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert_eq!(codes(&unattended, "V246").len(), 1);
    // With a default, or required only for the option the engine does not
    // choose, no V246.
    let ok = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], unattended: { decide: approve }, fields: [{ name: amount, type: number, required: [reject] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert!(codes(&ok, "V246").is_empty());
    // A blank literal default is no value for a required field (the
    // channel refuses it), so it is no default here either.
    let blank = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject], unattended: { decide: approve }, fields: [{ name: note, type: text, required: [approve], default: \"  \" }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n",
    )
    .unwrap();
    assert_eq!(codes(&blank, "V246").len(), 1);
}

/// V247 (warning): a field default that renders untrusted text names its
/// sources.
#[test]
fn v247_an_untrusted_default() {
    let p = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\ndefaults: { profile: main }\nparams:\n  - { name: body, type: text, untrusted: true }\nnodes:\n  - { id: start, type: start }\n  - { id: fetch, type: agent_task, prompt: fetch, profile: main, connectors: [{ name: gh, untrusted: true }] }\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    fields:\n      - { name: note, type: text, default: \"{{nodes.fetch.output}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: fetch }\n  - { from: fetch, to: gate }\n  - { from: gate, to: done, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: done, condition: { type: review_status, equals: reject } }\n",
    )
    .unwrap();
    let got = codes(&p, "V247");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].severity, Severity::Warning);
    assert!(
        got[0]
            .message
            .starts_with("field `note` defaults to untrusted text from "),
        "{}",
        got[0].message
    );
    assert!(
        got[0]
            .message
            .ends_with("; a person who keeps the default passes it on as their own value"),
        "{}",
        got[0].message
    );
    // A clean default raises nothing.
    let clean = Playbook::from_yaml(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    fields:\n      - { name: note, type: text, default: \"{{nodes.start.output}}\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: done, condition: { type: review_status, equals: reject } }\n",
    )
    .unwrap();
    assert!(codes(&clean, "V247").is_empty());
}

/// No V243 to V247 message repeats definition text an author may have put
/// a secret in: a literal default, an option, a withheld or required option
/// name, or a field name outside the field-name grammar. Each names the
/// node and the field by its name (in the grammar) or its position.
#[test]
fn review_form_messages_repeat_no_definition_text() {
    let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
    let p = Playbook::from_yaml(&format!(
        "schema: 2\nid: r\nname: R\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: gate\n    type: human_review\n    options: [approve, reject]\n    auto_decide: {{ allow: [approve] }}\n    max_scope: {{ files: 1, withhold: [\"{token}\"] }}\n    fields:\n      - {{ name: \"{token}\", type: text, required: [approve] }}\n      - {{ name: \"{token}\", type: text }}\n      - {{ name: amount, type: number, default: \"{token}\" }}\n      - {{ name: pick, type: enum, options: [\"{token}\", \"{token}\"], default: \"x{token}\" }}\n      - {{ name: flag, type: bool, required: [\"{token}\"], default: \"{token}\" }}\n      - {{ name: note, type: text, max_len: 3, default: \"{token}\" }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
    ))
    .unwrap();
    let report = validate(&p, &ValidationContext::default());
    let ours: Vec<&Issue> = report
        .issues
        .iter()
        .filter(|i| ["V243", "V244", "V245", "V246", "V247"].contains(&i.code))
        .collect();
    for code in ["V243", "V244", "V246"] {
        assert!(ours.iter().any(|i| i.code == code), "{code}: {ours:?}");
    }
    let lower = token.to_lowercase();
    for issue in &ours {
        assert_eq!(issue.node.as_deref(), Some("gate"), "{issue:?}");
        assert!(
            !issue.message.contains(token) && !issue.message.contains(&lower),
            "{}: {}",
            issue.code,
            issue.message
        );
    }
    let messages: Vec<&str> = ours.iter().map(|i| i.message.as_str()).collect();
    for want in [
        "max_scope.withhold entry 1 names an option the gate does not offer",
        "field 1: name must match [a-z][a-z0-9_]*",
        "field 1: name is longer than 32 characters",
        "field 2: named twice",
        "field `amount`: the literal default is not a number",
        "field `pick`: option 2 repeats an earlier option",
        "field `pick`: the literal default is not one of the field's options",
        "field `flag`: required entry 1 names an option the gate does not offer",
        "field `flag`: the literal default is not true or false",
        "field `note`: the literal default does not fit the field's length",
        "field 1 is required and has no default, but the gate may be decided without a person (auto_decide or unattended); give it a default or make it optional",
    ] {
        assert!(messages.contains(&want), "{want}: {messages:#?}");
    }
}
