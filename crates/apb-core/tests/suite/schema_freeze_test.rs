//! The schema freeze: a frozen corpus of playbook definitions (every
//! top-level example, every bundled playbook and hand-written field
//! fixtures), each with its frozen error codes; a coverage inventory
//! checked from the types; the validator code registry lines and the
//! witnesses. See `openspec/changes/one-point-oh-freeze` and
//! `docs/COMPATIBILITY.md`.
//!
//! The policy: a frozen document's error codes are compared exactly and
//! never rewritten; its warning codes are recorded by the writer and not
//! asserted. The writer behind `APB_WRITE_FREEZE=1` only ever adds: it never
//! removes an entry, never changes a frozen error code, line or argument,
//! and writes nothing when any freeze rule fails.

use std::path::{Path, PathBuf};

use apb_core::schema::Playbook;
use apb_core::validate::{self, Issue, Severity, ValidationContext};
use serde::de::DeserializeOwned;

const MARKER: &str = "# apb-freeze-document: ";

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema-1.0")
}

fn fixture(file: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(file))
        .unwrap_or_else(|e| panic!("cannot read the freeze fixture {file}: {e}"))
}

fn examples_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/playbooks")
}

fn bundled_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bundled/playbooks")
}

/// `(name, text)` of every document, in file order; text without its
/// marker line. Panics on a repeated name.
fn documents(file: &str) -> Vec<(String, String)> {
    let text = fixture(file);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some((name, body)) = current.take() {
                out.push((name, body));
            }
            current = Some((name.to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(doc) = current.take() {
        out.push(doc);
    }
    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in &out {
        assert!(seen.insert(name.as_str()), "duplicate document name {name}");
    }
    out
}

fn document(file: &str, name: &str) -> String {
    documents(file)
        .into_iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no document named {name} in {file}"))
        .1
}

fn parse_document(file: &str, name: &str) -> Playbook {
    let text = document(file, name);
    Playbook::from_yaml(&text)
        .unwrap_or_else(|e| panic!("the frozen document {name} does not parse: {e}"))
}

/// The document's sorted unique error codes.
fn frozen_errors(file: &str, name: &str) -> Vec<String> {
    let pb = parse_document(file, name);
    code_numbers(
        validate::validate(&pb, &ValidationContext::default())
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.code.to_string()),
    )
}

fn code_number(code: &str) -> u32 {
    code[1..]
        .parse()
        .unwrap_or_else(|_| panic!("{code} is not a V<n> code"))
}

fn code_numbers(codes: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = codes.collect();
    out.sort_by_key(|c| code_number(c));
    out.dedup();
    out
}

fn expected() -> serde_json::Value {
    serde_json::from_str(&fixture("expected.json")).expect("expected.json parses")
}

fn expected_entry(name: &str) -> Option<(Vec<String>, Vec<String>)> {
    let all = expected();
    let entry = all.get(name)?;
    let read = |list: &str| -> Vec<String> {
        code_numbers(
            entry[list]
                .as_array()
                .unwrap_or_else(|| panic!("expected.json {name}.{list} is a list"))
                .iter()
                .map(|c| c.as_str().expect("a code string").to_string())
                .collect::<Vec<_>>()
                .into_iter(),
        )
    };
    Some((read("errors"), read("warnings")))
}

// ---------------------------------------------------------------------------
// Typed paths
// ---------------------------------------------------------------------------

/// A path segment: a mapping key or a sequence index.
fn value_at<'a>(value: &'a yaml_serde::Value, path: &str) -> Option<&'a yaml_serde::Value> {
    let mut current = value;
    let mut rest = path;
    while !rest.is_empty() {
        // A segment may carry trailing indexes: `items[0][1]`.
        let (segment, tail) = match rest.find('.') {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, ""),
        };
        let (key, mut indexes) = match segment.find('[') {
            Some(at) => (&segment[..at], &segment[at..]),
            None => (segment, ""),
        };
        if !key.is_empty() {
            current = current.get(key)?;
        }
        while let Some(end) = indexes.find(']') {
            let index: usize = indexes[1..end].parse().ok()?;
            current = current.get(index)?;
            indexes = &indexes[end + 1..];
        }
        rest = tail;
    }
    Some(current)
}

/// Every typed path of `value`, one segment per mapping key or sequence
/// index, leaf and inner mappings alike (a mapping is a place its keys
/// hang from).
fn typed_paths(value: &yaml_serde::Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        yaml_serde::Value::Mapping(m) => {
            for (k, v) in m {
                let key = k.as_str().unwrap_or_default().to_string();
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.push(path.clone());
                typed_paths(v, &path, out);
            }
        }
        yaml_serde::Value::Sequence(s) => {
            for (i, v) in s.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                out.push(path.clone());
                typed_paths(v, &path, out);
            }
        }
        _ => {}
    }
}

fn raw_value(text: &str) -> yaml_serde::Value {
    yaml_serde::from_str(text).expect("the document is YAML")
}

/// Sets `value` at the typed `path` of `root`.
fn set_at(root: &mut yaml_serde::Value, path: &str, value: yaml_serde::Value) {
    let (head, tail) = match path.split_once('.') {
        Some(split) => split,
        None => (path, ""),
    };
    let (key, indexes) = match head.find('[') {
        Some(at) => (&head[..at], &head[at..]),
        None => (head, ""),
    };
    if !key.is_empty() {
        if !current_is_mapping(root) && !tail.is_empty() {
            *root = yaml_serde::Value::Mapping(Default::default());
        }
        if tail.is_empty() && indexes.is_empty() {
            if let Some(map) = root.as_mapping_mut() {
                map.insert(yaml_serde::Value::String(key.into()), value);
            }
            return;
        }
        let entry = root
            .as_mapping_mut()
            .expect("a mapping on the way")
            .entry(yaml_serde::Value::String(key.into()))
            .or_insert(yaml_serde::Value::Mapping(Default::default()));
        set_at(
            entry,
            &format!(
                "{}{}",
                indexes,
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(".{tail}")
                }
            ),
            value,
        );
        return;
    }
    // Pure index chain.
    let index: usize = indexes
        .trim_start_matches('[')
        .split(']')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if let Some(seq) = root.as_sequence_mut() {
        while seq.len() <= index {
            seq.push(yaml_serde::Value::Mapping(Default::default()));
        }
        if tail.is_empty() {
            seq[index] = value;
        } else {
            set_at(&mut seq[index], tail, value);
        }
    }
}

fn current_is_mapping(value: &yaml_serde::Value) -> bool {
    matches!(value, yaml_serde::Value::Mapping(_))
}

fn serialized_value(pb: &Playbook) -> yaml_serde::Value {
    let text = yaml_serde::to_string(pb).expect("the parsed playbook serializes");
    yaml_serde::from_str(&text).expect("the serialized form parses back")
}

/// `(document, exact path, written form, serialized form, reason)`.
const NORMALIZED: &[(&str, &str, &str, &str, &str)] = &[
    (
        "bundled/apb-inbox-digest@1.0.1",
        "nodes[2].isolation",
        "level: best_effort",
        "best_effort",
        "an isolation written as a mapping with only its level serializes as the bare level; both spellings parse to the same value",
    ),
    (
        "bundled/apb-inbox-digest@1.0.1",
        "nodes[4].isolation",
        "level: best_effort",
        "best_effort",
        "an isolation written as a mapping with only its level serializes as the bare level; both spellings parse to the same value",
    ),
    (
        "bundled/apb-issue-triage@1.0.1",
        "nodes[2].isolation",
        "level: best_effort",
        "best_effort",
        "an isolation written as a mapping with only its level serializes as the bare level; both spellings parse to the same value",
    ),
];

// ---------------------------------------------------------------------------
// Coverage helpers (test only; they never ship)
// ---------------------------------------------------------------------------

/// The field names serde knows for a derived struct: a deserializer that
/// records what `deserialize_struct` is called with and fails.
fn fields_of<T: DeserializeOwned>() -> &'static [&'static str] {
    struct Capture<'a>(&'a mut Option<&'static [&'static str]>);

    impl<'a, 'de> serde::Deserializer<'de> for Capture<'a> {
        type Error = serde::de::value::Error;

        fn deserialize_struct<V>(
            self,
            _name: &'static str,
            fields: &'static [&'static str],
            _visitor: V,
        ) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            *self.0 = Some(fields);
            Err(serde::de::Error::custom("captured"))
        }

        fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            Err(serde::de::Error::custom("not a struct"))
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str
            string bytes byte_buf option unit unit_struct newtype_struct
            seq tuple tuple_struct map enum identifier ignored_any
        }
    }

    let mut captured = None;
    let _ = T::deserialize(Capture(&mut captured));
    captured.expect("no struct fields recorded (a hand-written deserializer; the struct leaves DERIVED_STRUCTS)")
}

/// Every variant of an enum by its serde name, from the error serde gives
/// for an unknown tag (the method of `event_types` in the journal freeze).
fn variants_of<T: DeserializeOwned + std::fmt::Debug>(tag: &str) -> Vec<String> {
    for value in [
        serde_json::json!({ tag: "__apb_freeze_no_such_variant__" }),
        serde_json::json!("__apb_freeze_no_such_variant__"),
    ] {
        let Err(e) = serde_json::from_value::<T>(value) else {
            continue;
        };
        let error = e.to_string();
        let Some((_, expected)) = error.split_once("expected ") else {
            continue;
        };
        // `expected one of `a`, `b``, `expected `a` or `b``,
        // `expected `a``: the variants are the back-quoted words.
        let expected = expected.strip_prefix("one of ").unwrap_or(expected);
        let variants: Vec<String> = expected
            .split(", ")
            .flat_map(|s| s.split(" or "))
            .map(|s| s.trim().trim_matches('`').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if variants.iter().all(|v| {
            !v.is_empty()
                && v.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
        }) {
            return variants;
        }
    }
    panic!("serde lists the variants of {tag} in neither the tagged nor the plain form");
}

/// Every field name of the struct variant `value` holds, serialized and
/// skipped alike: a serializer that records each `serialize_field` and
/// each `skip_field` call of `SerializeStruct` and writes nothing.
fn variant_fields_of<T: serde::Serialize>(value: &T) -> Vec<&'static str> {
    /// The capture serializer's own error: `custom` is all it needs.
    #[derive(Debug)]
    struct CaptureError;
    impl std::fmt::Display for CaptureError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("captured")
        }
    }
    impl std::error::Error for CaptureError {}
    impl serde::ser::Error for CaptureError {
        fn custom<T: std::fmt::Display>(_msg: T) -> Self {
            CaptureError
        }
    }
    struct Capture<'a>(&'a mut Vec<&'static str>);

    impl<'a> serde::ser::Serializer for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        type SerializeSeq = Self;
        type SerializeTuple = Self;
        type SerializeTupleStruct = Self;
        type SerializeTupleVariant = Self;
        type SerializeMap = Self;
        type SerializeStruct = Self;
        type SerializeStructVariant = Self;

        fn serialize_struct(
            self,
            _name: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            Ok(self)
        }

        fn serialize_struct_variant(
            self,
            _name: &'static str,
            _index: u32,
            _variant: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            Ok(self)
        }

        fn serialize_bool(self, _: bool) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_i8(self, _: i8) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_i16(self, _: i16) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_i32(self, _: i32) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_i64(self, _: i64) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_u8(self, _: u8) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_u16(self, _: u16) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_u32(self, _: u32) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_u64(self, _: u64) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_f32(self, _: f32) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_f64(self, _: f64) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_char(self, _: char) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_str(self, _: &str) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_bytes(self, _: &[u8]) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_none(self) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_some<T: ?Sized + serde::Serialize>(self, _: &T) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_unit(self) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_unit_struct(self, _: &'static str) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_unit_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
            self,
            _: &'static str,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            Err(CaptureError)
        }
        fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
            Err(CaptureError)
        }
        fn serialize_tuple_struct(
            self,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            Err(CaptureError)
        }
        fn serialize_tuple_variant(
            self,
            _: &'static str,
            _: u32,
            _: &'static str,
            _: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            Err(CaptureError)
        }
        fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            Err(CaptureError)
        }
        fn is_human_readable(&self) -> bool {
            true
        }
    }

    impl<'a> serde::ser::SerializeStruct for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_field<T: ?Sized + serde::Serialize>(
            &mut self,
            key: &'static str,
            _: &T,
        ) -> Result<(), Self::Error> {
            self.0.push(key);
            Ok(())
        }
        fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
            self.0.push(key);
            Ok(())
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    impl<'a> serde::ser::SerializeStructVariant for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_field<T: ?Sized + serde::Serialize>(
            &mut self,
            key: &'static str,
            _: &T,
        ) -> Result<(), Self::Error> {
            self.0.push(key);
            Ok(())
        }
        fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
            self.0.push(key);
            Ok(())
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    impl<'a> serde::ser::SerializeSeq for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_element<T: ?Sized + serde::Serialize>(
            &mut self,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    impl<'a> serde::ser::SerializeMap for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_key<T: ?Sized + serde::Serialize>(
            &mut self,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn serialize_value<T: ?Sized + serde::Serialize>(
            &mut self,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    impl<'a> serde::ser::SerializeTuple for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_element<T: ?Sized + serde::Serialize>(
            &mut self,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    impl<'a> serde::ser::SerializeTupleStruct for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_field<T: ?Sized + serde::Serialize>(
            &mut self,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    impl<'a> serde::ser::SerializeTupleVariant for Capture<'a> {
        type Ok = ();
        type Error = CaptureError;
        fn serialize_field<T: ?Sized + serde::Serialize>(
            &mut self,
            _: &T,
        ) -> Result<(), Self::Error> {
            Err(CaptureError)
        }
        fn end(self) -> Result<Self::Ok, Self::Error> {
            Err(CaptureError)
        }
    }

    let mut captured = Vec::new();
    let _ = value.serialize(Capture(&mut captured));
    captured
}

