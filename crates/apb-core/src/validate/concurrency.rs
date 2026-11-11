//! The `concurrency:` block rules (V210, V211): the key may read only
//! declared non-secret `params.*`, and `supersede` on an irreversible
//! playbook is a warning.

use super::*;

/// Runs V210 and V211.
pub(super) fn check_concurrency(playbook: &Playbook, r: &mut ValidationReport) {
    let Some(block) = &playbook.concurrency else {
        return;
    };
    if let Some(key) = &block.key {
        if key.trim().is_empty() {
            r.error("V210", None, "concurrency.key is empty".to_string());
        } else if key.chars().count() > crate::concurrency::KEY_MAX_CHARS {
            let n = key.chars().count();
            r.error(
                "V210",
                None,
                format!(
                    "concurrency.key is {n} characters; at most {}",
                    crate::concurrency::KEY_MAX_CHARS
                ),
            );
        }
        for cap in super::templates::template_refs(key) {
            match cap.split('.').collect::<Vec<&str>>().as_slice() {
                ["params", name] => {
                    let Some(p) = playbook.params.iter().find(|p| &p.name == name) else {
                        r.error(
                            "V210",
                            None,
                            format!(
                                "concurrency.key reads {{{{{cap}}}}}; it may read only declared params (params.<name>)"
                            ),
                        );
                        continue;
                    };
                    if p.secret || crate::token_shape::name_looks_secret(&p.name) {
                        r.error(
                            "V210",
                            None,
                            format!(
                                "concurrency.key reads the secret param {name}; a key is kept in a file and shown, so it may not read a secret"
                            ),
                        );
                    }
                }
                _ => {
                    r.error(
                        "V210",
                        None,
                        format!(
                            "concurrency.key reads {{{{{cap}}}}}; it may read only declared params (params.<name>)"
                        ),
                    );
                }
            }
        }
    }
    if block.on_busy == crate::concurrency::OnBusy::Supersede
        && crate::effects::effective(playbook).contains(&crate::schema::Effect::Irreversible)
    {
        r.warn(
            "V211",
            None,
            "on_busy: supersede stops a running run of a playbook with irreversible effects; a stopped run may leave such an effect half done. Prefer queue_latest".to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues(extra: &str) -> Vec<(String, String)> {
        let mut yaml = String::from(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nparams:\n  - { name: account, type: text }\n  - { name: token, type: text, secret: true }\n  - { name: api_token, type: text }",
        );
        yaml.push_str(extra);
        yaml.push_str(
            "\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        );
        let pb = Playbook::from_yaml(&yaml).unwrap();
        crate::validate::validate(&pb, &ValidationContext::default())
            .issues
            .iter()
            .map(|i| (i.code.to_string(), severity_word(i.severity).to_string()))
            .collect()
    }

    fn codes(yaml_block: &str) -> Vec<(String, String)> {
        issues(&format!("\nconcurrency: {yaml_block}"))
    }

    #[test]
    fn v210_empty_key() {
        assert_eq!(
            codes(r#"{ key: "  " }"#),
            vec![("V210".into(), "error".into())]
        );
    }

    #[test]
    fn v210_key_over_500_characters() {
        assert_eq!(
            codes(&format!("{{ key: \"{}\" }}", "k".repeat(501))),
            vec![("V210".into(), "error".into())]
        );
        assert!(codes(&format!("{{ key: \"{}\" }}", "k".repeat(500))).is_empty());
    }

    #[test]
    fn v210_reads_a_node_output() {
        assert_eq!(
            codes(r#"{ key: "{{nodes.w.output}}" }"#),
            vec![("V210".into(), "error".into())]
        );
    }

    #[test]
    fn v210_reads_an_undeclared_param() {
        assert_eq!(
            codes(r#"{ key: "{{params.ghost}}" }"#),
            vec![("V210".into(), "error".into())]
        );
    }

    #[test]
    fn v210_reads_a_secret_param() {
        assert_eq!(
            codes(r#"{ key: "{{params.token}}" }"#),
            vec![("V210".into(), "error".into())]
        );
    }

    #[test]
    fn v210_reads_a_param_whose_name_looks_secret() {
        assert_eq!(
            codes(r#"{ key: "{{params.api_token}}" }"#),
            vec![("V210".into(), "error".into())]
        );
    }

    #[test]
    fn a_declared_plain_param_is_valid() {
        assert!(codes(r#"{ key: "{{params.account}}" }"#).is_empty());
    }

    #[test]
    fn an_absent_key_is_valid() {
        assert!(codes("{ on_busy: refuse }").is_empty());
    }

    #[test]
    fn v211_supersede_with_irreversible_effects() {
        assert_eq!(
            issues("\nconcurrency: { on_busy: supersede }\neffects: [irreversible]",),
            vec![("V211".into(), "warning".into())]
        );
    }

    #[test]
    fn queue_latest_with_irreversible_effects_is_quiet() {
        assert!(
            issues("\nconcurrency: { on_busy: queue_latest }\neffects: [irreversible]").is_empty()
        );
    }
}
