//! The output contract of a typed declared field (spec node-contracts D1).
//!
//! A node's `outputs.fields` and a list's `item_outputs` accept, per entry,
//! either a plain name or `{ name, type, values, required }`. When an attempt
//! reports success, the engine runs the contract over the output and rejects
//! the attempt on a mismatch: non-fatal, so `rejected_for` and the retry
//! briefing name it. The reason never holds the offending value, whole or in
//! part: a cut value no longer matches the full secret the scrubber looks
//! for, and the reason is journaled and sent in the retry prompt.
//!
//! A plain `Name` entry is never checked here; its absence stays the
//! `output_fields_missing` warning. The reason text depends only on the
//! declaration and the output, never on time or paths, so the stall
//! fingerprint of a repeated mismatch is stable.

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

/// The prefix of every contract reason.
pub const OUTPUT_CONTRACT: &str = "output_contract";

/// What one field of the output got instead of what its declaration wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The output is not a JSON object, so nothing can be type-checked and
    /// every required field is missing.
    NotObject,
    /// A required field absent or `null`.
    Missing,
    /// The value's JSON kind is not the declared one (an `enum` field never
    /// produces this: it produces [`Problem::NotInValues`] instead).
    WrongType { want: FieldType, got: &'static str },
    /// The value's text is outside the declared `values` of an `enum` field.
    NotInValues { got: &'static str },
}

/// One field of the declaration the output did not satisfy, in declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub field: String,
    pub problem: Problem,
}

/// The mismatches of `output` against `fields`. Only the typed entries are
/// checked, in declaration order. When no entry carries a `type` or
/// `required: true`, nothing is parsed and nothing is checked (rule 1).
pub fn check(fields: &[FieldDecl], output: &str) -> Vec<Mismatch> {
    let mut out = Vec::new();
    if !has_contract(fields) {
        return out;
    }
    let parsed: serde_json::Value = match serde_json::from_str(output) {
        Ok(v) => v,
        Err(_) => {
            // Rule 2: the output is not a JSON object. One NotObject mismatch
            // when any entry is required; otherwise empty, because the
            // `output_fields_missing` warning path covers the plain names.
            if fields.iter().any(is_required_entry) {
                out.push(Mismatch {
                    field: String::new(),
                    problem: Problem::NotObject,
                });
            }
            return out;
        }
    };
    let Some(obj) = parsed.as_object() else {
        if fields.iter().any(is_required_entry) {
            out.push(Mismatch {
                field: String::new(),
                problem: Problem::NotObject,
            });
        }
        return out;
    };
    for f in fields {
        let Some(t) = f.typed() else { continue };
        let value = obj.get(&t.name);
        // Rule 3: absent or null.
        let value = match value {
            None | Some(serde_json::Value::Null) => {
                if t.required {
                    out.push(Mismatch {
                        field: t.name.clone(),
                        problem: Problem::Missing,
                    });
                }
                continue;
            }
            Some(v) => v,
        };
        check_one(&t.name, t, value, &mut out);
    }
    out
}

/// Whether `fields` names a contract at all: at least one entry typed with a
/// `type` or `required: true` (rule 1).
fn has_contract(fields: &[FieldDecl]) -> bool {
    fields
        .iter()
        .any(|f| f.typed().is_some_and(|t| t.kind.is_some() || t.required))
}

/// Whether the entry must be present: a typed entry with `required: true`.
fn is_required_entry(f: &FieldDecl) -> bool {
    f.typed().is_some_and(|t| t.required)
}