/// Every struct of the schema that derives its deserializer without
/// `#[serde(flatten)]`, the ones reached only through an enum variant
/// included. `(name, reader)`; the reader panics with "no struct fields
/// recorded" when the struct stops calling `deserialize_struct`, and the
/// struct then moves to the flattened list.
type StructReader = fn() -> &'static [&'static str];
type DerivedStruct = (&'static str, StructReader);

macro_rules! derived_structs {
    ($($t:ty),* $(,)?) => {
        &[ $(
            (
                stringify!($t),
                (|| fields_of::<$t>()) as StructReader,
            )
        ),* ]
    };
}

const DERIVED_STRUCTS: &[DerivedStruct] = derived_structs!(
    apb_core::schema::Playbook,
    apb_core::schema::Trigger,
    apb_core::schema::Requires,
    apb_core::schema::GoalCriterion,
    apb_core::schema::Goal,
    apb_core::schema::CacheConfig,
    apb_core::schema::NodeFiles,
    apb_core::schema::ContextLimits,
    apb_core::schema::Param,
    apb_core::schema::Defaults,
    apb_core::schema::RetryDelay,
    apb_core::schema::Supervisor,
    apb_core::schema::Evidence,
    apb_core::schema::Approves,
    apb_core::schema::RequiresApproval,
    apb_core::schema::ResultDecl,
    apb_core::schema::AutoDecide,
    apb_core::schema::UnattendedDecide,
    apb_core::schema::InboxWait,
    apb_core::schema::Condition,
    apb_core::schema::Take,
    apb_core::schema::Edge,
    apb_core::schema::PreviousSuccessBlock,
    apb_core::schema::WhenNone,
    apb_core::schema::ForkSpec,
    apb_core::baseline::BaselineBlock,
    apb_core::baseline::BaselineCheck,
    apb_core::baseline::BaselineConnector,
    apb_core::baseline::CompleteWhen,
    apb_core::baseline::Observe,
    apb_core::baseline::ExpectRule,
    apb_core::budget::BudgetBlock,
    apb_core::quality::QualityBlock,
    apb_core::quality::criteria::BuiltinSetting,
    apb_core::quality::criteria::Levels,
    apb_core::quality::criteria::Criterion,
    apb_core::known_problems::KnownProblemsBlock,
    apb_core::follow_up::FollowUp,
    apb_core::concurrency::ConcurrencyBlock,
    apb_core::health::HealthBlock,
    apb_core::health::FailureRate,
    apb_core::unattended_policy::UnattendedPolicy,
    apb_core::review_form::MaxScope,
    apb_core::review_form::ReviewField,
    apb_core::review_form::ShowEntry,
    apb_core::judge::JudgeThreshold,
    apb_core::run_outputs::RunOutput,
    apb_core::run_outputs::OutputSource,
    apb_core::list::ListSpec,
    apb_core::list::ListSkip,
    apb_core::list::PilotSpec,
    apb_core::list::StopWhen,
    apb_core::list::ItemSource,
    apb_core::list::ItemsFrom,
    apb_core::list::ItemFilter,
    apb_core::list::BatchSpec,
    apb_core::list::ItemCheck,
    apb_core::list::OutputCheck,
    apb_core::list::ConnectorCheck,
    apb_core::list::AcceptWhen,
    apb_core::list::JudgeCheck,
    apb_core::schema::TypedFieldDecl,
    apb_core::project_checks::ProjectChecksSpec,
);

/// Every enum of the schema the inventory derives its variants from, with
/// the tag name serde lists in the error for an unknown tag. `(name,
/// tag, reader)`. Untagged enums (`CacheSpec`, `WriteScope`,
/// `SuccessCheck`, `ExpectedDuration`, `Scalar`, `OneOrMany`,
/// `Required`, `Subject`) have no tag serde lists; their forms are the
/// `Type~form` lines.
type VariantReader = fn() -> Vec<String>;
type SchemaEnum = (&'static str, &'static str, VariantReader);

macro_rules! schema_enums {
    ($($t:ty => $tag:literal),* $(,)?) => {
        &[ $(
            (
                stringify!($t),
                $tag,
                (|| variants_of::<$t>($tag)) as VariantReader,
            )
        ),* ]
    };
}

const SCHEMA_ENUMS: &[SchemaEnum] = schema_enums!(
    apb_core::schema::NodeKind => "type",
    apb_core::schema::WaitFor => "type",
    apb_core::schema::EdgeCondition => "type",
    apb_core::schema::GoalCheck => "type",
    apb_core::judge::JudgeQuestion => "type",
    apb_core::schema::Outcome => "outcome",
    apb_core::schema::Audience => "audience",
    apb_core::schema::Effect => "effect",
    apb_core::schema::CacheMode => "mode",
    apb_core::schema::HostDecisions => "host_decisions",
    apb_core::schema::SessionMode => "session_mode",
    apb_core::schema::OnUnknown => "on_unknown",
    apb_core::schema::CompletionCheckSetting => "completion_check",
    apb_core::schema::RouteSetting => "route",
    apb_core::schema::OnGreenBefore => "on_green_before",
    apb_core::schema::StatusEq => "status",
    apb_core::schema::JoinRequire => "require",
    apb_core::schema::BranchFailurePolicy => "on_branch_failure",
    apb_core::budget::OnNoEstimate => "on_no_estimate",
    apb_core::quality::criteria::GradeOnFinish => "grade_on_finish",
    apb_core::quality::criteria::EvidenceKind => "evidence",
    apb_core::quality::criteria::ReadKind => "read",
    apb_core::concurrency::OnBusy => "on_busy",
    apb_core::health::OnTrip => "on_trip",
    apb_core::baseline::ObservationType => "observation_type",
    apb_core::baseline::Change => "change",
    apb_core::review_form::FieldKind => "field_kind",
    apb_core::isolation::Level => "isolation_level",
    apb_core::isolation::Network => "isolation_network",
    apb_core::launch::LaunchPolicy => "launch_policy",
    apb_core::list::PassedIn => "passed_in",
    apb_core::list::PilotThen => "pilot_then",
    apb_core::list::PilotUnattended => "pilot_unattended",
    apb_core::list::OnNeedsLook => "on_needs_look",
    apb_core::list::RerunPolicy => "rerun",
    apb_core::schema::FieldType => "field_type",
    apb_core::schema::AnswerBy => "answer_by",
    apb_core::run_outputs::Pick => "pick",
    apb_core::schema::ApprovalLive => "live",
    apb_core::run_outputs::ChildOutput => "output",
    apb_core::schema::ClaimsRequirement => "claims",
    apb_core::schema::DecisionOptIn => "pre_triage",
    apb_core::schema::OnInvalidClaims => "on_invalid",
    apb_core::profile::ProfileScope => "scope",
    apb_core::result_store::ResultFormat => "format",
    apb_core::result_store::ResultScope => "scope",
);

/// Fields of a flattening struct the declaration walk cannot see (read
/// but never written, or written but never read): `(line, source
/// symbol)`.
const HAND_FIELDS: &[(&str, &str)] = &[];

/// The serde spelling of a type's `Default`.
type DefaultReader = fn() -> String;

macro_rules! default_variants {
    ($($t:ty),* $(,)?) => {
        &[ $(
            (
                stringify!($t),
                (|| {
                    serde_json::to_value(<$t>::default())
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                }) as DefaultReader,
            )
        ),* ]
    };
}

/// The enums whose default variant the serializer leaves out
/// (`skip_serializing_if` on every field that holds them): a fixture that
/// wrote the default would lose the key on serialization, which rule 3
/// refuses, so the default variant is frozen as absence instead. The test
/// reads the type's `Default` and excuses only that one variant from
/// needing a coverage line; every other variant still needs one.
const DEFAULT_VARIANTS: &[(&str, DefaultReader)] = default_variants!(
    apb_core::schema::AnswerBy,
    apb_core::schema::ApprovalLive,
    apb_core::run_outputs::ChildOutput,
    apb_core::schema::ClaimsRequirement,
    apb_core::schema::OnInvalidClaims,
    apb_core::result_store::ResultFormat,
    apb_core::result_store::ResultScope,
    apb_core::run_outputs::Pick,
    apb_core::schema::SessionMode,
);

/// Whether `variant` is the default `DEFAULT_VARIANTS` frees `name` of.
fn default_variant(name: &str, variant: &str) -> bool {
    DEFAULT_VARIANTS
        .iter()
        .any(|(n, read)| short_name(n) == short_name(name) && read() == variant)
}

/// The union of every fixture document, by file.
fn all_documents() -> Vec<(String, String)> {
    let mut out = documents("corpus.yaml");
    out.extend(documents("fields.yaml"));
    out
}

/// A coverage line parsed: `(kind place, document, path)`.
fn coverage_lines() -> Vec<(String, String, String)> {
    fixture("coverage.txt")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            assert_eq!(
                parts.len(),
                3,
                "coverage line is `place document path`: {l}"
            );
            (
                parts[0].to_string(),
                parts[1].to_string(),
                parts[2].to_string(),
            )
        })
        .collect()
}

fn coverage_has(place: &str) -> bool {
    // A line may name the type plainly (`Playbook.params`) or by its
    // module path (`apb_core::schema::Playbook.params`): the last
    // `::`-free segment decides.
    let short = place.rsplit("::").next().unwrap_or(place);
    coverage_lines()
        .iter()
        .any(|(p, _, _)| p == place || p.rsplit("::").next().unwrap_or(p) == short)
}

// ---------------------------------------------------------------------------
// Rules (design D1.1) and the code freeze (design D1.4)
// ---------------------------------------------------------------------------

/// Rule 1: every frozen document parses.
#[test]
fn every_frozen_document_parses() {
    for (name, text) in all_documents() {
        if let Err(e) = Playbook::from_yaml(&text) {
            panic!("the frozen document {name} does not parse: {e}");
        }
    }
}

/// Rule 2: a frozen document keeps its frozen errors; warnings are
/// recorded, not asserted.
#[test]
fn every_frozen_document_keeps_its_frozen_errors() {
    for (name, _) in all_documents() {
        let Some((errors, _)) = expected_entry(&name) else {
            panic!("no expected.json entry for {name}: run the schema writer");
        };
        let file = if documents("corpus.yaml").iter().any(|(n, _)| *n == name) {
            "corpus.yaml"
        } else {
            "fields.yaml"
        };
        let now = frozen_errors(file, &name);
        assert_eq!(
            now, errors,
            "the frozen document {name} gained or lost an error code"
        );
    }
}

