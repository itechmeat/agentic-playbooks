//! The run outputs a playbook declares (`outputs:` at the playbook level):
//! named values a finished run hands to whoever started it, each projected
//! from the journal by the engine (`apb_engine::run_outputs`), never
//! journaled.
//!
//! ```yaml
//! outputs:
//!   summary:  { from: { node: report } }
//!   pr_url:   { from: [ { node: publish_a, field: url },
//!                       { node: publish_b, field: url } ] }
//!   verdicts: { from: { node: review, field: verdict }, pick: all }
//!   checked:  { from: { node: files, item_field: status } }
//! ```
//!
//! A source is structured, never parsed from a dotted string, because a
//! node id may contain dots. Additive to schema 2; an older apb drops the
//! block and projects nothing.

use serde::{Deserialize, Serialize};

use crate::judge::OrderedMap;

/// The most outputs one playbook declares (V131).
pub const MAX_OUTPUTS: usize = 64;

/// The longest output name (V131).
pub const MAX_NAME_LEN: usize = 64;

/// The declared outputs, in declaration order.
pub type RunOutputsDecl = OrderedMap<RunOutput>;

/// One declared output. Unknown keys are refused (like the schema's other
/// closed blocks), so a misspelt `pick` is an error, not the default.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutput {
    /// Where the value comes from: one source, or several (alternative
    /// branches), all read the same way.
    pub from: OneOrMany<OutputSource>,
    /// Which candidate the output takes (default `last`).
    #[serde(default, skip_serializing_if = "Pick::is_last")]
    pub pick: Pick,
}

/// One value (a mapping), or a list of them. Read by shape, so the error
/// of the one form it has (an unknown key) reaches the author, not a
/// generic "no variant matched".
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OneOrMany<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
        struct Shape<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Shape<T> {
            type Value = OneOrMany<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("one source or a list of them")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                seq: A,
            ) -> Result<Self::Value, A::Error> {
                Vec::deserialize(SeqAccessDeserializer::new(seq)).map(OneOrMany::Many)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(OneOrMany::One)
            }
        }
        d.deserialize_any(Shape(std::marker::PhantomData))
    }
}

impl<T> OneOrMany<T> {
    pub fn as_slice(&self) -> &[T] {
        match self {
            OneOrMany::One(t) => std::slice::from_ref(t),
            OneOrMany::Many(v) => v,
        }
    }
}

/// A node whose succeeded executions give the output's candidates. Unknown
/// keys are refused: a misspelt `field` would otherwise project the whole
/// output text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSource {
    pub node: String,
    /// One key of the node's output read as a JSON object; absent: the
    /// whole output text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// List nodes only: one key of every item's output, item by item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_field: Option<String>,
}

/// Which candidate an output takes, by settlement order in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    /// The candidate settled last across every source.
    #[default]
    Last,
    /// The candidate settled first.
    First,
    /// Every candidate, in settlement order.
    All,
}

impl Pick {
    pub fn is_last(&self) -> bool {
        *self == Pick::Last
    }
}

/// What a `type: playbook` node hands its parent (`output:` on the node).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildOutput {
    /// The child's answer text (the behaviour before run outputs).
    #[default]
    Answer,
    /// A compact JSON object of the child's projected run outputs.
    Outputs,
}

impl ChildOutput {
    pub fn is_answer(&self) -> bool {
        *self == ChildOutput::Answer
    }
}

/// Whether `name` is a valid output name: `[a-z][a-z0-9_]*`, at most
/// [`MAX_NAME_LEN`] characters.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_source_or_many_and_the_default_pick_load() {
        let decl: RunOutputsDecl = yaml_serde::from_str(
            "summary: { from: { node: report } }\n\
             url: { from: [ { node: a, field: url }, { node: b.c, field: url } ], pick: first }\n",
        )
        .unwrap();
        let names: Vec<&str> = decl.keys().collect();
        assert_eq!(names, ["summary", "url"]);
        let summary = decl.get("summary").unwrap();
        assert_eq!(summary.pick, Pick::Last);
        assert_eq!(summary.from.as_slice()[0].node, "report");
        let url = decl.get("url").unwrap();
        assert_eq!(url.pick, Pick::First);
        assert_eq!(url.from.as_slice()[1].node, "b.c");
        assert_eq!(url.from.as_slice()[1].field.as_deref(), Some("url"));
    }

    #[test]
    fn an_unknown_pick_does_not_load() {
        let r: Result<RunOutputsDecl, _> =
            yaml_serde::from_str("x: { from: { node: a }, pick: middle }\n");
        assert!(r.is_err());
    }

    #[test]
    fn names_follow_the_grammar() {
        assert!(is_valid_name("pr_url2"));
        assert!(!is_valid_name("Pr"));
        assert!(!is_valid_name("2x"));
        assert!(!is_valid_name("a-b"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name(&"a".repeat(65)));
        assert!(is_valid_name(&"a".repeat(64)));
    }
}
