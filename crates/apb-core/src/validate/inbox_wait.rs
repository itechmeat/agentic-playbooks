//! The inbox wait rules (inbox-waits design D4 "Validator"): a malformed
//! `wait_for: { type: inbox }` block (V284), a connector that is not
//! installed or has no `webhook:` block (V285), a `types` filter a
//! connector that names no event type cannot ever meet (V286), and two
//! binding sites with one id (V287).

use crate::connector::event_filter::MAX_CONDITIONS;
use crate::inbox_wait::{MAX_TAKE, RESERVED_NAMES};
use crate::schema::{NodeKind, Playbook};

use super::*;

/// V284 to V286 for every `Wait` node whose `wait_for` is an inbox wait.
pub(super) fn check_inbox_waits(
    playbook: &Playbook,
    ctx: &ValidationContext,
    r: &mut ValidationReport,
) {
    for node in &playbook.nodes {
        let NodeKind::Wait {
            wait_for: crate::schema::WaitFor::Inbox(w),
            timeout_seconds,
            ..
        } = &node.kind
        else {
            continue;
        };
        let id = &node.id;
        let why = |what: String| format!("inbox wait of node `{id}`: {what}");

        // V284: the block's own shape. The connector and account names are
        // checked at the site by V23 and V25, and an absent
        // `timeout_seconds` never reaches the validator (a required field,
        // refused by the parser).
        if *timeout_seconds == 0 {
            r.error(
                "V284",
                Some(id),
                why("timeout_seconds is 0; use 1 or more".to_string()),
            );
        }
        // The wait's `types` and conditions hold the shape rules of the
        // inbox read's filter (`connector::event_filter`): the same caps
        // and the same type test, so a type a stored event can never
        // carry is refused here too.
        if let Err(msg) = crate::connector::event_filter::check_types(&w.types) {
            r.error("V284", Some(id), why(msg));
        }
        if w.conditions.len() > MAX_CONDITIONS {
            r.error(
                "V284",
                Some(id),
                why(format!(
                    "declares {} conditions, more than the {MAX_CONDITIONS} one wait keeps",
                    w.conditions.len()
                )),
            );
        }
        if let Err(msg) = crate::connector::event_filter::check_paths(
            w.conditions.iter().map(|c| c.path.as_str()),
        ) {
            r.error("V284", Some(id), why(msg));
        }
        if w.take.len() > MAX_TAKE {
            r.error(
                "V284",
                Some(id),
                why(format!(
                    "declares {} take entries, more than the {MAX_TAKE} one wait keeps",
                    w.take.len()
                )),
            );
        }
        let mut seen = std::collections::BTreeSet::new();
        for t in &w.take {
            if let Err(msg) = crate::connector::validate_snake_name(&t.name) {
                r.error(
                    "V284",
                    Some(id),
                    why(format!("take name `{}` is not a snake name: {msg}", t.name)),
                );
            }
            if RESERVED_NAMES.contains(&t.name.as_str()) {
                r.error(
                    "V284",
                    Some(id),
                    why(format!(
                        "take name `{}` is one of the keys apb sets itself ({})",
                        t.name,
                        RESERVED_NAMES.join(", ")
                    )),
                );
            }
            if !seen.insert(t.name.as_str()) {
                r.error(
                    "V284",
                    Some(id),
                    why(format!("take name `{}` repeats another entry", t.name)),
                );
            }
            match crate::baseline::extract::parse_path(&t.path) {
                Err(msg) => {
                    r.error(
                        "V284",
                        Some(id),
                        why(format!("take path `{}`: {msg}", t.path)),
                    );
                }
                Ok(segments) => {
                    if segments.iter().any(|s| s.map) {
                        r.error(
                            "V284",
                            Some(id),
                            why(format!(
                                "take path `{}` has a `[]` segment; a take reads one value",
                                t.path
                            )),
                        );
                    }
                }
            }
            if t.kind == Some(crate::baseline::ObservationType::List) {
                r.error(
                    "V284",
                    Some(id),
                    why(format!(
                        "take `{}` has type `list`; a take reads `field` or `number`",
                        t.name
                    )),
                );
            }
        }

        // V285: only when the caller has a connector store. Without one
        // the rule stays silent, as V42 does, and the run gate refuses the
        // start (`connector_unresolved`).
        if !ctx.connectors.is_empty() {
            match ctx.connectors.get(&w.connector) {
                None => r.error(
                    "V285",
                    Some(id),
                    why(format!(
                        "connector `{}` is not installed; its inbox can never receive anything",
                        w.connector
                    )),
                ),
                Some(facts) if !facts.has_webhook => {
                    let message = if facts.mcp_transport.is_some() {
                        format!(
                            "connector `{}` is an MCP connector and cannot declare `webhook:`; its inbox never receives anything",
                            w.connector
                        )
                    } else {
                        format!(
                            "connector `{}` has no `webhook:` block, so its inbox never receives anything",
                            w.connector
                        )
                    };
                    r.error("V285", Some(id), why(message));
                }
                _ => {}
            }
        }

        // V286: a `types` filter can only match when the connector's
        // webhook block names the event type.
        if !w.types.is_empty()
            && ctx
                .connectors
                .get(&w.connector)
                .is_some_and(|facts| !facts.names_event_type)
        {
            r.warn(
                "V286",
                Some(id),
                why(format!(
                    "sets `types` but connector `{}` names no event type; no stored event ever carries one",
                    w.connector
                )),
            );
        }
    }
}

