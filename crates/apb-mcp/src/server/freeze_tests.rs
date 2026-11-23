//! The MCP freeze: the served tools and their normalized input schemas,
//! the machine codes the server and the gate produce, and the result keys
//! of every documented result variant of the tools a host consumes. See
//! `openspec/changes/one-point-oh-freeze` (design D1.3 and D1.5 items 1
//! and 5) and `docs/COMPATIBILITY.md`.
//!
//! The writer behind `APB_WRITE_FREEZE=1` only ever adds: it never removes
//! an argument, never narrows a frozen input, and writes nothing when any
//! freeze rule fails.

use std::path::PathBuf;

use super::WfMcp;
use serde_json::{Value, json};

/// Set to `1` to let the test `write_freeze` add what this build has and
/// the freeze lacks. The writer only ever adds: it never removes an entry,
/// never changes a frozen error code, line or argument, and writes
/// nothing when any freeze rule fails.
const WRITE_FREEZE_ENV: &str = "APB_WRITE_FREEZE";

/// Whether the switch is set. Named `write_freeze_enabled`, never
/// `write_freeze`: that name is the writer test's.
fn write_freeze_enabled() -> bool {
    std::env::var(WRITE_FREEZE_ENV).as_deref() == Ok("1")
}

fn fixtures_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture(file: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(file))
        .unwrap_or_else(|e| panic!("cannot read the freeze fixture {file}: {e}"))
}

fn frozen_tools() -> serde_json::Map<String, Value> {
    let text = fixture("mcp-tools-1.0.json");
    let value: Value = serde_json::from_str(&text).expect("mcp-tools-1.0.json parses");
    value["tools"]
        .as_object()
        .expect("tools is an object")
        .clone()
}

fn frozen_results() -> serde_json::Map<String, Value> {
    let text = fixture("mcp-results-1.0.json");
    let value: Value = serde_json::from_str(&text).expect("mcp-results-1.0.json parses");
    value["tools"]
        .as_object()
        .expect("tools is an object")
        .clone()
}

fn served() -> Vec<(String, Value)> {
    let server = WfMcp::new(PathBuf::from("."));
    server
        .tool_router
        .list_all()
        .into_iter()
        .map(|t| (t.name.to_string(), Value::Object((*t.input_schema).clone())))
        .collect()
}

fn served_tool(name: &str) -> Value {
    served()
        .iter()
        .find(|(n, _)| **n == *name)
        .unwrap_or_else(|| panic!("the server serves no tool {name}"))
        .1
        .clone()
}

// ---------------------------------------------------------------------------
// The normalized input schema
// ---------------------------------------------------------------------------

/// The accepted input of a JSON schema, normalized: local `$ref` resolved
/// against the tool's root schema; the set of accepted types, `null`
/// included; for a union (`anyOf`, `oneOf`, a type array) every
/// alternative, each normalized; for an object its properties
/// (recursively) and its required members; for an array its item schema;
/// and every constraint that narrows the accepted input.
///
/// A schema that combines one of the constructs handled specially (a
/// `$ref`, `anyOf`, `oneOf`, `enum`, `const`) with any keyword that may
/// restrict the input is the intersection of the two, and is recorded as
/// one: `{"all": [<the construct>, <every other restricting keyword>]}`
/// ([`beside`]). The second part is the schema made of those keywords
/// alone, normalized by this same function, so a keyword beside a
/// construct is read exactly as it would be on its own, nested
/// combinations included, and none is dropped or merged away. A construct
/// the normalizer does not know fails the extraction;
/// `{"types": ["any"]}` is recorded only for a schema with no constraint
/// at all.
fn normalize(tool: &str, schema: &Value, root: &Value) -> Value {
    normalize_at(tool, "", schema, root)
}

/// [`normalize`] at the dotted argument `path`, which names the schema in
/// a failure.
fn normalize_at(tool: &str, path: &str, schema: &Value, root: &Value) -> Value {
    // A boolean schema: `true` accepts anything, `false` nothing.
    match schema {
        Value::Bool(true) => return json!({ "types": ["any"] }),
        Value::Bool(false) => return json!({ "types": [] }),
        Value::Object(_) => {}
        other => panic!("{tool}{path}: a schema is an object or a boolean, not {other}"),
    }
    known_keywords(tool, path, schema);
    // A local reference resolves against the root.
    if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        let mut target = root;
        let local = reference
            .strip_prefix("#/")
            .unwrap_or_else(|| panic!("{tool}{path}: the reference {reference} is not local"));
        for segment in local.split('/') {
            target = &target[segment];
        }
        assert!(
            !target.is_null(),
            "{tool}{path}: the reference {reference} resolves to nothing"
        );
        let construct = normalize_at(tool, path, target, root);
        return beside(tool, path, schema, &["$ref"], construct, root);
    }
    // A type array is a union over the alternatives; every other keyword
    // goes with each alternative.
    if let Some(types) = schema.get("type").and_then(|t| t.as_array())
        && types.len() > 1
    {
        let alternatives: Vec<Value> = types
            .iter()
            .map(|t| {
                let mut one = schema.clone();
                one["type"] = t.clone();
                normalize_at(tool, path, &one, root)
            })
            .collect();
        return json!({ "union": alternatives });
    }
    // anyOf and oneOf are unions over the alternatives.
    for key in ["anyOf", "oneOf"] {
        if let Some(alternatives) = schema.get(key).and_then(|a| a.as_array()) {
            let union: Vec<Value> = alternatives
                .iter()
                .map(|a| normalize_at(tool, path, a, root))
                .collect();
            return beside(tool, path, schema, &[key], json!({ "union": union }), root);
        }
    }
    // An enum or a const narrows to enumerated values; its own `type` is
    // the base it enumerates.
    if let Some(values) = schema.get("enum").and_then(|e| e.as_array()) {
        let mut construct = base_types(schema);
        construct["enum"] = json!(values);
        return beside(tool, path, schema, &["enum", "type"], construct, root);
    }
    if let Some(value) = schema.get("const") {
        let mut construct = base_types(schema);
        construct["const"] = value.clone();
        return beside(tool, path, schema, &["const", "type"], construct, root);
    }
    let mut out = base_types(schema);
    let types = out["types"].as_array().cloned().unwrap_or_default();
    let has = |t: &str| types.iter().any(|x| x == t);
    // Objects: properties, required members and extra members. A schema
    // with no type of its own (what the keywords beside a construct are)
    // applies them to the objects it meets, so they are recorded there
    // too, where written.
    if has("object") || has("any") {
        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            let mut normalized_props = serde_json::Map::new();
            for (name, prop) in props {
                normalized_props.insert(
                    name.clone(),
                    normalize_at(tool, &format!("{path}.{name}"), prop, root),
                );
            }
            out["properties"] = Value::Object(normalized_props);
        }
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            out["required"] = json!(required);
        } else if has("object") {
            out["required"] = json!([]);
        }
        if let Some(additional) = schema.get("additionalProperties") {
            if additional == &json!(false) {
                out["additionalProperties"] = json!(false);
            } else {
                out["additionalProperties"] =
                    normalize_at(tool, &format!("{path}.*"), additional, root);
            }
        }
    }
    // Arrays: the item schema.
    if (has("array") || has("any"))
        && let Some(items) = schema.get("items")
    {
        out["items"] = normalize_at(tool, &format!("{path}[]"), items, root);
    }
    // The narrowing constraints, where the schema has them.
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "pattern",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minProperties",
        "maxProperties",
    ] {
        if let Some(value) = schema.get(key) {
            out[key] = value.clone();
        }
    }
    // The format of an integer or number schema (the integer width), and
    // of a schema with no type of its own, which applies it to the
    // numbers it meets.
    if let Some(format) = schema.get("format").and_then(|f| f.as_str())
        && (has("integer") || has("number") || has("any"))
    {
        out["format"] = json!(format);
    }
    out
}

/// The keywords that never restrict what a schema accepts: the containers
/// a local `$ref` resolves through, the identifiers and the annotations
/// (D1.3). Every other keyword the normalizer knows may restrict.
const NEVER_RESTRICTS: &[&str] = &[
    "$defs",
    "definitions",
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
];

/// The normalized form of a schema whose construct (the keywords
/// `construct_keys` name, already normalized as `construct`) may sit beside
/// other keywords. JSON Schema applies every keyword of a schema, so the
/// keywords left once the construct and the ones that never restrict are
/// taken out form a schema of their own, and the whole is the intersection
/// of the two: `{"all": [construct, rest]}`, the rest normalized by
/// [`normalize_at`] like any schema. This holds for every restricting
/// keyword the normalizer knows (a type, the object and array keywords,
/// every bound, length and count, a pattern, a format, `multipleOf`, an
/// enum, a const or another union), without a list of its own that could
/// fall behind. With nothing restricting beside it, the construct is
/// recorded alone, as before.
fn beside(
    tool: &str,
    path: &str,
    schema: &Value,
    construct_keys: &[&str],
    construct: Value,
    root: &Value,
) -> Value {
    let mut rest = schema.as_object().cloned().unwrap_or_default();
    rest.retain(|key, _| {
        !construct_keys.contains(&key.as_str()) && !NEVER_RESTRICTS.contains(&key.as_str())
    });
    if rest.is_empty() {
        return construct;
    }
    let rest = normalize_at(tool, path, &Value::Object(rest), root);
    if accepts_anything(&rest) {
        return construct;
    }
    json!({ "all": [construct, rest] })
}

/// The schema keywords the normalizer knows. Anything else that appears in
/// a served schema fails the extraction: an unhandled keyword may narrow
/// what the tool accepts, and the freeze must not read past it.
const KNOWN_SCHEMA_KEYS: &[&str] = &[
    // References and the containers a local `$ref` resolves through.
    "$ref",
    "$defs",
    "definitions",
    "$schema",
    "$id",
    "$comment",
    // Annotations that never narrow what a tool accepts (D1.3).
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    // The accepted shape and the enumerated values.
    "type",
    "anyOf",
    "oneOf",
    "enum",
    "const",
    // Objects and arrays.
    "properties",
    "required",
    "additionalProperties",
    "items",
    // The recorded narrowing constraints (`format` is recorded on a
    // number and is an annotation on a string).
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "format",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];

/// Rejects a schema that carries a keyword the normalizer does not know
/// (`allOf`, `not`, `patternProperties`, `if`, `prefixItems`, ...): the
/// extraction fails rather than silently ignoring a construct that may
/// narrow the input.
fn known_keywords(tool: &str, path: &str, schema: &Value) {
    let Some(map) = schema.as_object() else {
        return;
    };
    for key in map.keys() {
        assert!(
            KNOWN_SCHEMA_KEYS.contains(&key.as_str()),
            "{tool}{path}: the schema uses the construct {key}, which the normalizer does not know; it may narrow what the tool accepts, so the extraction fails (handle it in normalize or reject the change)"
        );
    }
}

/// The types a schema accepts; an unconstrained schema is `["any"]`.
fn base_types(schema: &Value) -> Value {
    match schema.get("type") {
        Some(Value::String(t)) => json!({ "types": [t] }),
        Some(Value::Array(a)) => json!({ "types": a }),
        None => json!({ "types": ["any"] }),
        Some(other) => panic!("the schema's type is a {other}"),
    }
}

/// Whether a normalized schema accepts every value: no type restriction and
/// no constraint (what `true` and `{}` normalize to).
fn accepts_anything(schema: &Value) -> bool {
    schema == &json!({ "types": ["any"] })
}

/// How a current schema's missing members read in [`accepts_superset`].
/// The construct a tool serves is read under the freeze's own policy: a
/// frozen argument absent from it was removed, and a new argument it
/// declares is the addition the freeze allows. The keywords written beside
/// a construct (the second part of an `{"all": ...}`) are read by JSON
/// Schema alone: a member they do not name is governed by their
/// `additionalProperties`, and a member they name that the frozen object
/// took whole is a new restriction.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    Policy,
    Beside,
}

/// Whether the normalized schema `now` accepts everything `frozen` did
/// (design D1.3 rule 2). Every frozen type, union alternative, property and
/// item schema stays accepted, every frozen constraint stays or widens, and
/// no constraint the frozen schema lacked appears (`newly_narrows`). A
/// required member that became optional widens; one gained is rule 3's
/// (`gains_required`).
///
/// An intersection (`{"all": [...]}`, a construct with the keywords beside
/// it) is compared part by part: a current one accepts the frozen schema
/// only when every part does, which is exact, so any keyword beside any
/// construct that rejects a frozen input fails, whichever side carries it;
/// a frozen one is accepted when one of its parts is, which is sound (the
/// frozen inputs lie inside each part) and may only refuse a widening it
/// cannot prove, never pass a narrowing.
fn accepts_superset(tool: &str, path: &str, frozen: &Value, now: &Value) -> Result<(), String> {
    superset_as(tool, path, frozen, now, Reading::Policy)
}

