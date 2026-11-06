//! Typed output fields: V167 and V168 (spec node-contracts D1).
//!
//! V167 (error) refuses a malformed typed entry of `outputs.fields` or
//! `list.item_outputs`. V168 (warning) names a reader that cannot read a
//! typed field the way it reads it: an `output_field` `equals` against a
//! `bool`, a `number`, an `enum`, an `array` or an `object` field, a list's
//! `items.from` field that is not an `array`, and a `no_new_items` field
//! that is not an `array`, a threshold on a field typed other than a
//! `number` (D2), and a `writes` template reading a field typed other than
//! an `array` or a `string` (D3).

use super::*;

/// The typed declaration of `field` on `node`, when its `outputs.fields`
/// entry is typed.
fn typed_field<'a>(node: &'a Node, field: &str) -> Option<&'a TypedFieldDecl> {
    node.outputs
        .as_ref()?
        .fields
        .iter()
        .find(|f| f.name() == field)
        .and_then(|f| f.typed())
}

pub(crate) fn check_output_types(playbook: &Playbook, r: &mut ValidationReport) {
    // V167: the declarations.
    for n in &playbook.nodes {
        if let Some(files) = n.outputs.as_ref() {
            check_decls(&files.fields, "outputs.fields", &n.id, r);
        }
        if let Some(list) = n.list() {
            check_decls(&list.item_outputs, "item_outputs", &n.id, r);
        }
    }
    // V168: the readers that exist now.
    check_readers(playbook, r);
}

/// V167: every rule a typed entry can break. A declaration whose entries are
/// all plain names is never checked.
fn check_decls(decls: &[FieldDecl], where_: &str, node_id: &str, r: &mut ValidationReport) {
    if !decls.iter().any(|f| f.typed().is_some()) {
        return;
    }
    let id = Some(node_id);
    for f in decls {
        let Some(t) = f.typed() else { continue };
        if t.name.is_empty()
            || t.name.chars().any(|c| c.is_whitespace())
            || t.name.contains(['.', '{', '}'])
        {
            r.error(
                "V167",
                id,
                format!(
                    "{where_}: a typed entry name must not be empty or hold `.`, `{{`, `}}` or whitespace: `{}`",
                    t.name
                ),
            );
        }
        match t.kind {
            Some(FieldType::Enum) => {
                if t.values.is_empty() {
                    r.error(
                        "V167",
                        id,
                        format!("{where_}: enum entry `{}` declares no values", t.name),
                    );
                }
                if t.values.iter().any(|v| v.is_empty()) {
                    r.error(
                        "V167",
                        id,
                        format!("{where_}: enum entry `{}` holds an empty value", t.name),
                    );
                }
                let mut seen: HashSet<&str> = HashSet::new();
                for v in &t.values {
                    if !seen.insert(v) {
                        r.error(
                            "V167",
                            id,
                            format!("{where_}: enum entry `{}` repeats the value `{v}`", t.name),
                        );
                    }
                }
            }
            _ => {
                if !t.values.is_empty() {
                    r.error(
                        "V167",
                        id,
                        format!(
                            "{where_}: entry `{}` declares `values` without `type: enum`",
                            t.name
                        ),
                    );
                }
            }
        }
    }
    // A name declared twice, where one of the two entries is typed, is
    // refused; a duplicate among plain names only stays allowed.
    let mut reported: HashSet<&str> = HashSet::new();
    for f in decls {
        let name = f.name();
        if reported.insert(name) {
            let count = decls.iter().filter(|g| g.name() == name).count();
            if count > 1
                && decls
                    .iter()
                    .any(|g| g.name() == name && g.typed().is_some())
            {
                r.error(
                    "V167",
                    id,
                    format!(
                        "{where_}: name `{name}` is declared twice, and one of the entries is typed"
                    ),
                );
            }
        }
    }
}

