//! The `judge` node and the `judge` edge condition (issue #165 Parts 5 and
//! 7): the playbook-side types, shared by the schema, the validator and the
//! engine.
//!
//! A judge node asks a decision model typed questions over a small named
//! state and publishes the answers as a compact JSON object, so the existing
//! `output_field` edges and `{{nodes.<id>.output.<field>}}` templates route on
//! them. The playbook names no provider, URL, model or key: the machine's
//! `decisions.yaml` decides whether and where the questions are asked, and
//! `on_unavailable` says what the node does when they are not answered.
//!
//! Order matters on the wire (a model reads the options and the state in the
//! order they are sent), so every map here keeps the order it was written in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::profile::QualifiedProfileRef;

/// The output field every judge node writes: who produced the answers
/// (`<provider>/<model>`, `emulated:<provider>`, `default` or `unavailable`).
pub const DECIDED_BY: &str = "decided_by";
/// The output field naming why the declared fallback was taken.
pub const REASON: &str = "reason";
/// `decided_by` of a node that took `on_unavailable: { route: ... }`.
pub const UNAVAILABLE: &str = "unavailable";
/// `decided_by` of a node that took `on_unavailable: { default: ... }`.
pub const DEFAULT: &str = "default";
/// Most questions one judge node may ask.
pub const MAX_QUESTIONS: usize = 32;
/// Most options a `choice` question may carry.
pub const MAX_OPTIONS: usize = 255;
/// Fewest and most levels a `score` question may carry.
pub const MIN_LEVELS: usize = 2;
pub const MAX_LEVELS: usize = 10;
/// The option names that give a `choice` an honest way out.
pub const ESCAPE_OPTIONS: [&str; 3] = ["unclear", "other", "none"];
/// A `noul` answer at or above this probability is a yes, unless the node
/// declares its own `yes_at`.
pub const DEFAULT_YES_AT: f64 = 0.5;

/// A string-keyed map that keeps the order it was written in.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderedMap<V>(pub Vec<(String, V)>);

impl<V> OrderedMap<V> {
    pub fn get(&self, key: &str) -> Option<&V> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<V: Serialize> Serialize for OrderedMap<V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for OrderedMap<V> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor<V>(std::marker::PhantomData<V>);
        impl<'de, V: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<V> {
            type Value = OrderedMap<V>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut m: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out: Vec<(String, V)> = Vec::new();
                while let Some((k, v)) = m.next_entry::<ScalarKey, V>()? {
                    // A repeated key keeps its first position and its last value.
                    match out.iter_mut().find(|(x, _)| *x == k.0) {
                        Some(slot) => slot.1 = v,
                        None => out.push((k.0, v)),
                    }
                }
                Ok(OrderedMap(out))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(OrderedMap(Vec::new()))
            }
        }
        d.deserialize_any(Visitor(std::marker::PhantomData))
    }
}

/// A map key read as a string whatever scalar YAML made of it (`true:`,
/// `3:`), so a `noul` criteria map and a numeric option name both load.
struct ScalarKey(String);

