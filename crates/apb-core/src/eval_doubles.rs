//! Connector doubles of an eval case: recorded answers per connector
//! function, the rule that picks the answer for one call, and which
//! functions of a version's binding sites a case covers. Shared by the
//! suite validator (V280 and V281), `apb eval` and the engine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::connector::resolve::ConnectorFacts;
use crate::schema::{FunctionsAllow, Playbook};

/// connector -> function -> answers, in the order written.
pub type Doubles = BTreeMap<String, BTreeMap<String, Vec<DoubleAnswer>>>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleAnswer {
    /// When this answer applies; absent: every call.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub matcher: Option<DoubleMatch>,
    /// The recorded answer, in the shape of a `mock` function.
    pub answer: crate::connector::def::MockSpec,
    /// How many calls this answer serves; absent: every matching call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoubleMatch {
    /// A JSON object every key of which the call's args hold with a
    /// value that matches (objects recursively, everything else equal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Text some string value of the args contains (case-sensitive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Only calls made at this binding site (a node id, `<node>#check`,
    /// `@baseline`, `@follow_up`, or a site a later change adds to
    /// `connector_sites`, such as `<node>#inbox`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

/// One `checks.connector_calls` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CallCheck {
    /// `<connector>.<function>`.
    pub function: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// A JSON object every key of which the call's input arguments hold:
    /// the arguments the call was made with, before the connector's text
    /// modes, the neutralizing of forged markers and an appended send
    /// marker; never the body a real request would carry. It compares the
    /// captured arguments, which are scrubbed: a secret value in them reads
    /// as its redaction marker, never as the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Text some string value of the call's input arguments contains
    /// (case-sensitive), read as `args` is: the scrubbed captured
    /// arguments, before text modes and markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Exactly this many selected calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

/// Whether `pattern` (an object) is held by `args`.
pub fn args_hold(pattern: &Value, args: &Value) -> bool {
    match (pattern, args) {
        (Value::Object(p), Value::Object(a)) => p
            .iter()
            .all(|(k, pv)| a.get(k).is_some_and(|av| args_hold(pv, av))),
        _ => pattern == args,
    }
}

/// Whether some string leaf of `args` contains `text`.
pub fn args_contain(args: &Value, text: &str) -> bool {
    match args {
        Value::String(s) => s.contains(text),
        Value::Array(items) => items.iter().any(|v| args_contain(v, text)),
        Value::Object(map) => map.values().any(|v| args_contain(v, text)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Whether `m` holds for a call at `site` with `args`.
pub fn matches(m: &DoubleMatch, site: &str, args: &Value) -> bool {
    m.args.as_ref().is_none_or(|p| args_hold(p, args))
        && m.contains.as_deref().is_none_or(|t| args_contain(args, t))
        && m.site.as_deref().is_none_or(|s| s == site)
}

/// The answer for one call: the index of the first answer of the
/// function whose `match` holds and whose `times` is not used up
/// (`used[i]` is how many calls answer `i` already served), or `None`.
pub fn pick(answers: &[DoubleAnswer], site: &str, args: &Value, used: &[u32]) -> Option<usize> {
    answers.iter().enumerate().position(|(i, a)| {
        a.matcher.as_ref().is_none_or(|m| matches(m, site, args))
            && a.times
                .is_none_or(|t| used.get(i).copied().unwrap_or(0) < t)
    })
}

/// The functions every binding site of `playbook` may call, from the
/// installed connectors' facts: `all` is every function, `read_only`
/// every function not in `write_functions`, a list is that list, over
/// every site `Playbook::connector_sites` walks. Each function that
/// declares a `marker` send adds its `reconcile.function` and the
/// connector's `identity.function` (the send ledger's read-back reads the
/// own identity, then the listing, through the same call path, so both are
/// doubled too). A
/// connector missing from `facts` is reported, never guessed.
pub fn bound_functions(
    playbook: &Playbook,
    facts: &BTreeMap<String, ConnectorFacts>,
) -> (BTreeSet<(String, String)>, Vec<String>) {
    let mut bound = BTreeSet::new();
    let mut problems: Vec<String> = Vec::new();
    let mut report = |p: String| {
        if !problems.contains(&p) {
            problems.push(p);
        }
    };
    for (_, bindings) in playbook.connector_sites() {
        for b in bindings.iter() {
            let Some(f) = facts.get(&b.name) else {
                report(format!(
                    "connector `{}` is not installed, so its doubles cannot be checked without the installed connectors",
                    b.name
                ));
                continue;
            };
            if let Some(e) = &f.load_error {
                report(format!(
                    "connector `{}` does not load ({e}), so its doubles cannot be checked",
                    b.name
                ));
                continue;
            }
            let names: Vec<String> = match &b.functions {
                FunctionsAllow::All => f.functions.clone(),
                FunctionsAllow::ReadOnly => f
                    .functions
                    .iter()
                    .filter(|n| !f.write_functions.contains(n))
                    .cloned()
                    .collect(),
                FunctionsAllow::List(list) => list.clone(),
            };
            for name in names {
                if let Some(r) = f.reconcile_functions.get(&name) {
                    bound.insert((b.name.clone(), r.clone()));
                    if let Some(id) = &f.identity_function {
                        bound.insert((b.name.clone(), id.clone()));
                    }
                }
                bound.insert((b.name.clone(), name));
            }
        }
    }
    (bound, problems)
}

/// Why `case` cannot run `playbook` with its doubles: every bound
/// function without an answer, every connector the facts lack, every
/// connector entry with no function, and every double of a function no
/// site binds. Empty when the doubles
/// cover the version exactly enough to run.
pub fn coverage_gaps(
    playbook: &Playbook,
    doubles: &Doubles,
    facts: &BTreeMap<String, ConnectorFacts>,
) -> Vec<String> {
    let (bound, mut out) = bound_functions(playbook, facts);
    for (c, f) in &bound {
        let answered = doubles
            .get(c)
            .and_then(|fns| fns.get(f))
            .is_some_and(|a| !a.is_empty());
        if !answered {
            out.push(format!(
                "`{c}.{f}` is bound but the case gives it no double"
            ));
        }
    }
    for (c, fns) in doubles {
        // An entry with no function still turns doubles on: it is refused,
        // never a doubled run that answers nothing.
        if fns.is_empty() {
            out.push(format!(
                "`{c}` is named in `connectors:` but doubles no function"
            ));
        }
        for f in fns.keys() {
            if !bound.contains(&(c.clone(), f.clone())) {
                out.push(format!(
                    "`{c}.{f}` has a double, but no binding site of this version binds it"
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::def::MockSpec;
    use serde_json::json;

    fn answer(times: Option<u32>, matcher: Option<DoubleMatch>, status: u16) -> DoubleAnswer {
        DoubleAnswer {
            matcher,
            answer: MockSpec {
                status,
                body: json!({}),
            },
            times,
        }
    }

    fn facts() -> BTreeMap<String, ConnectorFacts> {
        BTreeMap::from([(
            "crm".to_string(),
            ConnectorFacts {
                functions: vec![
                    "list".to_string(),
                    "get".to_string(),
                    "update".to_string(),
                    "post".to_string(),
                ],
                write_functions: vec!["update".to_string(), "post".to_string()],
                reconcile_functions: BTreeMap::from([("post".to_string(), "list".to_string())]),
                identity_function: Some("get".to_string()),
                ..Default::default()
            },
        )])
    }

    fn pb(binding: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: P\nversion: 1.0.0\ndefaults: {{ profile: dev }}\nnodes:\n  - {{ id: start, type: start }}\n  - {{ id: w, type: agent_task, prompt: p, connectors: [{binding}] }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: w }}\n  - {{ from: w, to: done }}\n"
        ))
        .unwrap()
    }

    fn set(items: &[&str]) -> BTreeSet<(String, String)> {
        items
            .iter()
            .map(|s| {
                let (c, f) = s.split_once('.').unwrap();
                (c.to_string(), f.to_string())
            })
            .collect()
    }

    #[test]
    fn args_hold_is_a_subset_match() {
        let args = json!({"a": 1, "b": {"c": "x", "d": [1, 2]}, "e": true});
        assert!(args_hold(&json!({}), &args));
        assert!(args_hold(&json!({"a": 1}), &args));
        assert!(args_hold(&json!({"b": {"c": "x"}}), &args));
        assert!(args_hold(&json!({"b": {"d": [1, 2]}}), &args));
        assert!(!args_hold(&json!({"b": {"d": [1]}}), &args));
        assert!(!args_hold(&json!({"a": 2}), &args));
        assert!(!args_hold(&json!({"z": 1}), &args));
        assert!(!args_hold(&json!({"a": "1"}), &args));
        assert!(!args_hold(&json!({"b": {"c": "x"}}), &json!({"b": "x"})));
    }

    #[test]
    fn args_contain_reads_string_leaves_only() {
        let args =
            json!({"a": "hello world", "b": [{"c": "deep text"}], "n": 123, "t": true, "z": null});
        assert!(args_contain(&args, "world"));
        assert!(args_contain(&args, "deep"));
        assert!(!args_contain(&args, "123"));
        assert!(!args_contain(&args, "true"));
        assert!(!args_contain(&args, "null"));
        assert!(!args_contain(&args, "World"));
        // Keys are not string leaves.
        assert!(!args_contain(&json!({"needle": 1}), "needle"));
    }

    #[test]
    fn pick_walks_answers_in_order_with_times() {
        let seq = vec![answer(Some(1), None, 200), answer(None, None, 201)];
        assert_eq!(pick(&seq, "w", &json!({}), &[0, 0]), Some(0));
        assert_eq!(pick(&seq, "w", &json!({}), &[1, 0]), Some(1));
        assert_eq!(pick(&seq, "w", &json!({}), &[1, 7]), Some(1));

        let narrowed = vec![
            answer(
                None,
                Some(DoubleMatch {
                    args: Some(json!({"id": 1})),
                    ..Default::default()
                }),
                200,
            ),
            answer(
                Some(2),
                Some(DoubleMatch {
                    site: Some("w".into()),
                    contains: Some("hi".into()),
                    ..Default::default()
                }),
                201,
            ),
        ];
        assert_eq!(pick(&narrowed, "w", &json!({"id": 1}), &[0, 0]), Some(0));
        assert_eq!(
            pick(&narrowed, "w", &json!({"id": 2, "t": "hi"}), &[0, 0]),
            Some(1)
        );
        assert_eq!(
            pick(&narrowed, "w", &json!({"id": 2, "t": "hi"}), &[0, 2]),
            None
        );
        assert_eq!(
            pick(&narrowed, "x", &json!({"id": 2, "t": "hi"}), &[0, 0]),
            None
        );
        assert_eq!(pick(&narrowed, "w", &json!({"id": 2}), &[0, 0]), None);

        let empty_match = vec![answer(None, Some(DoubleMatch::default()), 200)];
        assert_eq!(pick(&empty_match, "any", &json!(null), &[0]), Some(0));
        assert_eq!(pick(&[], "w", &json!({}), &[]), None);
    }

    #[test]
    fn bound_functions_reads_all_read_only_and_lists() {
        let f = facts();
        let (all, problems) = bound_functions(&pb("{ name: crm }"), &f);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(all, set(&["crm.list", "crm.get", "crm.update", "crm.post"]));

        let (ro, _) = bound_functions(&pb("{ name: crm, functions: read_only }"), &f);
        assert_eq!(ro, set(&["crm.list", "crm.get"]));

        let (list, _) = bound_functions(&pb("{ name: crm, functions: [get] }"), &f);
        assert_eq!(list, set(&["crm.get"]));

        let (none, problems) = bound_functions(&pb("{ name: other }"), &f);
        assert!(none.is_empty());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("`other`"), "{problems:?}");
    }

    #[test]
    fn bound_functions_adds_reconcile_functions() {
        let (bound, problems) = bound_functions(&pb("{ name: crm, functions: [post] }"), &facts());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(bound, set(&["crm.post", "crm.list", "crm.get"]));
    }

    #[test]
    fn coverage_gaps_names_missing_connectors_and_functions() {
        let p = pb("{ name: crm, functions: [post] }");
        let mut doubles: Doubles = BTreeMap::new();
        doubles
            .entry("crm".into())
            .or_default()
            .insert("post".into(), vec![answer(None, None, 200)]);
        doubles
            .entry("crm".into())
            .or_default()
            .insert("update".into(), vec![answer(None, None, 200)]);
        let gaps = coverage_gaps(&p, &doubles, &facts());
        assert_eq!(gaps.len(), 3, "{gaps:?}");
        assert!(
            gaps.iter()
                .any(|g| g.contains("`crm.list`") && g.contains("no double")),
            "{gaps:?}"
        );
        // A marker reconcile reads the own identity first: it is bound too.
        assert!(
            gaps.iter()
                .any(|g| g.contains("`crm.get`") && g.contains("no double")),
            "{gaps:?}"
        );
        assert!(
            gaps.iter()
                .any(|g| g.contains("`crm.update`") && g.contains("no binding site")),
            "{gaps:?}"
        );

        doubles.get_mut("crm").unwrap().remove("update");
        doubles
            .get_mut("crm")
            .unwrap()
            .insert("list".into(), vec![answer(None, None, 200)]);
        doubles
            .get_mut("crm")
            .unwrap()
            .insert("get".into(), vec![answer(None, None, 200)]);
        assert!(coverage_gaps(&p, &doubles, &facts()).is_empty());

        // An empty answer list is no double.
        doubles
            .get_mut("crm")
            .unwrap()
            .insert("list".into(), vec![]);
        assert_eq!(coverage_gaps(&p, &doubles, &facts()).len(), 1);

        // A connector entry with no function is refused.
        let mut empty = doubles.clone();
        empty
            .get_mut("crm")
            .unwrap()
            .insert("list".into(), vec![answer(None, None, 200)]);
        empty.insert("crmm".into(), BTreeMap::new());
        let gaps = coverage_gaps(&p, &empty, &facts());
        assert!(
            gaps.iter()
                .any(|g| g.contains("`crmm`") && g.contains("doubles no function")),
            "{gaps:?}"
        );

        // Without the installed connectors nothing is guessed.
        let gaps = coverage_gaps(&p, &doubles, &BTreeMap::new());
        assert!(
            gaps.iter()
                .any(|g| g.contains("connector `crm` is not installed")),
            "{gaps:?}"
        );
    }
}