fn superset_as(
    tool: &str,
    path: &str,
    frozen: &Value,
    now: &Value,
    reading: Reading,
) -> Result<(), String> {
    // A current intersection: every part accepts the frozen schema. The
    // construct keeps the reading it was reached with; the keywords beside
    // it are read by JSON Schema alone.
    if let Some(parts) = now.get("all").and_then(|a| a.as_array()) {
        for (i, part) in parts.iter().enumerate() {
            let as_part = if i == 0 { reading } else { Reading::Beside };
            superset_as(tool, path, frozen, part, as_part)?;
        }
        return Ok(());
    }
    // A frozen intersection: one part the current schema accepts holds
    // every frozen input.
    if let Some(parts) = frozen.get("all").and_then(|a| a.as_array()) {
        let mut first = None;
        for part in parts {
            match superset_as(tool, path, part, now, reading) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        return Err(first.unwrap_or_else(|| format!("{tool}{path}: an empty intersection")));
    }
    // A union: every frozen alternative stays accepted.
    if let Some(alternatives) = frozen.get("union").and_then(|u| u.as_array()) {
        for alternative in alternatives {
            let kept = match now.get("union").and_then(|u| u.as_array()) {
                Some(now_union) => now_union
                    .iter()
                    .any(|a| superset_as(tool, path, alternative, a, reading).is_ok()),
                None => superset_as(tool, path, alternative, now, reading).is_ok(),
            };
            if !kept {
                return Err(format!(
                    "{tool}{path}: the frozen union alternative {alternative} is no longer accepted"
                ));
            }
        }
        return Ok(());
    }
    // A `now` union: accepted when one alternative accepts the frozen
    // schema.
    if let Some(alternatives) = now.get("union").and_then(|u| u.as_array()) {
        if alternatives
            .iter()
            .any(|a| superset_as(tool, path, frozen, a, reading).is_ok())
        {
            return Ok(());
        }
        return Err(format!(
            "{tool}{path}: no alternative of the new union accepts the frozen schema"
        ));
    }
    // One schema on each side: its constraints and its extra members.
    constraints_stay(tool, path, frozen, now)?;
    if applies_to("additionalProperties", frozen) {
        additional_widens(
            tool,
            path,
            frozen.get("additionalProperties"),
            now.get("additionalProperties"),
            reading,
        )?;
    }
    // Types: every frozen type stays accepted (a `number` accepts every
    // integer).
    let now_types = now["types"]
        .as_array()
        .unwrap_or_else(|| panic!("{tool}{path}: the re-read schema has no types ({now})"));
    for t in frozen["types"].as_array().expect("frozen types") {
        let kept = now_types == &[json!("any")]
            || now_types.contains(t)
            || (t == "integer" && now_types.contains(&json!("number")));
        if !kept {
            return Err(format!("{tool}{path}: the type {t} is no longer accepted"));
        }
    }
    properties_stay(tool, path, frozen, now, reading)?;
    // Arrays: the item schema stays or widens; an array of anything never
    // gains an item schema that restricts it.
    match (frozen.get("items"), now.get("items")) {
        (Some(frozen_items), Some(now_items)) => {
            superset_as(tool, &format!("{path}[]"), frozen_items, now_items, reading)?;
        }
        (None, Some(now_items)) if !accepts_anything(now_items) && applies_to("items", frozen) => {
            return Err(format!(
                "{tool}{path}[]: a new item schema restricts an array the frozen schema took whole"
            ));
        }
        // The item schema vanished (every item accepted, a widening) or
        // neither has one.
        _ => {}
    }
    Ok(())
}

/// The object half of [`superset_as`]: every frozen property stays and
/// accepts what it did. Under [`Reading::Policy`] a frozen property the
/// current object lacks was removed, and a property it gains is a new
/// argument. Under [`Reading::Beside`] a property the current keywords do
/// not name is held to their `additionalProperties`, and a property they
/// name that the frozen object did not is held against what the frozen
/// object took for it (nothing when closed, its extra-member schema, or
/// anything).
fn properties_stay(
    tool: &str,
    path: &str,
    frozen: &Value,
    now: &Value,
    reading: Reading,
) -> Result<(), String> {
    let empty = serde_json::Map::new();
    let frozen_props = frozen.get("properties").and_then(|p| p.as_object());
    let now_props = now.get("properties").and_then(|p| p.as_object());
    if reading == Reading::Policy {
        let Some(frozen_props) = frozen_props else {
            return Ok(());
        };
        let now_props = now_props
            .ok_or_else(|| format!("{tool}{path}: the frozen object lost its properties"))?;
        for (name, prop) in frozen_props {
            let Some(now_prop) = now_props.get(name) else {
                return Err(format!("{tool}{path}.{name}: the argument was removed"));
            };
            superset_as(tool, &format!("{path}.{name}"), prop, now_prop, reading)?;
        }
        return Ok(());
    }
    let frozen_props = frozen_props.unwrap_or(&empty);
    let now_props = now_props.unwrap_or(&empty);
    for (name, prop) in frozen_props {
        let at = format!("{path}.{name}");
        match (now_props.get(name), now.get("additionalProperties")) {
            (Some(now_prop), _) => superset_as(tool, &at, prop, now_prop, reading)?,
            (None, Some(Value::Bool(false))) => {
                return Err(format!(
                    "{tool}{at}: the argument is refused by a new additionalProperties false"
                ));
            }
            (None, Some(extra)) => superset_as(tool, &at, prop, extra, reading)?,
            (None, None) => {}
        }
    }
    // The property keywords hold for objects alone.
    if !applies_to("properties", frozen) {
        return Ok(());
    }
    for (name, now_prop) in now_props {
        if frozen_props.contains_key(name) {
            continue;
        }
        let at = format!("{path}.{name}");
        match frozen.get("additionalProperties") {
            Some(Value::Bool(false)) => {}
            Some(extra) => superset_as(tool, &at, extra, now_prop, reading)?,
            None if accepts_anything(now_prop) => {}
            None => {
                return Err(format!(
                    "{tool}{at}: a new property schema {now_prop} restricts a member the frozen object took whole"
                ));
            }
        }
    }
    Ok(())
}

/// The constraint half of [`accepts_superset`], at one schema level: every
/// frozen constraint stays or widens, and no constraint the frozen schema
/// lacked appears (`newly_narrows`). A union or an intersection carries no
/// constraint of its own (the keywords beside a construct are a part of
/// its intersection), so this runs once both sides are a single schema.
fn constraints_stay(tool: &str, path: &str, frozen: &Value, now: &Value) -> Result<(), String> {
    // Bounds: a lower one may only fall or vanish, an upper one only rise
    // or vanish.
    for key in [
        "minimum",
        "exclusiveMinimum",
        "minLength",
        "minItems",
        "minProperties",
    ] {
        if let Some(bound) = frozen.get(key).and_then(|v| v.as_f64())
            && let Some(now_bound) = now.get(key).and_then(|v| v.as_f64())
            && now_bound > bound
        {
            return Err(format!(
                "{tool}{path}: the frozen {key} {bound} rose to {now_bound}"
            ));
        }
    }
    for key in [
        "maximum",
        "exclusiveMaximum",
        "maxLength",
        "maxItems",
        "maxProperties",
    ] {
        if let Some(bound) = frozen.get(key).and_then(|v| v.as_f64())
            && let Some(now_bound) = now.get(key).and_then(|v| v.as_f64())
            && now_bound < bound
        {
            return Err(format!(
                "{tool}{path}: the frozen {key} {bound} fell to {now_bound}"
            ));
        }
    }
    // A `multipleOf` stays, vanishes or becomes a divisor of the frozen one
    // (every multiple of 4 is one of 2).
    if let Some(step) = frozen.get("multipleOf").and_then(|v| v.as_f64())
        && let Some(now_step) = now.get("multipleOf").and_then(|v| v.as_f64())
    {
        let ratio = step / now_step;
        if !(ratio >= 1.0 && ratio.fract() == 0.0) {
            return Err(format!(
                "{tool}{path}: the frozen multipleOf {step} changed to {now_step}, which refuses a frozen multiple"
            ));
        }
    }
    // An enum only gains values.
    if let Some(frozen_values) = frozen.get("enum").and_then(|e| e.as_array()) {
        match now.get("enum").and_then(|e| e.as_array()) {
            Some(now_values) => {
                for value in frozen_values {
                    if !now_values.contains(value) {
                        return Err(format!(
                            "{tool}{path}: the frozen enum value {value} is no longer accepted"
                        ));
                    }
                }
            }
            // The enum vanished: every value of its types is accepted,
            // unless a const took its place.
            None => {
                if let Some(c) = now.get("const")
                    && !(frozen_values.len() == 1 && &frozen_values[0] == c)
                {
                    return Err(format!(
                        "{tool}{path}: the frozen enum narrowed to the const {c}"
                    ));
                }
            }
        }
    }
    // A const only becomes an enum holding it, or vanishes.
    if let Some(const_value) = frozen.get("const") {
        let kept = match (now.get("const"), now.get("enum").and_then(|e| e.as_array())) {
            (Some(c), _) => c == const_value,
            (None, Some(values)) => values.contains(const_value),
            (None, None) => true,
        };
        if !kept {
            return Err(format!(
                "{tool}{path}: the frozen const {const_value} is no longer accepted"
            ));
        }
    }
    // A pattern stays as frozen or vanishes.
    if let Some(pattern) = frozen.get("pattern")
        && let Some(now_pattern) = now.get("pattern")
        && now_pattern != pattern
    {
        return Err(format!(
            "{tool}{path}: the frozen pattern {pattern} changed to {now_pattern}"
        ));
    }
    // An integer format stays or widens along `widens`; it may vanish.
    if let Some(format) = frozen.get("format").and_then(|f| f.as_str())
        && let Some(now_format) = now.get("format").and_then(|f| f.as_str())
        && !widens(format, now_format)
    {
        return Err(format!(
            "{tool}{path}: the frozen integer format {format} narrowed to {now_format}"
        ));
    }
    // New constraints: a constraint the frozen schema did not have narrows
    // what the tool accepts, unless it is vacuous for the frozen domain.
    newly_narrows(tool, path, frozen, now)
}

/// Rule 2 for `additionalProperties`: `false` may become a schema or
/// vanish, never the reverse; a schema may only widen; a member the
/// frozen schema took whole (absent or a schema accepting anything) is
/// never restricted.
fn additional_widens(
    tool: &str,
    path: &str,
    frozen: Option<&Value>,
    now: Option<&Value>,
    reading: Reading,
) -> Result<(), String> {
    let open = |v: Option<&Value>| v.is_none_or(accepts_anything);
    match frozen {
        Some(f) if f == &json!(false) => Ok(()),
        _ if open(frozen) => {
            if open(now) {
                Ok(())
            } else {
                Err(format!(
                    "{tool}{path}: a new additionalProperties {} rejects a member the frozen schema accepted",
                    now.expect("not open")
                ))
            }
        }
        Some(f) => match now {
            None => Ok(()),
            Some(n) if n == &json!(false) => Err(format!(
                "{tool}{path}: the frozen additionalProperties schema became false"
            )),
            Some(n) => superset_as(tool, &format!("{path}.*"), f, n, reading),
        },
        None => unreachable!("an absent schema is open"),
    }
}

/// The smallest and the largest value of an integer format: `(min, max)`.
/// `None` for a format outside the table (`uint`, `double`), whose domain
/// the freeze does not bound.
fn format_domain(format: &str) -> Option<(f64, f64)> {
    const FORMATS: &[(&str, f64, f64)] = &[
        ("int8", -128.0, 127.0),
        ("int16", -32768.0, 32767.0),
        ("int32", -2147483648.0, 2147483647.0),
        ("int64", -9223372036854775808.0, 9223372036854775807.0),
        ("uint8", 0.0, 255.0),
        ("uint16", 0.0, 65535.0),
        ("uint32", 0.0, 4294967295.0),
        ("uint64", 0.0, 18446744073709551615.0),
    ];
    FORMATS
        .iter()
        .find(|(f, _, _)| *f == format)
        .map(|(_, low, top)| (*low, *top))
}

/// The smallest and the largest value the frozen schema's numeric domain
/// holds when its own format bounds it: `(min, max)`. `None` where the
/// frozen schema has no such format, so it accepts every value of its type.
fn frozen_domain(schema: &Value) -> Option<(f64, f64)> {
    let types = schema["types"].as_array()?;
    if !types.contains(&json!("integer")) && !types.contains(&json!("number")) {
        return None;
    }
    format_domain(schema.get("format").and_then(|f| f.as_str())?)
}

/// Rejects a constraint the frozen schema did not carry: a newly
/// introduced bound, length, count, pattern, `multipleOf`, `uniqueItems`,
/// `enum`, `const` or integer format rejects a 1.0 input the frozen schema
/// accepted. A bound that is vacuous for the frozen format's own domain (a
/// `minimum: 0` on a `uint64`), a zero length or count floor and a
/// `multipleOf: 1` on integers do not narrow and pass. A keyword that
/// applies to none of the frozen types ([`applies_to`]: a `minLength`
/// written beside a union, met by its `null`) restricts no frozen value.
fn newly_narrows(tool: &str, path: &str, frozen: &Value, now: &Value) -> Result<(), String> {
    let domain = frozen_domain(frozen);
    let new =
        |key: &str| frozen.get(key).is_none() && now.get(key).is_some() && applies_to(key, frozen);
    let fail = |key: &str| {
        Err(format!(
            "{tool}{path}: a new {key} {} restricts a value the frozen schema accepted",
            now[key]
        ))
    };
    for key in ["minimum", "exclusiveMinimum"] {
        if new(key) {
            let bound = now[key].as_f64().unwrap_or(f64::INFINITY);
            let vacuous = match domain {
                Some((low, _)) if key == "minimum" => bound <= low,
                Some((low, _)) => bound < low,
                None => false,
            };
            if !vacuous {
                return fail(key);
            }
        }
    }
    for key in ["maximum", "exclusiveMaximum"] {
        if new(key) {
            let bound = now[key].as_f64().unwrap_or(f64::NEG_INFINITY);
            let vacuous = match domain {
                Some((_, top)) if key == "maximum" => bound >= top,
                Some((_, top)) => bound > top,
                None => false,
            };
            if !vacuous {
                return fail(key);
            }
        }
    }
    for key in ["minLength", "minItems", "minProperties"] {
        if new(key) && now[key].as_u64() != Some(0) {
            return fail(key);
        }
    }
    for key in [
        "maxLength",
        "maxItems",
        "maxProperties",
        "pattern",
        "enum",
        "const",
    ] {
        if new(key) {
            return fail(key);
        }
    }
    if new("multipleOf") {
        let integers = frozen["types"]
            .as_array()
            .is_some_and(|t| t.iter().all(|t| t == "integer" || t == "null"));
        if !(now["multipleOf"].as_f64() == Some(1.0) && integers) {
            return fail("multipleOf");
        }
    }
    if frozen.get("uniqueItems") != Some(&json!(true))
        && now.get("uniqueItems") == Some(&json!(true))
        && applies_to("uniqueItems", frozen)
    {
        return fail("uniqueItems");
    }
    if new("format")
        && let Some((low, top)) = now["format"].as_str().and_then(format_domain)
    {
        // A width the frozen schema lacked bounds the values, unless the
        // frozen bounds already lie inside it.
        let frozen_low = frozen
            .get("minimum")
            .and_then(|v| v.as_f64())
            .unwrap_or(f64::NEG_INFINITY);
        let frozen_top = frozen
            .get("maximum")
            .and_then(|v| v.as_f64())
            .unwrap_or(f64::INFINITY);
        if frozen_low < low || frozen_top > top {
            return fail("format");
        }
    }
    Ok(())
}

/// Whether the restricting keyword `key` can refuse a value of one of the
/// frozen schema's types: JSON Schema applies the string keywords to
/// strings only, the numeric ones to numbers, the array ones to arrays and
/// the object ones to objects; `enum` and `const` apply to every value. A
/// frozen schema with no type of its own (`any`) meets all of them.
fn applies_to(key: &str, frozen: &Value) -> bool {
    let kinds: &[&str] = match key {
        "minLength" | "maxLength" | "pattern" => &["string"],
        "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf"
        | "format" => &["integer", "number"],
        "minItems" | "maxItems" | "uniqueItems" | "items" => &["array"],
        "minProperties" | "maxProperties" | "properties" | "additionalProperties" => &["object"],
        _ => return true,
    };
    match frozen.get("types").and_then(|t| t.as_array()) {
        Some(types) => types
            .iter()
            .any(|t| t == "any" || kinds.iter().any(|k| t == k)),
        None => true,
    }
}

/// Whether an integer format change only widens: `uint8` to `uint16` to
/// `uint32` to `uint64`, `int8` to `int16` to `int32` to `int64`, and
/// `uintN` to `intM` for M greater than N.
fn widens(from: &str, to: &str) -> bool {
    const ORDER: &[&str] = &["int8", "int16", "int32", "int64"];
    const UORDER: &[&str] = &["uint8", "uint16", "uint32", "uint64"];
    if from == to {
        return true;
    }
    let signed = |f: &str| ORDER.iter().position(|x| *x == f);
    let unsigned = |f: &str| UORDER.iter().position(|x| *x == f);
    match (unsigned(from), signed(from), unsigned(to), signed(to)) {
        (_, Some(f), _, Some(t)) => t > f,
        (Some(f), _, Some(t), _) => t > f,
        (Some(f), _, _, Some(t)) => t > f,
        _ => false,
    }
}

/// Rule 3: no object of the frozen schema, the top level included, gains a
/// required member (a member left `required` is a widening and passes).
/// A union alternative is compared with the current alternative that
/// accepts it (rule 2 fails a frozen alternative nothing accepts). A
/// current intersection gains what any of its parts requires, so every
/// part is compared (a `required` written beside a construct included). A
/// frozen intersection stays whole: over a union it is the union of the
/// alternatives each intersected with the other parts, and otherwise its
/// parts read as one object ([`merged_parts`]) whose inputs carry every
/// member any part requires.
fn gains_required(tool: &str, frozen: &Value, now: &Value) -> Result<(), String> {
    gains_required_at(tool, "", frozen, now)
}

fn gains_required_at(tool: &str, path: &str, frozen: &Value, now: &Value) -> Result<(), String> {
    if let Some(parts) = now.get("all").and_then(|a| a.as_array()) {
        for part in parts {
            gains_required_at(tool, path, frozen, part)?;
        }
        return Ok(());
    }
    if let Some(alternatives) = frozen.get("union").and_then(|u| u.as_array()) {
        for alternative in alternatives {
            gains_required_at(tool, path, alternative, now)?;
        }
        return Ok(());
    }
    let frozen_parts = frozen.get("all").and_then(|a| a.as_array()).map(|parts| {
        let mut flat = Vec::new();
        flatten_parts(parts, &mut flat);
        flat
    });
    // A frozen intersection with a union part is the union of that union's
    // alternatives, each intersected with the other parts.
    if let Some(parts) = &frozen_parts
        && let Some(at) = parts.iter().position(|p| p.get("union").is_some())
    {
        let alternatives = parts[at]["union"].as_array().cloned().unwrap_or_default();
        let distributed: Vec<Value> = alternatives
            .into_iter()
            .map(|alternative| {
                let mut each = parts.clone();
                each[at] = alternative;
                json!({ "all": each })
            })
            .collect();
        return gains_required_at(tool, path, &json!({ "union": distributed }), now);
    }
    if let Some(alternatives) = now.get("union").and_then(|u| u.as_array()) {
        // The alternative that accepts the frozen schema is the one its
        // inputs reach.
        return match alternatives
            .iter()
            .find(|a| accepts_superset(tool, path, frozen, a).is_ok())
        {
            Some(a) => gains_required_at(tool, path, frozen, a),
            None => Ok(()),
        };
    }
    if let Some(parts) = &frozen_parts {
        return gains_required_at(tool, path, &merged_parts(parts), now);
    }
    // One schema on each side: its own required members.
    let is_object = frozen["types"]
        .as_array()
        .is_some_and(|t| t.contains(&json!("object")) || t.contains(&json!("any")));
    if is_object {
        let frozen_required = frozen
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let now_required = now
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let gained: Vec<&Value> = now_required
            .iter()
            .filter(|m| !frozen_required.contains(m))
            .collect();
        if !gained.is_empty() {
            let at = if path.is_empty() {
                "the top level"
            } else {
                path
            };
            return Err(format!(
                "{tool}: the object at {at} gained the required members {gained:?}"
            ));
        }
    }
    if let (Some(props), Some(now_props)) = (
        frozen.get("properties").and_then(|p| p.as_object()),
        now.get("properties").and_then(|p| p.as_object()),
    ) {
        for (name, prop) in props {
            if let Some(now_prop) = now_props.get(name) {
                gains_required_at(tool, &format!("{path}.{name}"), prop, now_prop)?;
            }
        }
    }
    if let (Some(items), Some(now_items)) = (frozen.get("items"), now.get("items")) {
        gains_required_at(tool, &format!("{path}[]"), items, now_items)?;
    }
    if let (Some(extra), Some(now_extra)) = (
        frozen.get("additionalProperties").filter(|v| v.is_object()),
        now.get("additionalProperties").filter(|v| v.is_object()),
    ) {
        gains_required_at(tool, &format!("{path}.*"), extra, now_extra)?;
    }
    Ok(())
}

/// The parts of an intersection with nested intersections spliced in.
fn flatten_parts(parts: &[Value], out: &mut Vec<Value>) {
    for part in parts {
        match part.get("all").and_then(|a| a.as_array()) {
            Some(nested) => flatten_parts(nested, out),
            None => out.push(part.clone()),
        }
    }
}

/// The parts of a frozen intersection (none a union) read as one object
/// for rule 3: it takes objects only when every part does, requires every
/// member any part requires, and holds each member, the item schema and
/// the extra members as the intersection of what the parts say of them.
/// A part that says nothing of a member is left out of that member's
/// intersection, which only makes the view accept more, so a member it
/// gains is never hidden.
fn merged_parts(parts: &[Value]) -> Value {
    let objects = parts.iter().all(|p| {
        p["types"]
            .as_array()
            .is_some_and(|t| t.contains(&json!("object")) || t.contains(&json!("any")))
    });
    let mut required: Vec<Value> = Vec::new();
    let mut members: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut items: Vec<Value> = Vec::new();
    let mut extra: Vec<Value> = Vec::new();
    for part in parts {
        for member in part
            .get("required")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
        {
            if !required.contains(member) {
                required.push(member.clone());
            }
        }
        for (name, schema) in part
            .get("properties")
            .and_then(|p| p.as_object())
            .into_iter()
            .flatten()
        {
            let entry = members
                .entry(name.clone())
                .or_insert_with(|| json!({ "all": [] }));
            entry["all"]
                .as_array_mut()
                .expect("parts")
                .push(schema.clone());
        }
        if let Some(schema) = part.get("items") {
            items.push(schema.clone());
        }
        if let Some(schema) = part.get("additionalProperties").filter(|v| v.is_object()) {
            extra.push(schema.clone());
        }
    }
    let one = |list: Vec<Value>| match list.len() {
        1 => list.into_iter().next().expect("one"),
        _ => json!({ "all": list }),
    };
    let members: serde_json::Map<String, Value> = members
        .into_iter()
        .map(|(name, schema)| {
            (
                name,
                one(schema["all"].as_array().cloned().unwrap_or_default()),
            )
        })
        .collect();
    let mut out = json!({
        "types": if objects { json!(["object"]) } else { json!([]) },
        "required": required,
        "properties": members,
    });
    if !items.is_empty() {
        out["items"] = one(items);
    }
    if !extra.is_empty() {
        out["additionalProperties"] = one(extra);
    }
    out
}

/// Unit proofs of the comparison of rules 2 and 3, each over a frozen and a
/// current schema built in memory: what narrows fails, what widens passes.
#[test]
fn the_input_comparison_refuses_every_narrowing() {
    let cmp = |frozen: Value, now: Value| accepts_superset("t", "", &frozen, &now);
    let s = |extra: Value| {
        let mut base = json!({ "types": ["string"] });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    };
    // New restrictions on an argument that had none.
    for key in [
        json!({ "minLength": 1 }),
        json!({ "maxLength": 9 }),
        json!({ "pattern": "^a" }),
        json!({ "enum": ["a"] }),
        json!({ "const": "a" }),
    ] {
        assert!(
            cmp(s(json!({})), s(key.clone())).is_err(),
            "{key} must narrow"
        );
    }
    assert!(cmp(s(json!({})), s(json!({ "minLength": 0 }))).is_ok());
    let obj = |extra: Value| {
        let mut base = json!({ "types": ["object"], "required": [], "properties": {} });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    };
    assert!(
        cmp(
            obj(json!({})),
            obj(json!({ "additionalProperties": false }))
        )
        .is_err()
    );
    assert!(
        cmp(
            obj(json!({})),
            obj(json!({ "additionalProperties": { "types": ["string"] } }))
        )
        .is_err()
    );
    assert!(
        cmp(
            obj(json!({ "additionalProperties": false })),
            obj(json!({}))
        )
        .is_ok()
    );
    assert!(
        cmp(
            obj(json!({ "additionalProperties": { "types": ["string"] } })),
            obj(json!({ "additionalProperties": false }))
        )
        .is_err()
    );
    // Numbers: a new floor below the format's own is vacuous, above it
    // narrows; a format may only widen.
    let n = |extra: Value| {
        let mut base = json!({ "types": ["integer"] });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    };
    assert!(
        cmp(
            n(json!({ "format": "uint64" })),
            n(json!({ "format": "uint64", "minimum": 0 }))
        )
        .is_ok()
    );
    assert!(
        cmp(
            n(json!({ "format": "uint64" })),
            n(json!({ "format": "uint64", "minimum": 1 }))
        )
        .is_err()
    );
    assert!(cmp(n(json!({})), n(json!({ "minimum": 0 }))).is_err());
    assert!(cmp(n(json!({})), n(json!({ "format": "uint32" }))).is_err());
    assert!(
        cmp(
            n(json!({ "format": "uint32" })),
            n(json!({ "format": "int32" }))
        )
        .is_err()
    );
    assert!(
        cmp(
            n(json!({ "format": "uint32" })),
            n(json!({ "format": "int64" }))
        )
        .is_ok()
    );
    assert!(
        cmp(
            n(json!({ "format": "uint64" })),
            n(json!({ "format": "uint32" }))
        )
        .is_err()
    );
    assert!(cmp(n(json!({})), n(json!({ "multipleOf": 1 }))).is_ok());
    assert!(cmp(n(json!({})), n(json!({ "multipleOf": 2 }))).is_err());
    assert!(cmp(n(json!({ "multipleOf": 4 })), n(json!({ "multipleOf": 2 }))).is_ok());
    assert!(cmp(n(json!({ "multipleOf": 4 })), n(json!({ "multipleOf": 3 }))).is_err());
    // What may vanish, vanishes.
    assert!(cmp(s(json!({ "pattern": "^a" })), s(json!({}))).is_ok());
    assert!(cmp(s(json!({ "pattern": "^a" })), s(json!({ "pattern": "^b" }))).is_err());
    let a = |extra: Value| {
        let mut base = json!({ "types": ["array"] });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base
    };
    assert!(cmp(a(json!({ "uniqueItems": true })), a(json!({}))).is_ok());
    assert!(cmp(a(json!({})), a(json!({ "uniqueItems": true }))).is_err());
    assert!(cmp(a(json!({})), a(json!({ "items": { "types": ["string"] } }))).is_err());
    // Rule 3: a member leaving required widens, one gained fails, a swap
    // too.
    let req = |r: Value| {
        json!({ "types": ["object"], "required": r, "properties": {
            "a": { "types": ["string"] }, "b": { "types": ["string"] } } })
    };
    assert!(gains_required("t", &req(json!(["a"])), &req(json!([]))).is_ok());
    assert!(cmp(req(json!(["a"])), req(json!([]))).is_ok());
    assert!(gains_required("t", &req(json!(["a"])), &req(json!(["b"]))).is_err());
    assert!(gains_required("t", &req(json!([])), &req(json!(["a", "b"]))).is_err());
}

/// An unknown schema keyword fails the extraction instead of being read
/// past.
#[test]
fn the_normalizer_refuses_an_unknown_keyword() {
    for schema in [
        json!({ "type": "object", "properties": { "a": { "type": "string", "allOf": [{ "minLength": 1 }] } } }),
        json!({ "type": "object", "properties": { "a": { "not": { "type": "string" } } } }),
        json!({ "type": "object", "patternProperties": { "^x": { "type": "string" } } }),
    ] {
        let result = std::panic::catch_unwind(|| normalize("t", &schema, &schema));
        assert!(result.is_err(), "{schema} must fail the extraction");
    }
}

/// A keyword that may restrict the input, written beside any construct the
/// normalizer handles specially, is compared both ways (closure finding 3).
/// For every (construct, keyword) pair: adding the keyword, or tightening
/// it where both sides carry it, is reported by the comparison of rules 2
/// and 3; dropping it, or loosening it, passes; and every schema compares
/// equal to itself. A keyword with no restricting meaning (an annotation)
/// leaves the construct as it was, and an unknown keyword beside a
/// construct still fails the extraction.
#[test]
fn every_sibling_keyword_beside_every_construct_is_compared() {
    // (keyword, the construct's target schema, a value the target accepts
    // and the tight keyword refuses, the tight keyword, a looser one where
    // the keyword can be loosened).
    let keywords: Vec<(&str, Value, Value, Value, Option<Value>)> = vec![
        (
            "type",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "type": "string" }),
            None,
        ),
        (
            "enum",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "enum": [5] }),
            Some(json!({ "enum": [4, 5] })),
        ),
        (
            "const",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "const": 5 }),
            None,
        ),
        (
            "minimum",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "minimum": 5 }),
            Some(json!({ "minimum": 2 })),
        ),
        (
            "maximum",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "maximum": 3 }),
            Some(json!({ "maximum": 9 })),
        ),
        (
            "exclusiveMinimum",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "exclusiveMinimum": 4 }),
            Some(json!({ "exclusiveMinimum": 1 })),
        ),
        (
            "exclusiveMaximum",
            json!({ "type": "integer" }),
            json!(4),
            json!({ "exclusiveMaximum": 4 }),
            Some(json!({ "exclusiveMaximum": 9 })),
        ),
        (
            "multipleOf",
            json!({ "type": "integer" }),
            json!(6),
            json!({ "multipleOf": 4 }),
            Some(json!({ "multipleOf": 2 })),
        ),
        (
            "format",
            json!({ "type": "integer" }),
            json!(300),
            json!({ "format": "uint8" }),
            Some(json!({ "format": "uint16" })),
        ),
        (
            "minLength",
            json!({ "type": "string" }),
            json!("ab"),
            json!({ "minLength": 3 }),
            Some(json!({ "minLength": 1 })),
        ),
        (
            "maxLength",
            json!({ "type": "string" }),
            json!("ab"),
            json!({ "maxLength": 1 }),
            Some(json!({ "maxLength": 5 })),
        ),
        (
            "pattern",
            json!({ "type": "string" }),
            json!("ab"),
            json!({ "pattern": "^z" }),
            None,
        ),
        (
            "items",
            json!({ "type": "array" }),
            json!(["a"]),
            json!({ "items": { "type": "integer" } }),
            Some(json!({ "items": { "type": ["string", "integer"] } })),
        ),
        (
            "minItems",
            json!({ "type": "array", "items": { "type": "string" } }),
            json!(["a"]),
            json!({ "minItems": 2 }),
            Some(json!({ "minItems": 1 })),
        ),
        (
            "maxItems",
            json!({ "type": "array", "items": { "type": "string" } }),
            json!(["a"]),
            json!({ "maxItems": 0 }),
            Some(json!({ "maxItems": 3 })),
        ),
        (
            "uniqueItems",
            json!({ "type": "array", "items": { "type": "string" } }),
            json!(["a", "a"]),
            json!({ "uniqueItems": true }),
            None,
        ),
        (
            "properties",
            json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
            json!({ "x": "a" }),
            json!({ "properties": { "x": { "type": "string", "minLength": 5 } } }),
            Some(json!({ "properties": { "x": { "type": "string", "minLength": 1 } } })),
        ),
        (
            "additionalProperties",
            json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
            json!({ "x": "a", "y": 1 }),
            json!({ "additionalProperties": false }),
            Some(json!({ "additionalProperties": { "type": ["integer", "string"] } })),
        ),
        (
            "required",
            json!({ "type": "object", "properties": { "x": { "type": "string" }, "y": { "type": "string" } } }),
            json!({ "x": "a" }),
            json!({ "required": ["x", "y"] }),
            Some(json!({ "required": ["x"] })),
        ),
        (
            "minProperties",
            json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
            json!({ "x": "a" }),
            json!({ "minProperties": 3 }),
            Some(json!({ "minProperties": 1 })),
        ),
        (
            "maxProperties",
            json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
            json!({ "x": "a" }),
            json!({ "maxProperties": 0 }),
            Some(json!({ "maxProperties": 5 })),
        ),
    ];
    let with = |base: Value, sibling: Option<&Value>| {
        let mut out = base;
        for (k, v) in sibling.and_then(|s| s.as_object()).into_iter().flatten() {
            out[k] = v.clone();
        }
        out
    };
    let null = json!({ "type": "null" });
    let reference = json!({ "$ref": "#/$defs/t" });
    // (construct, the keywords it cannot take beside it, the argument
    // schema built from the target, the sample and the sibling).
    type Build = fn(
        &Value,
        &Value,
        Option<&Value>,
        &dyn Fn(Value, Option<&Value>) -> Value,
        &Value,
        &Value,
    ) -> Value;
    let constructs: Vec<(&str, &[&str], Build)> = vec![
        ("$ref", &[], |_, _, s, w, _, r| w(r.clone(), s)),
        ("anyOf", &[], |t, _, s, w, n, _| {
            w(json!({ "anyOf": [t, n] }), s)
        }),
        ("oneOf", &[], |t, _, s, w, n, _| {
            w(json!({ "oneOf": [t, n] }), s)
        }),
        ("enum", &["enum"], |_, v, s, w, _, _| {
            w(json!({ "enum": [v] }), s)
        }),
        ("const", &["const"], |_, v, s, w, _, _| {
            w(json!({ "const": v }), s)
        }),
        (
            "anyOf of a $ref",
            &[],
            |_, _, s, w, n, r| json!({ "anyOf": [w(r.clone(), s), n] }),
        ),
        ("$ref beside anyOf", &[], |t, _, s, w, n, r| {
            let mut out = w(r.clone(), s);
            out["anyOf"] = json!([t, n]);
            out
        }),
        (
            "enum inside oneOf",
            &["enum"],
            |_, v, s, w, n, _| json!({ "oneOf": [w(json!({ "enum": [v] }), s), n] }),
        ),
        ("type array", &["type"], |t, _, s, w, _, _| {
            let mut out = t.clone();
            out["type"] = json!([t["type"], "null"]);
            w(out, s)
        }),
    ];
    let norm = |target: &Value, argument: Value| {
        let root = json!({
            "type": "object",
            "properties": { "a": argument },
            "$defs": { "t": target },
        });
        normalize("t", &root, &root)
    };
    let reported = |frozen: &Value, now: &Value| {
        accepts_superset("t", "", frozen, now).is_err() || gains_required("t", frozen, now).is_err()
    };
    let passes = |frozen: &Value, now: &Value| {
        accepts_superset("t", "", frozen, now)
            .and_then(|()| gains_required("t", frozen, now))
            .map_err(|e| e.to_string())
    };
    let mut pairs = 0;
    for (construct, refuses, build) in &constructs {
        for (keyword, target, sample, tight, loose) in &keywords {
            if refuses.contains(keyword) {
                continue;
            }
            pairs += 1;
            let at = |s: Option<&Value>| {
                norm(target, build(target, sample, s, &with, &null, &reference))
            };
            let bare = at(None);
            let narrowed = at(Some(tight));
            assert!(
                reported(&bare, &narrowed),
                "{construct} + {keyword}: a new sibling {tight} must be reported\nfrozen {bare}\nnow {narrowed}"
            );
            passes(&narrowed, &bare).unwrap_or_else(|e| {
                panic!("{construct} + {keyword}: dropping the sibling {tight} widens: {e}")
            });
            for schema in [&bare, &narrowed] {
                passes(schema, schema).unwrap_or_else(|e| {
                    panic!("{construct} + {keyword}: {schema} must accept itself: {e}")
                });
            }
            if let Some(loose) = loose {
                let loosened = at(Some(loose));
                assert!(
                    reported(&loosened, &narrowed),
                    "{construct} + {keyword}: tightening {loose} to {tight} must be reported\nfrozen {loosened}\nnow {narrowed}"
                );
                passes(&narrowed, &loosened).unwrap_or_else(|e| {
                    panic!("{construct} + {keyword}: loosening {tight} to {loose} widens: {e}")
                });
                passes(&loosened, &loosened).unwrap_or_else(|e| {
                    panic!("{construct} + {keyword}: {loosened} must accept itself: {e}")
                });
            }
        }
    }
    assert_eq!(pairs, constructs.len() * keywords.len() - 4);

    // The case of the review verbatim: `additionalProperties: false`
    // beside an unchanged `anyOf` of objects.
    let objects = json!({ "anyOf": [
        { "type": "object", "properties": { "x": { "type": "string" } } },
        { "type": "object", "properties": { "y": { "type": "integer" } } }
    ] });
    let mut closed = objects.clone();
    closed["additionalProperties"] = json!(false);
    let frozen = normalize("t", &objects, &objects);
    let now = normalize("t", &closed, &closed);
    assert!(accepts_superset("t", "", &frozen, &now).is_err());
    assert!(accepts_superset("t", "", &now, &frozen).is_ok());

    // An annotation beside a construct restricts nothing: the construct is
    // recorded alone.
    let any_of = json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] });
    let mut annotated = any_of.clone();
    annotated["description"] = json!("a name");
    annotated["default"] = json!(null);
    assert_eq!(
        normalize("t", &annotated, &annotated),
        normalize("t", &any_of, &any_of)
    );

    // An unknown keyword beside a union still fails the extraction.
    let unknown = json!({ "anyOf": [{ "type": "string" }], "not": {} });
    let result = std::panic::catch_unwind(|| normalize("t", &unknown, &unknown));
    assert!(result.is_err(), "{unknown} must fail the extraction");
}

// ---------------------------------------------------------------------------
// The argument types behind the served tools
// ---------------------------------------------------------------------------