/// V168 for the readers that exist now: an `output_field` `equals`, a list's
/// `items.from` and a `no_new_items` condition.
fn check_readers(playbook: &Playbook, r: &mut ValidationReport) {
    for e in &playbook.edges {
        let Some(cond) = &e.condition else { continue };
        match cond {
            EdgeCondition::OutputField {
                node,
                field,
                equals,
                ..
            } => {
                let Some(src) = playbook.node(node) else {
                    continue;
                };
                let Some(t) = typed_field(src, field) else {
                    continue;
                };
                if let Some(equals) = equals {
                    let bad = match t.kind {
                        Some(FieldType::Bool) => equals != "true" && equals != "false",
                        Some(FieldType::Number) => crate::output_contract::numeric(
                            &serde_json::Value::String(equals.clone()),
                        )
                        .is_none(),
                        Some(FieldType::Enum) => !t.values.iter().any(|v| v == equals),
                        Some(FieldType::Array) | Some(FieldType::Object) => true,
                        _ => false,
                    };
                    if bad {
                        r.warn(
                            "V168",
                            Some(&e.from),
                            format!(
                                "edge from `{}` reads typed field `{}.{}` with `equals: {equals}`, which its type cannot match",
                                e.from, node, field
                            ),
                        );
                    }
                } else {
                    // A threshold (V169 guarantees exactly one), on a
                    // field typed other than a number (an entry with only
                    // `required` declares no type).
                    if t.kind.is_some_and(|k| k != FieldType::Number) {
                        r.warn(
                            "V168",
                            Some(&e.from),
                            format!(
                                "edge from `{}` reads typed field `{}.{}` with a threshold, which is declared other than a number",
                                e.from, node, field
                            ),
                        );
                    }
                }
            }
            EdgeCondition::NoNewItems { node, field, .. } => {
                let Some(src) = playbook.node(node) else {
                    continue;
                };
                if let Some(t) = typed_field(src, field)
                    && t.kind != Some(FieldType::Array)
                {
                    r.warn(
                        "V168",
                        Some(&e.from),
                        format!(
                            "edge from `{}` reads typed field `{}.{}` with `no_new_items`, which is declared other than an array",
                            e.from, node, field
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    // A `writes` template reading a field typed other than `array` or
    // `string`: a number, a bool or an object never renders a scope.
    for n in &playbook.nodes {
        let Some(WriteScope::Template(text)) = &n.writes else {
            continue;
        };
        let Some((source, field)) = crate::write_scope::template_ref(text)
            .and_then(|inner| inner.strip_prefix("nodes."))
            .and_then(|rest| rest.split_once(".output."))
        else {
            continue;
        };
        let Some(t) = playbook
            .node(source)
            .and_then(|src| typed_field(src, field))
        else {
            continue;
        };
        if t.kind
            .is_some_and(|k| !matches!(k, FieldType::Array | FieldType::String))
        {
            r.warn(
                "V168",
                Some(&n.id),
                format!(
                    "`writes` of `{}` reads typed field `{source}.{field}`, which is declared other than an array or a string",
                    n.id
                ),
            );
        }
    }
    // A list's `items.from` field typed other than `array`.
    for n in &playbook.nodes {
        let Some(list) = n.list() else { continue };
        let Some(from) = &list.items.from else {
            continue;
        };
        let Some(src) = playbook.node(&from.node) else {
            continue;
        };
        if let Some(t) = typed_field(src, &from.field)
            && t.kind != Some(FieldType::Array)
        {
            r.warn(
                "V168",
                Some(&n.id),
                format!(
                    "list of `{}` takes its items from typed field `{}.{}`, which is declared other than an array",
                    n.id, from.node, from.field
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODES: &str = r#"
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
  - { id: bad, type: finish, outcome: failure }
"#;
    const EDGES: &str = r#"
edges:
  - { from: s, to: a }
  - { from: a, to: done, condition: { type: node_status, node: a, equals: success } }
  - { from: a, to: bad, condition: { type: node_status, node: a, equals: failure } }
"#;

    fn pb(nodes_extra: &str, edges_extra: &str) -> Playbook {
        let yaml = format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: {{ profile: x }}\nnodes:\n{NODES}{nodes_extra}{EDGES}{edges_extra}\n"
        );
        Playbook::from_yaml(&yaml).unwrap()
    }

    fn found(p: &Playbook, code: &str) -> Vec<Issue> {
        validate(p, &ValidationContext::default())
            .issues
            .into_iter()
            .filter(|i| i.code == code)
            .collect()
    }

    /// One issue of `code` exists, and it is an error (V167).
    fn one_error(p: &Playbook, code: &str) {
        let all = found(p, code);
        assert_eq!(all.len(), 1, "{code}: {all:?}");
        assert_eq!(all[0].severity, Severity::Error);
    }

    /// One issue of `code` exists, and it is a warning (V168).
    fn one_warning(p: &Playbook, code: &str) {
        let all = found(p, code);
        assert_eq!(all.len(), 1, "{code}: {all:?}");
        assert_eq!(all[0].severity, Severity::Warning);
    }

    fn agent_with_fields(fields: &str) -> String {
        format!("  - {{ id: a2, type: agent_task, prompt: hi, outputs: {{ fields: {fields} }} }}\n")
    }

    fn output_field_edge(equals: &str) -> String {
        format!(
            "  - {{ from: a, to: bad, condition: {{ type: output_field, node: a2, field: f, equals: {equals} }} }}\n"
        )
    }

    // --- V167: the declarations ----------------------------------------------

    #[test]
    fn v167_refuses_an_empty_name() {
        let p = pb(&agent_with_fields("[{ name: \"\", type: number }]"), "");
        one_error(&p, "V167");
    }

    #[test]
    fn v167_refuses_a_name_holding_a_dot_a_brace_or_whitespace() {
        for name in ["a.b", "a b", "a{b"] {
            let fields = format!("[{{ name: \"{name}\", type: number }}]");
            let p = pb(&agent_with_fields(&fields), "");
            one_error(&p, "V167");
        }
    }

    #[test]
    fn v167_refuses_a_name_declared_twice_where_one_entry_is_typed() {
        let p = pb(&agent_with_fields("[x, { name: x, type: number }]"), "");
        one_error(&p, "V167");
    }

    #[test]
    fn v167_allows_a_duplicate_among_plain_names_only() {
        let p = pb(&agent_with_fields("[x, x]"), "");
        assert!(found(&p, "V167").is_empty());
    }

    #[test]
    fn v167_refuses_values_without_enum() {
        let p = pb(
            &agent_with_fields("[{ name: r, type: string, values: [a] }]"),
            "",
        );
        one_error(&p, "V167");
    }

    #[test]
    fn v167_refuses_an_enum_without_values() {
        let p = pb(&agent_with_fields("[{ name: r, type: enum }]"), "");
        one_error(&p, "V167");
    }

    #[test]
    fn v167_refuses_an_enum_with_an_empty_or_repeated_value() {
        let p = pb(
            &agent_with_fields("[{ name: r, type: enum, values: [a, \"\"] }]"),
            "",
        );
        one_error(&p, "V167");
        let p = pb(
            &agent_with_fields("[{ name: r, type: enum, values: [a, a] }]"),
            "",
        );
        one_error(&p, "V167");
    }

    #[test]
    fn v167_checks_item_outputs_the_same_way() {
        let node = "  - { id: l, type: script, script: s, runner: sh, list: { items: { inline: [{}] }, item_outputs: [{ name: \"\", type: number }] } }\n";
        let p = pb(node, "");
        one_error(&p, "V167");
    }

    #[test]
    fn v167_stays_quiet_without_a_typed_entry() {
        let p = pb(&agent_with_fields("[a, b]"), "");
        assert!(found(&p, "V167").is_empty());
    }

    // --- V168: the readers ----------------------------------------------------

    #[test]
    fn v168_warns_on_equals_against_a_bool_number_enum_array_or_object_field() {
        let cases: [(&str, &str); 5] = [
            ("{ name: f, type: bool, required: true }", "maybe"),
            ("{ name: f, type: number, required: true }", "high"),
            (
                "{ name: f, type: enum, values: [a, b], required: true }",
                "c",
            ),
            ("{ name: f, type: array, required: true }", "x"),
            ("{ name: f, type: object, required: true }", "x"),
        ];
        for (decl, equals) in cases {
            let fields = format!("[{decl}]");
            let p = pb(&agent_with_fields(&fields), &output_field_edge(equals));
            one_warning(&p, "V168");
        }
    }

    #[test]
    fn v168_stays_quiet_on_a_readable_equals() {
        let cases: [(&str, &str); 4] = [
            ("{ name: f, type: number, required: true }", "\"80\""),
            ("{ name: f, type: bool, required: true }", "\"true\""),
            ("{ name: f, type: enum, values: [a], required: true }", "a"),
            ("f", "x"),
        ];
        for (decl, equals) in cases {
            let fields = format!("[{decl}]");
            let p = pb(&agent_with_fields(&fields), &output_field_edge(equals));
            assert!(found(&p, "V168").is_empty(), "{decl} {equals}");
        }
    }

    #[test]
    fn v168_warns_when_items_from_reads_a_non_array() {
        let node = "  - { id: l, type: script, script: s, runner: sh, list: { items: { from: { node: a2, field: f } } } }\n";
        let p = pb(
            &(agent_with_fields("[{ name: f, type: string, required: true }]") + node),
            "",
        );
        one_warning(&p, "V168");
    }

    #[test]
    fn v168_stays_quiet_when_items_from_reads_an_array() {
        let node = "  - { id: l, type: script, script: s, runner: sh, list: { items: { from: { node: a2, field: f } } } }\n";
        let p = pb(
            &(agent_with_fields("[{ name: f, type: array, required: true }]") + node),
            "",
        );
        assert!(found(&p, "V168").is_empty());
    }

    #[test]
    fn v168_warns_on_a_threshold_against_a_field_typed_other_than_number() {
        for decl in [
            "{ name: f, type: string, required: true }",
            "{ name: f, type: bool, required: true }",
            "{ name: f, type: array, required: true }",
            "{ name: f, type: enum, values: [a], required: true }",
        ] {
            let fields = format!("[{decl}]");
            let edge = "  - { from: a, to: bad, condition: { type: output_field, node: a2, field: f, lt: 80 } }\n";
            let p = pb(&agent_with_fields(&fields), edge);
            one_warning(&p, "V168");
        }
    }

    #[test]
    fn v168_stays_quiet_on_a_threshold_against_a_number_field_or_an_untyped_one() {
        for decl in [
            "{ name: f, type: number, required: true }",
            "f",
            // Required, but declares no type.
            "{ name: f, required: true }",
        ] {
            let fields = format!("[{decl}]");
            let edge = "  - { from: a, to: bad, condition: { type: output_field, node: a2, field: f, gte: 80 } }\n";
            let p = pb(&agent_with_fields(&fields), edge);
            assert!(found(&p, "V168").is_empty(), "{decl}");
        }
    }

    #[test]
    fn v168_warns_when_no_new_items_reads_a_non_array() {
        let node = "  - { id: hunt, type: agent_task, prompt: p }\n";
        let edge = "  - { from: hunt, to: done, condition: { type: no_new_items, node: a2, field: f, rounds: 2 } }\n";
        let p = pb(
            &(agent_with_fields("[{ name: f, type: string, required: true }]") + node),
            edge,
        );
        one_warning(&p, "V168");
    }

    /// A `writes` template reads a declared field of an earlier node: a
    /// field typed other than an array or a string warns; an array, a
    /// string or an untyped field does not.
    #[test]
    fn v168_checks_the_field_a_writes_template_reads() {
        let writer = "  - { id: w, type: script, script: s, runner: sh, writes: \"{{nodes.a2.output.f}}\" }\n";
        for (decl, warns) in [
            ("{ name: f, type: number }", true),
            ("{ name: f, type: object }", true),
            ("{ name: f, type: array }", false),
            ("{ name: f, type: string }", false),
            ("f", false),
        ] {
            let p = pb(&(agent_with_fields(&format!("[{decl}]")) + writer), "");
            assert_eq!(found(&p, "V168").len(), usize::from(warns), "{decl}");
        }
    }
}