/// V287 over the whole definition: a node id may not be a derived
/// binding site of another node (`a#inbox` beside the inbox wait `a`,
/// `a#check` beside the list node `a`), or `resolve_playbook`'s grant
/// map keyed by site id would have one site replace the other. Two nodes
/// with one id stay V01 and are not reported again.
pub(super) fn check_site_ids(playbook: &Playbook, r: &mut ValidationReport) {
    // The derived site ids of every list node and every inbox wait,
    // whether or not their site binds, beside the ids `connector_sites`
    // yields. A node id never equals its own derived id (a derived id
    // always carries `#`), so every hit is another node's site.
    let mut derived: std::collections::BTreeMap<String, crate::connector::sites::SiteKind> =
        std::collections::BTreeMap::new();
    for n in &playbook.nodes {
        if n.list().is_some() {
            derived.insert(
                crate::list::check_site(&n.id),
                crate::connector::sites::SiteKind::ItemCheck { node: n.id.clone() },
            );
        }
        if let NodeKind::Wait {
            wait_for: crate::schema::WaitFor::Inbox(_),
            ..
        } = &n.kind
        {
            derived.insert(
                crate::inbox_wait::inbox_site(&n.id),
                crate::connector::sites::SiteKind::InboxWait { node: n.id.clone() },
            );
        }
    }
    for n in &playbook.nodes {
        let Some(kind) = derived.get(n.id.as_str()) else {
            continue;
        };
        r.error(
            "V287",
            Some(n.id.as_str()),
            format!(
                "node id `{}` is the binding site of {}; rename the node",
                n.id,
                crate::connector::sites::kind_label(kind)
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::def::ConnectorDoc;
    use crate::connector::resolve::ConnectorFacts;

    /// The default inbox wait block: connector `echo`, account `main`, no
    /// conditions, types or take entries.
    fn plain_inbox() -> String {
        "      type: inbox\n      connector: echo\n      account: main\n".to_string()
    }

    /// A playbook with one inbox wait node `w` (the block given), `extra`
    /// nodes between the wait and the finish, and a chain that orders them.
    fn playbook(extra_nodes: &str, wait_for: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - id: start\n    type: start\n  - id: w\n    type: wait\n    timeout_seconds: 60\n    wait_for:\n{wait_for}{extra_nodes}  - id: done\n    type: finish\n    outcome: success\nedges:\n  - from: start\n    to: w\n  - from: w\n    to: done\n"
        ))
        .unwrap()
    }

    fn playbook_plain(extra_nodes: &str) -> Playbook {
        playbook(extra_nodes, &plain_inbox())
    }

    /// The issues of both checks over `playbook` against a connector store
    /// of `facts`, as `(code, message)` pairs.
    fn issues(
        pb: &Playbook,
        facts: std::collections::BTreeMap<String, ConnectorFacts>,
    ) -> Vec<(&'static str, String)> {
        let ctx = ValidationContext {
            connectors: facts,
            ..Default::default()
        };
        let mut r = ValidationReport::default();
        check_inbox_waits(pb, &ctx, &mut r);
        check_site_ids(pb, &mut r);
        r.issues
            .iter()
            .map(|i| (i.code, i.message.clone()))
            .collect()
    }

    fn issues_plain(pb: &Playbook) -> Vec<(&'static str, String)> {
        issues(pb, std::collections::BTreeMap::new())
    }

    fn echo_facts(
        has_webhook: bool,
        names_event_type: bool,
    ) -> std::collections::BTreeMap<String, ConnectorFacts> {
        std::collections::BTreeMap::from([(
            "echo".to_string(),
            ConnectorFacts {
                has_webhook,
                names_event_type,
                ..Default::default()
            },
        )])
    }

    fn with_types(types: &str) -> String {
        format!("{}      types: {}\n", plain_inbox(), types)
    }

    fn with_take(take: &str) -> String {
        format!("{}      take:\n{}", plain_inbox(), take)
    }

    // --- V284: a malformed block ------------------------------------

    #[test]
    fn v284_refuses_a_zero_timeout() {
        let yaml = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - id: w\n    type: wait\n    timeout_seconds: 0\n    wait_for:\n      type: inbox\n      connector: echo\n      account: main\nedges: []\n";
        let pb = Playbook::from_yaml(yaml).unwrap();
        let got = issues_plain(&pb);
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V284" && m.contains("timeout_seconds")),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_an_empty_condition_path() {
        let block = format!(
            "{}      match:\n        - path: \"\"\n          equals: x\n",
            plain_inbox()
        );
        let got = issues_plain(&playbook("", &block));
        assert!(
            got.iter().any(|(c, m)| *c == "V284" && m.contains("path")),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_a_condition_path_with_an_empty_segment() {
        let block = format!(
            "{}      match:\n        - path: a..b\n          equals: x\n",
            plain_inbox()
        );
        let got = issues_plain(&playbook("", &block));
        assert!(
            got.iter().any(|(c, m)| *c == "V284" && m.contains("a..b")),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_an_empty_type_entry() {
        let got = issues_plain(&playbook("", &with_types("[\"\"]")));
        assert!(got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
    }

    /// A type a stored event can never carry is refused: the filter's own
    /// shape rules hold for the wait's `types`.
    #[test]
    fn v284_refuses_a_type_that_cannot_be_stored() {
        let got = issues_plain(&playbook("", &with_types("[\"Pull Request\"]")));
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V284" && m.contains("Pull Request")),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_types_over_the_cap() {
        let cap = crate::connector::event_filter::MAX_TYPES;
        // Exactly the cap is accepted.
        let at = (0..cap)
            .map(|i| format!("\"t{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let got = issues_plain(&playbook("", &with_types(&format!("[{at}]"))));
        assert!(!got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
        // One above the cap is refused.
        let types = (0..=cap)
            .map(|i| format!("\"t{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let got = issues_plain(&playbook("", &with_types(&format!("[{types}]"))));
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V284" && m.contains(&cap.to_string())),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_conditions_over_the_cap() {
        // Exactly the cap is accepted.
        let at = (0..MAX_CONDITIONS)
            .map(|i| format!("{{ path: p{i}, equals: x }}"))
            .collect::<Vec<_>>()
            .join(", ");
        let block = format!("{}      match: [{at}]\n", plain_inbox());
        let got = issues_plain(&playbook("", &block));
        assert!(!got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
        // One above the cap is refused.
        let conditions = (0..=MAX_CONDITIONS)
            .map(|i| format!("{{ path: p{i}, equals: x }}"))
            .collect::<Vec<_>>()
            .join(", ");
        let block = format!("{}      match: [{conditions}]\n", plain_inbox());
        let got = issues_plain(&playbook("", &block));
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V284" && m.contains(&MAX_CONDITIONS.to_string())),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_take_over_the_cap() {
        // Exactly the cap is accepted.
        let at = (0..MAX_TAKE)
            .map(|i| format!("        - {{ name: f{i}, path: p{i} }}\n"))
            .collect::<String>();
        let got = issues_plain(&playbook("", &with_take(&at)));
        assert!(!got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
        // One above the cap is refused.
        let take = (0..(MAX_TAKE + 1))
            .map(|i| format!("        - {{ name: f{i}, path: p{i} }}\n"))
            .collect::<String>();
        let got = issues_plain(&playbook("", &with_take(&take)));
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V284" && m.contains(&MAX_TAKE.to_string())),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_a_take_name_that_is_not_snake() {
        let got = issues_plain(&playbook(
            "",
            &with_take("        - { name: BadName, path: p }\n"),
        ));
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V284" && m.contains("BadName")),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_a_repeated_take_name() {
        let got = issues_plain(&playbook(
            "",
            &with_take("        - { name: same, path: p }\n        - { name: same, path: q }\n"),
        ));
        assert!(
            got.iter().any(|(c, m)| *c == "V284" && m.contains("same")),
            "{got:?}"
        );
    }

    #[test]
    fn v284_refuses_a_reserved_take_name() {
        for reserved in RESERVED_NAMES {
            let got = issues_plain(&playbook(
                "",
                &with_take(&format!("        - {{ name: {reserved}, path: p }}\n")),
            ));
            assert!(
                got.iter()
                    .any(|(c, m)| *c == "V284" && m.contains(reserved)),
                "{reserved}: {got:?}"
            );
        }
    }

    #[test]
    fn v284_refuses_a_take_path_the_reader_refuses() {
        let got = issues_plain(&playbook(
            "",
            &with_take("        - { name: ok_name, path: a b }\n"),
        ));
        assert!(got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
    }

    #[test]
    fn v284_refuses_a_take_path_with_an_array_segment() {
        let got = issues_plain(&playbook(
            "",
            &with_take("        - { name: ok_name, path: \"items[].id\" }\n"),
        ));
        assert!(got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
    }

    #[test]
    fn v284_refuses_a_take_type_that_is_not_field_or_number() {
        let got = issues_plain(&playbook(
            "",
            &with_take("        - { name: ok_name, path: p, type: list }\n"),
        ));
        assert!(got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
    }

    #[test]
    fn v284_is_silent_for_a_well_formed_block() {
        let block = format!(
            "{}      match:\n        - path: from\n          equals: \"{{{{params.who}}}}\"\n      types: [push]\n      take:\n        - {{ name: action, path: action }}\n        - {{ name: count, path: n, type: number }}\n",
            plain_inbox()
        );
        let got = issues_plain(&playbook("", &block));
        assert!(!got.iter().any(|(c, _)| *c == "V284"), "{got:?}");
    }

    /// The connector and account names are checked at the site by V23 and
    /// V25, so a hyphenated connector and account pass: the wait adds no
    /// name rule of its own.
    #[test]
    fn the_site_accepts_hyphenated_connector_and_account() {
        let block =
            "      type: inbox\n      connector: echo-hooks\n      account: prod-eu\n".to_string();
        let pb = playbook("", &block);
        let ctx = ValidationContext {
            profiles: vec!["dev".into()],
            connectors: std::collections::BTreeMap::from([(
                "echo-hooks".to_string(),
                ConnectorFacts {
                    has_webhook: true,
                    names_event_type: true,
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let got = crate::validate::validate(&pb, &ctx);
        assert!(
            !got.issues
                .iter()
                .any(|i| i.code == "V23" || i.code == "V25"),
            "{:?}",
            got.issues
        );
        // The names reach the site's binding: V23 fires on a bad one.
        let block =
            "      type: inbox\n      connector: Echo Hooks\n      account: main\n".to_string();
        let pb = playbook("", &block);
        let got = crate::validate::validate(&pb, &ctx);
        assert!(
            got.issues.iter().any(|i| i.code == "V23"),
            "{:?}",
            got.issues
        );
    }

    // --- V285: the connector can never receive anything -------------

    #[test]
    fn v285_refuses_a_connector_that_is_not_installed() {
        // The store knows another connector only: `echo` is not installed.
        let got = issues(
            &playbook_plain(""),
            std::collections::BTreeMap::from([(
                "other".to_string(),
                ConnectorFacts {
                    has_webhook: true,
                    ..Default::default()
                },
            )]),
        );
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V285" && m.contains("echo") && m.contains("not installed")),
            "{got:?}"
        );
    }

    #[test]
    fn v285_needs_a_webhook_connector() {
        let got = issues(&playbook_plain(""), echo_facts(false, false));
        assert!(
            got.iter()
                .any(|(c, m)| *c == "V285" && m.contains("webhook")),
            "{got:?}"
        );
        // With the webhook block the wait is accepted.
        let got = issues(&playbook_plain(""), echo_facts(true, false));
        assert!(!got.iter().any(|(c, _)| *c == "V285"), "{got:?}");
    }

    /// V43 checks the account an inbox wait reads, though its binding grants
    /// no function: an account that does not define a field the webhook
    /// block verifies deliveries with is refused, one that defines it passes.
    #[test]
    fn v43_checks_the_account_an_inbox_wait_reads() {
        let facts = |fields: &[&str]| {
            std::collections::BTreeMap::from([(
                "echo".to_string(),
                ConnectorFacts {
                    has_webhook: true,
                    webhook_secret_fields: Vec::from(["signing_secret".to_string()]),
                    accounts: std::collections::BTreeMap::from([(
                        "main".to_string(),
                        fields.iter().map(|f| f.to_string()).collect(),
                    )]),
                    ..Default::default()
                },
            )])
        };
        let v43 = |facts| {
            let ctx = ValidationContext {
                connectors: facts,
                ..Default::default()
            };
            let mut r = ValidationReport::default();
            super::super::connectors::check_connectors(&playbook_plain(""), &ctx, &mut r);
            r.issues
                .iter()
                .filter(|i| i.code == "V43")
                .map(|i| i.message.clone())
                .collect::<Vec<_>>()
        };
        let missing = v43(facts(&[]));
        assert!(
            missing.len() == 1
                && missing[0].contains("reads the inbox")
                && missing[0].contains("signing_secret"),
            "{missing:?}"
        );
        assert!(v43(facts(&["signing_secret"])).is_empty());
    }

    #[test]
    fn v285_refuses_an_mcp_connector_without_an_inbox() {
        let doc = ConnectorDoc::from_yaml(
            r#"
name: echo
version: 0.1.0
mcp: { transport: stdio, command: echo-mcp-server }
functions:
  - name: get_event
    description: Get an event
    read_only: true
    mcp: { tool: get_event }
"#,
            "echo",
        )
        .unwrap();
        let facts = ConnectorFacts::of_manifest(&doc);
        assert!(facts.mcp_transport.is_some());
        assert!(!facts.has_webhook);
        let got = issues(
            &playbook_plain(""),
            std::collections::BTreeMap::from([("echo".into(), facts)]),
        );
        assert!(
            got.iter().any(|(code, message)| {
                *code == "V285"
                    && message.contains("is an MCP connector and cannot declare `webhook:`")
                    && message.contains("inbox never receives anything")
            }),
            "{got:?}"
        );
    }

    #[test]
    fn v285_is_silent_without_a_connector_store() {
        let got = issues_plain(&playbook_plain(""));
        assert!(!got.iter().any(|(c, _)| *c == "V285"), "{got:?}");
    }

    // --- V286: types without an event type --------------------------

    #[test]
    fn v286_warns_on_types_without_a_type_path() {
        let got = issues(
            &playbook("", &with_types("[push]")),
            echo_facts(true, false),
        );
        assert!(got.iter().any(|(c, _)| *c == "V286"), "{got:?}");
        // A connector that names its event type warns no more.
        let got = issues(&playbook("", &with_types("[push]")), echo_facts(true, true));
        assert!(!got.iter().any(|(c, _)| *c == "V286"), "{got:?}");
    }

    // --- V287: two binding sites with one id ------------------------

    /// A node id that is another node's derived site is refused.
    #[test]
    fn v287_refuses_a_node_named_like_an_inbox_site() {
        let pb = playbook(
            "  - id: w#inbox\n    type: prompt\n    prompt: hi\n",
            &plain_inbox(),
        );
        let got = issues_plain(&pb);
        assert!(
            got.iter().any(|(c, m)| *c == "V287"
                && m.contains("w#inbox")
                && m.contains("the inbox wait of node `w`")),
            "{got:?}"
        );
    }

    /// The same collision with a list node's check site, possible on main
    /// already.
    #[test]
    fn v287_refuses_a_node_named_like_a_check_site() {
        let pb = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - id: a\n    type: script\n    script: s.sh\n    runner: sh\n    list:\n      items:\n        inline: [one]\n  - id: a#check\n    type: prompt\n    prompt: hi\n  - id: done\n    type: finish\n    outcome: success\nedges:\n  - from: a\n    to: done\n",
        )
        .unwrap();
        let got = issues_plain(&pb);
        assert!(
            got.iter().any(|(c, m)| *c == "V287"
                && m.contains("a#check")
                && m.contains("the item check of node `a`")),
            "{got:?}"
        );
    }

    #[test]
    fn v287_is_silent_for_distinct_sites() {
        // A wait and a list whose derived sites name no node.
        let pb = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - id: a\n    type: script\n    script: s.sh\n    runner: sh\n    list:\n      items:\n        inline: [one]\n  - id: w\n    type: wait\n    timeout_seconds: 60\n    wait_for:\n      type: inbox\n      connector: echo\n      account: main\n  - id: done\n    type: finish\n    outcome: success\nedges:\n  - from: a\n    to: w\n  - from: w\n    to: done\n",
        )
        .unwrap();
        let got = issues_plain(&pb);
        assert!(!got.iter().any(|(c, _)| *c == "V287"), "{got:?}");
        // A node named `w#inboxx` (no site matches) stays silent.
        let pb = playbook(
            "  - id: w#inboxx\n    type: prompt\n    prompt: hi\n",
            &plain_inbox(),
        );
        let got = issues_plain(&pb);
        assert!(!got.iter().any(|(c, _)| *c == "V287"), "{got:?}");
    }
}