fn args_ok<A: serde::de::DeserializeOwned>(value: Value) -> Result<(), String> {
    serde_json::from_value::<A>(value)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Each tool's own argument type, by its handler's `Parameters<..>`. A
/// served tool missing from the table fails, naming it.
#[allow(clippy::type_complexity)]
const ARG_TYPES: &[(&str, fn(Value) -> Result<(), String>)] = &[
    ("agents_detect", |v| {
        args_ok::<super::args::AgentsDetectArgs>(v)
    }),
    ("connectors_list", |v| {
        args_ok::<super::args::WorkspaceArg>(v)
    }),
    ("correction_decide", |v| {
        args_ok::<super::corrections::CorrectionDecideArgs>(v)
    }),
    ("decision_ask", |v| {
        args_ok::<super::args::DecisionAskArgs>(v)
    }),
    ("judge_evaluate", |v| {
        args_ok::<super::judge::JudgeEvaluateArgs>(v)
    }),
    ("judge_example_save", |v| {
        args_ok::<super::judge::JudgeExampleSaveArgs>(v)
    }),
    ("judge_examples", |v| {
        args_ok::<super::judge::JudgeExamplesArgs>(v)
    }),
    ("judge_run_values", |v| {
        args_ok::<super::judge::JudgeRunValuesArgs>(v)
    }),
    ("known_problem_decide", |v| {
        args_ok::<super::args::KnownProblemDecideArgs>(v)
    }),
    ("known_problem_recheck", |v| {
        args_ok::<super::args::KnownProblemRecheckArgs>(v)
    }),
    ("playbook_adopt_report", |v| {
        args_ok::<super::args::AdoptReportArgs>(v)
    }),
    ("playbook_approve", |v| {
        args_ok::<super::args::PlaybookApproveArgs>(v)
    }),
    ("playbook_candidate", |v| {
        args_ok::<super::args::PlaybookIdArgs>(v)
    }),
    ("playbook_capture", |v| {
        args_ok::<super::args::PlaybookCaptureArgs>(v)
    }),
    ("playbook_catalog", |v| {
        args_ok::<super::args::PlaybookCatalogArgs>(v)
    }),
    ("playbook_corrections", |v| {
        args_ok::<super::corrections::CorrectionsArgs>(v)
    }),
    ("playbook_create", |v| {
        args_ok::<super::args::PlaybookWriteArgs>(v)
    }),
    ("playbook_delete", |v| {
        args_ok::<super::args::PlaybookIdArgs>(v)
    }),
    ("playbook_discover", |v| {
        args_ok::<super::args::PlaybookDiscoverArgs>(v)
    }),
    ("playbook_discover_dismiss", |v| {
        args_ok::<super::args::PlaybookDiscoverDismissArgs>(v)
    }),
    ("playbook_discover_results", |v| {
        args_ok::<super::args::PlaybookDiscoverResultsArgs>(v)
    }),
    ("playbook_discover_save", |v| {
        args_ok::<super::args::PlaybookDiscoverSaveArgs>(v)
    }),
    ("playbook_execute_plan", |v| {
        args_ok::<super::args::PlaybookExecutePlanArgs>(v)
    }),
    ("playbook_get", |v| {
        args_ok::<super::args::PlaybookGetArgs>(v)
    }),
    ("playbook_howto", |v| args_ok::<Value>(v)),
    ("playbook_interview", |v| args_ok::<Value>(v)),
    ("playbook_known_problems", |v| {
        args_ok::<super::args::KnownProblemsArgs>(v)
    }),
    ("playbook_list", |v| args_ok::<super::args::WorkspaceArg>(v)),
    ("playbook_make_global", |v| {
        args_ok::<super::args::PlaybookMakeGlobalArgs>(v)
    }),
    ("playbook_memory", |v| {
        args_ok::<super::args::PlaybookMemoryArgs>(v)
    }),
    ("playbook_prepare_run", |v| {
        args_ok::<super::args::PlaybookPrepareRunArgs>(v)
    }),
    ("playbook_run", |v| {
        args_ok::<super::args::PlaybookRunArgs>(v)
    }),
    ("playbook_scenario_run", |v| {
        args_ok::<super::scenario::PlaybookScenarioRunArgs>(v)
    }),
    ("playbook_scenarios", |v| {
        args_ok::<super::scenario::PlaybookScenariosArgs>(v)
    }),
    ("playbook_stats", |v| {
        args_ok::<super::args::PlaybookStatsArgs>(v)
    }),
    ("playbook_trash_list", |v| {
        args_ok::<super::args::WorkspaceArg>(v)
    }),
    ("playbook_trash_restore", |v| {
        args_ok::<super::args::TrashRestoreArgs>(v)
    }),
    ("playbook_trial", |v| {
        args_ok::<super::args::PlaybookTrialArgs>(v)
    }),
    ("playbook_update", |v| {
        args_ok::<super::args::PlaybookWriteArgs>(v)
    }),
    ("playbook_validate", |v| {
        args_ok::<super::args::PlaybookIdArgs>(v)
    }),
    ("profile_delete", |v| {
        args_ok::<super::args::ProfileDeleteArgs>(v)
    }),
    ("profile_get", |v| args_ok::<super::args::ProfileGetArgs>(v)),
    ("profile_howto", |v| args_ok::<Value>(v)),
    ("profile_list", |v| {
        args_ok::<super::args::ProfileListArgs>(v)
    }),
    ("profile_move", |v| {
        args_ok::<super::args::ProfileMoveArgs>(v)
    }),
    ("profile_write", |v| {
        args_ok::<super::args::ProfileWriteArgs>(v)
    }),
    ("projects_list", |v| args_ok::<Value>(v)),
    ("review_decide", |v| {
        args_ok::<super::args::ReviewDecideArgs>(v)
    }),
    ("run_answer", |v| args_ok::<super::args::RunAnswerArgs>(v)),
    ("run_evidence_read", |v| {
        args_ok::<super::args::RunEvidenceReadArgs>(v)
    }),
    ("run_events", |v| args_ok::<super::args::RunEventsArgs>(v)),
    ("run_grade", |v| args_ok::<super::args::RunGradeArgs>(v)),
    ("run_grade_read", |v| {
        args_ok::<super::args::RunGradeReadArgs>(v)
    }),
    ("run_grade_start", |v| {
        args_ok::<super::args::RunGradeStartArgs>(v)
    }),
    ("run_grade_submit", |v| {
        args_ok::<super::args::RunGradeSubmitArgs>(v)
    }),
    ("run_grades", |v| args_ok::<super::args::RunGradesArgs>(v)),
    ("run_intent_resolve", |v| {
        args_ok::<super::args::RunIntentResolveArgs>(v)
    }),
    ("run_notify", |v| args_ok::<super::args::RunNotifyArgs>(v)),
    ("run_pause", |v| args_ok::<super::args::RunPauseArgs>(v)),
    ("run_progress_report", |v| {
        args_ok::<super::args::ProgressReportArgs>(v)
    }),
    ("run_report", |v| args_ok::<super::args::RunReportArgs>(v)),
    ("run_report_html", |v| args_ok::<super::args::RunRefArgs>(v)),
    ("run_result_read", |v| {
        args_ok::<super::args::RunResultReadArgs>(v)
    }),
    ("run_results", |v| args_ok::<super::args::RunResultsArgs>(v)),
    ("run_resume", |v| args_ok::<super::args::RunResumeArgs>(v)),
    ("run_retro_context", |v| {
        args_ok::<super::args::RunRetroContextArgs>(v)
    }),
    ("run_scenario_capture", |v| {
        args_ok::<super::scenario::RunScenarioCaptureArgs>(v)
    }),
    ("run_status", |v| args_ok::<super::args::RunRefArgs>(v)),
    ("run_stop", |v| args_ok::<super::args::RunRefArgs>(v)),
    ("run_task_submit", |v| {
        args_ok::<super::args::RunTaskSubmitArgs>(v)
    }),
    ("run_wait", |v| args_ok::<super::args::RunWaitArgs>(v)),
    ("runs_list", |v| args_ok::<super::args::RunsListArgs>(v)),
    ("setup_howto", |v| args_ok::<Value>(v)),
    ("setup_plan", |v| args_ok::<super::args::SetupPlanArgs>(v)),
    ("subscriptions_set", |v| {
        args_ok::<super::args::SubscriptionsSetArgs>(v)
    }),
    ("suggestion_dismiss", |v| {
        args_ok::<super::args::SuggestionDismissArgs>(v)
    }),
    ("supervisor_context_append", |v| {
        args_ok::<super::args::SupervisorContextArgs>(v)
    }),
    ("supervisor_interrupt_attempt", |v| {
        args_ok::<super::args::SupervisorInterruptArgs>(v)
    }),
    ("supervisor_node_retry", |v| {
        args_ok::<super::args::SupervisorRetryArgs>(v)
    }),
    ("supervisor_patch_playbook", |v| {
        args_ok::<super::args::SupervisorPatchArgs>(v)
    }),
    ("supervisor_rebind_profile", |v| {
        args_ok::<super::args::SupervisorRebindArgs>(v)
    }),
    ("supervisor_report", |v| {
        args_ok::<super::args::SupervisorReportArgs>(v)
    }),
    ("supervisor_run_abort", |v| {
        args_ok::<super::args::SupervisorRunRefArgs>(v)
    }),
    ("supervisor_run_continue_from", |v| {
        args_ok::<super::args::SupervisorContinueArgs>(v)
    }),
    ("supervisor_run_inspect", |v| {
        args_ok::<super::args::SupervisorInspectArgs>(v)
    }),
    ("supervisor_run_pause", |v| {
        args_ok::<super::args::SupervisorRunRefArgs>(v)
    }),
    ("supervisor_wait_event", |v| {
        args_ok::<super::args::SupervisorWaitArgs>(v)
    }),
    ("trigger_list", |v| {
        args_ok::<super::args::TriggerListArgs>(v)
    }),
    ("trigger_test", |v| {
        args_ok::<super::args::TriggerTestArgs>(v)
    }),
    ("trust_list", |v| args_ok::<super::args::TrustListArgs>(v)),
    ("trust_revoke", |v| {
        args_ok::<super::args::TrustRevokeArgs>(v)
    }),
];

fn arg_type_of(name: &str) -> Option<fn(Value) -> Result<(), String>> {
    ARG_TYPES
        .iter()
        .find(|(n, _)| **n == *name)
        .map(|(_, f)| *f)
}

/// Every served tool has an entry in the table, and the table names no
/// tool the server lacks.
#[test]
fn every_served_tool_has_an_arg_type() {
    let served: Vec<String> = served().into_iter().map(|(n, _)| n).collect();
    for name in &served {
        assert!(
            arg_type_of(name).is_some(),
            "the served tool {name} has no ARG_TYPES entry: add its handler's argument type"
        );
    }
    for (name, _) in ARG_TYPES {
        assert!(
            served.iter().any(|s| s == name),
            "ARG_TYPES names {name}, which the server does not serve"
        );
    }
}

// ---------------------------------------------------------------------------
// The seven tool rules (design D1.3)
// ---------------------------------------------------------------------------

/// 1: every frozen tool is still served.
#[test]
fn every_frozen_tool_is_still_served() {
    let served: Vec<String> = served().into_iter().map(|(n, _)| n).collect();
    for name in frozen_tools().keys() {
        assert!(
            served.contains(name),
            "the frozen tool {name} is no longer served"
        );
    }
}

/// 2: the current normalized schema accepts a superset of the frozen one.
#[test]
fn every_frozen_input_is_still_accepted() {
    for name in frozen_tools().keys() {
        let frozen = frozen_tools()[name]["input"].clone();
        let served = served_tool(name);
        let now = normalize(name, &served, &served);
        accepts_superset(name, "", &frozen, &now).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// 3: no frozen object gains a required member.
#[test]
fn no_frozen_object_gains_a_required_member() {
    for name in frozen_tools().keys() {
        let frozen = frozen_tools()[name]["input"].clone();
        let served = served_tool(name);
        let now = normalize(name, &served, &served);
        gains_required(name, &frozen, &now).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// 4: each frozen union example still deserializes through the tool's own
/// argument type and is accepted by the current normalized schema. A union
/// argument's example is the list of its alternatives' values, one per
/// alternative of the frozen schema (nested unions flattened), at its
/// dotted path; every alternative has its value.
#[test]
fn every_union_example_still_deserializes() {
    check_union_examples(&frozen_tools());
}

/// Rule 4 over a tools map (the fixture, or the writer's proposed one).
fn check_union_examples(tools: &serde_json::Map<String, Value>) {
    for (name, entry) in tools {
        let name = name.clone();
        let check =
            arg_type_of(&name).unwrap_or_else(|| panic!("the tool {name} has no ARG_TYPES entry"));
        let served = served_tool(&name);
        let now = normalize(&name, &served, &served);
        let input = &entry["input"];
        let base = entry["base"].clone();
        let examples = entry["examples"].as_object().cloned().unwrap_or_default();
        for path in union_paths(input) {
            let values = examples
                .get(&path)
                .and_then(|v| v.as_array())
                .unwrap_or_else(|| {
                    panic!("{name}.{path}: the union argument has no example list; run the writer")
                });
            for alternative in leaf_alternatives(&schema_at(input, &path)) {
                assert!(
                    values.iter().any(|v| value_matches(&alternative, v)),
                    "{name}.{path}: no frozen example takes the alternative {alternative}"
                );
            }
            for value in values {
                let mut args = base.clone();
                place(input, &mut args, &path, value.clone());
                assert!(
                    value_matches(&now, &args),
                    "{name}.{path}: the frozen example {value} is no longer accepted by the schema"
                );
                check(args).unwrap_or_else(|e| {
                    panic!("{name}.{path}: the frozen example {value} no longer deserializes: {e}")
                });
            }
        }
    }
}

/// 5: `base` itself, and each frozen boundary value at its path,
/// deserializes through the tool's argument type. Every numeric or
/// enumerated argument path of the frozen schema holds its edges.
#[test]
fn every_boundary_value_is_still_accepted() {
    check_boundaries(&frozen_tools());
}

/// Rule 5 over a tools map (the fixture, or the writer's proposed one).
fn check_boundaries(tools: &serde_json::Map<String, Value>) {
    for (name, entry) in tools {
        let name = name.clone();
        let check =
            arg_type_of(&name).unwrap_or_else(|| panic!("the tool {name} has no ARG_TYPES entry"));
        let input = &entry["input"];
        let base = entry["base"].clone();
        check(base.clone())
            .unwrap_or_else(|e| panic!("{name}: the frozen base no longer deserializes: {e}"));
        let boundaries = entry["boundaries"].as_object().cloned().unwrap_or_default();
        for (path, edges) in synth_boundaries(input).as_object().expect("an object") {
            let frozen = boundaries
                .get(path)
                .and_then(|v| v.as_array())
                .unwrap_or_else(|| {
                    panic!("{name}.{path}: the argument has no frozen boundaries; run the writer")
                });
            for edge in edges.as_array().expect("a list") {
                assert!(
                    frozen.contains(edge),
                    "{name}.{path}: the edge {edge} of the frozen schema is not frozen; run the writer"
                );
            }
        }
        for (path, values) in &boundaries {
            for value in values.as_array().expect("a boundary list") {
                let mut args = base.clone();
                place(input, &mut args, path, value.clone());
                check(args.clone()).unwrap_or_else(|e| {
                    panic!(
                        "{name}: the frozen boundary value {value} at {path} no longer deserializes: {e}"
                    )
                });
            }
        }
    }
}

/// The segments of a dotted argument path: `a.b` is `a` then `b`, a
/// segment ending in `[]` is an array whose item the rest addresses.
fn path_segments(path: &str) -> Vec<(&str, bool)> {
    path.split('.')
        .map(|s| match s.strip_suffix("[]") {
            Some(name) => (name, true),
            None => (s, false),
        })
        .collect()
}

/// The object alternative of `schema` that declares the member `name`
/// (the schema itself when it is not a union).
fn object_with<'a>(schema: &'a Value, name: &str) -> Option<&'a Value> {
    if let Some(alternatives) = schema.get("union").and_then(|u| u.as_array()) {
        return alternatives.iter().find_map(|a| object_with(a, name));
    }
    schema
        .get("properties")
        .and_then(|p| p.get(name))
        .map(|_| schema)
}

/// The array alternative of `schema` (the schema itself when it is not a
/// union).
fn array_of(schema: &Value) -> Option<&Value> {
    if let Some(alternatives) = schema.get("union").and_then(|u| u.as_array()) {
        return alternatives.iter().find_map(array_of);
    }
    schema.get("items").map(|_| schema)
}

/// The normalized schema at the dotted argument `path` of `input`.
fn schema_at(input: &Value, path: &str) -> Value {
    let mut at = input.clone();
    for (name, array) in path_segments(path) {
        let object = object_with(&at, name)
            .unwrap_or_else(|| panic!("the schema holds no argument {path}"))
            .clone();
        at = object["properties"][name].clone();
        if array {
            at =
                array_of(&at).unwrap_or_else(|| panic!("the argument {path} is no array"))["items"]
                    .clone();
        }
    }
    at
}

/// Places `value` at the dotted argument `path` of `args`, building the
/// objects on the way from their own frozen required members (the
/// alternative of a union that declares the next member) and an array of
/// one item for a `[]` segment.
fn place(input: &Value, args: &mut Value, path: &str, value: Value) {
    let segments = path_segments(path);
    let mut schema = input.clone();
    let mut at = args;
    for (i, (name, array)) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let object = object_with(&schema, name)
            .unwrap_or_else(|| panic!("the frozen schema holds no argument {path}"))
            .clone();
        if !at.is_object() {
            *at = synth_value(&object);
        }
        let member = object["properties"][*name].clone();
        let map = at.as_object_mut().expect("an object");
        if *array {
            let items = array_of(&member)
                .unwrap_or_else(|| panic!("the argument {path} is no array"))["items"]
                .clone();
            let item = if last {
                value.clone()
            } else {
                synth_value(&items)
            };
            map.insert(name.to_string(), json!([item]));
            if last {
                return;
            }
            at = &mut map
                .get_mut(*name)
                .and_then(|a| a.as_array_mut())
                .expect("the array just placed")[0];
            schema = items;
        } else {
            if last {
                map.insert(name.to_string(), value);
                return;
            }
            at = map.entry(name.to_string()).or_insert(Value::Null);
            schema = member;
        }
    }
}

/// The alternatives of a normalized schema with nested unions flattened;
/// a schema that is no union is its own one alternative.
fn leaf_alternatives(schema: &Value) -> Vec<Value> {
    match schema.get("union").and_then(|u| u.as_array()) {
        Some(alternatives) => alternatives.iter().flat_map(leaf_alternatives).collect(),
        None => vec![schema.clone()],
    }
}

/// Every dotted argument path of a normalized schema whose own schema is a
/// union, through the object alternatives and array items on the way.
fn union_paths(input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    union_paths_into(input, "", &mut out);
    out.sort();
    out.dedup();
    out
}

fn union_paths_into(schema: &Value, prefix: &str, out: &mut Vec<String>) {
    for alternative in leaf_alternatives(schema) {
        if let Some(props) = alternative.get("properties").and_then(|p| p.as_object()) {
            for (name, prop) in props {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                if prop.get("union").is_some() {
                    out.push(path.clone());
                }
                union_paths_into(prop, &path, out);
            }
        }
        if let Some(items) = alternative.get("items")
            && !prefix.is_empty()
        {
            union_paths_into(items, &format!("{prefix}[]"), out);
        }
    }
}

/// Whether `value` is accepted by the normalized schema: its types, enum
/// and const, numeric bounds and integer width, string lengths, array
/// items and counts, and object members (required, declared, extra). A
/// `pattern` is not evaluated here; the argument type's own
/// deserialization stands for it.
fn value_matches(schema: &Value, value: &Value) -> bool {
    if let Some(parts) = schema.get("all").and_then(|a| a.as_array()) {
        return parts.iter().all(|p| value_matches(p, value));
    }
    if let Some(alternatives) = schema.get("union").and_then(|u| u.as_array()) {
        return alternatives.iter().any(|a| value_matches(a, value));
    }
    if let Some(values) = schema.get("enum").and_then(|e| e.as_array())
        && !values.contains(value)
    {
        return false;
    }
    if let Some(c) = schema.get("const")
        && c != value
    {
        return false;
    }
    let types = schema["types"].as_array().cloned().unwrap_or_default();
    let typed = types.iter().any(|t| match t.as_str().unwrap_or("") {
        "any" => true,
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        "number" => value.is_number(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    });
    if !typed {
        return false;
    }
    if let Some(n) = value.as_f64() {
        let bound = |k: &str| schema.get(k).and_then(|v| v.as_f64());
        let exact_top = || value.as_u64().map(|u| u as f64);
        if bound("minimum").is_some_and(|m| n < m)
            || bound("maximum").is_some_and(|m| n > m)
            || bound("exclusiveMinimum").is_some_and(|m| n <= m)
            || bound("exclusiveMaximum").is_some_and(|m| n >= m)
        {
            return false;
        }
        if let Some((low, top)) = schema
            .get("format")
            .and_then(|f| f.as_str())
            .and_then(format_domain)
            && (n < low || exact_top().unwrap_or(n) > top)
        {
            return false;
        }
    }
    if let Some(s) = value.as_str() {
        let length = s.chars().count() as u64;
        if schema
            .get("minLength")
            .and_then(|v| v.as_u64())
            .is_some_and(|m| length < m)
            || schema
                .get("maxLength")
                .and_then(|v| v.as_u64())
                .is_some_and(|m| length > m)
        {
            return false;
        }
    }
    if let Some(list) = value.as_array() {
        let count = list.len() as u64;
        if schema
            .get("minItems")
            .and_then(|v| v.as_u64())
            .is_some_and(|m| count < m)
            || schema
                .get("maxItems")
                .and_then(|v| v.as_u64())
                .is_some_and(|m| count > m)
        {
            return false;
        }
        if let Some(items) = schema.get("items")
            && !list.iter().all(|v| value_matches(items, v))
        {
            return false;
        }
    }
    if let Some(map) = value.as_object()
        && schema
            .get("types")
            .and_then(|t| t.as_array())
            .is_some_and(|t| t.contains(&json!("object")))
    {
        let props = schema.get("properties").and_then(|p| p.as_object());
        for member in schema
            .get("required")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
        {
            if !map.contains_key(member.as_str().unwrap_or("")) {
                return false;
            }
        }
        for (key, v) in map {
            match props.and_then(|p| p.get(key)) {
                Some(prop) => {
                    if !value_matches(prop, v) {
                        return false;
                    }
                }
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => return false,
                    Some(extra) if extra.is_object() && !value_matches(extra, v) => {
                        return false;
                    }
                    _ => {}
                },
            }
        }
    }
    true
}

/// 6: a frozen read-only tool stays read-only.
#[test]
fn a_frozen_read_only_tool_stays_read_only() {
    use super::read_only::{READ_ONLY, ReadOnlyUse};
    let class = |name: &str| -> Option<&'static str> {
        READ_ONLY
            .iter()
            .find(|(n, _)| **n == *name)
            .map(|(_, u)| match u {
                ReadOnlyUse::Full => "full",
                ReadOnlyUse::Restricted => "restricted",
            })
    };
    for (name, entry) in frozen_tools() {
        let frozen = entry["read_only"].as_str().expect("a class");
        match frozen {
            "full" => {
                assert!(
                    class(&name) == Some("full"),
                    "the frozen read-only tool {name} is no longer full"
                );
            }
            "restricted" => {
                assert!(
                    matches!(class(&name), Some("restricted") | Some("full")),
                    "the frozen restricted tool {name} lost its read-only service"
                );
            }
            _ => {}
        }
    }
}

/// 7: a served tool, or an argument of a frozen tool, the freeze lacks.
#[test]
fn every_served_tool_and_argument_is_frozen() {
    let served_tools: Vec<String> = served().into_iter().map(|(n, _)| n).collect();
    for name in &served_tools {
        assert!(
            frozen_tools().contains_key(name),
            "new tool `{name}`: run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze"
        );
        let frozen = frozen_tools()[name]["input"].clone();
        let served = served_tool(name);
        let now = normalize(name, &served, &served);
        let mut missing = Vec::new();
        list_arguments(&now, "", &mut missing);
        for path in missing {
            if !frozen_has_argument(&frozen, &path) {
                panic!(
                    "new argument `{name}.{path}`: run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze"
                );
            }
        }
    }
}

/// Every argument path of a normalized schema: a property name at each
/// object.
fn list_arguments(value: &Value, prefix: &str, out: &mut Vec<String>) {
    if let Some(parts) = value.get("all").and_then(|a| a.as_array()) {
        for part in parts {
            list_arguments(part, prefix, out);
        }
    }
    if let Some(alternatives) = value.get("union").and_then(|u| u.as_array()) {
        for alternative in alternatives {
            list_arguments(alternative, prefix, out);
        }
    }
    if let Some(props) = value.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            out.push(path.clone());
            list_arguments(prop, &path, out);
        }
    }
}

/// Whether the frozen input schema holds the argument at the dotted
/// property `path`: each segment is a member of its object's
/// `properties` (the same walk `list_arguments` does over the served
/// schema).
fn frozen_has_argument(frozen: &Value, path: &str) -> bool {
    // A union: the member lives in one alternative (the same walk
    // `list_arguments` does).
    if let Some(alternatives) = frozen
        .get("union")
        .or_else(|| frozen.get("all"))
        .and_then(|u| u.as_array())
    {
        return alternatives.iter().any(|a| frozen_has_argument(a, path));
    }
    let Some((segment, tail)) = path.split_once('.') else {
        return frozen
            .get("properties")
            .is_some_and(|p| p.get(path).is_some());
    };
    frozen
        .get("properties")
        .and_then(|p| p.get(segment))
        .is_some_and(|next| frozen_has_argument(next, tail))
}

// ---------------------------------------------------------------------------
// The machine codes (design D1.5 item 1)
// ---------------------------------------------------------------------------

