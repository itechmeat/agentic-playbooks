//! List mode (`list:` on an `agent_task` or `script` node): the node runs
//! its step once per item of a list, in batches, and checks each item's
//! result. The playbook-side types live here, shared by the schema, the
//! validator and the engine; [`items`] loads and filters the items and
//! [`verdict`] holds the per-item state machine.

pub mod items;
pub mod verdict;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::judge::{JudgeQuestion, JudgeQuestions, OrderedMap};

/// `max_items` when the playbook sets none.
pub const DEFAULT_MAX_ITEMS: u32 = 100;
/// The most items one list may hold (V85).
pub const HARD_MAX_ITEMS: u32 = 500;
/// The largest item JSON handed to a step; a larger item needs a look.
pub const ITEM_MAX_BYTES: usize = 8 * 1024;
/// The longest item id.
pub const ITEM_ID_MAX_CHARS: usize = 128;
/// The most re-runs of a rejected item (V85).
pub const MAX_BATCH_RETRIES: u32 = 5;
/// The fields of a list node's own output, which later nodes read by name.
pub const LIST_OUTPUT_FIELDS: [&str; 7] = [
    "total",
    "processed",
    "passed",
    "passed_with_note",
    "needs_look",
    "filtered_out",
    "items",
];
/// The connector site of a list node's item check: `<node>#check`.
pub const LIST_CHECK_SUFFIX: &str = "#check";
/// The one question a judge item check asks.
pub const VERDICT_QUESTION: &str = "verdict";
/// The verdicts a judge item check may name.
pub const VERDICT_OPTIONS: [&str; 3] = ["accept", "accept_with_note", "reject"];

/// Whether a template reference reads the item (`item.*`,
/// `item_output.*`), which only a list's item check may.
pub fn is_item_ref(cap: &str) -> bool {
    matches!(cap.split('.').next(), Some("item" | "item_output"))
}

/// The connector site of `node`'s item check.
pub fn check_site(node: &str) -> String {
    format!("{node}{LIST_CHECK_SUFFIX}")
}

/// The list node an item-check site belongs to, `None` for any other site.
pub fn check_site_node(site: &str) -> Option<&str> {
    site.strip_suffix(LIST_CHECK_SUFFIX)
}

/// A node's `list:` block.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListSpec {
    pub items: ItemSource,
    #[serde(default)]
    pub batch: BatchSpec,
    /// The fields each item's step writes in its status file outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<ItemCheck>,
    #[serde(default)]
    pub on_needs_look: OnNeedsLook,
    #[serde(default)]
    pub rerun: RerunPolicy,
}

/// Where a list's items come from: an earlier node's output field, or
/// written inline.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ItemsFrom>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline: Vec<Value>,
    /// The item field that names each item (a string or an integer).
    #[serde(default = "default_key")]
    pub key: String,
    #[serde(default = "default_max_items")]
    pub max_items: u32,
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub filter: Option<ItemFilter>,
}

/// A field that is present reads as `Some`, even when it is `null`.
fn present_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

fn default_key() -> String {
    "id".to_string()
}

fn default_max_items() -> u32 {
    DEFAULT_MAX_ITEMS
}

/// An earlier node's declared JSON output field holding the items.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemsFrom {
    pub node: String,
    pub field: String,
}

/// Which items of the source are kept: those whose `field` equals `equals`,
/// or, from a list node, those in `state`. `field` and `equals` go together
/// (V86); `equals: null` keeps items whose field is present and null (see
/// [`items::apply_filter`]).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// `Some(Value::Null)` for a written `equals: null`, `None` when absent.
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// How items are admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchSpec {
    #[serde(default = "default_size")]
    pub size: u32,
    #[serde(default = "default_one")]
    pub parallel: u32,
    /// Re-runs of a rejected item.
    #[serde(default = "default_one")]
    pub retries: u32,
}

fn default_size() -> u32 {
    10
}

fn default_one() -> u32 {
    1
}

impl Default for BatchSpec {
    fn default() -> Self {
        BatchSpec {
            size: default_size(),
            parallel: 1,
            retries: 1,
        }
    }
}