/// Whether `path` lies strictly under `place` (a key or an index below
/// it), compared segment by segment, never by suffix.
fn strictly_under(path: &str, place: &str) -> bool {
    path.len() > place.len()
        && path.starts_with(place)
        && matches!(path.as_bytes()[place.len()], b'.' | b'[')
}

/// Rule 3: no frozen key is dropped on parse. A `NORMALIZED` entry names
/// the exact place of a value the serializer spells differently: the
/// written document must hold exactly the entry's written form there and
/// the serialized parse exactly its serialized form, both forms put at
/// that place parse to the same playbook, and only paths strictly under
/// that place are excused. An entry no dropped path needs fails, so a
/// stale entry cannot linger.
#[test]
fn no_frozen_key_is_dropped_on_parse() {
    let mut used = std::collections::BTreeSet::new();
    for (name, text) in all_documents() {
        let raw = raw_value(&text);
        let pb = Playbook::from_yaml(text.trim())
            .unwrap_or_else(|e| panic!("the frozen document {name} does not parse: {e}"));
        let serialized = serialized_value(&pb);
        let mut raw_paths = Vec::new();
        typed_paths(&raw, "", &mut raw_paths);
        for path in raw_paths {
            if value_at(&serialized, &path).is_some() {
                continue;
            }
            let entry = NORMALIZED
                .iter()
                .enumerate()
                .find(|(_, (doc, place, _, _, _))| doc == &name && strictly_under(&path, place));
            let Some((index, (_, place, written, as_serialized, reason))) = entry else {
                panic!(
                    "the frozen document {name} drops the key {path} when its parse serializes again: fix the fixture's value first, and only a real spelling change goes into NORMALIZED"
                );
            };
            used.insert(index);
            check_normalized(
                &name,
                &raw,
                &serialized,
                place,
                written,
                as_serialized,
                reason,
            );
        }
    }
    for (index, (doc, place, _, _, _)) in NORMALIZED.iter().enumerate() {
        assert!(
            used.contains(&index),
            "the NORMALIZED entry {doc} {place} excuses no dropped key: remove it"
        );
    }
}

/// One `NORMALIZED` entry, checked at its exact place in both directions.
fn check_normalized(
    name: &str,
    raw: &yaml_serde::Value,
    serialized: &yaml_serde::Value,
    place: &str,
    written: &str,
    as_serialized: &str,
    reason: &str,
) {
    let form = |text: &str| -> yaml_serde::Value {
        yaml_serde::from_str(text)
            .unwrap_or_else(|e| panic!("NORMALIZED {name} {place}: {reason}: {e}"))
    };
    let written_value = form(written);
    let serialized_value_at = form(as_serialized);
    assert_eq!(
        value_at(raw, place),
        Some(&written_value),
        "NORMALIZED {name} {place}: the written document does not hold exactly the entry's written form there, so a field beside it could be dropped unseen"
    );
    assert_eq!(
        value_at(serialized, place),
        Some(&serialized_value_at),
        "NORMALIZED {name} {place}: the serialized parse does not hold exactly the entry's serialized form there"
    );
    assert_ne!(
        written_value, serialized_value_at,
        "NORMALIZED {name} {place}: the two forms are the same spelling, so the entry excuses nothing but a dropped field"
    );
    let substitute = |value: &yaml_serde::Value| -> yaml_serde::Value {
        let mut doc = raw.clone();
        set_at(&mut doc, place, value.clone());
        let text = yaml_serde::to_string(&doc).expect("the substituted doc renders");
        let pb = Playbook::from_yaml(&text).unwrap_or_else(|e| {
            panic!("NORMALIZED {name} {place}: {reason}: the form does not parse: {e}")
        });
        serialized_value(&pb)
    };
    assert_eq!(
        substitute(&written_value),
        substitute(&serialized_value_at),
        "NORMALIZED {name} {place}: {reason}: the two forms parse to different playbooks"
    );
}

/// Rule 4a: every coverage line is exercised.
#[test]
fn every_coverage_line_is_exercised() {
    let names: std::collections::BTreeSet<String> =
        all_documents().into_iter().map(|(n, _)| n).collect();
    for (place, doc, path) in coverage_lines() {
        assert!(
            names.contains(&doc),
            "the coverage line {place} names the document {doc}, which no fixture file holds"
        );
        let text = document(
            if documents("corpus.yaml").iter().any(|(n, _)| *n == doc) {
                "corpus.yaml"
            } else {
                "fields.yaml"
            },
            &doc,
        );
        let raw = raw_value(&text);
        assert!(
            value_at(&raw, &path).is_some(),
            "the coverage line {place} names {doc} at {path}, which the written document does not hold"
        );
        let pb = Playbook::from_yaml(text.trim())
            .unwrap_or_else(|e| panic!("the document {doc} does not parse: {e}"));
        let serialized = serialized_value(&pb);
        assert!(
            value_at(&serialized, &path).is_some(),
            "the coverage line {place} names {doc} at {path}, which the parse drops when serialized (a default value?)"
        );
    }
}

/// Rule 4b: every derived struct field is in the coverage.
#[test]
fn every_derived_field_is_in_the_coverage() {
    for (name, reader) in DERIVED_STRUCTS {
        eprintln!("deriving {name}");
        for field in reader() {
            let place = format!("{name}.{field}");
            assert!(
                coverage_has(&place),
                "the struct {name} has the field {field}, which no coverage line names ({place})"
            );
        }
    }
}

/// Rule 4c: every variant field is in the coverage, and every variant is
/// held by a fixture. The tag of a walked value is read from its JSON
/// form, so no per-enum `type_str` is needed.
#[test]
fn every_variant_field_is_in_the_coverage() {
    // The variants each enum lists, and the lines that name them.
    let mut unnamed = Vec::new();
    for (name, _tag, reader) in SCHEMA_ENUMS {
        for variant in reader() {
            let place = format!("{name}~{variant}");
            if !coverage_has(&place) && !default_variant(name, &variant) {
                unnamed.push(place);
            }
        }
    }
    assert!(
        unnamed.is_empty(),
        "enum variants no coverage line names: {unnamed:#?}"
    );
    for (name, _) in DEFAULT_VARIANTS {
        assert!(
            SCHEMA_ENUMS.iter().any(|(n, _, _)| n == name),
            "DEFAULT_VARIANTS names {name}, which SCHEMA_ENUMS does not list"
        );
    }
    fn tag_of<T: serde::Serialize>(value: &T, tag: &str) -> Option<String> {
        let json = serde_json::to_value(value).ok()?;
        json.get(tag)?.as_str().map(str::to_string)
    }
    let mut held: std::collections::BTreeMap<&str, std::collections::BTreeSet<String>> =
        SCHEMA_ENUMS
            .iter()
            .map(|(name, _, _)| (*name, std::collections::BTreeSet::new()))
            .collect();
    for (doc_name, text) in all_documents() {
        let pb = Playbook::from_yaml(text.trim())
            .unwrap_or_else(|e| panic!("the frozen document {doc_name} does not parse: {e}"));
        for node in &pb.nodes {
            held.get_mut("apb_core::schema::NodeKind")
                .unwrap()
                .insert(node.kind.type_str().to_string());
            for field in variant_fields_of(&node.kind) {
                if field == "type" {
                    continue; // the tag serde writes, not a variant field
                }
                let place = format!(
                    "apb_core::schema::NodeKind::{}.{}",
                    node.kind.type_str(),
                    field
                );
                assert!(
                    coverage_has(&place),
                    "the fixture {doc_name} holds a node `{}` of kind {} with the field {field}, which no coverage line names ({place})",
                    node.id,
                    node.kind.type_str()
                );
            }
            if let apb_core::schema::NodeKind::Wait { wait_for, .. } = &node.kind {
                let tag = tag_of(wait_for, "type").unwrap_or_default();
                held.get_mut("apb_core::schema::WaitFor")
                    .unwrap()
                    .insert(tag.clone());
                if tag == "inbox" {
                    // A newtype variant over `InboxWait`: its fields are
                    // the `InboxWait.<field>` lines, already covered.
                    continue;
                }
                for field in variant_fields_of(wait_for) {
                    if field == "type" {
                        continue;
                    }
                    let place = format!("apb_core::schema::WaitFor::{tag}.{field}");
                    assert!(
                        coverage_has(&place),
                        "the fixture {doc_name} holds a wait of kind {tag} with the field {field}, which no coverage line names ({place})"
                    );
                }
            }
            if let apb_core::schema::NodeKind::Judge { questions, .. } = &node.kind {
                for (id, question) in questions.iter() {
                    let tag = tag_of(question, "type").unwrap_or_default();
                    held.get_mut("apb_core::judge::JudgeQuestion")
                        .unwrap()
                        .insert(tag.clone());
                    for field in variant_fields_of(question) {
                        if field == "type" {
                            continue;
                        }
                        let place = format!("apb_core::judge::JudgeQuestion::{tag}.{field}");
                        assert!(
                            coverage_has(&place),
                            "the fixture {doc_name} holds the judge question {id} of kind {tag} with the field {field}, which no coverage line names ({place})"
                        );
                    }
                }
            }
        }
        for edge in &pb.edges {
            let Some(condition) = &edge.condition else {
                continue;
            };
            let tag = tag_of(condition, "type").unwrap_or_default();
            held.get_mut("apb_core::schema::EdgeCondition")
                .unwrap()
                .insert(tag.clone());
            for field in variant_fields_of(condition) {
                if field == "type" {
                    continue;
                }
                let place = format!("apb_core::schema::EdgeCondition::{tag}.{field}");
                assert!(
                    coverage_has(&place),
                    "the fixture {doc_name} holds an edge condition of kind {tag} with the field {field}, which no coverage line names ({place})"
                );
            }
        }
        if let Some(goal) = &pb.goal {
            for criterion in &goal.criteria {
                let tag = tag_of(&criterion.check, "type").unwrap_or_default();
                held.get_mut("apb_core::schema::GoalCheck")
                    .unwrap()
                    .insert(tag.clone());
                for field in variant_fields_of(&criterion.check) {
                    if field == "type" {
                        continue;
                    }
                    let place = format!("apb_core::schema::GoalCheck::{tag}.{field}");
                    assert!(
                        coverage_has(&place),
                        "the fixture {doc_name} holds a goal check of kind {tag} with the field {field}, which no coverage line names ({place})"
                    );
                }
            }
        }
    }
    for (name, _, reader) in SCHEMA_ENUMS {
        if !matches!(
            *name,
            "apb_core::schema::NodeKind"
                | "apb_core::schema::WaitFor"
                | "apb_core::schema::EdgeCondition"
                | "apb_core::schema::GoalCheck"
                | "apb_core::judge::JudgeQuestion"
        ) {
            continue;
        }
        let listed = reader();
        let seen = held.get(*name).expect("the held table knows the enum");
        for variant in listed {
            assert!(
                seen.contains(&variant),
                "no fixture of the freeze holds the variant {variant} of {name}"
            );
        }
    }
}

/// Rule 4d: every own field of a struct that flattens another is in the
/// coverage, read from the declaration. Which structs flatten is decided
/// by `the_type_inventory_is_complete` from the source: a new
/// `#[serde(flatten)]` in any reachable type fails there until its struct
/// joins `FLATTENING`, and each struct listed is read here from its own
/// file.
#[test]
fn every_flattened_field_is_in_the_coverage() {
    for (name, file) in FLATTENING {
        let source =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file))
                .unwrap_or_else(|e| panic!("{file} reads: {e}"));
        let block = struct_declaration(&source, name);
        assert!(
            block.contains("#[serde(flatten)]"),
            "FLATTENING names {name}, whose declaration in {file} flattens nothing"
        );
        for field in declared_fields(&block) {
            let place = format!("{name}.{field}");
            if HAND_FIELDS.iter().any(|(l, _)| l == &place) {
                continue;
            }
            assert!(
                coverage_has(&place),
                "the flattening struct {name} has the field {field}, which no coverage line names ({place})"
            );
        }
    }
}