/// `(group, code, source symbol)`: every machine code of the scanned
/// groups (gate, mcp, patch). Where a constant exists, the code is that
/// constant itself, so the table reads the value the production code
/// exports; a code with no constant (a JSON member name, a literal inside
/// a function) is a literal here and its source symbol names the function
/// that produces it.
const MACHINE_CODES: &[(&str, &str, &str)] = &[
    (
        "gate",
        apb_engine::start_key::CONFLICT,
        "apb_engine::start_key::CONFLICT",
    ),
    (
        "gate",
        apb_engine::gate::launch::TRIGGER_POLICY,
        "apb_engine::gate::launch::TRIGGER_POLICY",
    ),
    (
        "gate",
        apb_engine::gate::launch::BINDING_CHANGED,
        "apb_engine::gate::launch::BINDING_CHANGED",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_TRIGGER,
        "apb_engine::gate::billing::BILLING_TRIGGER",
    ),
    (
        "mcp",
        "already_started",
        "apb_mcp::tools::run::already_started",
    ),
    (
        "mcp",
        crate::tools::templates::IN_RUN_STEP,
        "apb_mcp::tools::templates::IN_RUN_STEP",
    ),
    (
        "mcp",
        apb_core::trigger::RULES_UNVERIFIED,
        "apb_core::trigger::model::RULES_UNVERIFIED",
    ),
    (
        "mcp",
        "trigger_rule_not_found",
        "apb_mcp::tools::trigger::stored_entry",
    ),
    (
        "mcp",
        crate::tools::trigger::TRIGGER_TEST_REFUSED,
        "apb_mcp::tools::trigger::TRIGGER_TEST_REFUSED",
    ),
    (
        "mcp",
        "supervise_self_global_unsupported",
        "apb_mcp::tools::run",
    ),
    (
        "patch",
        "patch_global_unsupported",
        "apb_engine::run_origin::patch_refusal",
    ),
    (
        "patch",
        "patch_origin_unreadable",
        "apb_engine::run_origin::patch_refusal",
    ),
    (
        "gate",
        apb_engine::gate::launch::CHALLENGE_SPENT,
        "apb_engine::gate::launch::CHALLENGE_SPENT",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_ASK,
        "apb_engine::gate::billing::BILLING_ASK",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_DENIED,
        "apb_engine::gate::billing::BILLING_DENIED",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_STEP,
        "apb_engine::gate::billing::BILLING_STEP",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_ENGINE,
        "apb_engine::gate::billing::BILLING_ENGINE",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_TRIGGER,
        "apb_engine::gate::billing::BILLING_TRIGGER",
    ),
    (
        "mcp",
        apb_mcp::tools::budget::BUDGET_NEEDS_DECISION,
        "apb_mcp::tools::budget::BUDGET_NEEDS_DECISION",
    ),
    (
        "mcp",
        apb_mcp::tools::budget::BUDGET_CHANGE_NEEDS_CONFIRMATION,
        "apb_mcp::tools::budget::BUDGET_CHANGE_NEEDS_CONFIRMATION",
    ),
    (
        "gate",
        apb_engine::gate::cloud::KEY_MISSING,
        "apb_engine::gate::cloud::KEY_MISSING",
    ),
    (
        "gate",
        "param_needs_value",
        "apb_engine::gate::params::PARAM_NEEDS_VALUE",
    ),
    (
        "gate",
        "param_pattern_mismatch",
        "apb_engine::gate::params::PARAM_PATTERN_MISMATCH",
    ),
    (
        "gate",
        crate::tools::notify::NOTIFY_UNAVAILABLE,
        "apb_mcp::tools::notify::NOTIFY_UNAVAILABLE",
    ),
    (
        "gate",
        crate::tools::notify::INVALID_NOTIFY,
        "apb_mcp::tools::notify::INVALID_NOTIFY",
    ),
    (
        "gate",
        "launch_requires_confirmation",
        "apb_engine::gate::launch::ASK_POLICY",
    ),
    (
        "gate",
        "launch_requires_person",
        "apb_engine::gate::launch::DENY_POLICY",
    ),
    (
        "gate",
        "launch_requires_rule_approval",
        "apb_engine::gate::launch::TRIGGER_POLICY",
    ),
    (
        "gate",
        apb_engine::gate::launch::BINDING_CHANGED,
        "apb_engine::gate::launch::BINDING_CHANGED",
    ),
    (
        "gate",
        "launch_refused_in_run_step",
        "apb_engine::gate::launch::STEP_POLICY",
    ),
    (
        "gate",
        "launch_refused_for_engine",
        "apb_engine::gate::launch::ENGINE_POLICY",
    ),
    (
        "gate",
        "launch_choice_changed",
        "apb_engine::gate::launch::CHOICE_CHANGED",
    ),
    (
        "gate",
        "launch_policy_changed",
        "apb_engine::gate::launch::POLICY_CHANGED",
    ),
    (
        "gate",
        "policy_file_invalid",
        "apb_engine::gate::launch::SETTINGS_INVALID",
    ),
    (
        "gate",
        apb_engine::gate::launch::CHALLENGE_SPENT,
        "apb_engine::gate::launch::CHALLENGE_SPENT",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_DENIED,
        "apb_engine::gate::billing::BILLING_DENIED",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_ENGINE,
        "apb_engine::gate::billing::BILLING_ENGINE",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_STEP,
        "apb_engine::gate::billing::BILLING_STEP",
    ),
    (
        "gate",
        apb_engine::gate::billing::BILLING_ASK,
        "apb_engine::gate::billing::BILLING_ASK",
    ),
    (
        "gate",
        apb_engine::gate::launch::CHILD_NOT_COVERED,
        "apb_engine::gate::launch::CHILD_NOT_COVERED",
    ),
    (
        "gate",
        apb_engine::gate::launch::TREE_UNVERIFIED,
        "apb_engine::gate::launch::TREE_UNVERIFIED",
    ),
    (
        "gate",
        apb_engine::run_config::PROBATION_REQUIRED,
        "apb_engine::run_config::PROBATION_REQUIRED",
    ),
    (
        "gate",
        apb_core::start_key::INVALID,
        "apb_core::start_key::INVALID",
    ),
    (
        "gate",
        apb_engine::run_config::UNATTENDED_UNSUPPORTED,
        "apb_engine::run_config::UNATTENDED_UNSUPPORTED",
    ),
    (
        "mcp",
        "budget_change_refused",
        "apb_mcp::tools::budget::confirm_budget_change",
    ),
    (
        "mcp",
        crate::tools::budget::BUDGET_CHANGE_NEEDS_CONFIRMATION,
        "apb_mcp::tools::budget::BUDGET_CHANGE_NEEDS_CONFIRMATION",
    ),
    (
        "mcp",
        crate::tools::budget::BUDGET_NEEDS_DECISION,
        "apb_mcp::tools::budget::BUDGET_NEEDS_DECISION",
    ),
    (
        "mcp",
        apb_engine::budget::resume::BUDGET_PAUSED,
        "apb_engine::budget::resume::BUDGET_PAUSED",
    ),
    (
        "mcp",
        "cannot_verify_current_workspace",
        "apb_mcp::server::run::playbook_prepare_run",
    ),
    (
        "mcp",
        "digest_mismatch",
        "apb_mcp::tools::trial::approve_playbook",
    ),
    (
        "mcp",
        "digest_required",
        "apb_mcp::tools::trial::approve_playbook",
    ),
    ("mcp", "in_project", "apb_mcp::setup::observe::IN_PROJECT"),
    (
        "mcp",
        "invalid_arguments",
        "apb_mcp::server::quality::invalid",
    ),
    ("mcp", "invalid_budget", "apb_mcp::tools::budget::invalid"),
    (
        "mcp",
        crate::tools::notify::INVALID_NOTIFY,
        "apb_mcp::tools::notify::INVALID_NOTIFY",
    ),
    (
        "mcp",
        "invalid_request",
        "apb_mcp::tools::decision::decision_ask",
    ),
    (
        "mcp",
        "invalid_scenario",
        "apb_mcp::tools::scenario::run_scenario",
    ),
    (
        "mcp",
        apb_engine::gate::launch::TRIGGER_POLICY,
        "apb_engine::gate::launch::TRIGGER_POLICY",
    ),
    (
        "mcp",
        "node_without_retry_now",
        "apb_mcp::tools::run::retry_now_arg_refusal",
    ),
    (
        "mcp",
        crate::tools::notify::NOTIFY_UNAVAILABLE,
        "apb_mcp::tools::notify::NOTIFY_UNAVAILABLE",
    ),
    (
        "mcp",
        "playbook_store_missing",
        "apb_mcp::advisory_tools::playbook_adopt_report",
    ),
    (
        "mcp",
        "playbook_unloadable",
        "apb_mcp::advisory_tools::playbook_adopt_report",
    ),
    (
        "mcp",
        "prompt_override_with_deferred_retry",
        "apb_mcp::tools::supervisor::node_retry",
    ),
    (
        "mcp",
        apb_engine::decision::standalone::READ_ONLY_SKIP,
        "apb_engine::decision::standalone::READ_ONLY_SKIP",
    ),
    (
        "mcp",
        crate::tools::actions::READ_ONLY_REFUSAL,
        "apb_mcp::tools::actions::READ_ONLY_REFUSAL",
    ),
    (
        "mcp",
        "retry_now_with_from_node",
        "apb_mcp::tools::run::retry_now_arg_refusal",
    ),
    (
        "mcp",
        crate::tools::notify::RUN_ENDED,
        "apb_mcp::tools::notify::RUN_ENDED",
    ),
    (
        "mcp",
        apb_engine::start_key::CONFLICT,
        "apb_engine::start_key::CONFLICT",
    ),
    (
        "mcp",
        crate::tools::trigger::TRIGGER_APPROVAL_CHANGED,
        "apb_mcp::tools::trigger::TRIGGER_APPROVAL_CHANGED",
    ),
    (
        "mcp",
        crate::tools::trigger::TRIGGER_BUDGET_REQUIRED,
        "apb_mcp::tools::trigger::TRIGGER_BUDGET_REQUIRED",
    ),
    (
        "mcp",
        crate::tools::trigger::TRIGGER_KEY_UNAVAILABLE,
        "apb_mcp::tools::trigger::TRIGGER_KEY_UNAVAILABLE",
    ),
    (
        "mcp",
        crate::tools::trigger::TRIGGER_LAUNCH_DENIED,
        "apb_mcp::tools::trigger::TRIGGER_LAUNCH_DENIED",
    ),
    (
        "mcp",
        "unknown_action",
        "apb_mcp::server::playbook::playbook_memory",
    ),
    (
        "mcp",
        "unknown_kind",
        "apb_mcp::tools::decision::decision_ask",
    ),
    (
        "mcp",
        "unknown_status",
        "apb_mcp::tools::run::run_task_submit",
    ),
    (
        "mcp",
        "unknown_variant",
        "apb_mcp::server::playbook::playbook_make_global",
    ),
    (
        "mcp",
        "unknown_what",
        "apb_mcp::server::playbook::playbook_memory",
    ),
    (
        "mcp",
        "use_playbook_run_for_current_workspace",
        "apb_mcp::server::run::playbook_prepare_run",
    ),
    (
        "mcp",
        "workspace_param_unsupported",
        "apb_mcp::server::playbook::playbook_memory",
    ),
    (
        "mcp",
        "workspace_unknown",
        "apb_mcp::server::effective_root",
    ),
    (
        "mcp",
        "workspace_unreachable",
        "apb_mcp::server::effective_root",
    ),
    (
        "gate",
        "connector_env_missing",
        "apb_engine::gate::check_connectors",
    ),
    (
        "gate",
        "connector_unresolved",
        "apb_engine::gate::check_connectors",
    ),
    (
        "gate",
        "cross_workspace_requires_plan",
        "apb_engine::gate::check_run",
    ),
    (
        "gate",
        "definition_unreadable",
        "apb_engine::gate::definition_digest_of",
    ),
    (
        "gate",
        "draft_requires_trial",
        "apb_engine::gate::check_lifecycle",
    ),
    (
        "gate",
        "imported_scripts_require_approval",
        "apb_engine::gate::check_import_approval",
    ),
    (
        "gate",
        "manifest_unwritable",
        "apb_engine::gate::resume_consent_need_inner",
    ),
    (
        "gate",
        "mcp_tools_unpinned",
        "apb_engine::gate::check_connectors",
    ),
    ("gate", "not_found", "apb_engine::gate::preflight"),
    (
        "gate",
        "profile_unresolved",
        "apb_engine::gate::check_rebind",
    ),
    ("gate", "requires_unmet", "apb_engine::gate::check_requires"),
    (
        "gate",
        "requires_unsafe_command",
        "apb_engine::gate::check_requires",
    ),
    (
        "gate",
        "requires_unsafe_path",
        "apb_engine::gate::check_requires",
    ),
    (
        "gate",
        "retired_not_runnable",
        "apb_engine::gate::check_lifecycle",
    ),
    (
        "gate",
        "run_not_created_locally",
        "apb_engine::gate::check_resume",
    ),
    (
        "gate",
        "snapshot_unreadable",
        "apb_engine::gate::check_resume",
    ),
    (
        "gate",
        "sub_playbook_cycle",
        "apb_engine::gate::collect_children",
    ),
    (
        "gate",
        "unapproved_connector_account",
        "apb_engine::gate::check_connectors",
    ),
    (
        "gate",
        "untrusted_connector_requires_approve",
        "apb_engine::gate::check_connectors",
    ),
    (
        "gate",
        "untrusted_profile_requires_acknowledge",
        "apb_engine::gate::check_profile_bundles",
    ),
    (
        "gate",
        "untrusted_requires_acknowledge",
        "apb_engine::gate::check_digest_trust",
    ),
    // Codes the scan reads since it resolves every member and every
    // file-level constant of a group (design D1.5 item 1.2).
    (
        "gate",
        apb_core::isolation::contract::UNAVAILABLE,
        "apb_core::isolation::contract::UNAVAILABLE",
    ),
    (
        "gate",
        apb_core::launch::floor::INVALID,
        "apb_core::launch::floor::INVALID",
    ),
    (
        "gate",
        "cloud_model_unknown",
        "apb_core::cloud_models::check::CloudIssue::code",
    ),
    (
        "gate",
        "cloud_model_no_tools",
        "apb_core::cloud_models::check::CloudIssue::code",
    ),
    (
        "gate",
        "cloud_model_not_chat",
        "apb_core::cloud_models::check::CloudIssue::code",
    ),
    (
        "gate",
        "cloud_effort_unsupported",
        "apb_core::cloud_models::check::CloudIssue::code",
    ),
    (
        "gate",
        apb_engine::run_redact::SECRET_SET_UNREADABLE,
        "apb_engine::run_redact::SECRET_SET_UNREADABLE",
    ),
    (
        "gate",
        apb_engine::run_redact::SECRET_PARAM_RENDERED,
        "apb_engine::run_redact::SECRET_PARAM_RENDERED",
    ),
    (
        "gate",
        apb_engine::run_redact::SECRET_DECLARATION_DROPPED,
        "apb_engine::run_redact::SECRET_DECLARATION_DROPPED",
    ),
    (
        "mcp",
        apb_engine::run_deadline::DEADLINE_INVALID,
        "apb_engine::run_deadline::DEADLINE_INVALID",
    ),
    ("mcp", crate::setup::RUN_STEP, "apb_mcp::setup::RUN_STEP"),
    (
        "mcp",
        apb_core::start_key::INVALID,
        "apb_core::start_key::INVALID",
    ),
    (
        "mcp",
        apb_engine::start_key::REUSE_REFUSED,
        "apb_engine::start_key::REUSE_REFUSED",
    ),
    (
        "mcp",
        apb_engine::start_key::BUSY,
        "apb_engine::start_key::BUSY",
    ),
    (
        "mcp",
        apb_engine::start_key::UNREADABLE,
        "apb_engine::start_key::UNREADABLE",
    ),
    (
        "mcp",
        "exactly_one_of_run_id_or_token_required",
        "apb_mcp::server::run::run_answer",
    ),
    (
        "mcp",
        "supervisor_session_requires_token",
        "apb_mcp::server::run::run_answer",
    ),
    (
        "mcp",
        "invalid_plan_token",
        "apb_mcp::server::run::playbook_execute_plan_for",
    ),
    (
        "mcp",
        "plan_expired",
        "apb_mcp::server::run::playbook_execute_plan_for",
    ),
    (
        "mcp",
        "plan_replayed",
        "apb_mcp::server::run::playbook_execute_plan_for",
    ),
    (
        "mcp",
        "plan_stale",
        "apb_mcp::server::run::playbook_execute_plan_for",
    ),
    (
        "mcp",
        "unknown_execution",
        "apb_mcp::server::run::playbook_run_for",
    ),
    (
        "mcp",
        "unknown_scope",
        "apb_mcp::server::run::playbook_run_for",
    ),
    ("mcp", "unknown_format", "apb_mcp::server::run::run_report"),
    (
        "mcp",
        "invalid_playbook",
        "apb_mcp::tools::scenario::run_scenario",
    ),
    (
        "mcp",
        "not_capturable",
        "apb_mcp::tools::scenario_capture::capture",
    ),
    (
        "mcp",
        "stale_preview",
        "apb_mcp::tools::scenario_capture::capture",
    ),
    (
        "mcp",
        "scenario_exists",
        "apb_mcp::tools::scenario_capture::capture",
    ),
    (
        "mcp",
        "store_path_linked",
        "apb_mcp::tools::capture::refuse_linked_store",
    ),
    (
        "mcp",
        "unknown_version",
        "apb_mcp::tools::corrections::correction_decide",
    ),
    (
        "mcp",
        "notify_failed",
        "apb_mcp::tools::notify::NotifyError::code",
    ),
    ("mcp", "not_found", "apb_engine::retry_wait::plan_retry_now"),
    (
        "mcp",
        "run_finished",
        "apb_engine::retry_wait::plan_retry_now",
    ),
    (
        "mcp",
        "no_deferred_retry",
        "apb_engine::retry_wait::plan_retry_now",
    ),
    ("mcp", "digest_changed", "apb_mcp::promote::make_global"),
    ("mcp", "copy_unreadable", "apb_mcp::promote::make_global"),
    (
        "mcp",
        "trigger_rule_busy",
        "apb_mcp::tools::trigger::rule_error_value",
    ),
    (
        "mcp",
        "trigger_project_unregistered",
        "apb_mcp::tools::trigger::rule_error_value",
    ),
    (
        "mcp",
        "trigger_enabled_not_settable",
        "apb_mcp::tools::trigger::rule_error_value",
    ),
    (
        "mcp",
        "rules_store_full",
        "apb_mcp::tools::trigger::rule_error_value",
    ),
    (
        "mcp",
        "trigger_rule_invalid",
        "apb_mcp::tools::trigger::rule_error_value",
    ),
    ("mcp", "off", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "send", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "redact", "apb_mcp::catalog_rank::error_fields"),
    (
        "mcp",
        "state_too_large",
        "apb_mcp::catalog_rank::error_fields",
    ),
    ("mcp", "busy", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "budget", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "invalid", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "unavailable", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "rate_limited", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "auth", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "timeout", "apb_mcp::catalog_rank::error_fields"),
    ("mcp", "cancelled", "apb_mcp::catalog_rank::error_fields"),
    (
        "mcp",
        "answer_question",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "decide_review",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "decide_publish",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "submit_host_task",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "supervisor_retry_node",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "supervisor_continue_from",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "supervisor_abort",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "resume_run",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "resume_with_budget",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "retry_now",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "resolve_intent",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "wait_again",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
    (
        "mcp",
        "read_report",
        "apb_engine::next_action::types::ActionCode::as_str",
    ),
];

/// The typed groups 1 to 6, each value the production enumeration exports.
fn typed_machine_codes() -> Vec<(&'static str, String)> {
    use apb_core::isolation::contract;
    let mut out = Vec::new();
    for r in contract::ALL_REASONS {
        out.push(("reason", r.id().to_string()));
    }
    for c in contract::ALL_CODES {
        out.push(("isolation", c.id().to_string()));
    }
    for r in apb_core::publish::Refusal::ALL {
        out.push(("publish", r.code().to_string()));
    }
    for r in apb_engine::decision_channel::CallerRefusal::ALL {
        out.push((
            "caller",
            serde_json::to_value(r)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        ));
    }
    for r in apb_engine::next_action::start::StartActionCode::ALL {
        out.push((
            "start_action",
            serde_json::to_value(r)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        ));
    }
    for g in apb_core::validate::GuardedFeature::ALL {
        out.push(("guard", g.as_str().to_string()));
    }
    out
}

/// 1: each line of the fixture is a value of its typed source or an entry
/// of `MACHINE_CODES`.
#[test]
fn every_frozen_machine_code_is_still_produced() {
    let typed = typed_machine_codes();
    let table: Vec<(&str, &str)> = MACHINE_CODES.iter().map(|(g, c, _)| (*g, *c)).collect();
    for line in fixture("machine-codes-1.0.txt")
        .lines()
        .filter(|l| !l.is_empty())
    {
        let (group, code) = line
            .split_once(' ')
            .unwrap_or_else(|| panic!("machine-codes-1.0.txt line is `<group> <code>`: {line}"));
        let known = typed.iter().any(|(g, c)| *g == group && c == code)
            || table.iter().any(|(g, c)| *g == group && *c == code);
        assert!(
            known,
            "the frozen machine code {group} {code} is produced by no typed source and no MACHINE_CODES entry"
        );
    }
}

/// Codes the scan finds but the freeze leaves out, each with the
/// decision that excludes it. The trigger dry test's four values stay
/// outside the promise (design D1.6 item 4): a code apb only writes into
/// a value of a result whose keys are not frozen is not frozen, and
/// `trigger_test` is not a tool a host is told to read.
const SCAN_EXCLUDED: &[(&str, &str)] = &[
    ("mcp", "trigger_launch_denied"),
    ("mcp", "trigger_approval_changed"),
    ("mcp", "trigger_budget_required"),
    ("mcp", "trigger_key_unavailable"),
];

/// A word that reads as a machine code: lowercase snake case, at least
/// two parts, never a single word or a whole sentence.
fn is_machine_code_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && word.contains('_')
        && !word.starts_with('_')
        && !word.ends_with('_')
        && word.len() >= 8
        && word.split('_').all(|part| !part.is_empty())
}

/// One Rust source, lexed: `masked` is the text with every string and
/// char literal and every comment blanked to spaces (newlines kept, byte
/// offsets unchanged), so brackets and keywords are read only where they
/// are code; `strings` are the string literals as `(start, end, content)`,
/// `start..end` the whole literal in the text, `content` its raw text
/// between the quotes.
struct Lexed {
    text: String,
    masked: Vec<u8>,
    strings: Vec<(usize, usize, String)>,
}

