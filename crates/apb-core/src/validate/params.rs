//! Param rules (V99, V100): a secret param keeps its value out of the
//! definition and is plain text. Search rules (V160 to V162): a searchable
//! param is never secret and never named like a secret, and a playbook
//! marks at most [`crate::run_search::MAX_SEARCH_PARAMS`] of them.
//! Pattern rules (V177, V178): a text param's `pattern` compiles and is a
//! bounded size, and the param's own default matches it.
//! V340: no string of the definition references a secret param, so apb
//! never renders a secret param's value into text
//! ([`secret_param_refs`], the one walk every check of a definition shares).

use std::collections::BTreeSet;

use super::*;

/// One reference to a secret param in a definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretParamRef {
    /// The node the string sits under, `None` outside `nodes`.
    pub node: Option<String>,
    /// The path inside the node, or inside the definition outside `nodes`:
    /// keys joined with `.`, sequence indices as `[i]`, a key that is not a
    /// string as its flow text, a hit on a key itself as `{key}`.
    pub field: String,
    /// The secret param the string references.
    pub param: String,
}

/// The definition's value tree could not be built, so its references are
/// unknown: every caller refuses rather than passing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretWalkError(pub String);

impl std::fmt::Display for SecretWalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretWalkError {}

/// The fields V104 and V210 already refuse a secret reference in at
/// validation, whatever else the definition gets wrong; only
/// [`check_params`] leaves them to those codes. (`previous_success.when_none`
/// is V340's: V176 runs only on a definition with no other error.)
const OWNED_FIELDS: [&str; 2] = ["quality.task_type", "concurrency.key"];

/// Every string of `playbook`, mapping keys included, that references a
/// param declared `secret: true` in it or named in `also_secret` (a run's
/// full secret set), in a form the template renderers resolve. No path is
/// skipped. An empty secret set answers at once; a definition whose value
/// tree cannot be built is an error, never an empty list.
pub fn secret_param_refs(
    playbook: &Playbook,
    also_secret: &BTreeSet<String>,
) -> Result<Vec<SecretParamRef>, SecretWalkError> {
    let mut secret: BTreeSet<String> = playbook
        .params
        .iter()
        .filter(|p| p.secret)
        .map(|p| p.name.clone())
        .collect();
    secret.extend(also_secret.iter().cloned());
    if secret.is_empty() {
        return Ok(vec![]);
    }
    walk_refs(playbook, &secret)
}

/// The walk of [`secret_param_refs`]. A definition whose value tree cannot
/// be built is an error (fail closed), never an empty list.
fn walk_refs(
    playbook: &Playbook,
    secret: &BTreeSet<String>,
) -> Result<Vec<SecretParamRef>, SecretWalkError> {
    let tree = yaml_serde::to_value(playbook).map_err(|e| SecretWalkError(e.to_string()))?;
    let mut walk = Walk {
        secret,
        hits: Vec::new(),
    };
    match &tree {
        yaml_serde::Value::Mapping(top) => {
            for (key, value) in top {
                if let (yaml_serde::Value::String(k), yaml_serde::Value::Sequence(nodes)) =
                    (key, value)
                    && k == "nodes"
                {
                    for (i, node) in nodes.iter().enumerate() {
                        match node.get("id").and_then(yaml_serde::Value::as_str) {
                            Some(id) => walk.value(node, &Some(id.to_string()), ""),
                            None => walk.value(node, &None, &format!("nodes[{i}]")),
                        }
                    }
                } else {
                    walk.entry(key, value, &None, "");
                }
            }
        }
        other => walk.value(other, &None, ""),
    }
    Ok(walk.hits)
}

struct Walk<'a> {
    secret: &'a BTreeSet<String>,
    hits: Vec<SecretParamRef>,
}

impl Walk<'_> {
    fn value(&mut self, value: &yaml_serde::Value, node: &Option<String>, path: &str) {
        match value {
            yaml_serde::Value::String(s) => self.text(s, node, path),
            yaml_serde::Value::Sequence(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.value(item, node, &format!("{path}[{i}]"));
                }
            }
            yaml_serde::Value::Mapping(map) => {
                for (key, value) in map {
                    self.entry(key, value, node, path);
                }
            }
            yaml_serde::Value::Tagged(tagged) => self.value(&tagged.value, node, path),
            yaml_serde::Value::Null | yaml_serde::Value::Bool(_) | yaml_serde::Value::Number(_) => {
            }
        }
    }

    /// One mapping entry: the key is checked like any string (or walked,
    /// when it is a sequence or a mapping), then its value.
    fn entry(
        &mut self,
        key: &yaml_serde::Value,
        value: &yaml_serde::Value,
        node: &Option<String>,
        path: &str,
    ) {
        let at_key = join(path, "{key}");
        let segment = match key {
            yaml_serde::Value::String(k) => {
                self.text(k, node, &at_key);
                k.clone()
            }
            other => {
                self.value(other, node, &at_key);
                flow(other)
            }
        };
        self.value(value, node, &join(path, &segment));
    }

    fn text(&mut self, text: &str, node: &Option<String>, path: &str) {
        for reference in crate::template::refs(text) {
            if let Some(("params", name)) = reference.split_once('.')
                && self.secret.contains(name)
            {
                self.hits.push(SecretParamRef {
                    node: node.clone(),
                    field: path.to_string(),
                    param: name.to_string(),
                });
            }
        }
    }
}