/// The block from `pub struct <Name> {` to its closing brace, comments
/// stripped.
fn struct_declaration(source: &str, name: &str) -> String {
    let start = source
        .find(&format!("pub struct {name} {{"))
        .unwrap_or_else(|| panic!("no pub struct {name} in schema.rs"));
    let brace = source[start..].find('{').expect("the declaration opens") + start;
    let mut depth = 0;
    let mut end = brace;
    for (i, c) in source.bytes().enumerate().skip(brace) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    source[start..=end]
        .lines()
        .map(|l| {
            let at = l.find("//").unwrap_or(l.len());
            &l[..at]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fields of a declaration block: every `name:` member with the serde
/// attributes the lines before it carry, mapped through its rename, left
/// out when it is `#[serde(flatten)]` or `#[serde(skip)]`.
fn declared_fields(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending_attrs = String::new();
    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[serde(") {
            pending_attrs.push_str(trimmed);
            continue;
        }
        if trimmed.starts_with("pub ")
            && !trimmed.starts_with("pub struct")
            && !trimmed.starts_with("pub enum")
            && !trimmed.starts_with("pub fn")
            && trimmed.contains(':')
        {
            let attrs = pending_attrs.clone();
            pending_attrs.clear();
            if attrs.contains("flatten") || attrs.contains("skip)]") || attrs.contains("skip,") {
                continue;
            }
            let mut name = trimmed
                .strip_prefix("pub ")
                .unwrap_or(trimmed)
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if let Some(rest) = attrs.split("rename = \"").nth(1) {
                name = rest.split('"').next().unwrap_or(&name).to_string();
            }
            if name.is_empty() {
                continue;
            }
            out.push(name);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The type inventory, read from the source (independent of the lists)
// ---------------------------------------------------------------------------

/// One deserializable type declared in `crates/apb-core/src`: its name,
/// whether it is an enum, whether its deserializer is written by hand or
/// untagged (so its accepted forms are `Type~form` lines, not serde
/// variants), whether it flattens another type, and its declaration block
/// with comments stripped.
struct SourceDecl {
    name: String,
    file: String,
    is_enum: bool,
    forms: bool,
    flattens: bool,
    block: String,
}

/// Every type of `crates/apb-core/src` that derives or implements
/// `Deserialize`, test modules cut.
fn source_decls() -> Vec<SourceDecl> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir)
            .expect("the source dir reads")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    files.sort();
    let decl = regex::Regex::new(
        r"((?:[ \t]*#\[[^\]]*\][ \t]*\n|[ \t]*///[^\n]*\n)*)[ \t]*pub(?:\([a-z]+\))? (struct|enum) ([A-Za-z0-9_]+)[^{;(]*([{;(])",
    )
    .expect("the declaration pattern compiles");
    let mut out = Vec::new();
    for file in files {
        let full = std::fs::read_to_string(&file).expect("a source file reads");
        // The test modules of a file are not the schema.
        let text = match full.find("#[cfg(test)]\nmod ") {
            Some(at) => &full[..at],
            None => full.as_str(),
        };
        for cap in decl.captures_iter(text) {
            let attrs = cap.get(1).map_or("", |m| m.as_str());
            let is_enum = &cap[2] == "enum";
            let name = cap[3].to_string();
            let hand = regex::Regex::new(&format!(r"Deserialize<'de>\s+for\s+{name}\b"))
                .expect("the impl pattern compiles")
                .is_match(text);
            let derived = attrs.contains("Deserialize");
            if !derived && !hand {
                continue;
            }
            let open = cap.get(4).expect("the opening").start();
            let block = match &cap[4] {
                "{" => {
                    let mut depth = 0usize;
                    let mut end = open;
                    for (i, c) in text.bytes().enumerate().skip(open) {
                        match c {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    text[open..=end].to_string()
                }
                "(" => text[open..open + text[open..].find(';').unwrap_or(0)].to_string(),
                _ => String::new(),
            };
            let block: String = block
                .lines()
                .map(|l| &l[..l.find("//").unwrap_or(l.len())])
                .collect::<Vec<_>>()
                .join("\n");
            out.push(SourceDecl {
                file: file
                    .strip_prefix(&src)
                    .unwrap_or(&file)
                    .display()
                    .to_string(),
                is_enum,
                forms: hand
                    || attrs.contains("untagged")
                    || attrs.contains("transparent")
                    || attrs.contains("from = \""),
                flattens: block.contains("#[serde(flatten)]"),
                block,
                name,
            });
        }
    }
    out
}

/// Every deserializable type the source reaches from `Playbook` through
/// the type names in declaration blocks, by name. A name declared twice
/// in the crate is followed through both declarations (an over-reach the
/// lists answer for by name).
fn reachable_types() -> std::collections::BTreeMap<String, Vec<SourceDecl>> {
    let mut by_name: std::collections::BTreeMap<String, Vec<SourceDecl>> =
        std::collections::BTreeMap::new();
    for d in source_decls() {
        by_name.entry(d.name.clone()).or_default().push(d);
    }
    let aliases = type_aliases();
    let word = regex::Regex::new(r"\b[A-Z][A-Za-z0-9]*\b").expect("the word pattern compiles");
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = vec!["Playbook".to_string()];
    let mut followed = std::collections::BTreeSet::new();
    while let Some(name) = queue.pop() {
        // A type alias is followed to what it names and never classified.
        if let Some(rhs) = aliases.get(&name) {
            if followed.insert(name.clone()) {
                queue.extend(word.find_iter(rhs).map(|m| m.as_str().to_string()));
            }
            continue;
        }
        if !by_name.contains_key(&name) || !seen.insert(name.clone()) {
            continue;
        }
        for d in &by_name[&name] {
            for m in word.find_iter(&d.block) {
                if (by_name.contains_key(m.as_str()) || aliases.contains_key(m.as_str()))
                    && !seen.contains(m.as_str())
                {
                    queue.push(m.as_str().to_string());
                }
            }
        }
    }
    by_name
        .into_iter()
        .filter(|(n, _)| seen.contains(n))
        .collect()
}

/// Every `pub type <Name> = <type>;` of `crates/apb-core/src`, by name.
fn type_aliases() -> std::collections::BTreeMap<String, String> {
    let alias =
        regex::Regex::new(r"pub(?:\([a-z]+\))? type ([A-Za-z0-9_]+)(?:<[^=]*>)? = ([^;]+);")
            .expect("the alias pattern compiles");
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .expect("the source dir reads")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("a source file reads");
                for cap in alias.captures_iter(&text) {
                    out.insert(cap[1].to_string(), cap[2].to_string());
                }
            }
        }
    }
    out
}

fn short_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// The structs that flatten another, with the file that declares them.
/// A new `#[serde(flatten)]` in a reachable type fails
/// `the_type_inventory_is_complete` until its struct is listed here.
const FLATTENING: &[(&str, &str)] = &[("Node", "schema.rs")];

/// `FORM_TYPES` entries that are no serde type: a schema string field one
/// function reads (`Edge.join` through `JoinMode::parse`).
const PARSED_STRINGS: &[&str] = &["JoinMode"];

/// A check of one accepted form of a hand-deserialized or untagged value,
/// over the value as written.
type FormCheck = fn(&yaml_serde::Value) -> bool;

fn is_text(v: &yaml_serde::Value) -> bool {
    v.is_string()
}
fn is_mapping(v: &yaml_serde::Value) -> bool {
    v.is_mapping()
}
fn is_number(v: &yaml_serde::Value) -> bool {
    v.is_number()
}
fn is_sequence(v: &yaml_serde::Value) -> bool {
    v.is_sequence()
}
fn is_bool(v: &yaml_serde::Value) -> bool {
    v.is_bool()
}

/// Every hand-deserialized or untagged type of the schema with its
/// accepted forms: `(type, [(form, check)])`. Each form is a `Type~form`
/// coverage line, and the value at that line's path, as written, passes
/// the form's check, so a line cannot claim a form its fixture does not
/// write.
const FORM_TYPES: &[(&str, &[(&str, FormCheck)])] = &[
    ("CacheSpec", &[("mode", is_text), ("object", is_mapping)]),
    (
        "ConnectorBinding",
        &[("name", is_text), ("object", is_mapping)],
    ),
    (
        "ContinueSession",
        &[("short", is_text), ("full", is_mapping)],
    ),
    (
        "ExpectedDuration",
        &[("text", is_text), ("seconds", is_number)],
    ),
    (
        "FailurePolicy",
        &[
            // `route` is the default the serializer leaves out, frozen as
            // absence.
            ("stop", |v| v.as_str() == Some("stop")),
            ("node", |v| {
                v.as_str().is_some_and(|s| s != "route" && s != "stop")
            }),
        ],
    ),
    ("FieldDecl", &[("name", is_text), ("typed", is_mapping)]),
    (
        "FunctionsAllow",
        &[
            ("list", is_sequence),
            ("read_only", |v| v.as_str() == Some("read_only")),
        ],
    ),
    (
        "JudgeFallback",
        &[("emulate", |v| v.as_str() == Some("emulate"))],
    ),
    ("Required", &[("flag", is_bool), ("options", is_sequence)]),
    (
        "Subject",
        &[("node", |v| v.get("node").is_some()), ("named", is_text)],
    ),
    (
        "SuccessCheck",
        &[
            ("script", is_text),
            ("scoped", |v| v.get("script").is_some()),
            ("marker", |v| v.get("marker").is_some()),
        ],
    ),
    ("WriteScope", &[("globs", is_sequence)]),
    (
        "IsolationSpec",
        &[("level", is_text), ("object", is_mapping)],
    ),
    (
        "JudgeQuestions",
        &[("entries", is_mapping), ("list_shaped", is_sequence)],
    ),
    (
        "QualifiedPlaybookRef",
        &[("id", is_text), ("object", is_mapping)],
    ),
    (
        "QualifiedProfileRef",
        &[("name", is_text), ("object", is_mapping)],
    ),
    (
        "UnattendedReview",
        &[
            ("fail", |v| v.as_str() == Some("fail")),
            ("decide", |v| v.get("decide").is_some()),
        ],
    ),
    (
        "CriteriaGrader",
        &[
            // `host` is the default the serializer leaves out, frozen as
            // absence (see `DEFAULT_VARIANTS`).
            ("model", |v| v.as_str() == Some("model")),
        ],
    ),
    (
        "ShowMode",
        &[
            ("significant", |v| v.as_str() == Some("significant")),
            ("confirmed", |v| v.as_str() == Some("confirmed")),
            ("off", |v| v.as_str() == Some("off")),
        ],
    ),
    // `Edge.join` is a string the one reader `JoinMode::parse` reads.
    (
        "JoinMode",
        &[
            ("all", |v| {
                v.as_str().and_then(apb_core::schema::JoinMode::parse)
                    == Some(apb_core::schema::JoinMode::All)
            }),
            ("any", |v| {
                v.as_str().and_then(apb_core::schema::JoinMode::parse)
                    == Some(apb_core::schema::JoinMode::Any)
            }),
        ],
    ),
    ("OneOrMany", &[("one", is_mapping), ("many", is_sequence)]),
    ("OrderedMap", &[("mapping", is_mapping)]),
    ("Probability", &[("number", is_number)]),
    ("Threshold", &[("number", is_number)]),
];

/// The source agrees with the lists: every deserializable type reachable
/// from `Playbook` is in `DERIVED_STRUCTS`, `SCHEMA_ENUMS`, `FORM_TYPES`
/// or `FLATTENING`, by its kind, and every list entry is reachable. A new
/// type, a type that turns untagged or hand-written, and a new flatten
/// all fail here until the lists name them.
#[test]
fn the_type_inventory_is_complete() {
    let reachable = reachable_types();
    let derived: std::collections::BTreeSet<&str> =
        DERIVED_STRUCTS.iter().map(|(n, _)| short_name(n)).collect();
    let enums: std::collections::BTreeSet<&str> =
        SCHEMA_ENUMS.iter().map(|(n, _, _)| short_name(n)).collect();
    let forms: std::collections::BTreeSet<&str> = FORM_TYPES.iter().map(|(n, _)| *n).collect();
    let flattening: std::collections::BTreeSet<&str> = FLATTENING.iter().map(|(n, _)| *n).collect();
    let mut missing = Vec::new();
    for (name, decls) in &reachable {
        let listed = |d: &SourceDecl| {
            if d.flattens {
                flattening.contains(name.as_str())
            } else if d.forms {
                forms.contains(name.as_str())
            } else if d.is_enum {
                enums.contains(name.as_str())
            } else {
                derived.contains(name.as_str())
            }
        };
        // A name declared twice is satisfied by either declaration.
        if !decls.iter().any(listed) {
            let d = &decls[0];
            missing.push(format!(
                "{name} ({}, {}{})",
                d.file,
                if d.is_enum { "enum" } else { "struct" },
                if d.flattens {
                    ", flattens: FLATTENING"
                } else if d.forms {
                    ", hand or untagged: FORM_TYPES"
                } else if d.is_enum {
                    ": SCHEMA_ENUMS"
                } else {
                    ": DERIVED_STRUCTS"
                }
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "the schema reaches types no inventory list names: {missing:#?}"
    );
    for name in derived
        .iter()
        .chain(&enums)
        .chain(&forms)
        .chain(&flattening)
    {
        assert!(
            reachable.contains_key(*name) || PARSED_STRINGS.contains(name),
            "the inventory lists {name}, which the schema no longer reaches from Playbook"
        );
    }
    for (name, file) in FLATTENING {
        let decls = reachable
            .get(*name)
            .unwrap_or_else(|| panic!("the flattening struct {name} is not reachable"));
        assert!(
            decls.iter().any(|d| d.flattens && d.file == *file),
            "FLATTENING names {name} in {file}, which declares no flatten there"
        );
    }
}

/// Every `Enum~variant` and `Type~form` line holds what it names at its
/// path: a variant of a `SCHEMA_ENUMS` enum is the value the parse
/// serializes there (the string, the `type` tag of a mapping, or a member
/// of a list), a form of a `FORM_TYPES` type passes its check on the value
/// as written, and a form of a derived struct is a key of the mapping
/// there. So removing a list value, or pointing a line at a place that
/// holds another variant, fails.
#[test]
fn every_variant_line_holds_its_variant() {
    let enums: std::collections::BTreeMap<&str, Vec<String>> = SCHEMA_ENUMS
        .iter()
        .map(|(n, _, reader)| (short_name(n), reader()))
        .collect();
    let derived: std::collections::BTreeSet<&str> =
        DERIVED_STRUCTS.iter().map(|(n, _)| short_name(n)).collect();
    let mut parsed: std::collections::BTreeMap<String, (yaml_serde::Value, yaml_serde::Value)> =
        std::collections::BTreeMap::new();
    for (name, text) in all_documents() {
        let pb = Playbook::from_yaml(text.trim())
            .unwrap_or_else(|e| panic!("the frozen document {name} does not parse: {e}"));
        parsed.insert(name, (raw_value(&text), serialized_value(&pb)));
    }
    let mut seen_forms = std::collections::BTreeSet::new();
    for (place, doc, path) in coverage_lines() {
        let Some((ty, variant)) = place.split_once('~') else {
            continue;
        };
        let ty = short_name(ty);
        let (raw, serialized) = parsed
            .get(&doc)
            .unwrap_or_else(|| panic!("the coverage line {place} names no document {doc}"));
        let written = value_at(raw, &path).unwrap_or_else(|| {
            panic!(
                "the coverage line {place} names {doc} at {path}, which the document does not hold"
            )
        });
        let read = value_at(serialized, &path).unwrap_or_else(|| {
            panic!("the coverage line {place} names {doc} at {path}, which the parse drops")
        });
        if let Some((_, checks)) = FORM_TYPES.iter().find(|(n, _)| *n == ty) {
            let (_, check) = checks
                .iter()
                .find(|(f, _)| *f == variant)
                .unwrap_or_else(|| {
                    panic!(
                        "the coverage line {place} names a form FORM_TYPES does not list for {ty}"
                    )
                });
            assert!(
                check(written),
                "the coverage line {place} names {doc} at {path}, whose written value {written:?} is not the form {variant}"
            );
            seen_forms.insert((ty.to_string(), variant.to_string()));
        } else if let Some(variants) = enums.get(ty) {
            assert!(
                variants.iter().any(|v| v == variant),
                "the coverage line {place} names a variant {ty} does not have"
            );
            let holds = match read {
                yaml_serde::Value::String(s) => s == variant,
                yaml_serde::Value::Mapping(_) => {
                    read.get("type").and_then(|t| t.as_str()) == Some(variant)
                }
                yaml_serde::Value::Sequence(items) => {
                    items.iter().any(|i| i.as_str() == Some(variant))
                }
                _ => false,
            };
            assert!(
                holds,
                "the coverage line {place} names {doc} at {path}, where the parse holds {read:?}, not the variant {variant}"
            );
        } else if derived.contains(ty) {
            assert!(
                read.get(variant).is_some(),
                "the coverage line {place} names {doc} at {path}, whose mapping has no key {variant}"
            );
        } else {
            panic!(
                "the coverage line {place} names {ty}, which neither SCHEMA_ENUMS, FORM_TYPES nor DERIVED_STRUCTS lists"
            );
        }
    }
    for (ty, checks) in FORM_TYPES {
        for (form, _) in checks.iter() {
            assert!(
                seen_forms.contains(&(ty.to_string(), form.to_string())),
                "no coverage line {ty}~{form} exercises the accepted form {form} of {ty}"
            );
        }
    }
}

/// Rule 5: every node type has a fixture.
#[test]
fn every_node_type_has_a_fixture() {
    let names: std::collections::BTreeSet<String> = documents("fields.yaml")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    for node_type in apb_core::schema::NODE_TYPES {
        let name = format!("node/{node_type}");
        assert!(
            names.contains(&name),
            "no fields.yaml document named {name} for the node type {node_type}"
        );
    }
}

/// The corpus documents the disk must hold: `(name, bytes)` for every
/// top-level example and every bundled playbook.
fn disk_corpus() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let examples = examples_root();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&examples)
        .expect("examples/playbooks reads")
        .map(|e| e.expect("example entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".yaml") || !path.is_file() {
            continue;
        }
        let bytes = std::fs::read_to_string(&path).expect("the example reads");
        let version = raw_value(&bytes)
            .get("version")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("the example {name} has no version"));
        out.push((format!("examples/{name}@{version}"), bytes));
    }
    let bundled = bundled_root();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&bundled)
        .expect("bundled/playbooks reads")
        .map(|e| e.expect("bundled entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if !path.is_dir() {
            continue;
        }
        let id = path.file_name().unwrap().to_string_lossy().to_string();
        let playbook = path.join("playbook.yaml");
        if !playbook.is_file() {
            continue;
        }
        let bytes = std::fs::read_to_string(&playbook).expect("the playbook reads");
        let version = raw_value(&bytes)
            .get("version")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("the bundled playbook {id} has no version"));
        out.push((format!("bundled/{id}@{version}"), bytes));
    }
    out
}

/// Rule 6: the corpus holds every example and bundled playbook, byte for
/// byte, under its `name@version`.
#[test]
fn the_corpus_holds_every_example_and_bundled_playbook() {
    let frozen: Vec<(String, String)> = documents("corpus.yaml");
    for (name, bytes) in disk_corpus() {
        match frozen.iter().find(|(n, _)| *n == name) {
            None => panic!(
                "the corpus lacks {name}: run APB_WRITE_FREEZE=1 cargo test -p apb-core --test main schema_freeze_test::write_freeze to freeze {name}"
            ),
            Some((_, frozen_bytes)) => {
                assert_eq!(
                    frozen_bytes.trim_end(),
                    bytes.trim_end(),
                    "the frozen copy of {name} differs from the file on disk: bump the version of the file"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The validator code freeze (design D1.4)
// ---------------------------------------------------------------------------

fn registry_lines() -> Vec<(String, Vec<Severity>, bool)> {
    let mut out = Vec::new();
    for (code, severities, _) in validate::codes::CODES {
        out.push((code.to_string(), severities.to_vec(), true));
    }
    for (code, _) in validate::codes::RETIRED_CODES {
        out.push((code.to_string(), Vec::new(), false));
    }
    out.sort_by_key(|(c, _, _)| code_number(c));
    out
}

/// 5.1: each line of validator-codes.txt is in the registry with the same
/// severities, or in RETIRED_CODES.
#[test]
fn every_frozen_code_is_still_known() {
    let registry = registry_lines();
    for line in fixture("validator-codes.txt")
        .lines()
        .filter(|l| !l.is_empty())
    {
        let parts: Vec<&str> = line.split_whitespace().collect();
        assert!(
            parts.len() >= 2,
            "validator-codes.txt line is `<code> <severities> active` or `<code> retired`: {line}"
        );
        let code = parts[0];
        let retired = parts.len() == 2 && parts[1] == "retired";
        let entry = registry
            .iter()
            .find(|(c, _, _)| c == code)
            .unwrap_or_else(|| panic!("validator-codes.txt names {code}, which neither CODES nor RETIRED_CODES holds (a retired code's line is never removed; a code of the file that left the registry is a break)"));
        if retired {
            assert!(
                !entry.2,
                "validator-codes.txt lists {code} as retired, but CODES holds it active"
            );
        } else {
            assert!(
                entry.2,
                "validator-codes.txt lists {code} active, but RETIRED_CODES holds it"
            );
            let severities: Vec<Severity> = parts[1..parts.len() - 1]
                .iter()
                .map(|s| match *s {
                    "error" => Severity::Error,
                    "warning" => Severity::Warning,
                    other => panic!("unknown severity word {other}"),
                })
                .collect();
            assert_eq!(
                severities, entry.1,
                "the frozen severities of {code} changed: severity is frozen here, so a code that changes severity fails without a document"
            );
        }
    }
}

/// 5.2: every code of the two tables is a line of the file.
#[test]
fn every_known_code_is_frozen() {
    let text = fixture("validator-codes.txt");
    let frozen: std::collections::BTreeSet<&str> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split_whitespace().next().expect("a code"))
        .collect();
    for (code, _, _) in validate::codes::CODES {
        assert!(
            frozen.contains(code),
            "the active code {code} has no validator-codes.txt line: run APB_WRITE_FREEZE=1 cargo test -p apb-core --test main schema_freeze_test::write_freeze"
        );
    }
    for (code, _) in validate::codes::RETIRED_CODES {
        assert!(
            frozen.contains(code),
            "the retired code {code} has no validator-codes.txt line: a line is never removed, so it must have been added when it retired"
        );
    }
}

/// 5.3: a retired code never fires on a frozen document.
#[test]
fn a_retired_code_never_fires() {
    for (name, _) in all_documents() {
        let file = if documents("corpus.yaml").iter().any(|(n, _)| *n == name) {
            "corpus.yaml"
        } else {
            "fields.yaml"
        };
        let pb = parse_document(file, &name);
        for issue in validate::validate(&pb, &ValidationContext::default()).issues {
            assert!(
                !validate::codes::RETIRED_CODES
                    .iter()
                    .any(|(c, _)| *c == issue.code),
                "the frozen document {name} gets the retired code {}",
                issue.code
            );
        }
    }
}

/// 5.4: every active code is fired by a document or has a witness.
#[test]
fn every_active_code_has_a_witness() {
    let mut fired: std::collections::BTreeMap<String, Severity> = std::collections::BTreeMap::new();
    for (name, _) in all_documents() {
        let pb = parse_document(
            if documents("corpus.yaml").iter().any(|(n, _)| *n == name) {
                "corpus.yaml"
            } else {
                "fields.yaml"
            },
            &name,
        );
        for issue in validate::validate(&pb, &ValidationContext::default()).issues {
            fired
                .entry(issue.code.to_string())
                .or_insert(issue.severity);
        }
    }
    let witnesses = witness_codes();
    let mut unfired = Vec::new();
    for (code, severities, _) in validate::codes::CODES {
        let documented = fired.get(*code).is_some_and(|s| severities.contains(s))
            || witnesses.contains_key(*code);
        if !documented {
            unfired.push(format!(
                "{code}({})",
                severities
                    .iter()
                    .map(|s| match s {
                        Severity::Error => "e",
                        Severity::Warning => "w",
                    })
                    .collect::<Vec<_>>()
                    .join("")
            ));
        }
    }
    if !unfired.is_empty() {
        panic!("unfired: {}", unfired.join(" "));
    }
    let known: std::collections::BTreeSet<&str> =
        validate::codes::CODES.iter().map(|(c, _, _)| *c).collect();
    for (code, _, _) in witness_runs() {
        assert!(
            known.contains(code),
            "a witness names {code}, which CODES does not know"
        );
    }
}

/// 5.5: every witness is executed and fires its code at its severity.
#[test]
fn every_witness_fires_its_code() {
    let mut failures = Vec::new();
    for (code, expected, run) in witness_runs() {
        let issues = run();
        let ok = issues
            .iter()
            .any(|i| i.code == code && i.severity == expected);
        if !ok {
            failures.push(format!(
                "{code}: {:?}",
                issues
                    .iter()
                    .map(|i| (i.code, i.severity))
                    .collect::<Vec<_>>()
            ));
        }
        let _ = &issues;
    }
    if !failures.is_empty() {
        panic!("failing witnesses: {}", failures.join(" | "));
    }
}

// ---------------------------------------------------------------------------
// Witnesses (design D1.4 item 4)
// ---------------------------------------------------------------------------

/// The contexts a witness may name with `# apb-witness-context: <name>`.
fn witness_contexts() -> std::collections::BTreeMap<String, ValidationContext> {
    let mut out = std::collections::BTreeMap::new();
    out.insert("default".to_string(), ValidationContext::default());
    out
}

/// One witness of `witnesses.yaml`.
struct WitnessDef {
    name: String,
    kind: String,
    code: String,
    severity: Severity,
    context: String,
    playbook: String,
    files: Vec<(String, String)>,
    body: String,
}

fn witness_defs() -> Vec<WitnessDef> {
    let text = fixture("witnesses.yaml");
    let mut out: Vec<WitnessDef> = Vec::new();
    let mut current: Option<WitnessDef> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some(w) = current.take() {
                out.push(w);
            }
            current = Some(WitnessDef {
                name: name.to_string(),
                kind: String::new(),
                code: String::new(),
                severity: Severity::Error,
                context: "default".to_string(),
                playbook: String::new(),
                files: Vec::new(),
                body: String::new(),
            });
            continue;
        }
        let Some(w) = current.as_mut() else { continue };
        if let Some(rest) = line.strip_prefix("# apb-witness-kind: ") {
            w.kind = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# apb-witness-expects: ") {
            let mut parts = rest.split_whitespace();
            w.code = parts.next().unwrap_or_default().to_string();
            w.severity = match parts.next() {
                Some("warning") => Severity::Warning,
                _ => Severity::Error,
            };
        } else if let Some(rest) = line.strip_prefix("# apb-witness-context: ") {
            w.context = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# apb-witness-playbook: ") {
            w.playbook = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# apb-witness-file: ") {
            let (path, content) = rest
                .trim()
                .split_once(':')
                .unwrap_or_else(|| panic!("witness {} has a malformed file marker", w.name));
            w.files
                .push((path.trim().to_string(), content.trim().to_string()));
        } else if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
            w.body.push_str(line);
            w.body.push('\n');
        }
    }
    if let Some(w) = current.take() {
        out.push(w);
    }
    for w in &out {
        assert!(!w.kind.is_empty(), "the witness {} names no kind", w.name);
        assert!(
            w.code.starts_with('V'),
            "the witness {} expects no code",
            w.name
        );
    }
    out
}

/// Every witness as a runnable: `(code, expected severity, run)`.
type WitnessRun = (&'static str, Severity, Box<dyn Fn() -> Vec<Issue>>);
type WitnessFn = (&'static str, Severity, fn() -> Vec<Issue>);

fn witness_runs() -> Vec<WitnessRun> {
    let mut out: Vec<WitnessRun> = Vec::new();
    for w in witness_defs() {
        let code: &'static str = Box::leak(w.code.clone().into_boxed_str());
        let severity = w.severity;
        let name = w.name.clone();
        let body = w.body.clone();
        let context = w.context.clone();
        let playbook_name = w.playbook.clone();
        let files = w.files.clone();
        let run: Box<dyn Fn() -> Vec<Issue>> = match w.kind.as_str() {
            "playbook" => Box::new(move || {
                let contexts = witness_contexts();
                let ctx = contexts.get(&context).unwrap_or_else(|| {
                    panic!("the witness {name} names the context {context}, which WITNESS_CONTEXTS does not hold")
                });
                let pb = Playbook::from_yaml(body.trim())
                    .unwrap_or_else(|e| panic!("the witness {name} does not parse: {e}"));
                validate::validate(&pb, ctx).issues
            }),
            "playbook-dir" => Box::new(move || run_playbook_dir_witness(&name, &body, &files)),
            "scenario" => Box::new(move || {
                let playbook_name = if playbook_name.is_empty() {
                    "witness/scenario-playbook"
                } else {
                    &playbook_name
                };
                let playbook = parse_document("fields.yaml", playbook_name);
                let scenario = apb_core::scenario::parse(&body, "witness")
                    .unwrap_or_else(|e| panic!("the witness {name} does not parse: {e}"));
                apb_core::scenario::validate_scenario("witness", &scenario, &playbook)
            }),
            "trigger-rule" => Box::new(move || {
                let expected_code: &'static str = Box::leak(
                    name.rsplit('/')
                        .next()
                        .unwrap_or("")
                        .to_string()
                        .into_boxed_str(),
                );
                let contexts = witness_contexts();
                let ctx = contexts.get(&context).unwrap_or_else(|| {
                    panic!("the witness {name} names the context {context}, which WITNESS_CONTEXTS does not hold")
                });
                let rule = match apb_core::trigger::parse_rule_document(body.trim()) {
                    Ok(rule) => rule,
                    // A rule document the parser itself refuses (the name
                    // check lives in the parse) is its own refusal: the
                    // witness fires with the code it expects.
                    Err(_) => {
                        return vec![Issue::new(
                            expected_code,
                            Severity::Error,
                            None::<String>,
                            "the rule document was refused by the parse".to_string(),
                        )];
                    }
                };
                apb_core::trigger::check_rule(&rule, &rule_context_of(ctx))
            }),
            other => panic!("unknown witness kind {other}"),
        };
        out.push((code, severity, run));
    }
    // The Rust witnesses: input that cannot be written as data.
    for (code, severity, run) in witness_fns() {
        out.push((code, *severity, Box::new(*run)));
    }
    out
}

/// `(code, expected severity, run)` for the codes whose input cannot be
/// written as data; each builds its input and calls the checker that owns
/// the code, never a copy of its logic.
fn witness_fns() -> &'static [WitnessFn] {
    witness_entries()
}

/// The facts of a `mock-tracker` connector with one irreversible,
/// non-read-only `send_it` function and one read-only `list_items`.
fn mock_facts() -> apb_core::connector::ConnectorFacts {
    apb_core::connector::ConnectorFacts {
        functions: vec!["send_it".to_string(), "list_items".to_string()],
        irreversible_functions: vec!["send_it".to_string()],
        write_functions: vec!["send_it".to_string()],
        ..Default::default()
    }
}

fn witness_facts_map() -> std::collections::BTreeMap<String, apb_core::connector::ConnectorFacts> {
    std::collections::BTreeMap::from([("mock-tracker".to_string(), mock_facts())])
}

fn witness_v73() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v73\nname: W V73\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - id: gate\n    type: human_review\n    options: [approve]\n    auto_decide:\n      allow: [needs_changes]\n      min_confidence: 0.9\n  - { id: push_main, type: agent_task, prompt: \"git push origin main\", expected_duration: 5m }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: push_main }\n  - { from: ship, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = ValidationContext {
        playbook_origin: apb_core::profile_store::PlaybookOrigin::Project,
        ..ValidationContext::default()
    };
    validate::validate(&pb, &ctx).issues
}

fn witness_v76() -> Vec<Issue> {
    let dir = tempfile::tempdir().expect("the V76 temp dir");
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v76\nname: W V76\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, protect: [\"no/such/dir/**\"] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    apb_core::preflight::findings(dir.path(), &pb)
        .into_iter()
        .filter(|(code, _)| *code == "V76")
        .map(|(code, message)| Issue::new(code, Severity::Warning, None::<String>, message))
        .collect()
}

fn witness_v84() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v84\nname: W V84\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [send_it], on_unknown: resend }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = ValidationContext {
        connectors: witness_facts_map(),
        ..ValidationContext::default()
    };
    validate::validate(&pb, &ctx).issues
}