fn lex(text: &str) -> Lexed {
    let b = text.as_bytes();
    let mut masked = b.to_vec();
    let mut strings = Vec::new();
    let blank = |m: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut m[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = text[i..].find('\n').map_or(b.len(), |n| i + n);
            blank(&mut masked, i, end);
            i = end;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < b.len() && depth > 0 {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut masked, i, j);
            i = j;
        } else if (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r')))
            && (i == 0 || !ident(b[i - 1]))
            && {
                let k = if c == b'b' { i + 2 } else { i + 1 };
                let hashes = b[k..].iter().take_while(|x| **x == b'#').count();
                b.get(k + hashes) == Some(&b'"')
            }
        {
            let k = if c == b'b' { i + 2 } else { i + 1 };
            let hashes = b[k..].iter().take_while(|x| **x == b'#').count();
            let open = k + hashes + 1;
            let close: String = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            let end_quote = text[open..].find(&close).map_or(b.len(), |n| open + n);
            let end = (end_quote + close.len()).min(b.len());
            strings.push((i, end, text[open..end_quote].to_string()));
            blank(&mut masked, i, end);
            i = end;
        } else if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(b.len());
            strings.push((i, end, text[i + 1..j.min(b.len())].to_string()));
            blank(&mut masked, i, end);
            i = end;
        } else if c == b'\'' {
            // A char literal (`'x'`, `'\n'`, `'\u{..}'`, `'{'`); otherwise a
            // lifetime, which stays.
            let end = if b.get(i + 1) == Some(&b'\\') {
                text[i + 2..].find('\'').map(|n| i + 2 + n + 1)
            } else {
                let ch = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                (b.get(i + 1 + ch) == Some(&b'\'')).then_some(i + 2 + ch)
            };
            match end {
                Some(end) => {
                    blank(&mut masked, i, end);
                    i = end;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    Lexed {
        text: text.to_string(),
        masked,
        strings,
    }
}

impl Lexed {
    fn masked(&self) -> &str {
        std::str::from_utf8(&self.masked).expect("masking keeps UTF-8 boundaries")
    }

    /// The offset of the bracket closing the one at `open`, by the masked
    /// text.
    fn closing(&self, open: usize) -> usize {
        let mut depth = 0i32;
        for (i, c) in self.masked.iter().enumerate().skip(open) {
            match c {
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        self.masked.len()
    }

    /// Blanks `from..to` in both texts (newlines kept) and drops the
    /// string literals inside.
    fn cut(&mut self, from: usize, to: usize) {
        let mut text = std::mem::take(&mut self.text).into_bytes();
        for (byte, mask) in text[from..to].iter_mut().zip(&mut self.masked[from..to]) {
            if *byte != b'\n' {
                *byte = b' ';
                *mask = b' ';
            }
        }
        self.text = String::from_utf8_lossy(&text).into_owned();
        self.strings.retain(|(s, _, _)| *s < from || *s >= to);
    }

    /// The name of the function the code at `at` is in: the last `fn
    /// <name>` before it.
    fn function_at(&self, at: usize) -> String {
        let masked = &self.masked()[..at];
        let mut name = String::from("<top level>");
        let mut from = 0;
        while let Some(found) = masked[from..].find("fn ") {
            let start = from + found;
            let before_ok = start == 0
                || !masked.as_bytes()[start - 1].is_ascii_alphanumeric()
                    && masked.as_bytes()[start - 1] != b'_';
            let rest = masked[start + 3..].trim_start();
            let ident: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if before_ok && !ident.is_empty() {
                name = ident;
            }
            from = start + 3;
        }
        name
    }
}

/// Cuts every item a test-only `cfg` guards (`#[cfg(test)]` and
/// `#[cfg(any(test, feature = "testing"))]`, the `testing` feature being
/// a compile error in a release build): an inline module, a function, an
/// impl or a statement, through its closing brace or semicolon. Returns
/// the names of the test modules declared as files (`mod tests;`).
fn cut_test_items(lexed: &mut Lexed) -> Vec<String> {
    let mut files = Vec::new();
    for marker in ["#[cfg(test)]", "#[cfg(any(test, feature = \"testing\"))]"] {
        loop {
            let masked = lexed.masked().to_string();
            // The marker's string literal is blanked in the masked text, so
            // the testing form is found by its code part.
            let probe = if marker.contains("feature") {
                "#[cfg(any(test, feature = "
            } else {
                marker
            };
            let Some(at) = masked.find(probe) else {
                break;
            };
            let attr_end = lexed.closing(at + 1) + 1;
            // Further attributes on the same item.
            let mut item = attr_end;
            loop {
                let rest = &masked[item..];
                let skip = rest.len() - rest.trim_start().len();
                if rest.trim_start().starts_with("#[") {
                    item = lexed.closing(item + skip + 1) + 1;
                } else {
                    item += skip;
                    break;
                }
            }
            let rest = &masked[item..];
            let semi = rest.find(';');
            let brace = rest.find('{');
            let end = match (semi, brace) {
                (Some(s), Some(b)) if s < b => item + s + 1,
                (Some(s), None) => item + s + 1,
                (_, Some(b)) => lexed.closing(item + b) + 1,
                (None, None) => masked.len(),
            };
            let head = rest.trim_start();
            // A module declared as a file: the item ends at its first `;`.
            if let Some(module) = head.strip_prefix("mod ")
                && semi.is_some_and(|s| end == item + s + 1)
            {
                let name: String = module
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                files.push(name);
            }
            lexed.cut(at, end);
        }
    }
    files
}

/// Every `.rs` file under `path` (or `path` itself), sorted.
fn rust_files(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// The production text of the files under `roots`, keyed by the path
/// relative to the workspace: test-only items cut, and every file a
/// `#[cfg(test)] mod <name>;` declares (with its directory) left out.
fn production_sources(
    workspace: &std::path::Path,
    roots: &[&str],
) -> std::collections::BTreeMap<String, Lexed> {
    let mut lexed = std::collections::BTreeMap::new();
    let mut test_paths: Vec<std::path::PathBuf> = Vec::new();
    for root in roots {
        for path in rust_files(&workspace.join(root)) {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let mut l = lex(&text);
            let modules = cut_test_items(&mut l);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let dir = path.parent().expect("a parent");
            let module_dir = if ["mod", "lib", "main"].contains(&stem) {
                dir.to_path_buf()
            } else {
                dir.join(stem)
            };
            for m in modules {
                test_paths.push(module_dir.join(format!("{m}.rs")));
                test_paths.push(module_dir.join(&m));
            }
            lexed.insert(path, l);
        }
    }
    lexed
        .into_iter()
        .filter(|(path, _)| !test_paths.iter().any(|t| path.starts_with(t)))
        .map(|(path, l)| {
            let rel = path
                .strip_prefix(workspace)
                .expect("under the workspace")
                .to_string_lossy()
                .into_owned();
            (rel, l)
        })
        .collect()
}

/// `NAME -> values` for every `const NAME: &str = "value";` (any
/// visibility) of the texts; one name may hold several values across the
/// workspace.
fn code_constants_of(
    texts: &std::collections::BTreeMap<String, Lexed>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for l in texts.values() {
        let masked = l.masked();
        let mut from = 0;
        while let Some(found) = masked[from..].find("const ") {
            let start = from + found;
            from = start + 6;
            if start > 0 && {
                let p = masked.as_bytes()[start - 1];
                p.is_ascii_alphanumeric() || p == b'_'
            } {
                continue;
            }
            let rest = &masked[start + 6..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let after = rest[name.len()..].trim_start();
            let Some(ty) = after.strip_prefix(':') else {
                continue;
            };
            let ty = ty.trim_start();
            if !(ty.starts_with("&str") || ty.starts_with("&'static str")) {
                continue;
            }
            let Some(eq) = rest.find('=') else {
                continue;
            };
            let value_at = start + 6 + eq + 1;
            let value_at = value_at + leading_space(&l.text[value_at..]);
            if let Some((_, _, content)) = l.strings.iter().find(|(s, _, _)| *s == value_at) {
                out.entry(name).or_default().push(content.clone());
            }
        }
    }
    for values in out.values_mut() {
        values.sort();
        values.dedup();
    }
    out
}

/// The length of the whitespace `text` starts with, read in the source
/// text (in the masked text a blanked literal would read as whitespace).
fn leading_space(text: &str) -> usize {
    text.len() - text.trim_start().len()
}

/// What the scan of one group found: the codes, and the expression sites
/// (`file`, `function`) whose value it could not resolve.
#[derive(Default)]
struct Scan {
    codes: std::collections::BTreeSet<String>,
    sites: std::collections::BTreeSet<(String, String)>,
}

/// The machine code shapes of one source, as the design lists them: the
/// value of every `pub const NAME: &str = "<code>"`; every `"policy"`,
/// `"error"` and `"code"` member (a literal, a constant resolved by name
/// anywhere in the workspace, or an expression the scan cannot resolve,
/// recorded as a site); and the leading `<code>` of a refusal message
/// `"<code>: ..."` or `"{CONST}: ..."` given to `format!`. A path to a
/// constant the workspace does not define fails.
fn scan_source(
    file: &str,
    l: &Lexed,
    constants: &std::collections::BTreeMap<String, Vec<String>>,
    scan: &mut Scan,
) {
    let masked = l.masked();
    let resolve = |name: &str, what: &str| -> Vec<String> {
        constants
            .get(name)
            .unwrap_or_else(|| {
                panic!("{file}: {what} names the constant {name}, which no crate of the workspace defines as a &str")
            })
            .iter()
            .filter(|v| is_machine_code_word(v))
            .cloned()
            .collect()
    };
    let const_path = |expr: &str| -> Option<String> {
        let last = expr.rsplit("::").next()?;
        let path_ok = expr.split("::").all(|seg| {
            !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        (path_ok
            && last.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && last
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .then(|| last.to_string())
    };
    // The constants this file defines.
    for (start, _, content) in &l.strings {
        let before = masked[..*start].trim_end();
        if before.ends_with('=')
            && let Some(line_start) = before.rfind('\n').map(|n| n + 1).or(Some(0))
            && masked[line_start..*start].trim_start().starts_with("pub")
            && masked[line_start..*start].contains("const ")
            && masked[line_start..*start].contains("str")
            && is_machine_code_word(content)
        {
            scan.codes.insert(content.clone());
        }
    }
    for (start, end, content) in &l.strings {
        // A member: `"policy": <expr>`.
        if ["policy", "error", "code"].contains(&content.as_str()) {
            let colon = end + leading_space(&l.text[*end..]);
            if masked[colon..].starts_with(':') && !masked[colon..].starts_with("::") {
                let value = colon + 1 + leading_space(&l.text[colon + 1..]);
                // The expression runs to the `,` or closing bracket at its
                // own depth.
                let mut depth = 0i32;
                let mut stop = masked.len();
                for (i, c) in masked.bytes().enumerate().skip(value) {
                    match c {
                        b'{' | b'(' | b'[' => depth += 1,
                        b'}' | b')' | b']' if depth == 0 => {
                            stop = i;
                            break;
                        }
                        b'}' | b')' | b']' => depth -= 1,
                        b',' if depth == 0 => {
                            stop = i;
                            break;
                        }
                        _ => {}
                    }
                }
                let expr = l.text[value..stop].trim();
                if let Some((_, _, literal)) = l
                    .strings
                    .iter()
                    .find(|(s, e, _)| *s == value && l.text[*s..*e].trim() == expr)
                {
                    if is_machine_code_word(literal) {
                        scan.codes.insert(literal.clone());
                    }
                } else if let Some(name) = const_path(expr) {
                    scan.codes
                        .extend(resolve(&name, &format!("the member \"{content}\"")));
                } else {
                    scan.sites.insert((file.to_string(), l.function_at(*start)));
                }
            }
        }
        // A refusal message given to `format!`: `"<code>: ..."` or
        // `"{CONST}: ..."`.
        if masked[..*start].trim_end().ends_with("format!(")
            && let Some((head, tail)) = content.split_once(':')
            && tail.starts_with(' ')
        {
            if is_machine_code_word(head) {
                scan.codes.insert(head.to_string());
            } else if let Some(name) = head
                .strip_prefix('{')
                .and_then(|h| h.strip_suffix('}'))
                .and_then(const_path)
            {
                scan.codes.extend(resolve(&name, "a refusal message"));
            }
        }
    }
}

/// `(group, file, function, codes, why)`: every member the scan cannot
/// resolve (a variable, a call), listed by hand with the codes it can
/// return, each read from its producer; an empty list names why the value
/// is no machine code. Every code listed is in `MACHINE_CODES` under its
/// group, and every entry is a site the scan finds.
const SCANNED_EXPRESSIONS: &[(&str, &str, &str, &[&str], &str)] = &[
    (
        "gate",
        "crates/apb-engine/src/gate/billing.rs",
        "refusal",
        &[
            "billing_denied",
            "billing_refused_in_run_step",
            "billing_refused_for_engine",
            "billing_refused_for_trigger",
            "billing_requires_confirmation",
        ],
        "the code launch::billing_refusal passes: BILLING_DENIED, BILLING_TRIGGER and billing_confirmation's four",
    ),
    (
        "gate",
        "crates/apb-engine/src/gate/cloud.rs",
        "check_pairs",
        &[
            "cloud_model_unknown",
            "cloud_model_no_tools",
            "cloud_model_not_chat",
            "cloud_effort_unsupported",
        ],
        "CloudIssue::code, one per variant",
    ),
    (
        "gate",
        "crates/apb-engine/src/gate/launch.rs",
        "refusal",
        &[
            "launch_requires_confirmation",
            "launch_requires_person",
            "launch_requires_rule_approval",
            "launch_refused_in_run_step",
            "launch_refused_for_engine",
        ],
        "the policy launch_confirmation refuses with: ASK, DENY, TRIGGER, STEP and ENGINE",
    ),
    (
        "gate",
        "crates/apb-engine/src/gate/mod.rs",
        "fixed_choice",
        &[
            "secret_set_unreadable",
            "secret_param_rendered",
            "secret_declaration_dropped",
        ],
        "SecretRefusal::code: the unreadable set and the two definition codes",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/catalog.rs",
        "build",
        &[],
        "a diagnostic's free text from the suggestion store, never a code",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/catalog.rs",
        "collect_scope",
        &[],
        "a playbook load error's text, never a code",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/catalog_rank.rs",
        "error_fields",
        &[
            "invalid",
            "read_only",
            "off",
            "send",
            "redact",
            "state_too_large",
            "busy",
            "budget",
            "unavailable",
            "rate_limited",
            "auth",
            "timeout",
            "cancelled",
        ],
        "the standalone decider's skip reasons and DecideError::kind",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/promote/mod.rs",
        "make_global",
        &["digest_changed", "copy_unreadable"],
        "the two checks of the global copy",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/server/playbook.rs",
        "playbook_get",
        &[],
        "the estimate's error text, never a code",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/tools/actions.rs",
        "render",
        &[
            "answer_question",
            "decide_review",
            "decide_publish",
            "submit_host_task",
            "supervisor_retry_node",
            "supervisor_continue_from",
            "supervisor_abort",
            "resume_run",
            "resume_with_budget",
            "retry_now",
            "resolve_intent",
            "wait_again",
            "read_report",
        ],
        "ActionCode::ALL through as_str (checked against it below)",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/tools/notify.rs",
        "body",
        &[
            "invalid_notify",
            "notify_unavailable",
            "not_found",
            "run_ended",
            "notify_failed",
        ],
        "NotifyError::code, one per variant",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/tools/run.rs",
        "keyed",
        &[
            "start_key_invalid",
            "trigger_key_unavailable",
            "start_key_reuse_refused",
            "start_key_busy",
            "start_key_state_unreadable",
        ],
        "the key's parse and capture, the peek without a key, and the engine's start_key_ prefixes (REUSE_REFUSED, BUSY, UNREADABLE, the fallback)",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/tools/run.rs",
        "retry_now_plan",
        &["not_found", "run_finished", "no_deferred_retry"],
        "retry_wait::plan_retry_now's refusals",
    ),
    (
        "mcp",
        "crates/apb-mcp/src/tools/trigger.rs",
        "rule_error_value",
        &[
            "trigger_rule_busy",
            "trigger_project_unregistered",
            "trigger_enabled_not_settable",
            "rules_store_full",
            "trigger_rule_not_found",
            "trigger_rule_invalid",
            "trigger_rules_unreadable",
        ],
        "one per TriggerError variant",
    ),
];

/// The site of the next actions lists every action code: the list is the
/// production enumeration, not a copy that may drift.
#[test]
fn the_action_site_lists_every_action_code() {
    let listed = SCANNED_EXPRESSIONS
        .iter()
        .find(|(_, _, f, _, _)| *f == "render")
        .map(|(_, _, _, codes, _)| codes.to_vec())
        .expect("the actions site");
    let all: Vec<&str> = apb_engine::next_action::ActionCode::ALL
        .iter()
        .map(|c| c.as_str())
        .collect();
    assert_eq!(listed, all);
}

/// 2: the completeness check. The test scans the production sources (test
/// items and test modules cut, comments stripped) of the gate, the MCP
/// server and `run_origin` for the code shapes the groups produce
/// (`scan_source`). Every code found must be in `MACHINE_CODES` under its
/// group, every member it cannot resolve must be a site of
/// `SCANNED_EXPRESSIONS` ("list the codes `<file>` `<function>` can return
/// in MACHINE_CODES"), and every code a site lists is in the table.
#[test]
fn every_scanned_code_is_in_the_table() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = workspace.canonicalize().expect("the workspace");
    let groups: &[(&str, &str)] = &[
        ("gate", "crates/apb-engine/src/gate"),
        ("mcp", "crates/apb-mcp/src"),
        ("patch", "crates/apb-engine/src/run_origin.rs"),
    ];
    let all = production_sources(
        &workspace,
        &[
            "crates/apb-core/src",
            "crates/apb-engine/src",
            "crates/apb-mcp/src",
        ],
    );
    let constants = code_constants_of(&all);
    let in_table = |group: &str, code: &str| {
        MACHINE_CODES
            .iter()
            .any(|(g, c, _)| *g == group && *c == code)
            || typed_machine_codes()
                .iter()
                .any(|(g, c)| *g == group && c == code)
            || SCAN_EXCLUDED.iter().any(|(g, c)| *g == group && *c == code)
    };
    let mut found_sites = std::collections::BTreeSet::new();
    for (group, dir) in groups {
        let mut scan = Scan::default();
        for (file, l) in all.iter().filter(|(f, _)| f.starts_with(dir)) {
            scan_source(file, l, &constants, &mut scan);
        }
        for code in &scan.codes {
            assert!(
                in_table(group, code),
                "the scan of {group} found the code {code}, which MACHINE_CODES does not list: list the codes the scan can return in MACHINE_CODES"
            );
        }
        for (file, function) in &scan.sites {
            let entry = SCANNED_EXPRESSIONS
                .iter()
                .find(|(g, f, func, _, _)| g == group && f == file && func == function)
                .unwrap_or_else(|| {
                    panic!(
                        "the scan of {group} cannot resolve a \"policy\", \"error\" or \"code\" member in {file} {function}: list the codes `{file}` `{function}` can return in MACHINE_CODES and the site in SCANNED_EXPRESSIONS"
                    )
                });
            for code in entry.3 {
                assert!(
                    in_table(group, code),
                    "the site {file} {function} returns {code}, which MACHINE_CODES does not list under {group}"
                );
            }
            assert!(
                !entry.3.is_empty() || !entry.4.is_empty(),
                "the site {file} {function} lists no code and says not why"
            );
            found_sites.insert((group.to_string(), file.clone(), function.clone()));
        }
    }
    for (group, file, function, _, _) in SCANNED_EXPRESSIONS {
        assert!(
            found_sites.contains(&(group.to_string(), file.to_string(), function.to_string())),
            "SCANNED_EXPRESSIONS lists {group} {file} {function}, which the scan no longer finds: remove the entry"
        );
    }
    // The source symbols of the table resolve against the sources.
    let mut text = String::new();
    for l in all.values() {
        text.push_str(&l.text);
    }
    for (_, code, symbol) in MACHINE_CODES {
        let word = symbol.rsplit("::").next().unwrap_or(symbol);
        assert!(
            text.contains(word),
            "the source symbol {symbol} of {code} names nothing the scanned groups hold"
        );
    }
}

/// The scan's own proofs over sources built in memory: a member it cannot
/// resolve is a site, a constant is resolved, a test item is cut, and a
/// string, a comment or a char never reads as code.
#[test]
fn the_code_scan_reads_only_code() {
    let src = r#"
const KNOWN: &str = "known_constant_code";
// "error": "comment_only_code",
fn producer(code: &str) -> Value {
    let brace = '{';
    json!({ "policy": code, "error": KNOWN, "code": "literal_code_word", "note": "x" })
}
fn refusal() -> String { format!("{KNOWN}: a sentence") }
fn plain() -> String { format!("format_code_word: a sentence {}", 1) }
#[cfg(test)]
fn helper() -> Value { json!({ "error": "test_only_code" }) }
#[cfg(test)]
mod tests {
    fn t() -> Value { json!({ "error": hidden }) }
}
"#;
    let mut l = lex(src);
    cut_test_items(&mut l);
    let mut map = std::collections::BTreeMap::new();
    map.insert("a.rs".to_string(), l);
    let constants = code_constants_of(&map);
    let mut scan = Scan::default();
    scan_source("a.rs", &map["a.rs"], &constants, &mut scan);
    let codes: Vec<&str> = scan.codes.iter().map(String::as_str).collect();
    assert_eq!(
        codes,
        vec![
            "format_code_word",
            "known_constant_code",
            "literal_code_word"
        ]
    );
    assert_eq!(
        scan.sites.into_iter().collect::<Vec<_>>(),
        vec![("a.rs".to_string(), "producer".to_string())]
    );
}

/// 3: every entry of `MACHINE_CODES` and every typed value is a fixture
/// line.
#[test]
fn every_table_code_is_frozen() {
    let frozen: std::collections::BTreeSet<String> = fixture("machine-codes-1.0.txt")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    let mut missing = Vec::new();
    for (group, code, _) in MACHINE_CODES {
        if !frozen.contains(&format!("{group} {code}")) {
            missing.push(format!("{group} {code}"));
        }
    }
    for (group, code) in typed_machine_codes() {
        if !frozen.contains(&format!("{group} {code}")) {
            missing.push(format!("{group} {code}"));
        }
    }
    assert!(
        missing.is_empty(),
        "the machine codes {missing:?} are produced but not frozen: run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze"
    );
}

/// 4: each source symbol names a function or constant of the production
/// sources (test items and test modules cut): its last path segment is an
/// identifier defined there as `fn <name>` or `const <name>`.
#[test]
fn every_table_source_exists() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace");
    let all = production_sources(
        &workspace,
        &[
            "crates/apb-core/src",
            "crates/apb-engine/src",
            "crates/apb-mcp/src",
        ],
    );
    let mut code = String::new();
    for l in all.values() {
        code.push_str(l.masked());
        code.push('\n');
    }
    for (_, value, symbol) in MACHINE_CODES {
        let word = symbol.rsplit("::").next().unwrap_or(symbol);
        assert!(
            !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "the source symbol {symbol} of {value} is no path to a function or constant"
        );
        let defined = [
            format!("fn {word}("),
            format!("fn {word}<"),
            format!("const {word}:"),
        ]
        .iter()
        .any(|d| code.contains(d.as_str()));
        assert!(
            defined,
            "the source symbol {symbol} of {value} names no function or constant of the production sources"
        );
    }
}

// ---------------------------------------------------------------------------
// The result keys (design D1.5 item 5)
// ---------------------------------------------------------------------------

fn collect_keys(value: &Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                out.push(path.clone());
                collect_keys(v, &path, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_keys(v, &format!("{prefix}[]"), out);
            }
        }
        _ => {}
    }
}

/// The contract of one producer's result: the MCP envelope (`is_error`),
/// the sorted dotted keys of a JSON payload, the head of an error text
/// (its category and machine code, `conflict: decide_again`,
/// `refused (agent_channel)`), and the machine code a JSON refusal
/// carries (its top-level `error`, `policy` or `refused`, or
/// `policy_refusal.policy`).
/// A JSON payload with no key and an error text with no readable head fail
/// the extraction: a documented variant has a contract a host can read.
#[derive(Debug, PartialEq)]
struct ResultContract {
    is_error: bool,
    keys: Vec<String>,
    error: Option<String>,
    code: Option<String>,
}

/// The categories an error text of the tool layer leads with
/// (`ToolError` and the `EngineError` it wraps).
const ERROR_CATEGORIES: &[&str] = &[
    "not found",
    "conflict",
    "engine error",
    "invalid",
    "superseded",
    "already answered",
    "already started",
    "workdir busy",
];

/// The head of an error text: its leading segments (split at `": "`) while
/// each is a category of the tool layer, a `refused (<refusal>)` or a
/// machine code word.
fn error_head(text: &str) -> Option<String> {
    let mut head: Vec<&str> = Vec::new();
    for segment in text.split(": ") {
        let known = ERROR_CATEGORIES.contains(&segment)
            || (segment.starts_with("refused (")
                && segment.ends_with(')')
                && is_machine_code_word(&segment["refused (".len()..segment.len() - 1]))
            || is_machine_code_word(segment);
        if !known {
            break;
        }
        head.push(segment);
    }
    (!head.is_empty()).then(|| head.join(": "))
}

fn result_contract(result: &super::CallToolResult) -> ResultContract {
    let text: String = result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|t| t.text.clone())
        .collect();
    let is_error = result.is_error.unwrap_or(false);
    if let Ok(value) = serde_json::from_str::<Value>(&text)
        && (value.is_object() || value.is_array())
    {
        let mut keys = Vec::new();
        collect_keys(&value, "", &mut keys);
        keys.sort();
        keys.dedup();
        assert!(
            !keys.is_empty(),
            "the result is a JSON payload with no key, which no host can read: {text}"
        );
        let code = ["error", "policy", "refused"]
            .iter()
            .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
            .or_else(|| value["policy_refusal"]["policy"].as_str())
            .filter(|c| is_machine_code_word(c))
            .map(str::to_string);
        return ResultContract {
            is_error,
            keys,
            error: None,
            code,
        };
    }
    assert!(
        is_error,
        "the result is neither a JSON payload nor an error a freeze can read: {text}"
    );
    let line = text.trim();
    assert!(
        !line.contains('\n'),
        "the error result is not one line: {line}"
    );
    let head = error_head(line)
        .unwrap_or_else(|| panic!("the error text names no category or code: {line}"));
    ResultContract {
        is_error,
        keys: Vec::new(),
        error: Some(head),
        code: None,
    }
}

/// The contract a frozen variant entry states.
fn frozen_contract(entry: &Value) -> ResultContract {
    ResultContract {
        is_error: entry["is_error"]
            .as_bool()
            .unwrap_or_else(|| panic!("the frozen variant {entry} states no is_error")),
        keys: entry["keys"]
            .as_array()
            .expect("keys is a list")
            .iter()
            .map(|k| k.as_str().expect("a key").to_string())
            .collect(),
        error: entry
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string),
        code: entry
            .get("code")
            .and_then(|e| e.as_str())
            .map(str::to_string),
    }
}

/// Every frozen variant states a readable contract: its envelope, and
/// either a nonempty key list (a JSON payload) or an error head (an error
/// text), never neither.
fn check_frozen_contracts(results: &serde_json::Map<String, Value>) {
    for (tool, variants) in results {
        for (variant, entry) in variants.as_object().expect("an object") {
            let c = frozen_contract(entry);
            match (&c.error, c.keys.is_empty()) {
                (None, false) => {}
                (Some(head), true) => assert!(
                    c.is_error && error_head(head).as_deref() == Some(head.as_str()),
                    "{tool}/{variant}: the frozen error {head} is no error head of an error result"
                ),
                (None, true) => panic!(
                    "{tool}/{variant}: the frozen variant holds no key and no error: a claimed result variant states a contract"
                ),
                (Some(_), false) => {
                    panic!("{tool}/{variant}: the frozen variant holds both keys and an error text")
                }
            }
        }
    }
}

/// What of the frozen contract the current one no longer keeps: a frozen
/// key it lacks, a changed envelope, error head or code.
fn contract_kept(tool: &str, variant: &str, frozen: &ResultContract, now: &ResultContract) {
    for key in &frozen.keys {
        assert!(
            now.keys.contains(key),
            "the frozen result key {key} of {tool}/{variant} is not in the current result (its keys: {:?})",
            now.keys
        );
    }
    assert!(
        frozen.is_error == now.is_error,
        "the frozen envelope of {tool}/{variant} (is_error {}) changed",
        frozen.is_error
    );
    assert!(
        frozen.error.is_none() || frozen.error == now.error,
        "the frozen error {:?} of {tool}/{variant} changed to {:?}",
        frozen.error,
        now.error
    );
    assert!(
        frozen.code.is_none() || frozen.code == now.code,
        "the frozen code {:?} of {tool}/{variant} changed to {:?}",
        frozen.code,
        now.code
    );
}

/// `(tool, variant, producer)`: one producer per documented result
/// variant. A producer builds the state the variant needs and returns the
/// result; the fixture holds the keys the producer returned.
type Producer = fn() -> Result<super::CallToolResult, String>;

const RESULT_VARIANTS: &[(&str, &str, Producer)] = &[
    // run_wait (design D1.5 item 5.1): finished; needs_input with each
    // needs kind; the stopped shapes; timeout.
    ("run_wait", "finished", run_wait_finished),
    ("run_wait", "needs_question", run_wait_needs_question),
    ("run_wait", "needs_review", run_wait_needs_review),
    ("run_wait", "needs_host_task", run_wait_needs_host_task),
    ("run_wait", "needs_supervisor", run_wait_needs_supervisor),
    ("run_wait", "paused", run_wait_paused),
    ("run_wait", "parked", run_wait_parked),
    ("run_wait", "released", run_wait_released),
    ("run_wait", "driver_dead", run_wait_driver_dead),
    ("run_wait", "budget_paused", run_wait_budget_paused),
    ("run_wait", "timeout", run_wait_timeout),
    // supervisor_wait_event (5.2)
    ("supervisor_wait_event", "wake", sv_wait_wake),
    ("supervisor_wait_event", "timeout", sv_wait_timeout),
    ("supervisor_wait_event", "with_review", sv_wait_with_review),
    (
        "supervisor_wait_event",
        "with_supervisor",
        sv_wait_with_supervisor,
    ),
    ("supervisor_wait_event", "with_tasks", sv_wait_with_tasks),
    // run_status (5.3)
    ("run_status", "running", run_status_running),
    ("run_status", "at_review", run_status_at_review),
    ("run_status", "with_tasks", run_status_with_tasks),
    ("run_status", "finished", run_status_finished),
    // playbook_run (5.4)
    ("playbook_run", "started", playbook_run_started),
    ("playbook_run", "refused_trust", playbook_run_refused_trust),
    (
        "playbook_run",
        "already_started",
        playbook_run_already_started,
    ),
    (
        "playbook_run",
        "refused_start_key_conflict",
        playbook_run_refused_start_key_conflict,
    ),
    // run_task_submit, review_decide, run_answer, run_resume (5.5)
    ("run_task_submit", "accepted", run_task_submit_accepted),
    (
        "run_task_submit",
        "refused_unknown_status",
        run_task_submit_refused_unknown_status,
    ),
    (
        "run_task_submit",
        "refused_not_pending",
        run_task_submit_refused_not_pending,
    ),
    (
        "run_task_submit",
        "refused_already_submitted",
        run_task_submit_refused_already_submitted,
    ),
    (
        "run_task_submit",
        "refused_blocked_without_question",
        run_task_submit_refused_blocked_without_question,
    ),
    (
        "run_task_submit",
        "refused_ambiguous",
        run_task_submit_refused_ambiguous,
    ),
    ("review_decide", "accepted", review_decide_accepted),
    (
        "review_decide",
        "refused_person_only",
        review_decide_refused_person_only,
    ),
    (
        "review_decide",
        "refused_binding_nonce_missing",
        review_decide_refused_binding_nonce_missing,
    ),
    (
        "review_decide",
        "refused_binding_nonce_stale",
        review_decide_refused_binding_nonce_stale,
    ),
    (
        "review_decide",
        "refused_decide_again",
        review_decide_refused_decide_again,
    ),
    (
        "review_decide",
        "refused_already_decided",
        review_decide_refused_already_decided,
    ),
    (
        "review_decide",
        "refused_publish_nonce_stale",
        review_decide_refused_publish_nonce_stale,
    ),
    (
        "review_decide",
        "refused_own_run",
        review_decide_refused_own_run,
    ),
    ("run_answer", "accepted", run_answer_accepted),
    (
        "run_answer",
        "refused_exactly_one",
        run_answer_refused_exactly_one,
    ),
    (
        "run_answer",
        "refused_supervisor_needs_token",
        run_answer_refused_supervisor_needs_token,
    ),
    (
        "run_answer",
        "refused_answer_by_human",
        run_answer_refused_answer_by_human,
    ),
    (
        "run_answer",
        "refused_ambiguous",
        run_answer_refused_ambiguous,
    ),
    (
        "run_answer",
        "refused_no_question",
        run_answer_refused_no_question,
    ),
    ("run_resume", "accepted", run_resume_accepted),
    (
        "run_resume",
        "refused_no_deferred_retry",
        run_resume_refused_no_deferred_retry,
    ),
    (
        "run_resume",
        "refused_untrusted",
        run_resume_refused_untrusted,
    ),
    (
        "run_resume",
        "refused_irreversible",
        run_resume_refused_irreversible,
    ),
    (
        "run_resume",
        "refused_environment_drift",
        run_resume_refused_environment_drift,
    ),
    (
        "run_resume",
        "refused_budget_paused",
        run_resume_refused_budget_paused,
    ),
    (
        "run_resume",
        "refused_budget_confirmation",
        run_resume_refused_budget_confirmation,
    ),
    (
        "run_resume",
        "refused_budget_change",
        run_resume_refused_budget_change,
    ),
    // decision_ask (5.6)
    ("decision_ask", "answered", decision_ask_answered),
    ("decision_ask", "refused", decision_ask_refused),
    // playbook_catalog (5.7)
    ("playbook_catalog", "matches", playbook_catalog_matches),
    (
        "playbook_catalog",
        "suppressed_suggestions",
        playbook_catalog_suppressed,
    ),
    // their one documented result (5.8)
    ("playbook_capture", "captured", playbook_capture_captured),
    (
        "suggestion_dismiss",
        "dismissed",
        suggestion_dismiss_dismissed,
    ),
    ("playbook_interview", "guide", playbook_interview_guide),
    ("playbook_howto", "howto", playbook_howto_howto),
    ("profile_list", "listed", profile_list_listed),
    ("profile_howto", "howto", profile_howto_howto),
    ("projects_list", "listed", projects_list_listed),
];

/// `(tool, variants)`: every documented result variant of every tool a
/// host is told to consume (`docs/HOST-INTEGRATION.md` and the server's
/// own instructions, design D1.5 item 5). The guard test keeps the
/// producers and this list from drifting apart.
const DOCUMENTED_VARIANTS: &[(&str, &[&str])] = &[
    (
        "run_wait",
        &[
            "finished",
            "needs_question",
            "needs_review",
            "needs_host_task",
            "needs_supervisor",
            "paused",
            "parked",
            "released",
            "driver_dead",
            "budget_paused",
            "timeout",
        ],
    ),
    (
        "supervisor_wait_event",
        &[
            "wake",
            "timeout",
            "with_review",
            "with_supervisor",
            "with_tasks",
        ],
    ),
    (
        "run_status",
        &["running", "at_review", "with_tasks", "finished"],
    ),
    (
        "playbook_run",
        &[
            "started",
            "refused_trust",
            "already_started",
            "refused_start_key_conflict",
        ],
    ),
    (
        "run_task_submit",
        &[
            "accepted",
            "refused_unknown_status",
            "refused_not_pending",
            "refused_already_submitted",
            "refused_blocked_without_question",
            "refused_ambiguous",
        ],
    ),
    (
        "review_decide",
        &[
            "accepted",
            "refused_person_only",
            "refused_binding_nonce_missing",
            "refused_binding_nonce_stale",
            "refused_decide_again",
            "refused_already_decided",
            "refused_publish_nonce_stale",
            "refused_own_run",
        ],
    ),
    (
        "run_answer",
        &[
            "accepted",
            "refused_exactly_one",
            "refused_supervisor_needs_token",
            "refused_answer_by_human",
            "refused_ambiguous",
            "refused_no_question",
        ],
    ),
    (
        "run_resume",
        &[
            "accepted",
            "refused_no_deferred_retry",
            "refused_untrusted",
            "refused_irreversible",
            "refused_environment_drift",
            "refused_budget_paused",
            "refused_budget_confirmation",
            "refused_budget_change",
        ],
    ),
    ("decision_ask", &["answered", "refused"]),
    ("playbook_catalog", &["matches", "suppressed_suggestions"]),
    ("playbook_capture", &["captured"]),
    ("suggestion_dismiss", &["dismissed"]),
    ("playbook_interview", &["guide"]),
    ("playbook_howto", &["howto"]),
    ("profile_list", &["listed"]),
    ("profile_howto", &["howto"]),
    ("projects_list", &["listed"]),
];

/// Every documented variant has a producer, every producer answers to a
/// documented variant, and every documented tool is one the server serves.
#[test]
fn every_documented_variant_has_a_producer() {
    for (tool, variants) in DOCUMENTED_VARIANTS {
        assert!(
            served().iter().any(|(name, _)| name == *tool),
            "the documented host tool {tool} is not served any more"
        );
        for variant in *variants {
            assert!(
                RESULT_VARIANTS
                    .iter()
                    .any(|(t, v, _)| t == tool && v == variant),
                "the documented result variant {tool}/{variant} has no producer in RESULT_VARIANTS"
            );
        }
    }
    for (tool, variant, _) in RESULT_VARIANTS {
        assert!(
            DOCUMENTED_VARIANTS
                .iter()
                .any(|(t, vs)| *t == *tool && vs.contains(variant)),
            "the producer {tool}/{variant} names a variant no document lists; name it in DOCUMENTED_VARIANTS or drop it"
        );
    }
    assert_eq!(
        DOCUMENTED_VARIANTS.len(),
        17,
        "the host documents name 17 tools; keep DOCUMENTED_VARIANTS in step with docs/HOST-INTEGRATION.md and the instructions"
    );
}

/// Every frozen variant has a producer, and every producer names a tool
/// and variant the freeze holds.
#[test]
fn every_variant_is_produced() {
    for (tool, variant) in frozen_variants() {
        assert!(
            RESULT_VARIANTS
                .iter()
                .any(|(t, v, _)| *t == tool && *v == variant),
            "the frozen result variant {tool}/{variant} has no producer in RESULT_VARIANTS"
        );
    }
    for (tool, variant, _) in RESULT_VARIANTS {
        assert!(
            frozen_variants()
                .iter()
                .any(|(t, v)| t == tool && v == variant),
            "the producer {tool}/{variant} names a variant the freeze does not hold"
        );
    }
}

/// `(tool, variant)` of every variant the freeze holds.
fn frozen_variants() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (tool, variants) in frozen_results() {
        if let Some(map) = variants.as_object() {
            for variant in map.keys() {
                out.push((tool.clone(), variant.clone()));
            }
        }
    }
    out
}

/// Each frozen key of each variant is in the current result of that
/// variant, and its envelope, error head and code are unchanged.
#[test]
fn every_frozen_result_key_is_still_returned() {
    let all = frozen_results();
    check_frozen_contracts(&all);
    for (tool, variant) in frozen_variants() {
        let producer = RESULT_VARIANTS
            .iter()
            .find(|(t, v, _)| *t == tool && *v == variant)
            .map(|(_, _, p)| *p)
            .expect("every variant has a producer");
        let result =
            producer().unwrap_or_else(|e| panic!("the producer {tool}/{variant} failed: {e}"));
        let now = result_contract(&result);
        let frozen = frozen_contract(&all[&tool][&variant]);
        contract_kept(&tool, &variant, &frozen, &now);
    }
}

/// A new key, variant, error head or code fails with the writer's command.
#[test]
fn every_returned_key_is_frozen() {
    let all = frozen_results();
    for (tool, variant, producer) in RESULT_VARIANTS {
        let entry = all
            .get(*tool)
            .and_then(|t| t.get(*variant))
            .unwrap_or_else(|| {
                panic!(
                    "new result variant `{tool}/{variant}`: run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze"
                )
            });
        let result =
            producer().unwrap_or_else(|e| panic!("the producer {tool}/{variant} failed: {e}"));
        let now = result_contract(&result);
        let frozen = frozen_contract(entry);
        for key in &now.keys {
            assert!(
                frozen.keys.contains(key),
                "new result key `{key}` of {tool}/{variant}: run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze"
            );
        }
        assert!(
            (now.error.is_none() || frozen.error.is_some())
                && (now.code.is_none() || frozen.code.is_some()),
            "new error head or code of {tool}/{variant} ({:?}, {:?}): run APB_WRITE_FREEZE=1 cargo test -p agentic-playbooks-mcp --lib server::freeze_tests::write_freeze",
            now.error,
            now.code
        );
    }
}

/// The writer's merge of one variant: the frozen entry with the current
/// result's new keys added, and its envelope, error head and code set
/// where the frozen entry has none. A frozen key the result no longer
/// returns, or a changed envelope, head or code, panics: the writer never
/// removes or changes what is frozen.
fn merge_variant(tool: &str, variant: &str, frozen: Option<&Value>, now: &ResultContract) -> Value {
    let mut entry = frozen.cloned().unwrap_or_else(|| json!({ "keys": [] }));
    if frozen.is_some_and(|f| f.get("is_error").is_some()) {
        contract_kept(tool, variant, &frozen_contract(&entry), now);
    } else {
        // An entry an earlier writer left without its envelope: every key
        // is still checked.
        let kept: Vec<String> = entry["keys"]
            .as_array()
            .expect("keys is a list")
            .iter()
            .map(|k| k.as_str().expect("a key").to_string())
            .collect();
        for key in &kept {
            assert!(
                now.keys.contains(key),
                "the writer never removes a key: the frozen key {key} of {tool}/{variant} is not in the current result; fix the code, never the fixture"
            );
        }
    }
    let mut keys: Vec<String> = entry["keys"]
        .as_array()
        .expect("keys is a list")
        .iter()
        .map(|k| k.as_str().expect("a key").to_string())
        .collect();
    for key in &now.keys {
        if !keys.contains(key) {
            keys.push(key.clone());
        }
    }
    keys.sort();
    entry["keys"] = json!(keys);
    entry["is_error"] = json!(now.is_error);
    if let Some(head) = &now.error {
        entry["error"] = json!(head);
    }
    if let Some(code) = &now.code {
        entry["code"] = json!(code);
    }
    entry
}

/// The merge refuses to drop a frozen key or change a frozen code, and
/// adds a new key.
#[test]
fn the_result_merge_never_drops_a_frozen_key() {
    let frozen = json!({ "keys": ["a", "consent_nonce"], "is_error": false, "code": "x_refused" });
    let lost = ResultContract {
        is_error: false,
        keys: vec!["a".into()],
        error: None,
        code: Some("x_refused".into()),
    };
    assert!(std::panic::catch_unwind(|| merge_variant("t", "v", Some(&frozen), &lost)).is_err());
    let recoded = ResultContract {
        is_error: false,
        keys: vec!["a".into(), "consent_nonce".into()],
        error: None,
        code: Some("y_refused".into()),
    };
    assert!(std::panic::catch_unwind(|| merge_variant("t", "v", Some(&frozen), &recoded)).is_err());
    let grown = ResultContract {
        is_error: false,
        keys: vec!["a".into(), "b".into(), "consent_nonce".into()],
        error: None,
        code: Some("x_refused".into()),
    };
    assert_eq!(
        merge_variant("t", "v", Some(&frozen), &grown),
        json!({ "keys": ["a", "b", "consent_nonce"], "is_error": false, "code": "x_refused" })
    );
}

// ---------------------------------------------------------------------------
// The fixture runs behind the producers (design D1.5 item 5)
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;

/// One process env var set or removed for the life of the guard and
/// restored on drop, whatever the producer returns. The producer holds the
/// cross-env lock for as long as the guard lives, which is what keeps every
/// other env-touching test out while the value is changed.
struct EnvVar {
    key: String,
    prior: Option<std::ffi::OsString>,
}

impl EnvVar {
    fn set(key: &str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: under the cross-env lock the producer holds.
        unsafe { std::env::set_var(key, value.as_ref()) };
        EnvVar {
            key: key.to_string(),
            prior,
        }
    }

    fn remove(key: &str) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: under the cross-env lock the producer holds.
        unsafe { std::env::remove_var(key) };
        EnvVar {
            key: key.to_string(),
            prior,
        }
    }
}

impl Drop for EnvVar {
    fn drop(&mut self) {
        // SAFETY: under the cross-env lock the producer holds.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}

/// The environment one producer runs in: the cross-env lock held, a private
/// `APB_CONFIG_DIR`, and every ambient apb variable the machine may carry
/// removed, so each producer sees the same empty machine. The installation
/// key is ensured, so the runs a producer starts are stamped as created
/// here and the keyed starts work.
///
/// The host is controlled too, so the fixture comes out the same on a bare
/// CI runner and on a machine with agent CLIs and logins: `HOME` is a
/// scratch directory holding only a claude subscription login, `PATH` holds only
/// the stub agent programs of [`stub_agent_body`] and links to the system tools
/// of [`SYSTEM_TOOLS`] (never an agent CLI, never `bwrap`), and every
/// variable an agent's detection or login reads ([`host_dependent_var`]) is
/// removed.
///
/// The fields drop in declaration order, so the variables are restored
/// (newest first, see `Drop`) and the config dir removed before the lock
/// is released: a test that takes the lock next never sees this
/// producer's restore remove the `APB_CONFIG_DIR` it has just set.
struct FreezeEnv {
    vars: Vec<EnvVar>,
    config: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for FreezeEnv {
    fn drop(&mut self) {
        // Newest first: a variable set twice (removed at the start, set
        // again by a producer) comes back to the value it had before the
        // producer ran.
        while let Some(var) = self.vars.pop() {
            drop(var);
        }
    }
}

impl FreezeEnv {
    fn new() -> Self {
        let lock = super::tests::CROSS_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let config = tempfile::tempdir().expect("a temp config dir");
        let home = config.path().join("home");
        let path = controlled_path(config.path());
        // A claude subscription login as detection and the billing basis
        // read it (the state file, the plan of the credentials file, no
        // token), so the stub claude is a logged-in agent on every machine
        // and the runs the producers start are covered by the default
        // billing policy instead of held on an unknown basis.
        fs::create_dir_all(home.join(".claude")).expect("mkdir the scratch home");
        fs::write(home.join(".claude.json"), "{}").expect("write .claude.json");
        fs::write(
            home.join(".claude/.credentials.json"),
            r#"{"claudeAiOauth":{"subscriptionType":"max"}}"#,
        )
        .expect("write .credentials.json");
        let mut vars: Vec<EnvVar> = std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .filter(|k| host_dependent_var(k))
            .map(|k| EnvVar::remove(&k))
            .collect();
        vars.extend([
            EnvVar::set("HOME", &home),
            EnvVar::set("PATH", &path),
            EnvVar::set("APB_CONFIG_DIR", config.path()),
            EnvVar::set("APB_DECISIONS_ALLOW_FAKE", "1"),
            EnvVar::remove("APB_DECISIONS"),
            EnvVar::remove("APB_AGENT_CMD"),
            EnvVar::remove("APB_EXECUTION"),
            EnvVar::remove("APB_RUN_ID"),
            EnvVar::remove("APB_HOST_TASK"),
            EnvVar::remove("APB_NODE_ID"),
            EnvVar::remove("APB_RELEASE_AFTER_MS"),
            EnvVar::remove("APB_WAKER_POLL_MS"),
            EnvVar::remove("APB_TEST_BILLING_DEFAULT"),
        ]);
        apb_core::run_origin::ensure_key().expect("the installation key");
        FreezeEnv {
            vars,
            config,
            _lock: lock,
        }
    }

    /// Sets one more variable for the producer's lifetime (a drive env such
    /// as `APB_AGENT_CMD` or a tuning knob like `APB_RELEASE_AFTER_MS`).
    fn set(&mut self, key: &'static str, value: &str) {
        self.vars.push(EnvVar::set(key, value));
    }
}

/// The stub agent program of one built-in agent, as every producer's
/// `PATH` holds it: the version line, and for an agent with a models command
/// (`HostSetup::models_command`) two model lines, so `agents[].models` is in
/// the result on every machine. Every built-in agent gets one, from the
/// agent registry, so the probes see a machine with all of them installed.
fn stub_agent_body(agent: &dyn apb_core::agents::Agent) -> String {
    match agent.setup().models_command() {
        Some((args, _)) => format!(
            "if [ \"$*\" = '{}' ]; then\n  printf '%s\\n' vendor/model-a vendor/model-b\nelse\n  echo '1.0.0'\nfi",
            args.join(" ")
        ),
        None => "echo '1.0.0'".to_string(),
    }
}

/// The system programs a producer may start (the stub agents' shell, git,
/// the engine's helpers), linked into the controlled `PATH` from where the
/// machine has them. None is an agent CLI and `bwrap` is left out, so the
/// sandbox reads as unavailable on every machine.
const SYSTEM_TOOLS: &[&str] = &[
    "sh", "bash", "cat", "printf", "echo", "env", "git", "sleep", "true", "false", "ps", "mkfifo",
    "hostname", "tar", "lsof", "kill", "mkdir", "rm", "ls", "grep", "sed", "head", "tail", "wc",
    "date", "tr", "cut", "sort", "touch", "cp", "mv", "chmod", "dirname", "basename", "id",
    "uname", "setsid",
];

/// Whether variable `key` is one a machine's agent detection, an agent's
/// login or the config dir resolution reads: the producers run with each
/// removed.
fn host_dependent_var(key: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "ANTHROPIC_",
        "CLAUDE_",
        "CODEX_",
        "OPENAI_",
        "OPENCODE_",
        "DSH_",
        "ZCODE_",
        "AWS_",
        "AZURE_",
        "GOOGLE_",
        "GCLOUD_",
        "CLOUDSDK_",
        "CLOUD_ML_",
        "GEMINI_",
        "XDG_CONFIG_",
        "XDG_DATA_",
        "XDG_STATE_",
        "XDG_CACHE_",
    ];
    const SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_AUTH_TOKEN"];
    PREFIXES.iter().any(|p| key.starts_with(p)) || SUFFIXES.iter().any(|s| key.ends_with(s))
}

/// Builds the controlled `PATH` under `root`: `stub-bin` with a stub of
/// every built-in agent, then `sys-bin` with links to the [`SYSTEM_TOOLS`] found on the
/// machine's own `PATH` (a tool the machine lacks is left out).
fn controlled_path(root: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    let stubs = root.join("stub-bin");
    let sys = root.join("sys-bin");
    fs::create_dir_all(&stubs).expect("mkdir stub-bin");
    fs::create_dir_all(&sys).expect("mkdir sys-bin");
    for agent in apb_core::agents::all() {
        let body = stub_agent_body(*agent);
        for name in agent.descriptor().bins {
            let path = stubs.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write a stub agent");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("chmod a stub agent");
        }
    }
    let machine = std::env::var_os("PATH").unwrap_or_default();
    for tool in SYSTEM_TOOLS {
        let found = std::env::split_paths(&machine)
            .map(|d| d.join(tool))
            .find(|p| p.is_file());
        if let Some(found) = found {
            std::os::unix::fs::symlink(&found, sys.join(tool)).expect("link a system tool");
        }
    }
    std::env::join_paths([stubs, sys]).expect("a PATH")
}

/// Runs one async handler call to completion on its own current-thread
/// runtime (a producer never runs inside one).
fn block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime")
        .block_on(future)
}

/// The text of a `CallToolResult`'s content blocks.
fn result_text(result: &super::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|t| t.text.clone())
        .collect()
}

/// The JSON payload of a `CallToolResult`, or null when the text does not
/// parse (an error result's text is the error line).
fn result_json(result: &super::CallToolResult) -> Value {
    serde_json::from_str(&result_text(result)).unwrap_or(Value::Null)
}

const PB_NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

/// The same pipeline with irreversible effects and no approval: the trust
/// refusal carries its sources and consent nonce.
const PB_IRREVERSIBLE: &str = r#"
schema: 1
id: irrevpb
name: Irreversible
version: 1.0.0
effects: [irreversible]
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

const PB_REVIEW: &str = r#"
schema: 1
id: revpb
name: Review
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected] }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