impl<'de> Deserialize<'de> for ScalarKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = ScalarKey;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a scalar key")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ScalarKey, E> {
                Ok(ScalarKey(v.to_string()))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<ScalarKey, E> {
                Ok(ScalarKey(v.to_string()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<ScalarKey, E> {
                Ok(ScalarKey(v.to_string()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<ScalarKey, E> {
                Ok(ScalarKey(v.to_string()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<ScalarKey, E> {
                Ok(ScalarKey(v.to_string()))
            }
        }
        d.deserialize_any(Visitor)
    }
}

/// A judge node's questions: a map from question id to question. A list is
/// read too, so the validator can say what is wrong (V50) instead of the
/// playbook failing to parse; the engine never runs a list-shaped node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JudgeQuestions {
    pub entries: Vec<(String, JudgeQuestion)>,
    /// Written as a YAML list rather than a map.
    pub list_shaped: bool,
}

impl JudgeQuestions {
    pub fn get(&self, id: &str) -> Option<&JudgeQuestion> {
        self.entries.iter().find(|(k, _)| k == id).map(|(_, q)| q)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &JudgeQuestion)> {
        self.entries.iter().map(|(k, q)| (k.as_str(), q))
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Serialize for JudgeQuestions {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        if self.list_shaped {
            // Written back the way it was read, so the validator's finding
            // survives a round trip through the dashboard.
            let mut seq = s.serialize_seq(Some(self.entries.len()))?;
            for (id, q) in &self.entries {
                let mut v = serde_json::to_value(q).map_err(serde::ser::Error::custom)?;
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("id".into(), serde_json::Value::String(id.clone()));
                }
                seq.serialize_element(&v)?;
            }
            return seq.end();
        }
        let mut map = s.serialize_map(Some(self.entries.len()))?;
        for (k, v) in &self.entries {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for JudgeQuestions {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        use serde_yaml_ng::Value;
        // Buffered as a YAML value, whose maps admit any scalar key (a noul's
        // `true:` / `false:` criteria are booleans to a YAML parser).
        let raw = Value::deserialize(d)?;
        match raw {
            Value::Null => Ok(JudgeQuestions::default()),
            Value::Mapping(_) => {
                let map: OrderedMap<JudgeQuestion> =
                    OrderedMap::deserialize(raw).map_err(D::Error::custom)?;
                Ok(JudgeQuestions {
                    entries: map.0,
                    list_shaped: false,
                })
            }
            Value::Sequence(items) => {
                let mut entries = Vec::new();
                for (i, mut item) in items.into_iter().enumerate() {
                    let id = item
                        .as_mapping_mut()
                        .and_then(|o| o.remove("id"))
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| format!("q{}", i + 1));
                    let q = JudgeQuestion::deserialize(item).map_err(D::Error::custom)?;
                    entries.push((id, q));
                }
                Ok(JudgeQuestions {
                    entries,
                    list_shaped: true,
                })
            }
            _ => Err(D::Error::custom(
                "questions must be a map of question id to question",
            )),
        }
    }
}

/// One typed question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum JudgeQuestion {
    /// One of the named options; a criterion describes its option (`null`
    /// lets the name speak for itself).
    Choice {
        #[serde(default)]
        instructions: String,
        #[serde(default)]
        criteria: OrderedMap<Option<String>>,
    },
    /// A position on 2 to 10 ordered levels, lowest first, each described in
    /// words (the model never sees their numbers).
    Score {
        #[serde(default)]
        instructions: String,
        #[serde(default)]
        levels: Vec<String>,
    },
    /// The probability of "yes". Optional criteria describe the two outcomes
    /// under the keys `true` and `false`.
    Noul {
        #[serde(default)]
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<OrderedMap<String>>,
    },
}

impl JudgeQuestion {
    pub fn type_str(&self) -> &'static str {
        match self {
            JudgeQuestion::Choice { .. } => "choice",
            JudgeQuestion::Score { .. } => "score",
            JudgeQuestion::Noul { .. } => "noul",
        }
    }

    pub fn instructions(&self) -> &str {
        match self {
            JudgeQuestion::Choice { instructions, .. }
            | JudgeQuestion::Score { instructions, .. }
            | JudgeQuestion::Noul { instructions, .. } => instructions,
        }
    }
}

/// How one question's answer becomes an output value (computed in code,
/// never by the model). Which keys apply depends on the question type: a
/// `choice` takes `min_confidence` and `below`, a `noul` takes `yes_at`, a
/// `score` takes `bands`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeThreshold {
    /// Below this confidence the answer is `below` instead of the model's pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    /// The safe option taken below `min_confidence`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<String>,
    /// A `noul` is `true` at or above this probability (default 0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yes_at: Option<f64>,
    /// Named `[lo, hi)` ranges over the expected level index (0 = lowest);
    /// the last band is closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<OrderedMap<[f64; 2]>>,
}

/// What a judge node does when its questions are not answered: the provider
/// is down, times out or is out of budget, an answer is invalid, or the
/// machine's `judge_node` use is below `enforce` (or not configured at all).
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeFallback {
    /// Succeed with `{"decided_by":"unavailable","reason":...}`; the route to
    /// the named node is an `output_field` edge on `decided_by`.
    Route(String),
    /// Succeed with these values (by question id) and `decided_by: default`.
    Default(BTreeMap<String, serde_json::Value>),
    /// Fail like any node: retries, failure edges and supervisor park apply.
    Fail,
    /// Ask an emulation backend: the configured `llm_emulation` providers,
    /// then the node's `profile`. Fails the node when that fails too.
    Emulate,
    /// A word this apb does not know (reported by the validator).
    Unknown(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteForm {
    route: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultForm {
    default: BTreeMap<String, serde_json::Value>,
}

impl Serialize for JudgeFallback {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            JudgeFallback::Route(node) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("route", node)?;
                m.end()
            }
            JudgeFallback::Default(values) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("default", values)?;
                m.end()
            }
            JudgeFallback::Fail => s.serialize_str("fail"),
            JudgeFallback::Emulate => s.serialize_str("emulate"),
            JudgeFallback::Unknown(word) => s.serialize_str(word),
        }
    }
}

impl<'de> Deserialize<'de> for JudgeFallback {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = serde_json::Value::deserialize(d)?;
        match &raw {
            serde_json::Value::String(word) => Ok(match word.as_str() {
                "fail" => JudgeFallback::Fail,
                "emulate" => JudgeFallback::Emulate,
                other => JudgeFallback::Unknown(other.to_string()),
            }),
            serde_json::Value::Object(o) if o.contains_key("route") => {
                serde_json::from_value::<RouteForm>(raw)
                    .map(|f| JudgeFallback::Route(f.route))
                    .map_err(D::Error::custom)
            }
            serde_json::Value::Object(o) if o.contains_key("default") => {
                serde_json::from_value::<DefaultForm>(raw)
                    .map(|f| JudgeFallback::Default(f.default))
                    .map_err(D::Error::custom)
            }
            _ => Err(D::Error::custom(
                "on_unavailable must be fail, emulate, { route: <node> } or { default: { <question>: <value> } }",
            )),
        }
    }
}

/// A probability written in a playbook (`min_p`). Compared and hashed by
/// its bits, so an edge condition carrying one can still be a routing key.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Probability(pub f64);

impl PartialEq for Probability {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}
impl Eq for Probability {}
impl std::hash::Hash for Probability {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.0.to_bits().hash(h);
    }
}