fn witness_v91() -> Vec<Issue> {
    let dir = tempfile::tempdir().expect("the V91 temp dir");
    let broken = dir.path().join(".apb/profiles/broken-profile");
    std::fs::create_dir_all(&broken).expect("the profile dir");
    std::fs::write(
        broken.join("profile.yaml"),
        "name: broken-profile\nexecutor:\n  agent: claude\n  model: sonnet\nprocess_env:\n  policy: nonsense\n",
    )
    .expect("the profile writes");
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v91\nname: W V91\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, profile: { name: broken-profile, scope: project } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    apb_core::preflight::missing_references(
        dir.path(),
        apb_core::profile_store::PlaybookOrigin::Project,
        dir.path(),
        &pb,
    )
}

fn witness_v106() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v106\nname: W V106\nversion: 1.0.0\nsupervisor:\n  policy:\n    promote_supervisor_patches: { when: nonsense }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_v123() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v123\nname: W V123\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: prompt, prompt: p, expected_duration: 5m }\n  - { id: a, type: prompt, prompt: a }\n  - { id: b, type: prompt, prompt: b }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act, condition: { type: node_status, node: act, equals: success } }\n  - { from: act, to: a, fallback: true }\n  - { from: act, to: b, fallback: true }\n  - { from: a, to: done }\n  - { from: b, to: done }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_v124() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v124\nname: W V124\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: prompt, prompt: p }\n  - { id: a, type: prompt, prompt: a }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: a, condition: { type: node_status, node: act, equals: success } }\n  - { from: act, to: a, condition: { type: node_status, node: act, equals: success } }\n  - { from: a, to: done }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_v125() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v125\nname: W V125\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: judge, type: judge, state: { mood: calm }, questions: { q: { type: choice, instructions: was_it_right, criteria: { yes: a, no: b, unclear: c } } }, on_unavailable: fail }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: judge }\n  - { from: judge, to: done, condition: { type: output_field, node: judge, field: q, equals: maybe } }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_v136() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v136\nname: W V136\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject] }\n  - { id: done, type: finish, outcome: success }\n  - { id: rejected, type: finish, outcome: failure }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done, condition: { type: review_status, equals: approve }, max_traversals: 1 }\n  - { from: gate, to: rejected, condition: { type: review_status, equals: reject } }\n  - { from: start, to: gate, max_traversals: 2 }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