/// A gate only a person may decide: the MCP channel is refused.
const PB_PERSON_ONLY: &str = r#"
schema: 1
id: popb
name: Person Only
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: gate, type: human_review, options: [approved, rejected], person_only: true }
  - { id: ok, type: finish, outcome: success }
  - { id: no, type: finish, outcome: failure }
edges:
  - { from: start, to: gate }
  - { from: gate, to: ok, condition: { type: review_status, equals: approved } }
  - { from: gate, to: no, condition: { type: review_status, equals: rejected } }
"#;

/// A script node that sleeps for a second: a window in which the run is
/// provably still running (the technique of `server/tests`' SLOWSCRIPT).
const PB_SLOW: &str = r#"
schema: 1
id: slowpb
name: Slow
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: work, type: script, script: "scripts/s.sh", runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// A long timer wait: with the release threshold below it, the drive parks.
const PB_WAIT: &str = r#"
schema: 1
id: waitpb
name: Wait
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: pause, type: wait, wait_for: { type: timer, seconds: 3 }, timeout_seconds: 3600 }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: pause }
  - { from: pause, to: done }
"#;

/// One agent step in host execution mode: the step comes back as a host
/// task instead of an agent CLI run.
const PB_HOST: &str = r#"
schema: 2
id: hostpb
name: Host
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "Do the work" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// An interactive node whose agent asks a question: the drive parks on it.
const PB_QUESTION: &str = r#"
schema: 2
id: qpb
name: Question
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: gate, type: agent_task, prompt: "QUESTION_OUT", interactive: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: done }
"#;

/// A flaky agent step: one attempt, a retry due in 3 s. With the release
/// threshold at 0 the drive releases the run once the attempt fails.
const PB_RELEASE: &str = r#"
schema: 2
id: relpb
name: Release
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "FLAKY", max_retries: 1, retry_delay: { initial: 3s } }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// Three agent steps: the budget estimate's history and its gated node.
const PB_BUDGET: &str = "schema: 2\nid: budpb\nname: Budget\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: b, type: agent_task, prompt: b }\n  - { id: c, type: agent_task, prompt: c }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: c }\n  - { from: c, to: done }\n";

/// One agent step that just succeeds: the finished status' attempt models.
const PB_AGENT_OK: &str = r#"
schema: 2
id: okpb
name: Agent Ok
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "go" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

/// The run snapshot the synthetic journals name: a plain prompt step.
const PB_SYNTHETIC: &str = "schema: 2\nid: svpb\nname: Synthetic\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: prompt, prompt: \"hi\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: done }\n";

const PB_RESUME_SNAPSHOT: &str = "schema: 2\nid: p\nname: P\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n";

/// Seeds the project at `root` with one playbook version.
fn seed_playbook(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).expect("mkdir version dir");
    fs::write(vdir.join("playbook.yaml"), yaml).expect("write playbook.yaml");
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .expect("write current pointer");
}

/// Seeds the project at `root` and one playbook version in it.
fn seed_project(root: &Path, id: &str, yaml: &str) {
    apb_core::registry::init_project(root).expect("init_project");
    seed_playbook(root, id, yaml);
}

/// Seeds the project with one playbook version and the script files the
/// version's script nodes name.
fn seed_scripts(root: &Path, id: &str, yaml: &str, scripts: &[(&str, &str)]) {
    apb_core::registry::init_project(root).expect("init_project");
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0/scripts");
    fs::create_dir_all(&vdir).expect("mkdir scripts dir");
    for (name, body) in scripts {
        fs::write(vdir.join(name), body).expect("write script");
    }
    let vdir = vdir.parent().expect("the version dir").to_path_buf();
    fs::write(vdir.join("playbook.yaml"), yaml).expect("write playbook.yaml");
    fs::write(
        root.join(".apb/playbooks").join(id).join("current"),
        "1.0.0",
    )
    .expect("write current pointer");
}

/// Seeds profile `main` (claude-code + haiku, the engine test suite's
/// stand-in executor) under the project root.
fn seed_main_profile(root: &Path) {
    let dir = root.join(".apb/profiles/main");
    fs::create_dir_all(&dir).expect("mkdir profile dir");
    fs::write(
        dir.join("profile.yaml"),
        "name: main\ndescription: test\nexecutor:\n  agent: claude-code\n  model: haiku\n",
    )
    .expect("write profile.yaml");
    fs::write(dir.join("SOUL.md"), "").expect("write SOUL.md");
}

/// Writes an executable stub agent script and returns its path (the value
/// of `APB_AGENT_CMD`).
fn stub_agent(root: &Path, body: &str) -> String {
    let path = root.join("agent.sh");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write agent.sh");
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(&path).expect("stat agent.sh").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod agent.sh");
    path.to_string_lossy().into_owned()
}

/// The stub agent that never succeeds: a failing first attempt so the
/// retry policy (and the release) takes over.
const FAILING_AGENT_BODY: &str = "case \"$*\" in\n  *FLAKY*) echo 'the agent made a mistake' 1>&2; exit 1;;\n  *) echo ok;;\nesac";

/// The stub agent that asks a question out of an interactive node and
/// parks the drive: the marker line, then the question as JSON.
const QUESTION_AGENT_BODY: &str =
    "printf '%s\\n' '<<<apb:question>>>' '{\"question\":\"Which DB?\"}'\nexit 0";

/// The stub agent of the budget fixture: it reports a claude result with
/// usage (200 billable tokens, a cost) through its status file and output.
fn budget_agent(root: &Path) -> String {
    let claude = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "done\n\n```yaml\nstatus: success\nsummary: done\n```",
        "total_cost_usd": 0.01,
        "usage": {"input_tokens": 150, "output_tokens": 50},
        "modelUsage": {"m": {"inputTokens": 150, "outputTokens": 50, "cacheReadInputTokens": 9000, "cacheCreationInputTokens": 0}}
    });
    let out = root.join("out.json");
    fs::write(&out, claude.to_string()).expect("write out.json");
    let d = root.display();
    stub_agent(
        root,
        &format!(
            "[ -n \"$APB_STATUS_FILE\" ] && printf '{{\"status\":\"success\"}}' > \"$APB_STATUS_FILE\"\ncat '{d}/out.json'"
        ),
    )
}

/// Blocks until `check` holds, failing after 30 seconds.
fn wait_for(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The journal of the run `run_id` of the project at `root`.
fn events_of(root: &Path, run_id: &str) -> Vec<apb_engine::event::Event> {
    apb_engine::event::read_all(&root.join(".apb/runs").join(run_id)).expect("read events")
}

/// Starts playbook `id` of the project at `root` in a background thread
/// (the drive lives in this process) and returns once the run's journal
/// exists, with the run id.
fn start_bg(root: &Path, id: &str) -> String {
    let r = root.to_path_buf();
    let id = id.to_string();
    let what = format!("no run dir for {id}");
    std::thread::spawn(move || {
        let _ = apb_engine::run(&r, &id, None, apb_engine::RunOptions::default());
    });
    let started = Instant::now();
    loop {
        let found = fs::read_dir(root.join(".apb/runs")).ok().and_then(|rd| {
            rd.filter_map(Result::ok)
                .find(|e| e.path().join("events.jsonl").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
        });
        if let Some(run_id) = found {
            return run_id;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Writes a synthetic run directory the way `server/tests`' journal tests
/// do: the journal `lines` (seq and ts filled in), a driver pid of this
/// process when `driver` is true, and the run's playbook snapshot, so the
/// run view computes its progress from it.
fn synthetic_run(root: &Path, run_id: &str, lines: &[Value], driver: bool) {
    let dir = root.join(".apb/runs").join(run_id);
    fs::create_dir_all(&dir).expect("mkdir run dir");
    let now = apb_core::clock::now_ms_u64();
    let mut text = String::new();
    for (seq, line) in lines.iter().enumerate() {
        let mut v = line.clone();
        v["seq"] = json!(seq as u64);
        v["ts"] = json!(now);
        text.push_str(&format!("{v}\n"));
    }
    fs::write(dir.join("events.jsonl"), text).expect("write events.jsonl");
    if driver {
        fs::write(
            apb_engine::driver::driver_pid_path(&dir),
            std::process::id().to_string(),
        )
        .expect("write driver.pid");
    }
    fs::write(dir.join("playbook.yaml"), PB_SYNTHETIC).expect("write run playbook.yaml");
}

/// A project with the plain pipeline, run to its end: the finished shapes.
fn finished_noagent(root: &Path) -> String {
    seed_project(root, "noagent", PB_NOAGENT);
    apb_engine::run(root, "noagent", None, apb_engine::RunOptions::default())
        .expect("the run finished")
        .run_id
}

/// A project whose review run sits at its gate: the pending_review shapes.
fn review_parked_run(root: &Path) -> String {
    seed_project(root, "revpb", PB_REVIEW);
    let run_id = start_bg(root, "revpb");
    wait_for("the review request", || {
        events_of(root, &run_id).iter().any(|e| {
            matches!(
                &e.payload,
                apb_engine::event::EventPayload::ReviewRequested { node, .. } if node == "gate"
            )
        })
    });
    run_id
}

/// Seeds the project whose interactive node waits for an answer: the
/// question shapes. Needs the stub agent on `APB_AGENT_CMD` and profile
/// main.
fn seed_question_project(root: &Path) {
    seed_project(root, "qpb", PB_QUESTION);
    seed_main_profile(root);
}

/// A host-mode run with one pending task: the pending_tasks shapes.
/// Returns the run id and the task id.
fn host_task_run(root: &Path) -> (String, String) {
    seed_project(root, "hostpb", PB_HOST);
    seed_main_profile(root);
    let run_id = apb_engine::scheduler::run_background(
        root,
        "hostpb",
        None,
        apb_engine::RunOptions {
            execution: apb_core::execution::ExecutionRequest {
                mode: Some(apb_core::execution::ExecutionMode::Host),
                host_session: true,
                client: Some("test-host".into()),
                inherited: false,
            },
            allow_shared_workdir: true,
            ..apb_engine::RunOptions::default()
        },
    )
    .expect("the host run started");
    wait_for("the host task", || {
        apb_engine::host_task::pending_for_run(root, &run_id)
            .map(|t| !t.is_empty())
            .unwrap_or(false)
    });
    let task_id = apb_engine::host_task::pending_for_run(root, &run_id).expect("the pending tasks")
        [0]
    .task_id
    .clone();
    (run_id, task_id)
}

/// A project whose slow run is parked at its timer wait: the park shapes.
fn parked_run(root: &Path, env: &mut FreezeEnv) -> String {
    seed_scripts(root, "waitpb", PB_WAIT, &[]);
    env.set("APB_RELEASE_AFTER_MS", "300");
    env.set("APB_WAKER_POLL_MS", "50");
    let run_id = start_bg(root, "waitpb");
    wait_for("the park", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::RunPaused { reason, .. }
                        if reason == "wait (pause)"
                )
            })
        }
    });
    run_id
}

/// A project whose flaky run is released for its deferred retry: the
/// released shape. Needs the failing agent on `APB_AGENT_CMD`.
fn released_run(root: &Path, env: &mut FreezeEnv) -> String {
    seed_project(root, "relpb", PB_RELEASE);
    seed_main_profile(root);
    let agent = stub_agent(root, FAILING_AGENT_BODY);
    env.set("APB_AGENT_CMD", &agent);
    env.set("APB_RELEASE_AFTER_MS", "0");
    env.set("APB_WAKER_POLL_MS", "50");
    let res = apb_engine::run(root, "relpb", None, apb_engine::RunOptions::default())
        .expect("the drive released the run");
    assert_eq!(
        res.outcome,
        apb_engine::state::RunStatus::Running,
        "the drive released the run"
    );
    res.run_id
}

/// A project with the budget estimate in place (three history runs of the
/// stub agent) whose next run pauses before node `b`: the budget shapes.
fn budget_paused_run(root: &Path, env: &mut FreezeEnv) -> String {
    seed_project(root, "budpb", PB_BUDGET);
    seed_main_profile(root);
    let agent = budget_agent(root);
    env.set("APB_AGENT_CMD", &agent);
    for _ in 0..3 {
        let done = apb_engine::run(root, "budpb", None, apb_engine::RunOptions::default())
            .expect("a history run");
        assert_eq!(
            done.outcome,
            apb_engine::state::RunStatus::Succeeded,
            "a history run"
        );
    }
    let res = apb_engine::run(
        root,
        "budpb",
        None,
        apb_engine::RunOptions {
            budget: Some(apb_core::budget::BudgetRequest {
                tokens: Some(300),
                ..apb_core::budget::BudgetRequest::default()
            }),
            ..apb_engine::RunOptions::default()
        },
    )
    .expect("the budgeted run");
    assert_eq!(
        res.outcome,
        apb_engine::state::RunStatus::Paused,
        "the budget paused the run"
    );
    res.run_id
}

/// The `PlaybookRunArgs` of a plain start of playbook `id`.
fn run_args(id: &str) -> super::PlaybookRunArgs {
    super::PlaybookRunArgs {
        deadline: None,
        start_key: None,
        start_key_reuse: None,
        id: id.to_string(),
        version: None,
        params: BTreeMap::new(),
        instruction: None,
        supervise: None,
        background: None,
        acknowledge_untrusted: None,
        confirm_irreversible: None,
        scope: None,
        continued_from: None,
        worktree: None,
        execution: None,
        budget: None,
        remember: None,
        notify: None,
        confirm_launch: None,
        launch_version: None,
        launch_choice: None,
        unattended: None,
        external_data: None,
    }
}

/// The `RunResumeArgs` of a plain resume of run `run_id`.
fn resume_args(run_id: &str) -> super::RunResumeArgs {
    super::RunResumeArgs {
        deadline: None,
        run_id: run_id.to_string(),
        from_node: None,
        retry_now: false,
        node: None,
        allow_environment_drift: false,
        acknowledge_untrusted: Some(true),
        confirm_irreversible: None,
        workspace: None,
        budget: None,
        confirm_budget: None,
        confirm_launch: None,
        launch_version: None,
        launch_choice: None,
        external_data: None,
    }
}

// --- run_wait (design D1.5 item 5.1) ---

