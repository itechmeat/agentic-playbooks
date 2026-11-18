//! The playbook-side types of a review gate's form (people-in-the-loop
//! D2/D3): `max_scope`, the typed `fields` and the `show` entries, with
//! the pure checks the channel and the drive share. The validator rules
//! for them live in `validate::review_form`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most fields one gate takes.
pub const MAX_FIELDS: usize = 20;
/// The most `show:` entries one gate has.
pub const MAX_SHOW: usize = 10;
/// The longest field name.
pub const FIELD_NAME_MAX: usize = 32;
/// The longest `show:` label, in characters.
pub const SHOW_LABEL_MAX: usize = 60;
/// The most bytes of one rendered `show:` text or default; more is cut.
pub const RENDERED_MAX_BYTES: usize = 2000;
/// A text field's `max_len` when none is given, and its upper bound.
pub const TEXT_DEFAULT_MAX_LEN: u32 = 2000;
pub const TEXT_MAX_LEN: u32 = 10_000;
/// The key of the decision in a gate's JSON output; no field may use it.
pub const DECISION_KEY: &str = "decision";

/// Options a gate does not offer above a scope (`human_review.max_scope`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MaxScope {
    /// Changed files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<u32>,
    /// Lines added plus lines removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    /// Connector writes plus send intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<u32>,
    /// The options withheld above a limit, as the author wrote them;
    /// `None` withholds `approve` when the gate offers it
    /// ([`MaxScope::withhold_for`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withhold: Option<Vec<String>>,
}

/// The option a `max_scope` without `withhold` withholds.
pub const DEFAULT_WITHHOLD: &str = "approve";

impl MaxScope {
    /// The options this limit withholds from a gate offering `offered`:
    /// the authored `withhold` list, else `approve` when the gate offers
    /// it, else nothing. Only offered options are returned.
    pub fn withhold_for(&self, offered: &[String]) -> Vec<String> {
        match &self.withhold {
            Some(list) => list
                .iter()
                .filter(|o| offered.contains(o))
                .cloned()
                .collect(),
            None => offered
                .iter()
                .filter(|o| o.as_str() == DEFAULT_WITHHOLD)
                .cloned()
                .collect(),
        }
    }
}

/// One value a person fills or corrects at a gate.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewField {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    /// `true`: every decision needs a value; a list: the decisions that do.
    #[serde(default, skip_serializing_if = "Required::is_off")]
    pub required: Required,
    /// A template over earlier outputs, rendered once per request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The allowed values of an `enum` field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The longest `text` value, in characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_len: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Number,
    Bool,
    Enum,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Required {
    Flag(bool),
    Options(Vec<String>),
}

impl Default for Required {
    fn default() -> Self {
        Required::Flag(false)
    }
}

impl Required {
    pub fn is_off(&self) -> bool {
        matches!(self, Required::Flag(false))
    }

    /// Whether a decision of `option` needs a value.
    pub fn applies(&self, option: &str) -> bool {
        match self {
            Required::Flag(b) => *b,
            Required::Options(o) => o.iter().any(|x| x == option),
        }
    }
}

/// One rendered earlier output a gate shows above its options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShowEntry {
    pub label: String,
    /// A template over earlier outputs, rendered once per request.
    pub text: String,
}

/// How a refusal names a field: `field \`<name>\`` when the name is in the
/// grammar the validator holds field names to (V244: `[a-z][a-z0-9_]*`, at
/// most [`FIELD_NAME_MAX`]) and is not shaped like a credential, else
/// `a field`. A refusal of this module carries nothing else from the
/// definition or from the post: no value, no option, no key, no decision
/// word; a field's kind and its numeric limit only.
fn field_ref(field: &ReviewField) -> String {
    let name = field.name.as_str();
    if nameable(name) {
        format!("field `{name}`")
    } else {
        "a field".to_string()
    }
}

/// Whether a message may repeat the field name `name`: it is in the
/// field-name grammar (V244: `[a-z][a-z0-9_]*`, at most [`FIELD_NAME_MAX`])
/// and not shaped like a credential.
pub fn nameable(name: &str) -> bool {
    let in_grammar = name.len() <= FIELD_NAME_MAX
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    in_grammar && !crate::token_shape::looks_like_credential(name)
}