/// What a list node does when an item needs a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnNeedsLook {
    /// The node succeeds; the items are reported.
    #[default]
    Continue,
    /// The node fails, with the same output.
    Fail,
}

/// Which items a new visit of a list node runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RerunPolicy {
    /// Every item.
    #[default]
    All,
    /// Items that did not pass in the previous visit (an unchanged item
    /// that passed is carried over).
    NotPassed,
}

/// An item check: exactly one source (V87).
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ItemCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<ConnectorCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeCheck>,
}

/// The source of an item check, when it names exactly one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CheckSource<'a> {
    Output(&'a OutputCheck),
    Script(&'a str),
    Connector(&'a ConnectorCheck),
    Judge(&'a JudgeCheck),
}

impl CheckSource<'_> {
    /// The source key as written in YAML.
    pub fn kind(&self) -> &'static str {
        match self {
            CheckSource::Output(_) => "output",
            CheckSource::Script(_) => "script",
            CheckSource::Connector(_) => "connector",
            CheckSource::Judge(_) => "judge",
        }
    }
}

impl ItemCheck {
    /// The check's source, `None` unless exactly one is given (V87).
    pub fn source(&self) -> Option<CheckSource<'_>> {
        let mut found = [
            self.output.as_ref().map(CheckSource::Output),
            self.script.as_deref().map(CheckSource::Script),
            self.connector.as_ref().map(CheckSource::Connector),
            self.judge.as_ref().map(CheckSource::Judge),
        ]
        .into_iter()
        .flatten();
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// How many source keys the check sets.
    pub fn source_count(&self) -> usize {
        usize::from(self.output.is_some())
            + usize::from(self.script.is_some())
            + usize::from(self.connector.is_some())
            + usize::from(self.judge.is_some())
    }
}

/// A deterministic check on one item output field, with the semantics of
/// the `output_field` edge: equal is accept, anything else is reject.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCheck {
    pub field: String,
    pub equals: String,
}

/// A connector read that confirms an item: one read-only function whose
/// args may template `item.*`, `item_output.*` and `params.*` (V87).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorCheck {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub function: String,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub args: serde_json::Map<String, Value>,
    pub accept_when: AcceptWhen,
}

/// When a connector check's body accepts the item: `path` holds `equals`,
/// or (with `exists: true`) `path` holds any non-null value.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptWhen {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

/// A decision-model check of one item: one choice question named
/// [`VERDICT_QUESTION`] over the rendered `state`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeCheck {
    #[serde(default)]
    pub state: OrderedMap<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub criteria: OrderedMap<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    /// `accept_with_note` (default), `reject` or `needs_look`; any other
    /// word is reported by V89.
    #[serde(default = "default_on_unavailable")]
    pub on_unavailable: String,
}

fn default_on_unavailable() -> String {
    "accept_with_note".to_string()
}

/// What a judge item check does when it has no usable answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnUnavailable {
    AcceptWithNote,
    Reject,
    NeedsLook,
}

impl OnUnavailable {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "accept_with_note" => Some(Self::AcceptWithNote),
            "reject" => Some(Self::Reject),
            "needs_look" => Some(Self::NeedsLook),
            _ => None,
        }
    }
}

impl JudgeCheck {
    /// The question the check asks, as a judge node would declare it.
    pub fn questions(&self) -> JudgeQuestions {
        JudgeQuestions {
            entries: vec![(
                VERDICT_QUESTION.to_string(),
                JudgeQuestion::Choice {
                    instructions: self.instructions.clone(),
                    criteria: self.criteria.clone(),
                },
            )],
            list_shaped: false,
        }
    }

    /// Its `on_unavailable`, `None` for a word this apb does not know.
    pub fn on_unavailable(&self) -> Option<OnUnavailable> {
        OnUnavailable::parse(&self.on_unavailable)
    }
}

impl ListSpec {
    /// The connector check's site binding, when it reads a connector.
    pub fn connector_check(&self) -> Option<&ConnectorCheck> {
        self.check.as_ref().and_then(|c| c.connector.as_ref())
    }

    /// Items one worker pool runs at once under the run's `max_parallel`.
    pub fn workers(&self, max_parallel: usize) -> usize {
        (self.batch.parallel.max(1) as usize).min(max_parallel.max(1))
    }
}