fn run_wait_finished() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = finished_noagent(root);
    let res = apb_engine::run_wait::wait_run(root, &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.reason != apb_engine::run_wait::WaitReason::Finished {
        return Err(format!("expected a finished wait, got {:?}", res.reason));
    }
    let out = crate::tools::run_wait_result(root, &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_needs_question() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_question_project(root);
    let agent = stub_agent(root, QUESTION_AGENT_BODY);
    env.set("APB_AGENT_CMD", &agent);
    let run_id = start_bg(root, "qpb");
    wait_for("the question", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::QuestionAsked { node, .. } if node == "gate"
                )
            })
        }
    });
    let res = apb_engine::run_wait::wait_run(root, &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.needs != Some(apb_engine::run_wait::NeedsInput::Question) {
        return Err(format!("expected a question wait, got {:?}", res.needs));
    }
    let out = crate::tools::run_wait_result(root, &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_needs_review() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = review_parked_run(proj.path());
    let res = apb_engine::run_wait::wait_run(proj.path(), &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.needs != Some(apb_engine::run_wait::NeedsInput::Review) {
        return Err(format!("expected a review wait, got {:?}", res.needs));
    }
    let out =
        crate::tools::run_wait_result(proj.path(), &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_needs_host_task() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, _) = host_task_run(proj.path());
    let res = apb_engine::run_wait::wait_run(proj.path(), &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.needs != Some(apb_engine::run_wait::NeedsInput::HostTask) {
        return Err(format!("expected a host_task wait, got {:?}", res.needs));
    }
    let out =
        crate::tools::run_wait_result(proj.path(), &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_needs_supervisor() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "svpb", PB_SYNTHETIC);
    synthetic_run(
        root,
        "sv-sup",
        &[
            json!({"type": "run_started", "playbook": "svpb", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "w", "attempt": 1}),
            json!({"type": "wake_raised", "trigger": "node_failed", "node": "w", "detail": "exit 1"}),
        ],
        true,
    );
    let res = apb_engine::run_wait::wait_run(root, "sv-sup", Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.needs != Some(apb_engine::run_wait::NeedsInput::Supervisor) {
        return Err(format!("expected a supervisor wait, got {:?}", res.needs));
    }
    let out = crate::tools::run_wait_result(root, "sv-sup", &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_paused() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_scripts(root, "slowpb", PB_SLOW, &[("s.sh", "sleep 1\necho ok")]);
    let run_id = start_bg(root, "slowpb");
    wait_for("the node to start", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::NodeStarted { node, .. } if node == "work"
                )
            })
        }
    });
    apb_engine::pause::request_pause(
        root,
        &run_id,
        &apb_engine::pause::PauseRequest {
            by: Some(apb_engine::pause::kind::PauseBy::Operator),
            mode: apb_engine::pause::kind::PauseMode::Drain,
            ..apb_engine::pause::PauseRequest::default()
        },
    )
    .map_err(|e| e.to_string())?;
    wait_for("the pause", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::RunPaused { .. }
                )
            })
        }
    });
    let res = apb_engine::run_wait::wait_run(root, &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.pause().is_none() {
        return Err("expected a paused run with its pause details".to_string());
    }
    let out = crate::tools::run_wait_result(root, &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_parked() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = parked_run(proj.path(), &mut env);
    let res = apb_engine::run_wait::wait_run(proj.path(), &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let cause = res.pause().and_then(|p| p.cause);
    if cause != Some(apb_engine::pause::PauseCause::Wait) {
        return Err(format!("expected a park at a wait, got pause {cause:?}"));
    }
    let out =
        crate::tools::run_wait_result(proj.path(), &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_released() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = released_run(proj.path(), &mut env);
    let res = apb_engine::run_wait::wait_run(proj.path(), &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.reason != apb_engine::run_wait::WaitReason::Stopped {
        return Err(format!("expected a stopped wait, got {:?}", res.reason));
    }
    let out =
        crate::tools::run_wait_result(proj.path(), &run_id, &res).map_err(|e| e.to_string())?;
    if out.get("released").is_none() {
        return Err(format!("expected the released member, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_driver_dead() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "svpb", PB_SYNTHETIC);
    synthetic_run(
        root,
        "sv-dead",
        &[
            json!({"type": "run_started", "playbook": "svpb", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "start", "attempt": 1}),
            json!({"type": "node_finished", "node": "start", "status": "succeeded", "attempt": 1, "output": ""}),
        ],
        false,
    );
    // The run's drive claim names a parent that is gone: `driver_alive`
    // reads false through `parent_driven_alive` ("the relationship is
    // recorded but nothing can be driving through it"), the same verdict a
    // dead driver between two nodes gets. Probing a stale `driver.pid`
    // needs `ps`, which a hardened test sandbox may not run; the parent
    // route yields the identical result without it.
    fs::write(
        apb_engine::driver::driven_by_path(&root.join(".apb/runs/sv-dead")),
        "gone-parent",
    )
    .map_err(|e| e.to_string())?;
    let res = apb_engine::run_wait::wait_run(root, "sv-dead", Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if res.driver_alive != Some(false) {
        return Err(format!(
            "expected a dead driver, got {:?}",
            res.driver_alive
        ));
    }
    let out = crate::tools::run_wait_result(root, "sv-dead", &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_budget_paused() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = budget_paused_run(proj.path(), &mut env);
    let res = apb_engine::run_wait::wait_run(proj.path(), &run_id, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let out =
        crate::tools::run_wait_result(proj.path(), &run_id, &res).map_err(|e| e.to_string())?;
    if out.get("pause").and_then(|p| p.get("cause")) != Some(&json!("budget")) {
        return Err(format!("expected a budget pause, got {out}"));
    }
    // The estimate snapshot of this run must name the three history runs
    // this producer made (the note names a shortfall, so its presence
    // means a history run went uncounted and the result is not stable).
    assert!(
        out.get("estimate").and_then(|e| e.get("note")).is_none(),
        "the estimate counts fewer than three comparable runs: {}",
        out.get("estimate")
            .map(|e| e.to_string())
            .unwrap_or_default()
    );
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_wait_timeout() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_scripts(root, "slowpb", PB_SLOW, &[("s.sh", "sleep 1\necho ok")]);
    let run_id = start_bg(root, "slowpb");
    // The node must be mid-attempt when the wait times out: on a loaded
    // host the drive may take longer than the window to start it, and a
    // `nodes` map without a running node is the `stopped` shape, not the
    // timeout one. Waiting for the attempt pins the shape.
    wait_for("the node to start", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::NodeStarted { node, .. } if node == "work"
                )
            })
        }
    });
    let res = apb_engine::run_wait::wait_run(root, &run_id, Duration::from_millis(300))
        .map_err(|e| e.to_string())?;
    if res.reason != apb_engine::run_wait::WaitReason::Timeout {
        return Err(format!("expected a timeout, got {:?}", res.reason));
    }
    let out = crate::tools::run_wait_result(root, &run_id, &res).map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

// --- supervisor_wait_event (design D1.5 item 5.2) ---

fn sv_wait_wake() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "svpb", PB_SYNTHETIC);
    synthetic_run(
        root,
        "sv-wake",
        &[
            json!({"type": "run_started", "playbook": "svpb", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "w", "attempt": 1}),
            json!({"type": "wake_raised", "trigger": "anomaly", "node": "w", "detail": "the attempt repeated one rejection fingerprint"}),
        ],
        true,
    );
    let outcome =
        apb_engine::run_wait::wait_supervisor_event(root, "sv-wake", None, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
    if !matches!(outcome, apb_engine::run_wait::SupervisorWait::Wake(_)) {
        return Err(format!("expected a wake, got {outcome:?}"));
    }
    let out = crate::tools::supervisor_wait_result(root, "sv-wake", None, &outcome)
        .map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn sv_wait_timeout() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "svpb", PB_SYNTHETIC);
    synthetic_run(
        root,
        "sv-to",
        &[
            json!({"type": "run_started", "playbook": "svpb", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "w", "attempt": 1}),
        ],
        true,
    );
    let outcome = apb_engine::run_wait::wait_supervisor_event(
        root,
        "sv-to",
        None,
        Duration::from_millis(300),
    )
    .map_err(|e| e.to_string())?;
    if !matches!(outcome, apb_engine::run_wait::SupervisorWait::TimedOut) {
        return Err(format!("expected a timeout, got {outcome:?}"));
    }
    let out = crate::tools::supervisor_wait_result(root, "sv-to", None, &outcome)
        .map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn sv_wait_with_review() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = review_parked_run(proj.path());
    let outcome = apb_engine::run_wait::wait_supervisor_event(
        proj.path(),
        &run_id,
        None,
        Duration::from_secs(5),
    )
    .map_err(|e| e.to_string())?;
    if !matches!(outcome, apb_engine::run_wait::SupervisorWait::Review { .. }) {
        return Err(format!("expected a review, got {outcome:?}"));
    }
    let out = crate::tools::supervisor_wait_result(proj.path(), &run_id, None, &outcome)
        .map_err(|e| e.to_string())?;
    if out.get("pending_review").map(Value::is_null) != Some(false) {
        return Err(format!("expected the pending review, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn sv_wait_with_supervisor() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "svpb", PB_SYNTHETIC);
    synthetic_run(
        root,
        "sv-sup",
        &[
            json!({"type": "run_started", "playbook": "svpb", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "w", "attempt": 1}),
            json!({"type": "wake_raised", "trigger": "node_failed", "node": "w", "detail": "exit 1"}),
        ],
        true,
    );
    let outcome =
        apb_engine::run_wait::wait_supervisor_event(root, "sv-sup", None, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
    if !matches!(outcome, apb_engine::run_wait::SupervisorWait::Wake(_)) {
        return Err(format!("expected a wake, got {outcome:?}"));
    }
    let out = crate::tools::supervisor_wait_result(root, "sv-sup", None, &outcome)
        .map_err(|e| e.to_string())?;
    if out.get("pending_supervisor").map(Value::is_null) != Some(false) {
        return Err(format!(
            "expected the pending supervisor decision, got {out}"
        ));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn sv_wait_with_tasks() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "svpb", PB_SYNTHETIC);
    synthetic_run(
        root,
        "sv-task",
        &[
            json!({"type": "run_started", "playbook": "svpb", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "w", "attempt": 1}),
            json!({"type": "host_task_requested", "task_id": "t1", "node": "w", "attempt": 1, "prompt_ref": "tasks/t1/prompt.md", "workdir": root.display().to_string()}),
        ],
        true,
    );
    // The task the journal asks for, as `host_task::write_task` would leave
    // it: the record and the prompt, so `pending_tasks` reads it.
    let task_dir = root.join(".apb/runs/sv-task/tasks/t1");
    fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
    fs::write(
        task_dir.join("task.json"),
        json!({
            "task_id": "t1",
            "node": "w",
            "attempt": 1,
            "workdir": root.display().to_string(),
            "env": {},
            "requested_ts": apb_core::clock::now_ms_u64(),
        })
        .to_string(),
    )
    .map_err(|e| e.to_string())?;
    fs::write(task_dir.join("prompt.md"), "Do the work").map_err(|e| e.to_string())?;
    let outcome =
        apb_engine::run_wait::wait_supervisor_event(root, "sv-task", None, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
    if !matches!(
        outcome,
        apb_engine::run_wait::SupervisorWait::HostTask { .. }
    ) {
        return Err(format!("expected a host task, got {outcome:?}"));
    }
    let out = crate::tools::supervisor_wait_result(root, "sv-task", None, &outcome)
        .map_err(|e| e.to_string())?;
    if out.get("pending_tasks").map(Value::is_null) != Some(false) {
        return Err(format!("expected the pending tasks, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

// --- run_status (design D1.5 item 5.3) ---

fn run_status_running() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_scripts(root, "slowpb", PB_SLOW, &[("s.sh", "sleep 1\necho ok")]);
    let run_id = start_bg(root, "slowpb");
    wait_for("the node to start", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::NodeStarted { node, .. } if node == "work"
                )
            })
        }
    });
    let out = crate::tools::run_status(root, &run_id).map_err(|e| e.to_string())?;
    if out["run_status"] != json!("running") {
        return Err(format!("expected a running run, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_status_at_review() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = review_parked_run(proj.path());
    let out = crate::tools::run_status(proj.path(), &run_id).map_err(|e| e.to_string())?;
    if out.get("pending_review").map(Value::is_null) != Some(false) {
        return Err(format!("expected the pending review, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_status_with_tasks() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, _) = host_task_run(proj.path());
    let out = crate::tools::run_status(proj.path(), &run_id).map_err(|e| e.to_string())?;
    if out.get("pending_tasks").map(Value::is_null) != Some(false) {
        return Err(format!("expected the pending tasks, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_status_finished() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "okpb", PB_AGENT_OK);
    seed_main_profile(root);
    let agent = stub_agent(root, "echo ok");
    env.set("APB_AGENT_CMD", &agent);
    let run_id = apb_engine::run(root, "okpb", None, apb_engine::RunOptions::default())
        .map_err(|e| e.to_string())?
        .run_id;
    let out = crate::tools::run_status(root, &run_id).map_err(|e| e.to_string())?;
    if out.get("attempt_models").map(Value::is_null) != Some(false) {
        return Err(format!("expected the attempt models, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

// --- playbook_run (design D1.5 item 5.4) ---

fn playbook_run_started() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    let server = super::WfMcp::new(root.to_path_buf());
    let mut args = run_args("noagent");
    args.background = Some(true);
    args.acknowledge_untrusted = Some(true);
    let result = block(server.playbook_run_for(Parameters(args), None));
    let out = result_json(&result);
    if out["run_id"].as_str().is_none() {
        return Err(format!("the background start did not answer a run: {out}"));
    }
    Ok(result)
}

fn playbook_run_refused_trust() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "irrevpb", PB_IRREVERSIBLE);
    let server = super::WfMcp::new(root.to_path_buf());
    let mut args = run_args("irrevpb");
    args.background = Some(true);
    let result = block(server.playbook_run_for(Parameters(args), None));
    let out = result_json(&result);
    if out["policy_refusal"]["consent_nonce"].as_str().is_none() {
        return Err(format!(
            "expected the trust refusal with its nonce, got {out}"
        ));
    }
    Ok(result)
}

fn playbook_run_already_started() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    let server = super::WfMcp::new(root.to_path_buf());
    let mut first = run_args("noagent");
    first.background = Some(true);
    first.acknowledge_untrusted = Some(true);
    first.start_key = Some("k1".to_string());
    let result = block(server.playbook_run_for(Parameters(first), None));
    if result_json(&result)["run_id"].as_str().is_none() {
        return Err(format!(
            "the keyed start did not start: {}",
            result_json(&result)
        ));
    }
    let mut again = run_args("noagent");
    again.background = Some(true);
    again.acknowledge_untrusted = Some(true);
    again.start_key = Some("k1".to_string());
    let result = block(server.playbook_run_for(Parameters(again), None));
    let out = result_json(&result);
    if out["already_started"] != json!(true) {
        return Err(format!("expected the run already started, got {out}"));
    }
    Ok(result)
}

fn playbook_run_refused_start_key_conflict() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    let server = super::WfMcp::new(root.to_path_buf());
    let mut first = run_args("noagent");
    first.background = Some(true);
    first.acknowledge_untrusted = Some(true);
    first.start_key = Some("k2".to_string());
    let result = block(server.playbook_run_for(Parameters(first), None));
    if result_json(&result)["run_id"].as_str().is_none() {
        return Err(format!(
            "the keyed start did not start: {}",
            result_json(&result)
        ));
    }
    let mut second = run_args("noagent");
    second.background = Some(true);
    second.acknowledge_untrusted = Some(true);
    second.start_key = Some("k2".to_string());
    second.params.insert("a".to_string(), "1".to_string());
    let result = block(server.playbook_run_for(Parameters(second), None));
    let out = result_json(&result);
    if out["policy_refusal"]["policy"].as_str() != Some(apb_engine::start_key::CONFLICT) {
        return Err(format!("expected the start key conflict, got {out}"));
    }
    Ok(result)
}

// --- run_task_submit, review_decide, run_answer, run_resume (design D1.5 item 5.5) ---

fn run_task_submit_accepted() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, task_id) = host_task_run(proj.path());
    let out = crate::tools::run_task_submit(
        proj.path(),
        &run_id,
        crate::tools::TaskReply {
            task_id: task_id.clone(),
            status: "succeeded".to_string(),
            output: "done\n\n```yaml\nstatus: success\nsummary: done\n```".to_string(),
            usage: None,
            note: None,
            submitted_by: "host".to_string(),
            client: None,
            model: None,
            findings: None,
            warnings: None,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["task_id"] != json!(task_id) {
        return Err(format!("the receipt names another task: {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_task_submit_refused_unknown_status() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let out = crate::tools::run_task_submit(
        proj.path(),
        "no-run",
        crate::tools::TaskReply {
            task_id: "t1".to_string(),
            status: "nope".to_string(),
            output: String::new(),
            usage: None,
            note: None,
            submitted_by: "host".to_string(),
            client: None,
            model: None,
            findings: None,
            warnings: None,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["error"] != json!("unknown_status") {
        return Err(format!("expected the unknown status refusal, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn review_decide_accepted() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = review_parked_run(proj.path());
    let request = events_of(proj.path(), &run_id).iter().rev().find_map(|e| {
        matches!(
            &e.payload,
            apb_engine::event::EventPayload::ReviewRequested { node, .. } if node == "gate"
        )
        .then_some(e.seq)
    });
    let out = crate::tools::review_decide(
        proj.path(),
        &run_id,
        "gate",
        "approved",
        "",
        None,
        request,
        &BTreeMap::new(),
    )
    .map_err(|e| e.to_string())?;
    if out.get("posted_seq").is_none() {
        return Err(format!("expected the posted decision, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn review_decide_refused_person_only() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "popb", PB_PERSON_ONLY);
    let run_id = start_bg(root, "popb");
    wait_for("the review request", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::ReviewRequested { node, .. } if node == "gate"
                )
            })
        }
    });
    match crate::tools::review_decide(
        root,
        &run_id,
        "gate",
        "approved",
        "",
        None,
        None,
        &BTreeMap::new(),
    ) {
        Err(e) => Ok(super::to_call_tool_result(Err(e))),
        Ok(v) => Err(format!("expected the person-only refusal, got {v}")),
    }
}

fn run_answer_accepted() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_question_project(root);
    let agent = stub_agent(root, QUESTION_AGENT_BODY);
    env.set("APB_AGENT_CMD", &agent);
    let run_id = start_bg(root, "qpb");
    wait_for("the question", {
        let root = root.to_path_buf();
        let run_id = run_id.clone();
        move || {
            events_of(&root, &run_id).iter().any(|e| {
                matches!(
                    &e.payload,
                    apb_engine::event::EventPayload::QuestionAsked { node, .. } if node == "gate"
                )
            })
        }
    });
    let out = crate::tools::run_answer(root, &run_id, None, "pg", "human", None)
        .map_err(|e| e.to_string())?;
    if out.get("posted_seq").is_none() {
        return Err(format!("expected the posted answer, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn run_resume_accepted() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = parked_run(proj.path(), &mut env);
    let server = super::WfMcp::new(proj.path().to_path_buf());
    let result = server.run_resume_for(resume_args(&run_id), None);
    let out = result_json(&result);
    if out.get("policy_refusal").is_some() {
        return Err(format!("the resume was refused: {out}"));
    }
    Ok(result)
}

fn run_resume_refused_no_deferred_retry() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    let dir = root.join(".apb/runs/r-defer");
    fs::create_dir_all(&dir).expect("mkdir run dir");
    synthetic_run(
        root,
        "r-defer",
        &[
            json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            json!({"type": "node_started", "node": "work", "attempt": 1}),
        ],
        true,
    );
    fs::write(dir.join("playbook.yaml"), PB_RESUME_SNAPSHOT).expect("write run playbook.yaml");
    apb_core::run_origin::stamp(&dir, "r-defer").expect("stamp the run");
    let server = super::WfMcp::new(root.to_path_buf());
    let mut args = resume_args("r-defer");
    args.retry_now = true;
    args.node = Some("nope".to_string());
    let result = server.run_resume_for(args, None);
    let out = result_json(&result);
    if out["error"] != json!("no_deferred_retry") {
        return Err(format!("expected the no deferred retry refusal, got {out}"));
    }
    Ok(result)
}

// --- the documented refusals of run_task_submit, review_decide, run_answer
// and run_resume (design D1.5 item 5.5) ---

/// The `TaskReply` of a host's submission.
fn task_reply(task_id: &str, status: &str, output: &str) -> crate::tools::TaskReply {
    crate::tools::TaskReply {
        task_id: task_id.to_string(),
        status: status.to_string(),
        output: output.to_string(),
        usage: None,
        note: None,
        submitted_by: "host".to_string(),
        client: None,
        model: None,
        findings: None,
        warnings: None,
    }
}

/// An error result of the tool layer, or the producer's failure when the
/// call went through.
fn refused(
    what: &str,
    result: Result<Value, crate::tools::ToolError>,
) -> Result<super::CallToolResult, String> {
    match result {
        Err(e) => Ok(super::to_call_tool_result(Err(e))),
        Ok(v) => Err(format!("expected the {what} refusal, got {v}")),
    }
}

/// A JSON refusal carrying `code` at the member `member` (a top-level
/// `error` or `policy`, or `policy_refusal.policy`), or the producer's
/// failure.
fn refused_with(
    result: super::CallToolResult,
    code: &str,
) -> Result<super::CallToolResult, String> {
    let out = result_json(&result);
    let found = ["error", "policy"]
        .iter()
        .find_map(|k| out.get(*k).and_then(|v| v.as_str()))
        .or_else(|| out["policy_refusal"]["policy"].as_str());
    if found != Some(code) {
        return Err(format!(
            "expected the {code} refusal, got {}",
            result_text(&result)
        ));
    }
    Ok(result)
}

/// A copy of the host run `run_id` under `copy`, with no driver: the same
/// journal and task files, so its pending task takes a submission no drive
/// consumes.
fn copy_host_run(root: &Path, run_id: &str, copy: &str) {
    let from = root.join(".apb/runs").join(run_id);
    let to = root.join(".apb/runs").join(copy);
    let mut stack = vec![(from.clone(), to.clone())];
    while let Some((src, dst)) = stack.pop() {
        fs::create_dir_all(&dst).expect("mkdir the copy");
        for entry in fs::read_dir(&src).expect("read the run dir").flatten() {
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                stack.push((path, target));
            } else if path.file_name().is_some_and(|n| n != "driver.pid") {
                fs::copy(&path, &target).expect("copy a run file");
            }
        }
    }
}

/// Appends one journal line to the run `run_id` without a driver.
fn append_line(root: &Path, run_id: &str, line: Value) {
    let dir = root.join(".apb/runs").join(run_id);
    let seq = apb_engine::event::read_all(&dir)
        .expect("read the journal")
        .len();
    let mut v = line;
    v["seq"] = json!(seq as u64);
    v["ts"] = json!(apb_core::clock::now_ms_u64());
    let mut text = fs::read_to_string(dir.join("events.jsonl")).expect("read events.jsonl");
    text.push_str(&format!("{v}\n"));
    fs::write(dir.join("events.jsonl"), text).expect("write events.jsonl");
}

fn run_task_submit_refused_not_pending() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, _) = host_task_run(proj.path());
    refused(
        "not pending",
        crate::tools::run_task_submit(
            proj.path(),
            &run_id,
            task_reply("work-9", "succeeded", "done"),
        ),
    )
}

fn run_task_submit_refused_already_submitted() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, task_id) = host_task_run(proj.path());
    copy_host_run(proj.path(), &run_id, "r-copy");
    crate::tools::run_task_submit(
        proj.path(),
        "r-copy",
        task_reply(&task_id, "succeeded", "done"),
    )
    .map_err(|e| format!("the first submission: {e}"))?;
    refused(
        "already submitted",
        crate::tools::run_task_submit(
            proj.path(),
            "r-copy",
            task_reply(&task_id, "succeeded", "done"),
        ),
    )
}

fn run_task_submit_refused_blocked_without_question() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, task_id) = host_task_run(proj.path());
    refused(
        "blocked without a question",
        crate::tools::run_task_submit(proj.path(), &run_id, task_reply(&task_id, "blocked", " ")),
    )
}

/// A parent run and its child both hold the pending task: through the
/// parent's id the submission is ambiguous (docs/HOST-INTEGRATION.md).
fn run_task_submit_refused_ambiguous() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (run_id, task_id) = host_task_run(proj.path());
    copy_host_run(proj.path(), &run_id, "r-parent");
    append_line(
        proj.path(),
        "r-parent",
        json!({ "type": "child_run_started", "node_id": "sub", "run_id": run_id }),
    );
    refused(
        "ambiguous task",
        crate::tools::run_task_submit(
            proj.path(),
            "r-parent",
            task_reply(&task_id, "succeeded", "done"),
        ),
    )
}

/// A review gate that approves the snapshot of `w` and loops back once on
/// reject: the binding nonce and the replaced request shapes.
const PB_BOUND: &str = r#"
schema: 2
id: boundpb
name: Bound
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: w, type: script, script: scripts/w.sh, runner: sh, results: [ { path: report.md } ] }
  - { id: g, type: human_review, options: [approve, reject], approves: { results: [w] } }
  - { id: x, type: script, script: scripts/x.sh, runner: sh }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: w }
  - { from: w, to: g }
  - { from: g, to: x, condition: { type: review_status, equals: reject }, max_traversals: 2 }
  - { from: x, to: g }
  - { from: g, to: done, condition: { type: review_status, equals: approve } }
"#;

/// The `n`th review request of gate `g` (1-based): its seq and binding
/// nonce, once journaled.
fn bound_request(root: &Path, run_id: &str, n: usize) -> (u64, String) {
    let nth = || {
        events_of(root, run_id)
            .iter()
            .filter_map(|e| match &e.payload {
                apb_engine::event::EventPayload::ReviewRequested { node, binding, .. }
                    if node == "g" =>
                {
                    binding.as_ref().map(|b| (e.seq, b.nonce.clone()))
                }
                _ => None,
            })
            .nth(n - 1)
    };
    wait_for("the bound review request", || nth().is_some());
    nth().expect("the request")
}

/// A run of `boundpb` waiting at its first bound request.
fn bound_run(root: &Path) -> String {
    seed_scripts(
        root,
        "boundpb",
        PB_BOUND,
        &[
            ("w.sh", "printf 'draft\\n' > report.md\necho ok\n"),
            ("x.sh", "echo ok\n"),
        ],
    );
    start_bg(root, "boundpb")
}

fn decide(
    root: &Path,
    run_id: &str,
    node: &str,
    decision: &str,
    nonce: Option<&str>,
    request: Option<u64>,
) -> Result<Value, crate::tools::ToolError> {
    crate::tools::review_decide(
        root,
        run_id,
        node,
        decision,
        "",
        nonce,
        request,
        &BTreeMap::new(),
    )
}

fn review_decide_refused_binding_nonce_missing() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = bound_run(root);
    let (request, _) = bound_request(root, &run_id, 1);
    refused(
        "missing binding nonce",
        decide(root, &run_id, "g", "approve", None, Some(request)),
    )
}

fn review_decide_refused_binding_nonce_stale() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = bound_run(root);
    let (request, _) = bound_request(root, &run_id, 1);
    refused(
        "stale binding nonce",
        decide(
            root,
            &run_id,
            "g",
            "approve",
            Some("approval-0000"),
            Some(request),
        ),
    )
}

/// A decision naming a request that was already decided.
fn review_decide_refused_already_decided() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = bound_run(root);
    let (first, _) = bound_request(root, &run_id, 1);
    decide(root, &run_id, "g", "reject", None, Some(first))
        .map_err(|e| format!("the reject: {e}"))?;
    let (_, nonce) = bound_request(root, &run_id, 2);
    refused(
        "already decided",
        decide(root, &run_id, "g", "approve", Some(&nonce), Some(first)),
    )
}

/// A decision naming the request a later one replaced before anyone
/// decided it (a scope re-ask, a resumed drive asking again), journaled by
/// hand: `decide_again`.
fn review_decide_refused_decide_again() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    let ask = json!({ "type": "review_requested", "node": "g", "options": ["approve", "reject"] });
    synthetic_run(
        root,
        "r-again",
        &[
            json!({ "type": "run_started", "playbook": "boundpb", "version": "1.0.0" }),
            ask.clone(),
            ask,
        ],
        false,
    );
    let dir = root.join(".apb/runs/r-again");
    fs::write(dir.join("playbook.yaml"), PB_BOUND).expect("write run playbook.yaml");
    apb_core::run_origin::stamp(&dir, "r-again").expect("stamp the run");
    refused(
        "decide again",
        decide(root, "r-again", "g", "reject", None, Some(1)),
    )
}

/// A run with a publish node whose plan is pending, journaled by hand: the
/// consent names a nonce that is not the current plan's.
const PB_PUBLISH: &str = "schema: 2\nid: pubpb\nname: Publish\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: work, type: script, script: scripts/work.sh, runner: sh }\n  - { id: publish, type: publish, from: work, remote: origin, branch: \"apb/out\" }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: publish }\n  - { from: publish, to: done }\n";

fn review_decide_refused_publish_nonce_stale() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    synthetic_run(
        root,
        "r-pub",
        &[
            json!({ "type": "run_started", "playbook": "pubpb", "version": "1.0.0" }),
            json!({ "type": "node_started", "node": "publish", "attempt": 1 }),
            json!({
                "type": "publish_planned", "node": "publish", "from": "work", "from_seq": 0,
                "branch": "apb/out", "local_branch": "apb/out", "after": "0123456789abcdef",
                "url": "file:///remote.git", "commits": [], "plan_digest": "plan-digest",
                "nonce": "publish-current"
            }),
            json!({ "type": "review_requested", "node": "publish", "options": ["publish", "skip"] }),
        ],
        false,
    );
    let dir = root.join(".apb/runs/r-pub");
    fs::write(dir.join("playbook.yaml"), PB_PUBLISH).expect("write run playbook.yaml");
    apb_core::run_origin::stamp(&dir, "r-pub").expect("stamp the run");
    refused(
        "stale publish nonce",
        decide(
            root,
            "r-pub",
            "publish",
            "publish",
            Some("publish-stale"),
            None,
        ),
    )
}

/// A run step deciding a gate of its own run.
fn review_decide_refused_own_run() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = review_parked_run(root);
    env.set("APB_RUN_ID", &run_id);
    refused(
        "own run",
        decide(root, &run_id, "gate", "approved", None, None),
    )
}

/// The `RunAnswerArgs` of an answer.
fn answer_args(run_id: Option<&str>, token: Option<&str>) -> super::RunAnswerArgs {
    super::RunAnswerArgs {
        run_id: run_id.map(str::to_string),
        token: token.map(str::to_string),
        node: None,
        answer: "pg".to_string(),
        request: None,
        workspace: None,
    }
}

fn run_answer_refused_exactly_one() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    apb_core::registry::init_project(proj.path()).expect("init_project");
    let server = super::WfMcp::new(proj.path().to_path_buf());
    let result = block(server.run_answer(Parameters(answer_args(None, None))));
    refused_with(result, "exactly_one_of_run_id_or_token_required")
}

fn run_answer_refused_supervisor_needs_token() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    apb_core::registry::init_project(proj.path()).expect("init_project");
    let server = super::WfMcp::for_supervisor(proj.path().to_path_buf());
    let result = block(server.run_answer(Parameters(answer_args(Some("r1"), None))));
    refused_with(result, "supervisor_session_requires_token")
}

/// Two interactive nodes, run one after the other; node `gate` takes only
/// a person's answer.
const PB_TWO_QUESTIONS: &str = r#"
schema: 2
id: q2pb
name: Two questions
version: 1.0.0
defaults: { profile: main }
nodes:
  - { id: start, type: start }
  - { id: gate, type: agent_task, prompt: "QUESTION_OUT", interactive: true, answer_by: human }
  - { id: other, type: agent_task, prompt: "QUESTION_OUT", interactive: true }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: gate }
  - { from: gate, to: other }
  - { from: other, to: done }
"#;

/// A run of `q2pb` whose node `gate` asks its question. With `both`, the
/// question channel also holds an open question of `other`, its line a
/// copy of `gate`'s (the drive asks them one at a time; two open at once is
/// what a resumed or a parallel run leaves).
fn questions_run(root: &Path, env: &mut FreezeEnv, both: bool) -> String {
    seed_project(root, "q2pb", PB_TWO_QUESTIONS);
    seed_main_profile(root);
    let agent = stub_agent(root, QUESTION_AGENT_BODY);
    env.set("APB_AGENT_CMD", &agent);
    let run_id = start_bg(root, "q2pb");
    let dir = root.join(".apb/runs").join(&run_id);
    wait_for("the question", || {
        apb_engine::question::read_questions_after(&dir, None)
            .map(|q| !q.is_empty())
            .unwrap_or(false)
    });
    if both {
        let path = dir.join("questions.jsonl");
        let text = fs::read_to_string(&path).expect("read questions.jsonl");
        let mut line: Value =
            serde_json::from_str(text.lines().next().expect("a question")).expect("a line");
        line["node"] = json!("other");
        line["seq"] = json!(1);
        fs::write(&path, format!("{text}{line}\n")).expect("write questions.jsonl");
    }
    run_id
}

fn run_answer_refused_answer_by_human() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = questions_run(root, &mut env, false);
    refused(
        "answer_by human",
        crate::tools::run_answer(root, &run_id, Some("gate"), "pg", "supervisor", None),
    )
}

fn run_answer_refused_ambiguous() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = questions_run(root, &mut env, true);
    refused(
        "ambiguous question",
        crate::tools::run_answer(root, &run_id, None, "pg", "human", None),
    )
}