/// One present value against one typed entry (rule 4).
fn check_one(
    name: &str,
    t: &crate::schema::TypedFieldDecl,
    value: &serde_json::Value,
    out: &mut Vec<Mismatch>,
) {
    let got = json_kind(value);
    match t.kind {
        None => {}
        Some(FieldType::String) => {
            if !value.is_string() {
                out.push(Mismatch {
                    field: name.to_string(),
                    problem: Problem::WrongType {
                        want: FieldType::String,
                        got,
                    },
                });
            }
        }
        Some(FieldType::Number) => {
            let ok = numeric(value).is_some_and(f64::is_finite);
            if !ok {
                out.push(Mismatch {
                    field: name.to_string(),
                    problem: Problem::WrongType {
                        want: FieldType::Number,
                        got,
                    },
                });
            }
        }
        Some(FieldType::Bool) => {
            let ok = matches!(value, serde_json::Value::Bool(_))
                || matches!(value, serde_json::Value::String(s) if s == "true" || s == "false");
            if !ok {
                out.push(Mismatch {
                    field: name.to_string(),
                    problem: Problem::WrongType {
                        want: FieldType::Bool,
                        got,
                    },
                });
            }
        }
        Some(FieldType::Array) | Some(FieldType::Object) => {
            let want_array = matches!(t.kind, Some(FieldType::Array));
            let ok = json_in(value).is_some_and(|v| {
                if want_array {
                    v.is_array()
                } else {
                    v.is_object()
                }
            });
            if !ok {
                out.push(Mismatch {
                    field: name.to_string(),
                    problem: Problem::WrongType {
                        want: t.kind.unwrap(),
                        got,
                    },
                });
            }
        }
        Some(FieldType::Enum) => {
            // The value's text, as `output_field_value` reads it: the string
            // itself, or the JSON text of a number or a bool. An array, an
            // object or a text outside `values` is NotInValues.
            let text = match value {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            let ok = text.is_some_and(|text| t.values.iter().any(|v| v == &text));
            if !ok {
                out.push(Mismatch {
                    field: name.to_string(),
                    problem: Problem::NotInValues { got },
                });
            }
        }
    }
}

/// The JSON kind of `value`, as the reasons name it.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The reason of the mismatches: `None` when there are none. The text
/// depends only on the declaration and the output, never on time or paths,
/// so the stall fingerprint of a repeated mismatch is stable. It names the
/// field, the expected type and the JSON kind it got, and never the
/// offending value, whole or in part.
pub fn reason(fields: &[FieldDecl], mismatches: &[Mismatch]) -> Option<String> {
    if mismatches.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for m in mismatches {
        match &m.problem {
            Problem::NotObject => {
                let names: Vec<String> = fields
                    .iter()
                    .filter(|f| is_required_entry(f))
                    .map(|f| format!("`{}`", f.name()))
                    .collect();
                parts.push(format!(
                    "the output is not a JSON object, so the required fields {} are missing",
                    names.join(", ")
                ));
            }
            Problem::Missing => {
                parts.push(format!("field `{}` is required and missing", m.field));
            }
            Problem::WrongType { want, got } => {
                parts.push(format!(
                    "field `{}` must be {}, got {}",
                    m.field,
                    want_phrase(*want, &[]),
                    got_phrase(got)
                ));
            }
            Problem::NotInValues { got } => {
                let values: Vec<String> = fields
                    .iter()
                    .find(|f| f.name() == m.field)
                    .and_then(|f| f.typed())
                    .map(|t| t.values.iter().map(|v| format!("`{v}`")).collect())
                    .unwrap_or_default();
                parts.push(format!(
                    "field `{}` must be one of {}, got {} outside them",
                    m.field,
                    values.join(", "),
                    got_phrase(got)
                ));
            }
        }
    }
    Some(format!("{OUTPUT_CONTRACT}: {}", parts.join("; ")))
}

/// The phrase for a declared kind in a reason or the status-file note:
/// string "a string", number "a number", bool "true or false",
/// array "a JSON array", object "a JSON object", enum "one of `a`, `b`".
pub fn want_phrase(want: FieldType, values: &[String]) -> String {
    match want {
        FieldType::String => "a string".to_string(),
        FieldType::Number => "a number".to_string(),
        FieldType::Bool => "true or false".to_string(),
        FieldType::Array => "a JSON array".to_string(),
        FieldType::Object => "a JSON object".to_string(),
        FieldType::Enum => format!(
            "one of {}",
            values
                .iter()
                .map(|v| format!("`{v}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The phrase for the JSON kind a value has: "a string", "a number",
/// "a bool", "an array", "an object".
pub fn got_phrase(got: &str) -> String {
    match got {
        "array" | "object" => format!("an {got}"),
        _ => format!("a {got}"),
    }
}

/// The JSON kind a typed output field declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Number,
    Bool,
    Array,
    Object,
    Enum,
}

/// One typed entry of `outputs.fields` or `list.item_outputs`: the name, the
/// JSON kind the value must have, the allowed values of an `enum` and whether
/// the field must be present. The engine checks it when the attempt reports
/// success; a mismatch rejects the attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedFieldDecl {
    pub name: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FieldType>,
    /// The allowed values of an `enum` field, compared as text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

/// One declared output field: today's plain name, or a typed entry.
/// `untagged`, so `fields: [a, b]` parses and serializes exactly as before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldDecl {
    Name(String),
    Typed(TypedFieldDecl),
}

impl FieldDecl {
    /// The field's name, plain or typed.
    pub fn name(&self) -> &str {
        match self {
            FieldDecl::Name(n) => n,
            FieldDecl::Typed(t) => &t.name,
        }
    }

    /// The typed entry when this is one, else `None`.
    pub fn typed(&self) -> Option<&TypedFieldDecl> {
        match self {
            FieldDecl::Name(_) => None,
            FieldDecl::Typed(t) => Some(t),
        }
    }
}

/// The names of `fields`, in declaration order.
pub fn field_names(fields: &[FieldDecl]) -> Vec<String> {
    fields.iter().map(|f| f.name().to_string()).collect()
}

/// A JSON number, or a string that is exactly the text of a JSON number
/// (rule 5): `"80"`, `"-1.5"` and `"1e3"` are numbers; `" 80"`, `"+80"`,
/// `"inf"`, `"NaN"`, `"0x10"` and `""` are not. Never `str::parse::<f64>`.
pub fn numeric(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            if s.trim() != s {
                return None;
            }
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(serde_json::Value::Number(n)) => n.as_f64(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// An array or an object, or a string that parses as one. A string holding
/// a number, a bool or `null` is `None`.
pub fn json_in(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        v @ (serde_json::Value::Array(_) | serde_json::Value::Object(_)) => Some(v.clone()),
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .filter(|v| v.is_array() || v.is_object()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TypedFieldDecl;
    use serde_json::json;

    fn typed(name: &str, kind: Option<FieldType>, values: &[&str], required: bool) -> FieldDecl {
        FieldDecl::Typed(TypedFieldDecl {
            name: name.to_string(),
            kind,
            values: values.iter().map(|s| s.to_string()).collect(),
            required,
        })
    }

    fn plain(name: &str) -> FieldDecl {
        FieldDecl::Name(name.to_string())
    }

    // --- numeric (rule 5) ---------------------------------------------------

    #[test]
    fn numeric_reads_json_numbers_and_exact_numeric_strings() {
        assert_eq!(numeric(&json!(80)), Some(80.0));
        assert_eq!(numeric(&json!(-1.5)), Some(-1.5));
        assert_eq!(numeric(&json!("80")), Some(80.0));
        assert_eq!(numeric(&json!("1e3")), Some(1000.0));
        assert_eq!(numeric(&json!("-1.5e-2")), Some(-0.015));
    }

    #[test]
    fn numeric_refuses_everything_that_is_not_a_json_number_text() {
        for v in [
            json!(" 80"),
            json!("+80"),
            json!("inf"),
            json!("NaN"),
            json!("0x10"),
            json!(""),
            json!(true),
            serde_json::Value::Null,
        ] {
            assert_eq!(numeric(&v), None);
        }
    }

    // --- json_in -------------------------------------------------------------

    #[test]
    fn json_in_reads_a_container_or_a_string_holding_one() {
        assert_eq!(json_in(&json!([1, 2])), Some(json!([1, 2])));
        assert_eq!(json_in(&json!({ "a": 1 })), Some(json!({ "a": 1 })));
        assert_eq!(json_in(&json!("[1, 2]")), Some(json!([1, 2])));
        assert_eq!(json_in(&json!("{\"a\": 1}")), Some(json!({"a": 1})));
    }

    #[test]
    fn json_in_refuses_a_string_holding_a_number() {
        assert_eq!(json_in(&json!("42")), None);
        assert_eq!(json_in(&json!("true")), None);
        assert!(json_in(&json!("null")).is_none());
        assert_eq!(json_in(&json!(42)), None);
        assert_eq!(json_in(&json!(true)), None);
    }

    // --- check per type (rule 4) ----------------------------------------------

    #[test]
    fn check_accepts_a_value_of_each_declared_type() {
        let fields = vec![
            typed("s", Some(FieldType::String), &[], false),
            typed("n", Some(FieldType::Number), &[], false),
            typed("b", Some(FieldType::Bool), &[], false),
            typed("a", Some(FieldType::Array), &[], false),
            typed("o", Some(FieldType::Object), &[], false),
            typed("e", Some(FieldType::Enum), &["x", "y"], false),
        ];
        let output = r#"{"s": "hi", "n": "80", "b": "true", "a": [1], "o": {"k": 1}, "e": "x"}"#;
        assert!(check(&fields, output).is_empty());
    }

    #[test]
    fn check_rejects_a_wrong_value_of_each_declared_type() {
        let fields = vec![
            typed("s", Some(FieldType::String), &[], false),
            typed("n", Some(FieldType::Number), &[], false),
            typed("b", Some(FieldType::Bool), &[], false),
            typed("a", Some(FieldType::Array), &[], false),
            typed("o", Some(FieldType::Object), &[], false),
            typed("e", Some(FieldType::Enum), &["x", "y"], false),
        ];
        let output = r#"{"s": 3, "n": "large", "b": "yes", "a": 4, "o": 5, "e": "z"}"#;
        let m = check(&fields, output);
        assert_eq!(m.len(), 6);
        assert_eq!(m[0].field, "s");
        assert_eq!(m[1].field, "n");
        assert_eq!(m[2].field, "b");
        assert_eq!(m[3].field, "a");
        assert_eq!(m[4].field, "o");
        assert_eq!(m[5].field, "e");
    }

    #[test]
    fn required_field_absent_or_null_is_missing() {
        let fields = vec![typed("r", Some(FieldType::String), &[], true)];
        let m = check(&fields, "{}");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].problem, Problem::Missing);
        let m = check(&fields, r#"{"r": null}"#);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].problem, Problem::Missing);
    }

    #[test]
    fn optional_absent_field_is_not_checked() {
        let fields = vec![typed("o", Some(FieldType::Number), &[], false)];
        assert!(check(&fields, "{}").is_empty());
        assert!(check(&fields, r#"{"o": null}"#).is_empty());
    }

    #[test]
    fn output_not_an_object_yields_one_mismatch_only_when_an_entry_is_required() {
        let with_req = vec![
            plain("notes"),
            typed("blocking", Some(FieldType::Number), &[], true),
        ];
        for output in ["not json", "[1]", "3", "\"x\"", "null"] {
            let m = check(&with_req, output);
            assert_eq!(m.len(), 1, "{output}");
            assert_eq!(m[0].problem, Problem::NotObject);
        }
        let without_req = vec![
            plain("notes"),
            typed("o", Some(FieldType::Number), &[], false),
        ];
        for output in ["not json", "[1]", "null"] {
            assert!(check(&without_req, output).is_empty(), "{output}");
        }
    }

    #[test]
    fn plain_only_list_never_yields_a_mismatch() {
        let fields = vec![plain("a"), plain("b")];
        assert!(check(&fields, "not json").is_empty());
        assert!(check(&fields, "{}").is_empty());
    }

    // --- reason ---------------------------------------------------------------

    #[test]
    fn reason_texts_match_design_d1_exactly() {
        let fields = vec![
            typed("role", Some(FieldType::Enum), &["correctness"], true),
            typed("blocking", Some(FieldType::Number), &[], true),
            typed("findings", Some(FieldType::Array), &[], true),
        ];
        let output = r#"{"role": "correctness", "blocking": "two", "findings": "none"}"#;
        let mismatches = check(&fields, output);
        assert_eq!(
            reason(&fields, &mismatches).unwrap(),
            "output_contract: field `blocking` must be a number, got a string; field `findings` must be a JSON array, got a string"
        );
    }

    #[test]
    fn reason_for_enum_outside_values() {
        let fields = vec![typed("role", Some(FieldType::Enum), &["correctness"], true)];
        let mismatches = check(&fields, r#"{"role": "wrong"}"#);
        assert_eq!(
            reason(&fields, &mismatches).unwrap(),
            "output_contract: field `role` must be one of `correctness`, got a string outside them"
        );
    }

    #[test]
    fn reason_for_not_an_object_lists_required_fields() {
        let fields = vec![
            plain("notes"),
            typed("blocking", Some(FieldType::Number), &[], true),
            typed("findings", Some(FieldType::Array), &[], true),
        ];
        let mismatches = check(&fields, "not json");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].problem, Problem::NotObject);
        assert_eq!(
            reason(&fields, &mismatches).unwrap(),
            "output_contract: the output is not a JSON object, so the required fields `blocking`, `findings` are missing"
        );
    }

    #[test]
    fn reason_is_none_without_mismatches() {
        let fields = vec![typed("n", Some(FieldType::Number), &[], true)];
        assert!(reason(&fields, &[]).is_none());
    }

    #[test]
    fn mismatches_come_in_declaration_order() {
        let fields = vec![
            typed("aa", Some(FieldType::Number), &[], true),
            typed("bb", Some(FieldType::String), &[], true),
            typed("cc", Some(FieldType::Number), &[], true),
        ];
        let output = r#"{"aa": "x", "bb": 3, "cc": false}"#;
        let m = check(&fields, output);
        assert_eq!(
            m.iter().map(|x| x.field.as_str()).collect::<Vec<_>>(),
            ["aa", "bb", "cc"]
        );
    }

    #[test]
    fn reason_never_holds_the_offending_value() {
        let secret: String = "k".repeat(200);
        let output = format!(r#"{{"n": "{secret}"}}"#);
        let fields = vec![typed("n", Some(FieldType::Number), &[], true)];
        let m = check(&fields, &output);
        let r = reason(&fields, &m).unwrap();
        assert!(!r.contains(&secret[0..8]));
    }
}