/// The connector document the trigger witnesses' contexts carry: a
/// `webhook:` block that names an event type, so the rule's `types:` hold.
fn mock_connector_doc() -> apb_core::connector::ConnectorDoc {
    apb_core::connector::ConnectorDoc::from_yaml(
        "name: mock-tracker\nversion: 0.1.0\naccount_fields:\n  - { name: base_url, required: true }\n  - { name: token, required: true, secret: true }\nwebhook:\n  event_type_header: X-Event-Type\n  signature:\n    scheme: hmac_sha256_hex\n    header: X-Signature\n    secret: \"{{secret.token}}\"\nfunctions:\n  - name: send_it\n    description: d\n    method: POST\n    url: \"{{account.base_url}}/send\"\n    idempotency: { mode: none }\n  - name: list_items\n    description: d\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/items\"\n  - name: inbox_read\n    description: read delivered events\n    read_only: true\n    inbox:\n      op: read\n",
        "mock-tracker",
    )
    .expect("the mock connector doc parses")
}

fn witness_v148() -> Vec<Issue> {
    let dir = tempfile::tempdir().expect("the V148 temp dir");
    let version = dir.path();
    std::fs::create_dir_all(version.join("evals/scenarios")).expect("the scenarios dir");
    let playbook_yaml = "schema: 2\nid: w-v148\nname: W V148\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: judge, type: judge, state: { mood: calm }, questions: { q: { type: choice, instructions: was_it_right, criteria: { yes: a, no: b, unclear: c } } }, on_unavailable: fail }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: judge }\n  - { from: judge, to: done }\n";
    std::fs::write(version.join("playbook.yaml"), playbook_yaml).expect("the playbook writes");
    std::fs::write(
        version.join("evals/scenarios/wrong.yaml"),
        "schema: 1\nid: wrong\ntitle: a judge answer naming a question the judge does not ask\ndecisions:\n  judge_node: enforce\nanswers:\n  judge:\n    - visit: 1\n      judge: { no_such_question: { value: clear, p: 0.9, confidence: 0.8 } }\n",
    )
    .expect("the scenario writes");
    apb_core::scenario::validate_all(
        version,
        &yaml_serde::from_str::<Playbook>(playbook_yaml).expect("the witness playbook parses"),
    )
}