fn run_answer_refused_no_question() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = finished_noagent(root);
    refused(
        "no pending question",
        crate::tools::run_answer(root, &run_id, None, "pg", "human", None),
    )
}

fn run_resume_refused_untrusted() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = finished_noagent(proj.path());
    let server = super::WfMcp::new(proj.path().to_path_buf());
    let mut args = resume_args(&run_id);
    args.acknowledge_untrusted = None;
    refused_with(
        server.run_resume_for(args, None),
        "untrusted_requires_acknowledge",
    )
}

fn run_resume_refused_irreversible() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    let run_id = finished_noagent(root);
    // The snapshot is the irreversible version an older apb ignored: the
    // run carries no consent to it.
    let snap = root.join(".apb/runs").join(&run_id).join("playbook.yaml");
    let yaml = fs::read_to_string(&snap).map_err(|e| e.to_string())?;
    fs::write(
        &snap,
        yaml.replace("nodes:", "effects: [irreversible]\nnodes:"),
    )
    .map_err(|e| e.to_string())?;
    let server = super::WfMcp::new(root.to_path_buf());
    let mut args = resume_args(&run_id);
    args.from_node = Some("note".to_string());
    // A nonce that answers no refusal: the resume asks for the consent.
    args.confirm_irreversible = Some(super::args::ConfirmIrreversibleArg::Nonce(
        "consent-stale".to_string(),
    ));
    refused_with(
        server.run_resume_for(args, None),
        "irreversible_requires_confirmation",
    )
}

fn run_resume_refused_environment_drift() -> Result<super::CallToolResult, String> {
    let mut env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "okpb", PB_AGENT_OK);
    seed_main_profile(root);
    let agent = stub_agent(root, "echo ok");
    env.set("APB_AGENT_CMD", &agent);
    let run_id = apb_engine::run(root, "okpb", None, apb_engine::RunOptions::default())
        .map_err(|e| e.to_string())?
        .run_id;
    // The agent binary changed since the run started.
    stub_agent(root, "# a changed binary, another size\necho ok");
    let server = super::WfMcp::new(root.to_path_buf());
    let mut args = resume_args(&run_id);
    args.from_node = Some("work".to_string());
    let result = server.run_resume_for(args, None);
    if !result.is_error.unwrap_or(false) || !result_text(&result).contains("drift") {
        return Err(format!(
            "expected the environment drift refusal, got {}",
            result_text(&result)
        ));
    }
    Ok(result)
}

/// The budget playbook whose `ask` pauses the run before its first agent
/// step (no history, so no estimate): no agent runs.
const PB_BUDGET_ASK: &str = "schema: 2\nid: askpb\nname: Budget ask\nversion: 1.0.0\ndefaults: { profile: main }\nbudget: { tokens: 1000 }\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: agent_task, prompt: a }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: done }\n";

/// A run of `askpb` its unanswered `ask` paused.
fn ask_paused_run(root: &Path) -> String {
    seed_project(root, "askpb", PB_BUDGET_ASK);
    seed_main_profile(root);
    // The machine allows every billing basis: the pause is the budget's.
    let policy = apb_core::policy_file::machine_path().expect("a config dir");
    fs::write(
        policy,
        "billing:\n  billed: allow\n  unknown_basis: allow\n",
    )
    .expect("write policy.yaml");
    let res = apb_engine::run(root, "askpb", None, apb_engine::RunOptions::default())
        .expect("the budgeted run");
    assert_eq!(
        res.outcome,
        apb_engine::state::RunStatus::Paused,
        "the budget paused the run"
    );
    res.run_id
}

fn run_resume_refused_budget_paused() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = ask_paused_run(proj.path());
    let server = super::WfMcp::new(proj.path().to_path_buf());
    refused_with(
        server.run_resume_for(resume_args(&run_id), None),
        "budget_paused",
    )
}

/// The resume arguments of a budget change on run `run_id`.
fn budget_change_args(run_id: &str) -> super::RunResumeArgs {
    let mut args = resume_args(run_id);
    args.budget = Some(super::BudgetResumeArg {
        on_no_estimate: Some("continue".into()),
        ..super::BudgetResumeArg::default()
    });
    args
}

fn run_resume_refused_budget_confirmation() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = ask_paused_run(proj.path());
    let server = super::WfMcp::new(proj.path().to_path_buf());
    refused_with(
        server.run_resume_for(budget_change_args(&run_id), None),
        "budget_change_requires_confirmation",
    )
}

/// A budget change on a run a supervisor agent watches.
fn run_resume_refused_budget_change() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let run_id = ask_paused_run(proj.path());
    let dir = proj.path().join(".apb/runs").join(&run_id);
    let mut cfg = apb_engine::run_config::read_run_config(&dir).map_err(|e| e.to_string())?;
    cfg.mode = apb_engine::RunMode::AgentSupervised;
    apb_engine::run_config::write_run_config(&dir, &cfg).map_err(|e| e.to_string())?;
    let server = super::WfMcp::new(proj.path().to_path_buf());
    refused_with(
        server.run_resume_for(budget_change_args(&run_id), None),
        "budget_change_refused",
    )
}

// --- decision_ask (design D1.5 item 5.6) ---

/// The fake decision provider of the config dir: it answers the standalone
/// `is` question (its question id is `answer`) with a calibrated-looking
/// noul of 0.9, no network.
const DECISIONS_YAML: &str = "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      answer: { type: noul, noul: 0.9 }\n";

fn decision_ask_answered() -> Result<super::CallToolResult, String> {
    let env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    fs::write(env.config.path().join("decisions.yaml"), DECISIONS_YAML)
        .expect("write decisions.yaml");
    let out = crate::tools::decision_ask(
        root,
        crate::tools::DecisionAskInput {
            kind: "is".to_string(),
            question: "Ship the release?".to_string(),
            options: Vec::new(),
            items: Vec::new(),
            criteria: None,
            run_id: None,
            node_id: None,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["answered"] != json!(true) {
        return Err(format!("expected an answered ask, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn decision_ask_refused() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    let out = crate::tools::decision_ask(
        root,
        crate::tools::DecisionAskInput {
            kind: "is".to_string(),
            question: "Ship the release?".to_string(),
            options: Vec::new(),
            items: Vec::new(),
            criteria: None,
            run_id: None,
            node_id: None,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["answered"] != json!(false) || out.get("refused").is_none() {
        return Err(format!("expected the refusal, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

// --- playbook_catalog (design D1.5 item 5.7) ---

fn playbook_catalog_matches() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    let out = crate::tools::playbook_catalog(
        root,
        &crate::catalog::CatalogRequest {
            workspace_id: None,
            revision: None,
            limit: None,
            fit: false,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["entries"].as_array().is_none_or(|e| e.is_empty()) {
        return Err(format!("expected the catalog entries, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn playbook_catalog_suppressed() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    apb_core::dismiss::record_decision(
        root,
        apb_core::dismiss::DecisionInput {
            pattern: "tidy-imports".to_string(),
            synopsis: "tidy the imports and run the tests".to_string(),
            kind: apb_core::dismiss::DecisionKind::Hard,
            scope: apb_core::dismiss::DecisionScope::Project,
            hard_ttl_days_override: None,
        },
    )
    .map_err(|e| e.to_string())?;
    let out = crate::tools::playbook_catalog(
        root,
        &crate::catalog::CatalogRequest {
            workspace_id: None,
            revision: None,
            limit: None,
            fit: false,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["suppressed_suggestions"]
        .as_array()
        .is_none_or(|s| s.is_empty())
    {
        return Err(format!("expected the suppressed suggestions, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

// --- their one documented result (design D1.5 item 5.8) ---

fn playbook_capture_captured() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    let out = crate::tools::playbook_capture(
        root,
        &json!("tidy the imports and run the tests"),
        "project",
        PB_NOAGENT.trim_start(),
    )
    .map_err(|e| e.to_string())?;
    Ok(super::to_call_tool_result(Ok(out)))
}

fn suggestion_dismiss_dismissed() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    apb_core::registry::init_project(root).expect("init_project");
    let out = crate::tools::suggestion_dismiss(
        root,
        crate::tools::DismissRequest {
            pattern: "tidy-imports",
            synopsis: "tidy the imports and run the tests",
            kind: None,
            scope: None,
            ttl_days: None,
        },
    )
    .map_err(|e| e.to_string())?;
    if out["dismissed"] != json!("tidy-imports") {
        return Err(format!("expected the dismissal, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn playbook_interview_guide() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let out = crate::tools::playbook_interview().map_err(|e| e.to_string())?;
    if out["guide"].as_str().is_none() {
        return Err("expected the interview guide".to_string());
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn playbook_howto_howto() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let out = crate::tools::playbook_howto().map_err(|e| e.to_string())?;
    if out["howto"].as_str().is_none() {
        return Err("expected the howto".to_string());
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn profile_list_listed() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    seed_main_profile(root);
    let out = crate::profile_tools::profile_list(root).map_err(|e| e.to_string())?;
    if out["profiles"].as_array().is_none_or(|p| p.is_empty()) {
        return Err(format!("expected the profiles, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn profile_howto_howto() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let out = crate::advisory_tools::profile_howto().map_err(|e| e.to_string())?;
    if out["format"].is_null() {
        return Err(format!("expected the profile howto, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

fn projects_list_listed() -> Result<super::CallToolResult, String> {
    let _env = FreezeEnv::new();
    let proj = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = proj.path();
    seed_project(root, "noagent", PB_NOAGENT);
    apb_core::projects::bind_id(root).map_err(|e| e.to_string())?;
    let out = crate::tools::projects_list().map_err(|e| e.to_string())?;
    if out.as_array().is_none_or(|p| p.is_empty()) {
        return Err(format!("expected the registered workspaces, got {out}"));
    }
    Ok(super::to_call_tool_result(Ok(out)))
}

// ---------------------------------------------------------------------------
// The writer (design D0)
// ---------------------------------------------------------------------------

/// The normalized schema the freeze would hold for a tool, with its
/// synthesized `base`, `examples` and `boundaries`.
fn proposed_tool_entry(name: &str, frozen_entry: Option<&Value>) -> Value {
    let served_schema = served_tool(name);
    let input = normalize(name, &served_schema, &served_schema);
    assert!(
        !holds_intersection(&input),
        "{name}: the served schema writes a restricting keyword beside a $ref, a union, an enum or a const; the writer does not synthesize the base, examples and boundaries of such an intersection, so teach synth_base, synth_examples and synth_boundaries the \"all\" form before freezing it"
    );
    let base = synth_base(&input);
    let boundaries = synth_boundaries(&input);
    let examples = synth_examples(&input);
    let class = {
        use super::read_only::{READ_ONLY, ReadOnlyUse};
        READ_ONLY
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, u)| match u {
                ReadOnlyUse::Full => "full",
                ReadOnlyUse::Restricted => "restricted",
            })
            .unwrap_or("removed")
    };
    let mut entry = json!({
        "read_only": class,
        "input": input,
        "base": base,
        "examples": examples,
        "boundaries": boundaries,
    });
    if let Some(frozen) = frozen_entry {
        // The frozen base is never changed; the frozen `required` may only
        // shrink, each constraint may only widen and the class may only
        // move toward more read-only service (rules 2, 3 and 6), so the
        // frozen values stay valid to keep. The boundaries and the
        // examples merge add-only: every frozen value stays, at its
        // frozen path, in frozen order; the build's new values (a new
        // alternative of a union, a new argument's domain edge) are
        // appended after them, and a wholly new path sorts in. A frozen
        // value the build no longer proposes stays (the writer never
        // removes).
        entry["base"] = frozen["base"].clone();
        let proposed_boundaries = entry["boundaries"].clone();
        if let Some(frozen_boundaries) = frozen.get("boundaries").and_then(|b| b.as_object()) {
            merge_path_lists(
                frozen_boundaries,
                proposed_boundaries.as_object().expect("an object"),
                &mut entry["boundaries"],
            );
        }
        if let Some(frozen_examples) = frozen.get("examples").and_then(|e| e.as_object()) {
            merge_examples(
                frozen_examples,
                &entry["input"].clone(),
                &mut entry["examples"],
            );
        }
    }
    entry
}

/// Whether a normalized schema holds an intersection (`{"all": ...}`, a
/// construct with restricting keywords beside it) anywhere.
fn holds_intersection(schema: &Value) -> bool {
    if schema.get("all").is_some() {
        return true;
    }
    let alternatives = schema
        .get("union")
        .and_then(|u| u.as_array())
        .into_iter()
        .flatten();
    let members = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .into_iter()
        .flat_map(|p| p.values());
    let nested = ["items", "additionalProperties"]
        .into_iter()
        .filter_map(|k| schema.get(k));
    alternatives
        .chain(members)
        .chain(nested)
        .any(holds_intersection)
}

/// Merges two boundary maps add-only: for every path in either map the
/// value list is the frozen values in frozen order, then the proposed
/// values not present; the keys sort.
fn merge_path_lists(
    frozen: &serde_json::Map<String, Value>,
    proposed: &serde_json::Map<String, Value>,
    out: &mut Value,
) {
    let mut paths: Vec<&String> = frozen.keys().chain(proposed.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut merged = serde_json::Map::new();
    for path in paths {
        let mut values: Vec<Value> = frozen
            .get(path)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for value in proposed
            .get(path)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
        merged.insert(path.clone(), Value::Array(values));
    }
    *out = Value::Object(merged);
}

/// Merges the example lists add-only. A union argument's example is the
/// list of its alternatives' values; every frozen value stays, in frozen
/// order, and a value is appended for each alternative of the current
/// schema no frozen value takes (a new alternative). A frozen single value
/// (an earlier writer kept one value of a `T | null` union) becomes the
/// first entry of its list.
fn merge_examples(frozen: &serde_json::Map<String, Value>, input: &Value, out: &mut Value) {
    let mut paths: Vec<String> = frozen.keys().cloned().collect();
    paths.extend(union_paths(input));
    paths.sort();
    paths.dedup();
    let mut merged = serde_json::Map::new();
    for path in paths {
        let alternatives = if union_paths(input).contains(&path) {
            leaf_alternatives(&schema_at(input, &path))
        } else {
            Vec::new()
        };
        if alternatives.is_empty() {
            // No union at this path in the build: the frozen entry stays
            // as it is (rule 2 fails a union that lost an alternative).
            if let Some(kept) = frozen.get(&path) {
                merged.insert(path, kept.clone());
            }
            continue;
        }
        let mut values: Vec<Value> = match frozen.get(&path) {
            Some(Value::Array(list))
                if !list.is_empty()
                    && list
                        .iter()
                        .all(|v| alternatives.iter().any(|a| value_matches(a, v))) =>
            {
                list.clone()
            }
            Some(single) => vec![single.clone()],
            None => Vec::new(),
        };
        for alternative in &alternatives {
            if !values.iter().any(|v| value_matches(alternative, v)) {
                values.push(synth_value(alternative));
            }
        }
        merged.insert(path, Value::Array(values));
    }
    *out = Value::Object(merged);
}

/// One minimal argument object the tool accepts: its required members
/// only, each the simplest accepted value of its schema.
fn synth_base(input: &Value) -> Value {
    let mut out = serde_json::Map::new();
    synth_base_into(input, &mut out);
    Value::Object(out)
}

fn synth_base_into(input: &Value, out: &mut serde_json::Map<String, Value>) {
    if let Some(alternatives) = input.get("union") {
        // A union: the first alternative's required members.
        if let Some(first) = alternatives.as_array().and_then(|a| a.first()) {
            synth_base_into(first, out);
        }
        return;
    }
    if let Some(props) = input.get("properties").and_then(|p| p.as_object()) {
        let required = input
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        for name in required.iter().filter_map(|r| r.as_str()) {
            let Some(schema) = props.get(name) else {
                continue;
            };
            out.insert(name.to_string(), synth_value(schema));
        }
    }
}

/// The simplest accepted value of a normalized schema.
fn synth_value(schema: &Value) -> Value {
    if let Some(alternatives) = schema.get("union").and_then(|u| u.as_array()) {
        return alternatives.first().map(synth_value).unwrap_or(Value::Null);
    }
    if let Some(values) = schema.get("enum").and_then(|e| e.as_array()) {
        return values.first().cloned().unwrap_or(Value::Null);
    }
    if let Some(const_value) = schema.get("const") {
        return const_value.clone();
    }
    let types = schema["types"].as_array().cloned().unwrap_or_default();
    let kind = types.first().and_then(|t| t.as_str()).unwrap_or("any");
    match kind {
        "string" => json!("x"),
        "integer" | "number" => schema.get("minimum").cloned().unwrap_or(json!(0)),
        "boolean" => json!(false),
        "object" => {
            let mut out = serde_json::Map::new();
            synth_base_into(schema, &mut out);
            Value::Object(out)
        }
        "array" => {
            // An array with a fixed length (a span `[from, to]`): that
            // many item values, else empty.
            let length = schema.get("minItems").and_then(|m| m.as_u64()).unwrap_or(0) as usize;
            let item = schema.get("items").cloned().unwrap_or(Value::Null);
            Value::Array((0..length).map(|_| synth_value(&item)).collect())
        }
        "null" => Value::Null,
        _ => Value::Null,
    }
}

/// The two edges of an integer format, as exact JSON integers (design
/// D1.3, the boundary domains): `0` and the top of `uint8` to `uint64`, the
/// signed range of `int8` to `int64`. `uint` and `int` (a Rust `usize` and
/// `isize`) are 64 bits wide on every platform apb ships for. `None` for a
/// format with no integer domain (`double`, `float`).
fn format_edges(format: &str) -> Option<(Value, Value)> {
    Some(match format {
        "uint8" => (json!(0), json!(u8::MAX)),
        "uint16" => (json!(0), json!(u16::MAX)),
        "uint32" => (json!(0), json!(u32::MAX)),
        "uint64" | "uint" => (json!(0), json!(u64::MAX)),
        "int8" => (json!(i8::MIN), json!(i8::MAX)),
        "int16" => (json!(i16::MIN), json!(i16::MAX)),
        "int32" => (json!(i32::MIN), json!(i32::MAX)),
        "int64" | "int" => (json!(i64::MIN), json!(i64::MAX)),
        _ => return None,
    })
}

/// The boundary values of every numeric or enumerated argument path,
/// through the object alternatives of unions and array items on the way:
/// for a number its lower edge (the `minimum`, else its format's floor,
/// else 0) and its upper edge (the `maximum`, else its format's top, where
/// the format has one), for an enumeration every value of its `enum` or
/// its `const`.
fn synth_boundaries(input: &Value) -> Value {
    let mut out = serde_json::Map::new();
    synth_boundaries_into(input, "", &mut out);
    Value::Object(out)
}

fn synth_boundaries_into(input: &Value, prefix: &str, out: &mut serde_json::Map<String, Value>) {
    for alternative in leaf_alternatives(input) {
        if let Some(props) = alternative.get("properties").and_then(|p| p.as_object()) {
            for (name, prop) in props {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                let mut values: Vec<Value> = Vec::new();
                for leaf in leaf_alternatives(prop) {
                    let types = leaf["types"].as_array().cloned().unwrap_or_default();
                    let numeric = types.iter().any(|t| t == "integer" || t == "number");
                    let edges = leaf
                        .get("format")
                        .and_then(|f| f.as_str())
                        .and_then(format_edges);
                    if numeric {
                        match (leaf.get("minimum"), &edges) {
                            (Some(minimum), _) => values.push(minimum.clone()),
                            (None, Some((low, _))) => values.push(low.clone()),
                            (None, None) => values.push(json!(0)),
                        }
                        match (leaf.get("maximum"), &edges) {
                            (Some(maximum), _) => values.push(maximum.clone()),
                            (None, Some((_, top))) => values.push(top.clone()),
                            (None, None) => {}
                        }
                    }
                    if let Some(enum_values) = leaf.get("enum").and_then(|e| e.as_array()) {
                        values.extend(enum_values.iter().cloned());
                    }
                    if let Some(const_value) = leaf.get("const") {
                        values.push(const_value.clone());
                    }
                }
                if !values.is_empty() {
                    values.sort_by_key(|v| v.to_string());
                    values.dedup();
                    out.insert(path.clone(), Value::Array(values));
                }
                synth_boundaries_into(prop, &path, out);
            }
        }
        if let Some(items) = alternative.get("items")
            && !prefix.is_empty()
        {
            synth_boundaries_into(items, &format!("{prefix}[]"), out);
        }
    }
}

/// One accepted value per union alternative (nested unions flattened),
/// keyed by the argument's dotted path: the freeze shows that every
/// alternative of a union argument still deserializes.
fn synth_examples(input: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for path in union_paths(input) {
        let values: Vec<Value> = leaf_alternatives(&schema_at(input, &path))
            .iter()
            .map(synth_value)
            .collect();
        out.insert(path, Value::Array(values));
    }
    Value::Object(out)
}

/// The writer's merges only add: a frozen boundary or example value the
/// build no longer proposes stays, a new alternative's value is appended,
/// and a frozen single example becomes the head of its list.
#[test]
fn the_writer_merges_examples_and_boundaries_add_only() {
    let input = json!({ "types": ["object"], "required": [], "properties": {
        "n": { "union": [
            { "types": ["integer"], "format": "int32" },
            { "types": ["string"] },
            { "types": ["null"] }
        ] }
    } });
    // A frozen single value of the first alternative.
    let frozen = json!({ "n": 7 });
    let mut out = Value::Null;
    merge_examples(frozen.as_object().unwrap(), &input, &mut out);
    assert_eq!(out, json!({ "n": [7, "x", null] }));
    // A frozen list keeps its order; nothing is dropped.
    let frozen = json!({ "n": [null, 5], "gone": [1] });
    merge_examples(frozen.as_object().unwrap(), &input, &mut out);
    assert_eq!(out, json!({ "gone": [1], "n": [null, 5, "x"] }));
    // The signed edges of an int32.
    assert_eq!(
        synth_boundaries(&input),
        json!({ "n": [-2147483648, 2147483647] })
    );
    let mut merged = Value::Null;
    merge_path_lists(
        json!({ "n": [3], "old": [1] }).as_object().unwrap(),
        synth_boundaries(&input).as_object().unwrap(),
        &mut merged,
    );
    assert_eq!(
        merged,
        json!({ "n": [3, -2147483648, 2147483647], "old": [1] })
    );
}

/// The merge the writer refuses a break with: a frozen tool whose
/// argument the current one lacks panics, and nothing is written.
#[test]
fn the_writer_refuses_a_removed_argument() {
    let frozen = json!({
        "playbook_run": {
            "read_only": "removed",
            "input": { "types": ["object"], "required": ["id"], "properties": {
                "id": { "types": ["string"] },
                "gone": { "types": ["string"] }
            } }
        }
    });
    let served = served_tool("playbook_run");
    let current = normalize("playbook_run", &served, &served);
    let result = std::panic::catch_unwind(|| {
        accepts_superset("playbook_run", "", &frozen["input"], &current).unwrap();
    });
    assert!(
        result.is_err(),
        "the writer must refuse a frozen argument the tool no longer takes"
    );
}

/// The one writer of the MCP group: `mcp-tools-1.0.json`,
/// `mcp-results-1.0.json` and `machine-codes-1.0.txt`.
#[test]
fn write_freeze() {
    if !write_freeze_enabled() {
        return;
    }
    // 1. The proposed files.
    let frozen = frozen_tools();
    let mut tools = serde_json::Map::new();
    for name in served().into_iter().map(|(n, _)| n) {
        let entry = proposed_tool_entry(&name, frozen.get(&name));
        // Rules 2 and 3 over the proposed snapshot: a frozen input the
        // current one does not accept, or a gained required member, is a
        // break and nothing is written.
        if let Some(frozen_entry) = frozen.get(&name) {
            accepts_superset(&name, "", &frozen_entry["input"], &entry["input"])
                .unwrap_or_else(|e| panic!("{e}: fix the code, never the fixture"));
            gains_required(&name, &frozen_entry["input"], &entry["input"])
                .unwrap_or_else(|e| panic!("{e}: fix the code, never the fixture"));
        }
        tools.insert(name, entry);
    }
    // Every frozen tool is still served (rule 1): the writer never drops
    // one, and rules 4 and 5 hold over the proposed entries.
    for name in frozen.keys() {
        assert!(
            tools.contains_key(name),
            "the frozen tool {name} is no longer served: fix the code, never the fixture"
        );
    }
    check_union_examples(&tools);
    check_boundaries(&tools);
    let proposed_tools = json!({ "format": 1, "tools": Value::Object(tools) });
    // The machine codes: every entry of the table and every typed value,
    // sorted; a frozen line is never removed.
    let frozen_codes: Vec<String> = fixture("machine-codes-1.0.txt")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let mut lines: Vec<String> = Vec::new();
    for (group, code) in typed_machine_codes() {
        lines.push(format!("{group} {code}"));
    }
    for (group, code, _) in MACHINE_CODES {
        lines.push(format!("{group} {code}"));
    }
    lines.sort();
    lines.dedup();
    for line in &frozen_codes {
        assert!(
            lines.contains(line),
            "the writer never removes a line: machine-codes-1.0.txt holds {line}, which this build no longer produces"
        );
    }
    let proposed_codes = lines.join("\n") + "\n";
    // The results: every frozen entry kept, each producer's variant merged
    // add-only (`merge_variant`), then the result checks over the proposed
    // map: every frozen variant still produced, every contract readable.
    let mut results = frozen_results();
    for (tool, variant, producer) in RESULT_VARIANTS {
        let result =
            producer().unwrap_or_else(|e| panic!("the producer {tool}/{variant} failed: {e}"));
        let now = result_contract(&result);
        let frozen_entry = results.get(*tool).and_then(|t| t.get(*variant)).cloned();
        let merged = merge_variant(tool, variant, frozen_entry.as_ref(), &now);
        results
            .entry(tool.to_string())
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("an object")
            .insert(variant.to_string(), merged);
    }
    for (tool, variants) in &results {
        for variant in variants.as_object().expect("an object").keys() {
            assert!(
                RESULT_VARIANTS
                    .iter()
                    .any(|(t, v, _)| t == tool && v == variant),
                "the frozen result variant {tool}/{variant} has no producer: the writer never drops a variant"
            );
        }
    }
    check_frozen_contracts(&results);
    let proposed_results = json!({ "format": 1, "tools": Value::Object(results) });

    // 3. Write, atomically.
    let dir = fixtures_dir();
    std::fs::write(
        dir.join("mcp-tools-1.0.json.tmp-freeze"),
        serde_json::to_string_pretty(&proposed_tools).unwrap(),
    )
    .unwrap();
    std::fs::rename(
        dir.join("mcp-tools-1.0.json.tmp-freeze"),
        dir.join("mcp-tools-1.0.json"),
    )
    .unwrap();
    std::fs::write(
        dir.join("machine-codes-1.0.txt.tmp-freeze"),
        &proposed_codes,
    )
    .unwrap();
    std::fs::rename(
        dir.join("machine-codes-1.0.txt.tmp-freeze"),
        dir.join("machine-codes-1.0.txt"),
    )
    .unwrap();
    std::fs::write(
        dir.join("mcp-results-1.0.json.tmp-freeze"),
        serde_json::to_string_pretty(&proposed_results).unwrap(),
    )
    .unwrap();
    std::fs::rename(
        dir.join("mcp-results-1.0.json.tmp-freeze"),
        dir.join("mcp-results-1.0.json"),
    )
    .unwrap();
    panic!(
        "freeze written: mcp-tools-1.0.json, machine-codes-1.0.txt, mcp-results-1.0.json under {}; review the diff and run the test again without APB_WRITE_FREEZE",
        dir.display()
    );
}
