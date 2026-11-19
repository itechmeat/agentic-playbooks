//! The form of a `human_review` gate (people-in-the-loop D2/D3): a
//! malformed `max_scope` (V243), malformed `fields` (V244), malformed
//! `show` (V245), a required field without a default on a gate the engine
//! may decide (V246), and a field default over untrusted text (V247).

use super::*;

pub(super) fn check_review_form(playbook: &Playbook, r: &mut ValidationReport) {
    let analysis = crate::untrusted::analyze(playbook);
    for node in &playbook.nodes {
        if let crate::schema::NodeKind::HumanReview {
            options,
            fields,
            show,
            max_scope,
            ..
        } = &node.kind
        {
            check_max_scope(&node.id, options, max_scope, r);
            check_fields(playbook, &node.id, options, fields, &analysis, r);
            check_show(&node.id, show, r);
        }
    }
}

/// V244 (error), one issue per problem: more than `MAX_FIELDS` fields; a
/// name not matching `[a-z][a-z0-9_]*` or longer than `FIELD_NAME_MAX`; a
/// name used twice; the name `decision`; an `enum` without `options`,
/// with a duplicate option, or `options` on a non-enum field; `max_len`
/// on a non-text field, or `max_len` 0 or above `TEXT_MAX_LEN`;
/// `required` as a list naming an option the gate does not offer; a
/// literal `default` that does not coerce to the field's type. V246: a
/// field required for an option the engine may choose without a default
/// (or with a blank literal one) on a gate with `auto_decide` or
/// `unattended: { decide }`. V247
/// (warning): a field default that reads untrusted text.
fn check_fields(
    playbook: &Playbook,
    node: &str,
    options: &[String],
    fields: &[crate::review_form::ReviewField],
    analysis: &crate::untrusted::Analysis,
    r: &mut ValidationReport,
) {
    use crate::review_form::{FIELD_NAME_MAX, FieldKind, MAX_FIELDS, Required, TEXT_MAX_LEN};
    if fields.len() > MAX_FIELDS {
        r.error(
            "V244",
            Some(node),
            format!("the gate takes at most {MAX_FIELDS} fields"),
        );
    }
    let offered = crate::schema::effective_review_options(options);
    // The options the engine may choose: every `auto_decide` allow, the
    // unattended outcome's declared option.
    let mut engine_options: Vec<&str> = Vec::new();
    if let Some(n) = playbook.node(node) {
        if let Some(auto) = &n.auto_decide {
            engine_options.extend(auto.allow.iter().map(String::as_str));
        }
        if let Some(d) = n.unattended.as_ref().and_then(|u| u.decide()) {
            engine_options.push(&d.decide);
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let name = f.name.as_str();
        // How every message names the field: by its name only when the
        // name is in the field-name grammar and not credential-shaped, else
        // by its position. A message never repeats an author's literal.
        let label = crate::review_form::field_label(name, i);
        let name_ok = {
            let mut chars = name.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        if !name_ok {
            r.error(
                "V244",
                Some(node),
                format!("{label}: name must match [a-z][a-z0-9_]*"),
            );
        }
        if name.chars().count() > FIELD_NAME_MAX {
            r.error(
                "V244",
                Some(node),
                format!("{label}: name is longer than {FIELD_NAME_MAX} characters"),
            );
        }
        if seen.contains(&name) {
            r.error("V244", Some(node), format!("{label}: named twice"));
        }
        seen.push(name);
        if name == crate::review_form::DECISION_KEY {
            r.error(
                "V244",
                Some(node),
                format!(
                    "{label}: `decision` is the key the gate's JSON output names the decision with"
                ),
            );
        }
        match f.kind {
            FieldKind::Enum if f.options.is_empty() => {
                r.error(
                    "V244",
                    Some(node),
                    format!("{label}: an enum needs options"),
                );
            }
            FieldKind::Enum => {
                // Each repeat named by its position, never by its text.
                for (n, o) in f.options.iter().enumerate() {
                    if f.options[..n].contains(o) {
                        r.error(
                            "V244",
                            Some(node),
                            format!("{label}: option {} repeats an earlier option", n + 1),
                        );
                    }
                }
            }
            _ if !f.options.is_empty() => {
                r.error(
                    "V244",
                    Some(node),
                    format!("{label}: `options` belongs to an enum field"),
                );
            }
            _ => {}
        }
        match (f.kind, f.max_len) {
            (FieldKind::Text, Some(0)) => {
                r.error("V244", Some(node), format!("{label}: `max_len` is 0"));
            }
            (FieldKind::Text, Some(len)) if len > TEXT_MAX_LEN => {
                r.error(
                    "V244",
                    Some(node),
                    format!("{label}: `max_len` is above {TEXT_MAX_LEN}"),
                );
            }
            (FieldKind::Text, _) => {}
            (_, Some(_)) => {
                r.error(
                    "V244",
                    Some(node),
                    format!("{label}: `max_len` belongs to a text field"),
                );
            }
            (_, None) => {}
        }
        if let Required::Options(req) = &f.required {
            for (n, o) in req.iter().enumerate() {
                if !offered.contains(o) {
                    r.error(
                        "V244",
                        Some(node),
                        format!(
                            "{label}: required entry {} names an option the gate does not offer",
                            n + 1
                        ),
                    );
                }
            }
        }
        // A literal default (no `{{`) must coerce to the field's type.
        if let Some(d) = f.default.as_ref().filter(|d| !d.contains("{{")) {
            let ok = match f.kind {
                FieldKind::Text => {
                    !d.contains('\0')
                        && d.chars().count()
                            <= f.max_len
                                .unwrap_or(crate::review_form::TEXT_DEFAULT_MAX_LEN)
                                as usize
                }
                // Trimmed like the value a decision carries
                // (`review_form::coerce`).
                FieldKind::Number => d.trim().parse::<f64>().is_ok_and(|n| n.is_finite()),
                FieldKind::Bool => d == "true" || d == "false",
                FieldKind::Enum => f.options.iter().any(|o| o == d),
            };
            if !ok {
                let what = match f.kind {
                    FieldKind::Number => "is not a number",
                    FieldKind::Bool => "is not true or false",
                    FieldKind::Enum => "is not one of the field's options",
                    FieldKind::Text => "does not fit the field's length",
                };
                r.error(
                    "V244",
                    Some(node),
                    format!("{label}: the literal default {what}"),
                );
            }
        }
        // V246: required for an option the engine may choose, no default.
        // A blank literal default is no value for a required field (the
        // channel's `check_values` refuses it), so it is no default.
        let no_default = f.default.as_ref().is_none_or(|d| d.trim().is_empty());
        if no_default && engine_options.iter().any(|o| f.required.applies(o)) {
            r.error(
                "V246",
                Some(node),
                format!(
                    "{label} is required and has no default, but the gate may be decided without a person (auto_decide or unattended); give it a default or make it optional"
                ),
            );
        }
        // V247: a default that renders untrusted text.
        if let Some(d) = f.default.as_ref().filter(|d| d.contains("{{")) {
            let sources = crate::untrusted::text_reads_untrusted(playbook, analysis, d);
            if !sources.is_empty() {
                let list = sources
                    .iter()
                    .map(|s| s.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                r.warn(
                    "V247",
                    Some(node),
                    format!(
                        "{label} defaults to untrusted text from {list}; a person who keeps the default passes it on as their own value"
                    ),
                );
            }
        }
    }
}

/// V245 (error): more than `MAX_SHOW` entries; a label empty or longer
/// than `SHOW_LABEL_MAX` characters; an empty `text`.
fn check_show(node: &str, show: &[crate::review_form::ShowEntry], r: &mut ValidationReport) {
    use crate::review_form::{MAX_SHOW, SHOW_LABEL_MAX};
    if show.len() > MAX_SHOW {
        r.error(
            "V245",
            Some(node),
            format!("the gate has at most {MAX_SHOW} entries"),
        );
    }
    for (i, s) in show.iter().enumerate() {
        let n = i + 1;
        if s.label.is_empty() {
            r.error(
                "V245",
                Some(node),
                format!("show entry {n}: label is empty"),
            );
        } else if s.label.chars().count() > SHOW_LABEL_MAX {
            r.error(
                "V245",
                Some(node),
                format!("show entry {n}: label is longer than {SHOW_LABEL_MAX} characters"),
            );
        }
        if s.text.is_empty() {
            r.error("V245", Some(node), format!("show entry {n}: text is empty"));
        }
    }
}

/// V243 (error): a malformed `max_scope`: no limit set, a limit of 0, a
/// withheld option the gate does not offer, or every offered option
/// withheld.
fn check_max_scope(
    node: &str,
    options: &[String],
    max_scope: &Option<crate::review_form::MaxScope>,
    r: &mut ValidationReport,
) {
    let Some(max) = max_scope else {
        return;
    };
    if max.files.is_none() && max.lines.is_none() && max.writes.is_none() {
        r.error(
            "V243",
            Some(node),
            "max_scope sets no limit; give files, lines or writes".to_string(),
        );
    }
    for (key, limit) in [
        ("files", max.files),
        ("lines", max.lines),
        ("writes", max.writes),
    ] {
        if limit == Some(0) {
            r.error(
                "V243",
                Some(node),
                format!("max_scope.{key} is 0; a limit is at least 1"),
            );
        }
    }
    let offered = crate::schema::effective_review_options(options);
    // Only what the author wrote is checked against the options: left out,
    // `withhold` is `approve` where the gate offers it.
    let authored = max.withhold.as_deref().unwrap_or_default();
    for (n, w) in authored.iter().enumerate() {
        if !offered.contains(w) {
            r.error(
                "V243",
                Some(node),
                format!(
                    "max_scope.withhold entry {} names an option the gate does not offer",
                    n + 1
                ),
            );
        }
    }
    if max.withhold.is_none() && max.withhold_for(&offered).is_empty() {
        r.error(
            "V243",
            Some(node),
            format!(
                "max_scope withholds nothing: the gate does not offer `{}`; name the options to withhold in max_scope.withhold",
                crate::review_form::DEFAULT_WITHHOLD
            ),
        );
    }
    // What the limit withholds in effect, `withhold` written or left out:
    // a gate that offers only `approve` loses it to the default too.
    let withheld = max.withhold_for(&offered);
    if !offered.is_empty() && offered.iter().all(|o| withheld.contains(o)) {
        r.error(
            "V243",
            Some(node),
            "max_scope withholds every option of the gate; keep at least one to decide with"
                .to_string(),
        );
    }
}