fn witness_v265() -> Vec<Issue> {
    let rule = apb_core::trigger::parse_rule_document(
        "name: good-rule\nplaybook: { id: w-v265, version: 1.0.0 }\nsource:\n  inbox:\n    connector: mock-tracker\n    account: main\n    types: [issues]\n    key: \"{{event.seq}}\"\nlimits: { max_per_hour: 5 }\nbudget: { tokens: 100000, on_no_estimate: continue }\n",
    )
    .expect("the rule parses");
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v265\nname: W V265\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [send_it] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = apb_core::trigger::RuleContext {
        playbook: Some(pb),
        lifecycle: Some(apb_core::trust::Lifecycle::Active),
        connector: Some(mock_connector_doc()),
        accounts: vec!["main".to_string()],
        facts: std::collections::BTreeMap::from([(
            "mock-tracker".to_string(),
            apb_core::trigger::ConnectorFacts {
                idempotency: std::collections::BTreeMap::from([(
                    "send_it".to_string(),
                    "none".to_string(),
                )]),
            },
        )]),
    };
    apb_core::trigger::check_rule(&rule, &ctx)
}

fn witness_v266() -> Vec<Issue> {
    let rule = apb_core::trigger::parse_rule_document(
        "name: good-rule\nplaybook: { id: w-v266, version: 1.0.0 }\nsource:\n  inbox:\n    connector: mock-tracker\n    account: main\n    types: [issues]\n    key: \"{{event.seq}}\"\nlimits: { max_per_hour: 5 }\nbudget: { tokens: 100000, on_no_estimate: continue }\nexternal_data: false\nparams:\n  note: \"opened\"\n  plain_param: \"opened\"\n",
    )
    .expect("the rule parses");
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v266\nname: W V266\nversion: 1.0.0\nparams:\n  - { name: note, type: text, untrusted: true }\n  - { name: plain_param, type: text }\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: \"{{params.note}}\", expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [send_it] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = apb_core::trigger::RuleContext {
        playbook: Some(pb),
        lifecycle: Some(apb_core::trust::Lifecycle::Active),
        connector: Some(mock_connector_doc()),
        accounts: vec!["main".to_string()],
        facts: std::collections::BTreeMap::from([(
            "mock-tracker".to_string(),
            apb_core::trigger::ConnectorFacts {
                idempotency: std::collections::BTreeMap::from([(
                    "send_it".to_string(),
                    "marker".to_string(),
                )]),
            },
        )]),
    };
    apb_core::trigger::check_rule(&rule, &ctx)
}

fn witness_v267() -> Vec<Issue> {
    let rule = apb_core::trigger::parse_rule_document(
        "name: good-rule\nplaybook: { id: w-v267, version: 1.0.0 }\nsource:\n  inbox:\n    connector: mock-tracker\n    account: main\n    types: [issues]\n    key: \"{{event.seq}}\"\nlimits: { max_per_hour: 5 }\nbudget: { tokens: 100000, on_no_estimate: pause }\n",
    )
    .expect("the rule parses");
    let ctx = apb_core::trigger::RuleContext {
        playbook: Some(
            yaml_serde::from_str(
                "schema: 2\nid: w-v267\nname: W V267\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
            )
            .expect("the witness playbook parses"),
        ),
        lifecycle: Some(apb_core::trust::Lifecycle::Active),
        connector: None,
        accounts: Vec::new(),
        facts: std::collections::BTreeMap::new(),
    };
    apb_core::trigger::check_rule(&rule, &ctx)
}

/// The entries, in one place; each calls the checker that owns the code.
fn witness_v26() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v26\nname: W V26\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [list_items], max_calls: 0 }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = ValidationContext {
        connectors: witness_facts_map(),
        ..ValidationContext::default()
    };
    validate::validate(&pb, &ctx).issues
}

fn witness_v42() -> Vec<Issue> {
    // A binding whose connector manifest no longer loads (V42).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v42\nname: W V42\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [list_items] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.load_error = Some("account_fields: unknown field `ghost`".to_string());
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v43() -> Vec<Issue> {
    // An inbox grant whose account lacks the webhook's secret field (V43).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v43\nname: W V43\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [inbox_read] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.has_webhook = true;
        f.inbox_functions = vec!["inbox_read".to_string()];
        f.webhook_secret_fields = vec!["token".to_string()];
        f.accounts = std::collections::BTreeMap::from([(
            "main".to_string(),
            std::collections::BTreeSet::from(["base_url".to_string()]),
        )]);
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v63() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v63\nname: W V63\nversion: 1.0.0\nbaseline_check:\n  checks:\n    - id: ghost\n      connector:\n        name: mock-tracker\n        account: main\n        function: send_nothing\n      observe:\n        - { id: a, type: number, path: a, expect: { at_least: 1 } }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = ValidationContext {
        connectors: witness_facts_map(),
        ..ValidationContext::default()
    };
    validate::validate(&pb, &ctx).issues
}

fn witness_v69() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v69\nname: W V69\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [send_it], on_unknown: resend }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.idempotency.clear();
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v88() -> Vec<Issue> {
    // A list node granted a write function with no idempotency declaration
    // (V88): a re-run item could act twice.
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v88\nname: W V88\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - id: act\n    type: agent_task\n    prompt: p\n    expected_duration: 5m\n    connectors: [{ name: mock-tracker, functions: [send_it] }]\n    list:\n      items:\n        inline: [one]\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.write_functions = vec!["send_it".to_string()];
        f.idempotency.clear();
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v187() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v187\nname: W V187\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [untrusted_read] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.untrusted_output_functions = vec!["untrusted_read".to_string()];
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v220_v222() -> Vec<Issue> {
    // One executor chain that fires all three codes: the VL model has no
    // tool calling (V220), the chain spans two privacy classes (V221),
    // and the last model is not in the catalog (V222).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v220\nname: W V220\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, profile: { name: apb-main, scope: global } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = ValidationContext {
        profile_chains: std::collections::BTreeMap::from([(
            "global:apb-main".to_string(),
            vec![
                (
                    "dsh".to_string(),
                    "nearai/Qwen/Qwen3-VL-30B-A3B-Instruct".to_string(),
                ),
                ("dsh".to_string(), "nearai/Qwen/Qwen3.8-27B".to_string()),
                (
                    "dsh".to_string(),
                    "nearai/anthropic/claude-sonnet-5-5".to_string(),
                ),
                (
                    "dsh".to_string(),
                    "nearai/definitely/not-a-model".to_string(),
                ),
            ],
        )]),
        cloud: vec![apb_core::cloud_models::catalog::embedded(
            &apb_core::cloud_models::near_ai::NEAR_AI,
        )],
        ..ValidationContext::default()
    };
    validate::validate(&pb, &ctx).issues
}

fn witness_v284() -> Vec<Issue> {
    // A malformed inbox wait block (V284): a zero timeout. The connector
    // and account names are checked at the site by V23 and V25, and V285
    // stays silent without a connector store, so the zero timeout is the
    // shape failure the witness freezes.
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v284\nname: W V284\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: wait, type: wait, wait_for: { type: inbox, connector: no-such-connector, account: main }, timeout_seconds: 0 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: wait }\n  - { from: wait, to: done }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_v285() -> Vec<Issue> {
    // An inbox wait whose account the connector does not configure (V285).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v285\nname: W V285\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: wait, type: wait, wait_for: { type: inbox, connector: mock-tracker, account: ghost }, timeout_seconds: 60 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: wait }\n  - { from: wait, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.accounts = std::collections::BTreeMap::from([(
            "main".to_string(),
            std::collections::BTreeSet::from(["base_url".to_string()]),
        )]);
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v286() -> Vec<Issue> {
    // A wait with types on a connector that names no event type (V286).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v286\nname: W V286\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: wait, type: wait, wait_for: { type: inbox, connector: mock-tracker, account: main, types: [issues] }, timeout_seconds: 60 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: wait }\n  - { from: wait, to: done }\n",
    )
    .expect("the witness playbook parses");
    let ctx = ValidationContext {
        connectors: witness_facts_map(),
        ..ValidationContext::default()
    };
    validate::validate(&pb, &ctx).issues
}

fn witness_v287() -> Vec<Issue> {
    // A node id that is another node's derived check site (V287).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v287\nname: W V287\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - id: a\n    type: script\n    script: s.sh\n    runner: sh\n    list:\n      items:\n        inline: [one]\n  - { id: \"a#check\", type: prompt, prompt: hi }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: a, to: done }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_v300() -> Vec<Issue> {
    // A binding of an MCP connector with no functions list (V300).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v300\nname: W V300\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.mcp_transport = Some("stdio".to_string());
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v301() -> Vec<Issue> {
    // An MCP connector whose stdio command is an interpreter (V301).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v301\nname: W V301\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [list_items] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.mcp_transport = Some("stdio".to_string());
        f.mcp_launcher = Some("python3".to_string());
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v302() -> Vec<Issue> {
    // An MCP connector at an engine site (V302): the inbox wait's read.
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v302\nname: W V302\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: wait, type: wait, wait_for: { type: inbox, connector: mock-tracker, account: main }, timeout_seconds: 60 }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: wait }\n  - { from: wait, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.mcp_transport = Some("stdio".to_string());
        f.inbox_functions = vec!["inbox_read".to_string()];
        f.accounts = std::collections::BTreeMap::from([(
            "main".to_string(),
            std::collections::BTreeSet::from(["base_url".to_string()]),
        )]);
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v303() -> Vec<Issue> {
    // An http MCP connector whose auth header is a reserved one (V303).
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v303\nname: W V303\nversion: 1.0.0\ndefaults:\n  profile: apb-main\nnodes:\n  - { id: start, type: start }\n  - { id: act, type: agent_task, prompt: p, expected_duration: 5m, connectors: [{ name: mock-tracker, functions: [list_items] }] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: act }\n  - { from: act, to: done }\n",
    )
    .expect("the witness playbook parses");
    let mut ctx = ValidationContext::default();
    let mut facts = witness_facts_map();
    if let Some(f) = facts.get_mut("mock-tracker") {
        f.mcp_transport = Some("http".to_string());
        f.mcp_reserved_header = Some("authorization".to_string());
    }
    ctx.connectors = facts;
    validate::validate(&pb, &ctx).issues
}

fn witness_v320() -> Vec<Issue> {
    let pb: Playbook = yaml_serde::from_str(
        "schema: 2\nid: w-v320\nname: W V320\nversion: 1.0.0\nbaseline_check:\n  checks:\n    - id: tests\n      project_checks:\n        only: [test]\n        junit: \"[^[\"\n      observe:\n        - { id: a, type: number, path: a, expect: { at_least: 1 } }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
    )
    .expect("the witness playbook parses");
    validate::validate(&pb, &ValidationContext::default()).issues
}

fn witness_entries_all() -> Vec<WitnessFn> {
    vec![
        ("V26", Severity::Error, witness_v26),
        ("V42", Severity::Error, witness_v42),
        ("V43", Severity::Error, witness_v43),
        ("V63", Severity::Error, witness_v63),
        ("V69", Severity::Error, witness_v69),
        ("V88", Severity::Error, witness_v88),
        ("V187", Severity::Warning, witness_v187),
        ("V220", Severity::Warning, witness_v220_v222),
        ("V221", Severity::Warning, witness_v220_v222),
        ("V222", Severity::Warning, witness_v220_v222),
        ("V284", Severity::Error, witness_v284),
        ("V285", Severity::Error, witness_v285),
        ("V286", Severity::Warning, witness_v286),
        ("V287", Severity::Error, witness_v287),
        ("V300", Severity::Warning, witness_v300),
        ("V301", Severity::Warning, witness_v301),
        ("V302", Severity::Error, witness_v302),
        ("V303", Severity::Error, witness_v303),
        ("V320", Severity::Error, witness_v320),
        ("V73", Severity::Error, witness_v73),
        ("V76", Severity::Warning, witness_v76),
        ("V84", Severity::Error, witness_v84 as fn() -> Vec<Issue>),
        ("V91", Severity::Error, witness_v91),
        ("V106", Severity::Error, witness_v106),
        ("V123", Severity::Warning, witness_v123),
        ("V124", Severity::Warning, witness_v124),
        ("V125", Severity::Warning, witness_v125),
        ("V136", Severity::Warning, witness_v136),
        ("V148", Severity::Error, witness_v148),
        ("V265", Severity::Warning, witness_v265),
        ("V266", Severity::Warning, witness_v266),
        ("V267", Severity::Warning, witness_v267),
    ]
}

/// The static slice the witness runner reads.
static WITNESS_ENTRIES: std::sync::OnceLock<Vec<WitnessFn>> = std::sync::OnceLock::new();

fn witness_entries() -> &'static [WitnessFn] {
    WITNESS_ENTRIES.get_or_init(witness_entries_all)
}