/// The output fields a judge node publishes for its questions, in question
/// order, plus `decided_by` and `reason`: what an `output_field` edge or a
/// `{{nodes.<id>.output.<field>}}` template may read.
pub fn output_fields(
    questions: &JudgeQuestions,
    thresholds: &OrderedMap<JudgeThreshold>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (id, q) in questions.iter() {
        match q {
            JudgeQuestion::Choice { .. } => {
                out.extend([
                    id.to_string(),
                    format!("{id}_p"),
                    format!("{id}_confidence"),
                ]);
            }
            JudgeQuestion::Noul { .. } => out.extend([id.to_string(), format!("{id}_p")]),
            JudgeQuestion::Score { .. } => {
                if thresholds.get(id).is_some_and(|t| t.bands.is_some()) {
                    out.push(id.to_string());
                }
                out.push(format!("{id}_score"));
            }
        }
    }
    out.extend([DECIDED_BY.to_string(), REASON.to_string()]);
    out
}

/// Where a judge node's `emulate` fallback runs: its own `profile`, else the
/// playbook's default profile (`defaults.profile`). `None` for any other fallback.
pub fn emulation_profile(
    on_unavailable: Option<&JudgeFallback>,
    profile: Option<&QualifiedProfileRef>,
    default_profile: Option<&QualifiedProfileRef>,
) -> Option<QualifiedProfileRef> {
    match on_unavailable {
        Some(JudgeFallback::Emulate) => profile.or(default_profile).cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct Holder {
        questions: JudgeQuestions,
        #[serde(default)]
        on_unavailable: Option<JudgeFallback>,
    }

    #[test]
    fn questions_keep_their_order_and_noul_keys_load_as_strings() {
        let h: Holder = serde_yaml_ng::from_str(
            "questions:\n  zeta: { type: noul, instructions: z, criteria: { true: yes, false: no } }\n  alpha:\n    type: choice\n    instructions: a\n    criteria: { b: x, a: null }\n",
        )
        .unwrap();
        let ids: Vec<&str> = h.questions.iter().map(|(k, _)| k).collect();
        assert_eq!(ids, ["zeta", "alpha"]);
        let JudgeQuestion::Noul { criteria, .. } = h.questions.get("zeta").unwrap() else {
            panic!()
        };
        assert_eq!(
            criteria.as_ref().unwrap().keys().collect::<Vec<_>>(),
            ["true", "false"]
        );
        let JudgeQuestion::Choice { criteria, .. } = h.questions.get("alpha").unwrap() else {
            panic!()
        };
        assert_eq!(criteria.keys().collect::<Vec<_>>(), ["b", "a"]);
        assert!(!h.questions.list_shaped);
    }

    #[test]
    fn a_list_of_questions_loads_marked_so_the_validator_can_refuse_it() {
        let h: Holder = serde_yaml_ng::from_str(
            "questions:\n  - { id: verdict, type: noul, instructions: x }\n  - { type: noul, instructions: y }\n",
        )
        .unwrap();
        assert!(h.questions.list_shaped);
        let ids: Vec<&str> = h.questions.iter().map(|(k, _)| k).collect();
        assert_eq!(ids, ["verdict", "q2"]);
    }

    #[test]
    fn every_fallback_form_round_trips() {
        for (yaml, want) in [
            ("fail", JudgeFallback::Fail),
            ("emulate", JudgeFallback::Emulate),
            ("{ route: human }", JudgeFallback::Route("human".into())),
            (
                "{ default: { verdict: unclear, risky: true } }",
                JudgeFallback::Default(BTreeMap::from([
                    ("risky".to_string(), serde_json::json!(true)),
                    ("verdict".to_string(), serde_json::json!("unclear")),
                ])),
            ),
            ("maybe", JudgeFallback::Unknown("maybe".into())),
        ] {
            let h: Holder =
                serde_yaml_ng::from_str(&format!("questions: {{}}\non_unavailable: {yaml}\n"))
                    .unwrap();
            assert_eq!(h.on_unavailable.as_ref(), Some(&want), "{yaml}");
            let back: Holder = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
            assert_eq!(back.on_unavailable, Some(want));
        }
        assert!(
            serde_yaml_ng::from_str::<Holder>(
                "questions: {}\non_unavailable: { route: a, x: 1 }\n"
            )
            .is_err()
        );
    }

    #[test]
    fn output_fields_follow_the_question_types() {
        let h: Holder = serde_yaml_ng::from_str(
            "questions:\n  v: { type: choice, instructions: i, criteria: { a: x, b: y } }\n  r: { type: noul, instructions: i }\n  e: { type: score, instructions: i, levels: [low, high] }\n",
        )
        .unwrap();
        let mut th = OrderedMap::default();
        assert_eq!(
            output_fields(&h.questions, &th),
            [
                "v",
                "v_p",
                "v_confidence",
                "r",
                "r_p",
                "e_score",
                "decided_by",
                "reason"
            ]
        );
        th.0.push((
            "e".into(),
            JudgeThreshold {
                bands: Some(OrderedMap(vec![("small".into(), [0.0, 1.0])])),
                ..Default::default()
            },
        ));
        assert!(output_fields(&h.questions, &th).contains(&"e".to_string()));
    }
}