fn join(path: &str, segment: &str) -> String {
    if path.is_empty() {
        segment.to_string()
    } else {
        format!("{path}.{segment}")
    }
}

/// A value as YAML flow text, for a path segment of a key that is not a
/// string.
fn flow(value: &yaml_serde::Value) -> String {
    match value {
        yaml_serde::Value::Null => "~".to_string(),
        yaml_serde::Value::Bool(b) => b.to_string(),
        yaml_serde::Value::Number(n) => n.to_string(),
        yaml_serde::Value::String(s) => s.clone(),
        yaml_serde::Value::Sequence(items) => {
            format!(
                "[{}]",
                items.iter().map(flow).collect::<Vec<_>>().join(", ")
            )
        }
        yaml_serde::Value::Mapping(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", flow(k), flow(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        yaml_serde::Value::Tagged(tagged) => format!("{} {}", tagged.tag, flow(&tagged.value)),
    }
}

/// V340: every reference [`secret_param_refs`] finds, except in the fields
/// V104 and V210 report already, and a walk that failed.
fn check_secret_refs(playbook: &Playbook, r: &mut ValidationReport) {
    report_walk(secret_param_refs(playbook, &BTreeSet::new()), r);
}

/// Reports one walk's hits and its error as V340.
fn report_walk(walk: Result<Vec<SecretParamRef>, SecretWalkError>, r: &mut ValidationReport) {
    match walk {
        Ok(hits) => {
            for hit in hits {
                if hit.node.is_none() && OWNED_FIELDS.contains(&hit.field.as_str()) {
                    continue;
                }
                r.error(
                    "V340",
                    hit.node.as_deref(),
                    format!(
                        "`{}` references secret param `{}`; apb never renders a secret param into text. Read it in a script from run.yaml (`$APB_RUN_DIR`), use a connector account secret, or drop `secret: true` if it is not a secret",
                        hit.field, hit.param
                    ),
                );
            }
        }
        Err(e) => r.error(
            "V340",
            None,
            format!(
                "`(definition)`: the definition could not be checked for secret param references: {e}"
            ),
        ),
    }
}

/// Runs every param rule.
pub(crate) fn check_params(playbook: &Playbook, r: &mut ValidationReport) {
    for p in playbook.params.iter().filter(|p| p.secret) {
        if p.default.is_some() {
            r.error(
                "V99",
                None,
                format!(
                    "param `{}` is secret but has a default: the secret would be stored in the playbook; remove the default",
                    p.name
                ),
            );
        }
        if p.kind != "text" {
            r.error(
                "V100",
                None,
                format!(
                    "param `{}` is secret, so it must be `type: text`, not `{}`",
                    p.name, p.kind
                ),
            );
        }
    }
    check_search(playbook, r);
    check_patterns(playbook, r);
    check_secret_refs(playbook, r);
}

/// V177 and V178: the shape of a text param's `pattern` and its default.
fn check_patterns(playbook: &Playbook, r: &mut ValidationReport) {
    for p in &playbook.params {
        let Some(pattern) = &p.pattern else {
            continue;
        };
        if p.kind != "text" {
            r.error(
                "V177",
                None,
                format!(
                    "param `{}` has a pattern but is `type: {}`; a pattern applies to text params only",
                    p.name, p.kind
                ),
            );
            continue;
        }
        if pattern.is_empty() {
            r.error(
                "V177",
                None,
                format!("param `{}`: the pattern is empty", p.name),
            );
            continue;
        }
        if let Err(why) = crate::param_pattern::compile(pattern) {
            r.error(
                "V177",
                None,
                format!("param `{}`: pattern does not compile: {why}", p.name),
            );
            continue;
        }
        if let Some(default) = p.default_text()
            && crate::param_pattern::check(p, &default).is_err()
        {
            r.error(
                "V178",
                None,
                format!(
                    "param `{}`: default does not match its pattern `{pattern}`",
                    p.name
                ),
            );
        }
    }
}

/// V160 to V162: what a run search may list.
fn check_search(playbook: &Playbook, r: &mut ValidationReport) {
    let searchable: Vec<&crate::schema::Param> =
        playbook.params.iter().filter(|p| p.search).collect();
    for p in &searchable {
        if p.secret {
            r.error(
                "V160",
                None,
                format!(
                    "param `{}` is secret, so it cannot be `search: true`: a run listing would show its value; remove `search`",
                    p.name
                ),
            );
        } else if crate::token_shape::name_looks_secret(&p.name) {
            r.error(
                "V161",
                None,
                format!(
                    "param `{}` is named like a secret, so it cannot be `search: true`: a run listing would show its value; rename it or remove `search`",
                    p.name
                ),
            );
        }
    }
    let max = crate::run_search::MAX_SEARCH_PARAMS;
    if searchable.len() > max {
        r.error(
            "V162",
            None,
            format!(
                "{} params are `search: true`; at most {max} may be",
                searchable.len()
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_walk_error_is_v340_on_the_definition() {
        let mut r = ValidationReport::default();
        report_walk(Err(SecretWalkError("broken".into())), &mut r);
        assert_eq!(r.issues.len(), 1);
        assert_eq!(r.issues[0].code, "V340");
        assert_eq!(r.issues[0].node, None);
        assert!(
            r.issues[0].message.starts_with("`(definition)`"),
            "{}",
            r.issues[0].message
        );
    }
}