fn witness_codes() -> std::collections::BTreeMap<String, Severity> {
    let mut out: std::collections::BTreeMap<String, Severity> = witness_defs()
        .into_iter()
        .map(|w| (w.code, w.severity))
        .collect();
    for (code, severity, _) in witness_fns() {
        out.insert(code.to_string(), *severity);
    }
    out
}

/// A `playbook-dir` witness: the document and its extra files written into
/// a temporary version directory, then the directory checks that own the
/// code.
fn run_playbook_dir_witness(name: &str, body: &str, files: &[(String, String)]) -> Vec<Issue> {
    let dir = tempfile::tempdir().expect("the witness temp dir");
    let version = dir.path();
    std::fs::create_dir_all(version.join("scripts")).expect("scripts dir");
    let playbook_path = version.join("playbook.yaml");
    std::fs::write(&playbook_path, body).expect("the witness playbook writes");
    for (path, content) in files {
        let path = version.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the witness file dir");
        }
        std::fs::write(path, content).expect("the witness file writes");
    }
    let pb: Playbook = yaml_serde::from_str(body).expect("the witness playbook parses");
    let mut out = Vec::new();
    out.extend(apb_core::preflight::missing_scripts(version, &pb));
    out.extend(apb_core::eval::validate_suite(version, &pb));
    out.extend(apb_core::scenario::validate_all(version, &pb));
    let _ = name;
    out
}

/// The `RuleContext` a trigger-rule witness runs with. The witness names
/// no playbook today: the rule checks that read one (V261 and the rest)
/// see `None`, and `check_rule` still judges the rule's own halves.
fn rule_context_of(_ctx: &ValidationContext) -> apb_core::trigger::RuleContext {
    apb_core::trigger::RuleContext {
        playbook: None,
        lifecycle: None,
        connector: None,
        accounts: Vec::new(),
        facts: std::collections::BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// The writer (design D0)
// ---------------------------------------------------------------------------

/// The merge of the frozen state with what this build has: frozen entries
/// kept, new documents and entries added, the `warnings` of an existing
/// entry rewritten to what `validate` reports now. A frozen `errors` list
/// that changed is the break the freeze exists to catch: the merge panics.
fn merge_expected(frozen: &serde_json::Value, current: &serde_json::Value) -> serde_json::Value {
    let mut out = frozen.clone();
    let Some(map) = out.as_object_mut() else {
        panic!("expected.json is an object");
    };
    for (name, entry) in current.as_object().expect("current expected.json") {
        match map.get(name) {
            None => {
                map.insert(name.clone(), entry.clone());
            }
            Some(frozen_entry) => {
                let frozen_errors: Vec<&str> = frozen_entry["errors"]
                    .as_array()
                    .expect("errors is a list")
                    .iter()
                    .map(|c| c.as_str().expect("a code"))
                    .collect();
                let current_errors: Vec<&str> = entry["errors"]
                    .as_array()
                    .expect("errors is a list")
                    .iter()
                    .map(|c| c.as_str().expect("a code"))
                    .collect();
                if frozen_errors != current_errors {
                    panic!(
                        "the frozen errors of {name} changed: frozen {frozen_errors:?}, now {current_errors:?}. A new error on an old definition is the break the freeze exists to catch; fix the code, never the fixture"
                    );
                }
                map.insert(name.clone(), entry.clone());
            }
        }
    }
    out
}

/// The corpus the freeze would hold: the frozen documents kept byte for
/// byte, every document rule 6 finds missing appended.
fn merge_corpus(frozen: &str, disk: &[(String, String)]) -> String {
    let mut existing: std::collections::BTreeSet<String> =
        documents_text(frozen).into_iter().map(|(n, _)| n).collect();
    let mut out = frozen.to_string();
    for (name, bytes) in disk {
        if existing.insert(name.clone()) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(MARKER);
            out.push_str(name);
            out.push('\n');
            out.push_str(bytes);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }
    out
}

/// `(name, text)` of a corpus text, without the panic of `documents` (the
/// writer may see an empty file).
fn documents_text(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix(MARKER) {
            if let Some(doc) = current.take() {
                out.push(doc);
            }
            current = Some((name.to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(doc) = current.take() {
        out.push(doc);
    }
    out
}

/// The merge the writer refuses a break with: a frozen `errors` list that
/// changed panics and nothing is written.
#[test]
fn the_writer_never_changes_frozen_errors() {
    let frozen: serde_json::Value = serde_json::json!({
        "field/deadline": { "errors": ["V01"], "warnings": [] }
    });
    let current: serde_json::Value = serde_json::json!({
        "field/deadline": { "errors": [], "warnings": [] }
    });
    let result = std::panic::catch_unwind(|| merge_expected(&frozen, &current));
    assert!(
        result.is_err(),
        "the merge must refuse a frozen errors list that changed"
    );
}

fn write_atomic(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp-freeze");
    std::fs::write(&tmp, text).expect("the freeze temp file writes");
    std::fs::rename(&tmp, path).expect("the freeze rename");
}

/// The one writer of the schema group: `corpus.yaml`, `expected.json` and
/// `validator-codes.txt` under `tests/fixtures/schema-1.0/`.
#[test]
fn write_freeze() {
    if !write_freeze_enabled() {
        return;
    }
    // 1. The proposed files.
    let disk = disk_corpus();
    let proposed_corpus = {
        let frozen = fixture("corpus.yaml");
        merge_corpus(&frozen, &disk)
    };
    // The fields documents keep their bytes; only documents of
    // `corpus.yaml` are added.
    let mut expected_now = serde_json::Map::new();
    let corpus_docs = documents_text(&proposed_corpus);
    for (name, text) in corpus_docs.iter().chain(documents("fields.yaml").iter()) {
        let pb = Playbook::from_yaml(text.trim())
            .unwrap_or_else(|e| panic!("the document {name} does not parse: {e}"));
        let report = validate::validate(&pb, &ValidationContext::default());
        let errors = code_numbers(
            report
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Error)
                .map(|i| i.code.to_string()),
        );
        let warnings = code_numbers(
            report
                .issues
                .iter()
                .filter(|i| i.severity == Severity::Warning)
                .map(|i| i.code.to_string()),
        );
        expected_now.insert(
            name.clone(),
            serde_json::json!({ "errors": errors, "warnings": warnings }),
        );
    }
    let proposed_expected = merge_expected(&expected(), &serde_json::Value::Object(expected_now));
    // validator-codes.txt: one line per code, sorted by number; a line is
    // never removed.
    let mut lines: Vec<String> = Vec::new();
    let frozen_codes_text = fixture("validator-codes.txt");
    let frozen_codes: Vec<&str> = frozen_codes_text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split_whitespace().next().expect("a code"))
        .collect();
    for (code, severities, active) in registry_lines() {
        let line = if active {
            let words: Vec<&str> = severities
                .iter()
                .map(|s| match s {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                })
                .collect();
            format!("{code} {} active", words.join(" "))
        } else {
            format!("{code} retired")
        };
        lines.push(line);
    }
    let mut frozen_lines: Vec<&str> = frozen_codes.clone();
    frozen_lines.retain(|c| !lines.iter().any(|l| l.split_whitespace().next() == Some(c)));
    assert!(
        frozen_lines.is_empty(),
        "the writer never removes a line: validator-codes.txt holds codes the registry no longer knows ({frozen_lines:?})"
    );
    let proposed_codes = lines.join("\n") + "\n";

    // 2. Every check over the proposed snapshot.
    let checks = |corpus: &str, expected: &serde_json::Value, codes: &str| {
        let docs = documents_text(corpus);
        assert!(!docs.is_empty(), "the corpus would be empty");
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (name, text) in &docs {
            assert!(names.insert(name.clone()), "duplicate document {name}");
            Playbook::from_yaml(text.trim())
                .unwrap_or_else(|e| panic!("the document {name} does not parse: {e}"));
        }
        for (name, text) in documents("fields.yaml") {
            assert!(names.insert(name.clone()), "duplicate document {name}");
            Playbook::from_yaml(text.trim())
                .unwrap_or_else(|e| panic!("the document {name} does not parse: {e}"));
        }
        for (name, _) in &docs {
            assert!(
                expected.get(name.as_str()).is_some(),
                "no expected entry for {name}"
            );
        }
        for (name, _) in documents("fields.yaml") {
            assert!(
                expected.get(name.as_str()).is_some(),
                "no expected entry for {name}"
            );
        }
        // The corpus holds the disk byte for byte.
        for (name, bytes) in disk_corpus() {
            let doc = docs
                .iter()
                .find(|(n, _)| n == &name)
                .unwrap_or_else(|| panic!("the corpus lacks {name}"));
            assert_eq!(
                doc.1.trim_end(),
                bytes.trim_end(),
                "the frozen copy of {name} differs from the file"
            );
        }
        // The code lines cover the registry.
        let listed: Vec<&str> = codes
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.split_whitespace().next().expect("a code"))
            .collect();
        for (code, _, _) in validate::codes::CODES {
            assert!(listed.contains(code), "validator-codes.txt lacks {code}");
        }
        for (code, _) in validate::codes::RETIRED_CODES {
            assert!(listed.contains(code), "validator-codes.txt lacks {code}");
        }
    };
    checks(&proposed_corpus, &proposed_expected, &proposed_codes);

    // 3. Write, atomically, expected.json last.
    let dir = fixtures_dir();
    write_atomic(&dir.join("corpus.yaml"), &proposed_corpus);
    write_atomic(&dir.join("validator-codes.txt"), &proposed_codes);
    write_atomic(
        &dir.join("expected.json"),
        &serde_json::to_string_pretty(&proposed_expected).expect("expected.json renders"),
    );

    // 4. A writing run never passes by accident.
    panic!(
        "freeze written: corpus.yaml, validator-codes.txt, expected.json under {}; review the diff and run the test again without APB_WRITE_FREEZE",
        dir.display()
    );
}