/// How a validator message names the field `name` at 0-based `index` of
/// a gate's `fields`: `field \`<name>\`` when [`nameable`], else by its
/// 1-based position, `field <n>`.
pub fn field_label(name: &str, index: usize) -> String {
    if nameable(name) {
        format!("field `{name}`")
    } else {
        format!("field {}", index + 1)
    }
}

/// Checks the values a decision carries against the gate's `fields`: a
/// key that names no field is refused; every field is coerced from the
/// given value, else filled from `defaults`, else, when the decision is
/// one the field is required for, refused; the rest read as null. Returns
/// every field name with its value, nulls included.
///
/// A blank text (empty or only whitespace) is no value for a field the
/// decision requires, given or defaulted, so a person who cleared a
/// required field is refused on every surface; for a decision the field
/// is not required for, a given blank stays as typed on a `text` field
/// (a person may clear a default) and reads as no value on any other.
/// This is the one rule: the channel and the drive call it, and the
/// dashboard posts what was typed and shows its refusal.
pub fn check_values(
    spec: &[ReviewField],
    decision: &str,
    given: &BTreeMap<String, Value>,
    defaults: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, String> {
    // The key is a posted value: the refusal never repeats it.
    if given.keys().any(|k| !spec.iter().any(|f| &f.name == k)) {
        return Err("a posted value names no field of this gate".to_string());
    }
    let mut out = BTreeMap::new();
    for f in spec {
        // A null given for a field means "no value": the default, then
        // the requirement.
        let required = f.required.applies(decision);
        let value = given.get(&f.name).filter(|v| !v.is_null());
        let value = match value {
            Some(v) if is_blank(v) && required => {
                return Err(format!("{} is required for this decision", field_ref(f)));
            }
            Some(v) if is_blank(v) && f.kind != FieldKind::Text => None,
            other => other,
        };
        let value = match value {
            Some(v) => coerce(f, v)?,
            // A default that does not fit the field is no value: only the
            // requirement can refuse the decision then.
            None => match defaults
                .get(&f.name)
                .filter(|v| !v.is_null() && !(required && is_blank(v)))
                .and_then(|v| coerce(f, v).ok())
            {
                Some(v) => v,
                None if required => {
                    return Err(format!("{} is required for this decision", field_ref(f)));
                }
                None => Value::Null,
            },
        };
        out.insert(f.name.clone(), value);
    }
    Ok(out)
}

/// Whether `v` is a blank text: a string that is empty or only
/// whitespace.
fn is_blank(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.trim().is_empty())
}

/// Coerces one posted value to the field's type. An error names the field
/// ([`field_ref`]) and the kind of value it takes, never the value nor the
/// field's options: the reason may end up in the journal or an HTTP
/// answer, and either may be a secret.
pub fn coerce(field: &ReviewField, v: &Value) -> Result<Value, String> {
    let name = field_ref(field);
    match field.kind {
        FieldKind::Text => {
            let s = v.as_str().ok_or_else(|| format!("{name}: is not text"))?;
            if s.contains('\0') {
                return Err(format!("{name}: contains a NUL character"));
            }
            let max = field.max_len.unwrap_or(TEXT_DEFAULT_MAX_LEN);
            if s.chars().count() > max as usize {
                return Err(format!("{name}: longer than {max} characters"));
            }
            Ok(Value::String(s.to_string()))
        }
        FieldKind::Number => {
            let n = match v {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            match n {
                Some(n) if n.is_finite() => Ok(Value::Number(
                    number_of(n).ok_or_else(|| format!("{name}: is not a number"))?,
                )),
                _ => Err(format!("{name}: is not a number")),
            }
        }
        FieldKind::Bool => match v {
            Value::Bool(b) => Ok(Value::Bool(*b)),
            Value::String(s) if s == "true" => Ok(Value::Bool(true)),
            Value::String(s) if s == "false" => Ok(Value::Bool(false)),
            _ => Err(format!("{name}: is not true or false")),
        },
        FieldKind::Enum => {
            let not_an_option = || format!("{name}: is not one of the field's options");
            let s = v.as_str().ok_or_else(not_an_option)?;
            if !field.options.iter().any(|o| o == s) {
                return Err(not_an_option());
            }
            Ok(Value::String(s.to_string()))
        }
    }
}

/// A finite number as JSON: an integral value in range stays an integer
/// (`42`, never `42.0`), anything else a float.
fn number_of(n: f64) -> Option<serde_json::Number> {
    const LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53: exact in an f64
    if n.fract() == 0.0 && n.abs() <= LIMIT {
        return Some(serde_json::Number::from(n as i64));
    }
    serde_json::Number::from_f64(n)
}

/// The output of a gate with fields: compact JSON of one object, the
/// decision and every field.
pub fn gate_output(decision: &str, values: &BTreeMap<String, Value>) -> String {
    let mut object = serde_json::Map::new();
    object.insert(
        DECISION_KEY.to_string(),
        Value::String(decision.to_string()),
    );
    for (k, v) in values {
        object.insert(k.clone(), v.clone());
    }
    Value::Object(object).to_string()
}

/// What a passkey challenge covers for one decision: the decision itself
/// without fields (so every existing challenge is unchanged), else the
/// decision bound to the compact JSON of the field values (a `BTreeMap`,
/// so key order is fixed).
pub fn decision_material(decision: &str, fields: &BTreeMap<String, Value>) -> String {
    if fields.is_empty() {
        return decision.to_string();
    }
    let json =
        Value::Object(fields.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).to_string();
    let digest = crate::content::hex_lower(&{
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(json.as_bytes());
        h.finalize()
    });
    format!("{decision}\0fields:{digest}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, kind: FieldKind) -> ReviewField {
        ReviewField {
            name: name.into(),
            kind,
            required: Required::default(),
            default: None,
            options: Vec::new(),
            max_len: None,
        }
    }

    /// Each type coerces: a text length, a number from a string, the bool
    /// words, an enum option.
    #[test]
    fn coerce_reads_each_type() {
        let mut f = field("note", FieldKind::Text);
        assert_eq!(coerce(&f, &json!("hello")).unwrap(), json!("hello"));
        f.max_len = Some(4);
        assert_eq!(
            coerce(&f, &json!("hello")).unwrap_err(),
            "field `note`: longer than 4 characters"
        );
        let n = field("amount", FieldKind::Number);
        assert_eq!(coerce(&n, &json!(1180.5)).unwrap(), json!(1180.5));
        // An integral value stays an integer, from a number or a string.
        assert_eq!(coerce(&n, &json!("42")).unwrap().to_string(), "42");
        assert_eq!(coerce(&n, &json!(3)).unwrap().to_string(), "3");
        assert_eq!(coerce(&n, &json!(-7.0)).unwrap().to_string(), "-7");
        assert_eq!(coerce(&n, &json!("12.5")).unwrap(), json!(12.5));
        // Spaces around a number are read as the dashboard reads them.
        assert_eq!(coerce(&n, &json!(" 42 ")).unwrap().to_string(), "42");
        assert_eq!(
            coerce(&n, &json!("ten")).unwrap_err(),
            "field `amount`: is not a number"
        );
        let b = field("ok", FieldKind::Bool);
        assert_eq!(coerce(&b, &json!(true)).unwrap(), json!(true));
        assert_eq!(coerce(&b, &json!("false")).unwrap(), json!(false));
        assert_eq!(
            coerce(&b, &json!("yes")).unwrap_err(),
            "field `ok`: is not true or false"
        );
        let mut e = field("currency", FieldKind::Enum);
        e.options = vec!["EUR".into(), "USD".into()];
        assert_eq!(coerce(&e, &json!("EUR")).unwrap(), json!("EUR"));
        assert_eq!(
            coerce(&e, &json!("CHF")).unwrap_err(),
            "field `currency`: is not one of the field's options"
        );
    }

    /// `required: [approve]` needs a value for an approve only; a reject
    /// goes through without one.
    #[test]
    fn required_applies_to_the_named_options_only() {
        let spec = vec![ReviewField {
            required: Required::Options(vec!["approve".into()]),
            ..field("amount", FieldKind::Number)
        }];
        let empty = BTreeMap::new();
        let given = check_values(&spec, "approve", &empty, &empty).unwrap_err();
        assert_eq!(given, "field `amount` is required for this decision");
        let out = check_values(&spec, "reject", &empty, &empty).unwrap();
        assert_eq!(out.get("amount"), Some(&Value::Null));
    }

    /// A default fills a value nobody gave; a given value wins over it.
    #[test]
    fn a_default_fills_and_a_given_value_wins() {
        let mut currency = field("currency", FieldKind::Enum);
        currency.options = vec!["EUR".into(), "USD".into()];
        let spec = vec![currency];
        let defaults = BTreeMap::from([("currency".to_string(), json!("EUR"))]);
        let out = check_values(&spec, "approve", &BTreeMap::new(), &defaults).unwrap();
        assert_eq!(out.get("currency"), Some(&json!("EUR")));
        let given = BTreeMap::from([("currency".to_string(), json!("USD"))]);
        let out = check_values(&spec, "approve", &given, &defaults).unwrap();
        assert_eq!(out.get("currency"), Some(&json!("USD")));
    }

    /// A default that does not fit its field is no value: a decision the
    /// field is not required for goes through, one it is required for is
    /// refused as missing.
    #[test]
    fn a_default_that_does_not_fit_is_no_value() {
        let mut currency = field("currency", FieldKind::Enum);
        currency.options = vec!["EUR".into()];
        currency.required = Required::Options(vec!["approve".into()]);
        let spec = vec![currency];
        let defaults = BTreeMap::from([("currency".to_string(), json!("CHF"))]);
        let out = check_values(&spec, "reject", &BTreeMap::new(), &defaults).unwrap();
        assert_eq!(out.get("currency"), Some(&Value::Null));
        assert_eq!(
            check_values(&spec, "approve", &BTreeMap::new(), &defaults).unwrap_err(),
            "field `currency` is required for this decision"
        );
    }

    /// `withhold` left out withholds `approve` only where the gate offers
    /// it; an authored list keeps the offered options it names.
    #[test]
    fn the_default_withhold_follows_the_offered_options() {
        let max = MaxScope {
            files: Some(1),
            lines: None,
            writes: None,
            withhold: None,
        };
        let offered = |o: &[&str]| o.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            max.withhold_for(&offered(&["approve", "reject"])),
            ["approve"]
        );
        assert!(max.withhold_for(&offered(&["yes", "no"])).is_empty());
        let authored = MaxScope {
            withhold: Some(vec!["yes".into(), "ship".into()]),
            ..max
        };
        assert_eq!(authored.withhold_for(&offered(&["yes", "no"])), ["yes"]);
    }

    /// A blank text (empty or only spaces), given or defaulted, is no
    /// value for a field the decision requires, whatever its type and
    /// default; a decision the field is not required for keeps a given
    /// blank text as typed, and a blank for a field of another type reads
    /// as no value.
    #[test]
    fn a_blank_value_is_no_value_for_a_required_field() {
        let note = ReviewField {
            required: Required::Options(vec!["approve".into()]),
            ..field("note", FieldKind::Text)
        };
        let spec = vec![note.clone()];
        let none = BTreeMap::new();
        for blank in ["", "   ", "\t\n"] {
            let given = BTreeMap::from([("note".to_string(), json!(blank))]);
            assert_eq!(
                check_values(&spec, "approve", &given, &none).unwrap_err(),
                "field `note` is required for this decision",
                "{blank:?}"
            );
            // A default does not stand in for a value a person cleared.
            let defaults = BTreeMap::from([("note".to_string(), json!("from the run"))]);
            assert_eq!(
                check_values(&spec, "approve", &given, &defaults).unwrap_err(),
                "field `note` is required for this decision"
            );
            // A blank default is no value either.
            let blank_default = BTreeMap::from([("note".to_string(), json!(blank))]);
            assert_eq!(
                check_values(&spec, "approve", &none, &blank_default).unwrap_err(),
                "field `note` is required for this decision"
            );
            // A reject keeps the text as typed.
            let out = check_values(&spec, "reject", &given, &none).unwrap();
            assert_eq!(out.get("note"), Some(&json!(blank)));
        }
        // A number: a blank is refused as missing when required, else it
        // is no value and the default stands.
        let amount = ReviewField {
            required: Required::Options(vec!["approve".into()]),
            ..field("amount", FieldKind::Number)
        };
        let spec = vec![amount];
        let given = BTreeMap::from([("amount".to_string(), json!(" "))]);
        let defaults = BTreeMap::from([("amount".to_string(), json!(5))]);
        assert_eq!(
            check_values(&spec, "approve", &given, &defaults).unwrap_err(),
            "field `amount` is required for this decision"
        );
        let out = check_values(&spec, "reject", &given, &defaults).unwrap();
        assert_eq!(out.get("amount"), Some(&json!(5)));
    }

    /// A key that names no field is refused.
    #[test]
    fn an_unknown_field_is_refused() {
        let spec = vec![field("amount", FieldKind::Number)];
        let given = BTreeMap::from([("total".to_string(), json!(1))]);
        assert_eq!(
            check_values(&spec, "approve", &given, &BTreeMap::new()).unwrap_err(),
            "a posted value names no field of this gate"
        );
    }

    /// No refusal carries a posted value, a posted key, the decision word
    /// or the field's options; a field whose name is outside the grammar
    /// or shaped like a credential is named `a field`.
    #[test]
    fn a_refusal_carries_no_value_option_key_or_decision() {
        let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
        let mut pick = field("pick", FieldKind::Enum);
        pick.options = vec!["plain".into(), token.into()];
        pick.required = Required::Flag(true);
        let spec = vec![pick, field("note", FieldKind::Text)];
        let none = BTreeMap::new();
        let refusals = [
            check_values(
                &spec,
                token,
                &BTreeMap::from([("pick".to_string(), json!("other"))]),
                &none,
            ),
            check_values(
                &spec,
                token,
                &BTreeMap::from([(token.to_string(), json!(1))]),
                &none,
            ),
            check_values(&spec, token, &none, &none),
            check_values(
                &spec,
                "approve",
                &BTreeMap::from([("pick".to_string(), json!(token.to_uppercase()))]),
                &none,
            ),
            check_values(
                &spec,
                "approve",
                &BTreeMap::from([("note".to_string(), json!(7))]),
                &none,
            ),
        ];
        for r in refusals {
            let e = r.unwrap_err();
            assert!(
                !e.contains(token) && !e.contains("plain") && !e.contains("other"),
                "{e}"
            );
        }
        // A name the validator would refuse is not repeated either.
        let odd = field("Bad Name", FieldKind::Number);
        assert_eq!(
            coerce(&odd, &json!("x")).unwrap_err(),
            "a field: is not a number"
        );
        // Built from parts so the source holds no credential-shaped literal.
        let shaped = field(
            concat!("sk_", "live_", "0123456789abcdefghijklmn"),
            FieldKind::Number,
        );
        assert!(crate::token_shape::looks_like_credential(&shaped.name));
        assert_eq!(
            coerce(&shaped, &json!("x")).unwrap_err(),
            "a field: is not a number"
        );
    }

    /// A gate with fields outputs one JSON object: `decision` and every
    /// field, nulls included.
    #[test]
    fn the_output_has_the_decision_and_every_field() {
        let values = BTreeMap::from([
            ("amount".to_string(), json!(1180.5)),
            ("remark".to_string(), Value::Null),
        ]);
        let out = gate_output("approve", &values);
        assert_eq!(
            out,
            r#"{"amount":1180.5,"decision":"approve","remark":null}"#
        );
    }

    /// What a passkey challenge covers: the bare decision without fields,
    /// the decision bound to the field values with them.
    #[test]
    fn the_material_changes_with_the_fields() {
        let empty = BTreeMap::new();
        assert_eq!(decision_material("approve", &empty), "approve");
        let fields = BTreeMap::from([("amount".to_string(), json!(1))]);
        let material = decision_material("approve", &fields);
        assert!(material.starts_with("approve\0fields:"), "{material}");
        assert_ne!(material, decision_material("approve", &empty));
        // Other values give other material.
        let other = BTreeMap::from([("amount".to_string(), json!(2))]);
        assert_ne!(material, decision_material("approve", &other));
    }
}
