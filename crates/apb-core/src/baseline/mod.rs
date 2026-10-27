//! The run's baseline check: facts the engine reads once before the first
//! node that can act and again at the finish, so a run reports how the
//! problem changed instead of only what its agents said about it.
//!
//! This module is the schema and the pure parts: the YAML shape
//! ([`BaselineBlock`]), the path reader ([`extract`]) and the rule
//! comparison ([`rule`]). Reading a source and journaling the snapshots is
//! the engine's job.
//!
//! Every check reads exactly one source (validator V62):
//! - `connector`: one read-only connector function, called in-process under
//!   the reserved grant site [`BASELINE_SITE`];
//! - `script`: a script under the version's `scripts/`, whose last
//!   non-empty stdout line is one JSON object (`"complete": false` in it
//!   reads as unknown);
//! - `project_checks`: the project's own test, lint, typecheck and check
//!   commands ([`crate::project_checks`]), with fixed observations.

pub mod extract;
pub mod rule;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project_checks::ProjectChecksSpec;

pub use extract::extract;
pub use rule::{Comparison, Rule, Verdict, compare};

/// The grant site a connector baseline read is made under: the run manifest
/// records its grant next to the nodes' grants, and the connector call gate
/// accepts it as a node id. No node may use this id (validator V68).
pub const BASELINE_SITE: &str = "@baseline";
/// The longest list an observation keeps; a longer one reads as unknown.
pub const MAX_LIST_ITEMS: usize = 500;
/// The longest list item or field text, in characters; a longer one reads
/// as unknown.
pub const MAX_ITEM_CHARS: usize = 256;
/// The default time one source read may take.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;
/// The longest time one source read may take.
pub const MAX_TIMEOUT_SECONDS: u64 = 1800;

fn is_false(b: &bool) -> bool {
    !*b
}

fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

/// The playbook's `baseline_check` block.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineBlock {
    /// Opt-in: a not-met or unknown comparison fails a run that would
    /// otherwise succeed. Default `false`: the results are reported only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub enforce: bool,
    #[serde(default)]
    pub checks: Vec<BaselineCheck>,
}

/// One check: a source, how to tell its read is complete, and what to
/// observe in it.
///
/// The three source keys are parsed side by side so the validator can name
/// a check with none or several of them (V62) instead of failing the parse;
/// [`BaselineCheck::source`] is the one accessor the engine uses.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineCheck {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<BaselineConnector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_checks: Option<ProjectChecksSpec>,
    /// A connector read counts only when this path holds this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_when: Option<CompleteWhen>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observe: Vec<Observe>,
}

/// Where a check reads from, when it names exactly one source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BaselineSource<'a> {
    Connector(&'a BaselineConnector),
    Script(&'a str),
    ProjectChecks(&'a ProjectChecksSpec),
}

impl BaselineSource<'_> {
    /// The source key as written in YAML.
    pub fn kind(&self) -> &'static str {
        match self {
            BaselineSource::Connector(_) => "connector",
            BaselineSource::Script(_) => "script",
            BaselineSource::ProjectChecks(_) => "project_checks",
        }
    }
}

impl BaselineCheck {
    /// The check's source, `None` unless exactly one is given (V62).
    pub fn source(&self) -> Option<BaselineSource<'_>> {
        let mut found = [
            self.connector.as_ref().map(BaselineSource::Connector),
            self.script.as_deref().map(BaselineSource::Script),
            self.project_checks
                .as_ref()
                .map(BaselineSource::ProjectChecks),
        ]
        .into_iter()
        .flatten();
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// How many source keys the check sets.
    pub fn source_count(&self) -> usize {
        usize::from(self.connector.is_some())
            + usize::from(self.script.is_some())
            + usize::from(self.project_checks.is_some())
    }
}

/// A connector source: one function of one connector, with fixed args that
/// may template only `params.*` (V65).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineConnector {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub function: String,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub args: serde_json::Map<String, Value>,
}

/// A read is complete only when `path` holds `equals`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteWhen {
    pub path: String,
    pub equals: Value,
}

/// What an observation holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationType {
    /// A finite number, or with `count: true` the length of an array.
    Number,
    /// A list of scalars, kept as strings.
    List,
    /// One scalar: a string, a bool or a number.
    Field,
}

impl ObservationType {
    pub fn as_str(self) -> &'static str {
        match self {
            ObservationType::Number => "number",
            ObservationType::List => "list",
            ObservationType::Field => "field",
        }
    }
}

/// One value read out of a source and the rule it must satisfy.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Observe {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ObservationType,
    pub path: String,
    /// The path points at an array and the observation is its length.
    #[serde(default, skip_serializing_if = "is_false")]
    pub count: bool,
    pub expect: ExpectRule,
}

/// A direction of change between the two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Decrease,
    Increase,
    NoIncrease,
    NoDecrease,
    Unchanged,
    NoNew,
    Shrink,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Change::Decrease => "decrease",
            Change::Increase => "increase",
            Change::NoIncrease => "no_increase",
            Change::NoDecrease => "no_decrease",
            Change::Unchanged => "unchanged",
            Change::NoNew => "no_new",
            Change::Shrink => "shrink",
        }
    }
}

/// The `expect` block as written. Parsed permissively so the validator can
/// name a block with no rule or several (V64); [`ExpectRule::rule`] turns
/// it into the one [`Rule`] the comparison applies.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_most: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<bool>,
}

impl ExpectRule {
    /// The single rule this block states, or why it states none: no rule,
    /// several, a non-finite bound, or an `equals` that is not a scalar.
    pub fn rule(&self) -> Result<Rule, String> {
        let set = usize::from(self.equals.is_some())
            + usize::from(self.at_most.is_some())
            + usize::from(self.at_least.is_some())
            + usize::from(self.change.is_some())
            + usize::from(self.empty.is_some());
        if set != 1 {
            return Err(format!(
                "`expect` must state exactly one rule (equals, at_most, at_least, change or empty), found {set}"
            ));
        }
        let finite = |n: f64, key: &str| {
            if n.is_finite() {
                Ok(n)
            } else {
                Err(format!("`expect.{key}` must be a finite number"))
            }
        };
        if let Some(v) = &self.equals {
            return Scalar::from_json(v).map(Rule::Equals).ok_or_else(|| {
                "`expect.equals` must be a string, a bool or a finite number".into()
            });
        }
        if let Some(n) = self.at_most {
            return finite(n, "at_most").map(Rule::AtMost);
        }
        if let Some(n) = self.at_least {
            return finite(n, "at_least").map(Rule::AtLeast);
        }
        if let Some(c) = self.change {
            return Ok(Rule::Change(c));
        }
        match self.empty {
            Some(true) => Ok(Rule::Empty),
            _ => Err("`expect.empty` can only be true".into()),
        }
    }
}

/// A scalar a rule compares with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Scalar {
    /// A JSON scalar as a [`Scalar`]; `None` for null, arrays, objects and
    /// numbers that are not finite.
    pub fn from_json(v: &Value) -> Option<Self> {
        match v {
            Value::Bool(b) => Some(Scalar::Bool(*b)),
            Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).map(Scalar::Number),
            Value::String(s) => Some(Scalar::Text(s.clone())),
            _ => None,
        }
    }

    /// Whether an observed value equals this scalar (same type, same value).
    pub fn matches(&self, v: &ObservedValue) -> bool {
        match (self, v) {
            (Scalar::Bool(a), ObservedValue::Bool(b)) => a == b,
            (Scalar::Number(a), ObservedValue::Number(b)) => a == b,
            (Scalar::Text(a), ObservedValue::Text(b)) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for Scalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scalar::Bool(b) => write!(f, "{b}"),
            Scalar::Number(n) => write!(f, "{}", format_number(*n)),
            Scalar::Text(s) => write!(f, "`{s}`"),
        }
    }
}

/// A number as people read it: `37`, not `37.0`.
pub fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A value an observation read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ObservedValue {
    Number(f64),
    List(Vec<String>),
    Text(String),
    Bool(bool),
}

impl std::fmt::Display for ObservedValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObservedValue::Number(n) => write!(f, "{}", format_number(*n)),
            ObservedValue::List(items) => write!(f, "{} item(s)", items.len()),
            ObservedValue::Text(s) => write!(f, "`{s}`"),
            ObservedValue::Bool(b) => write!(f, "{b}"),
        }
    }
}

/// What one observation came to: a value, or unknown with the reason. An
/// unknown is never read as zero, empty or met.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    Value { value: ObservedValue },
    Unknown { reason: String },
}

impl Observation {
    pub fn value(v: ObservedValue) -> Self {
        Observation::Value { value: v }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Observation::Unknown {
            reason: reason.into(),
        }
    }

    /// The value, `None` when unknown.
    pub fn as_value(&self) -> Option<&ObservedValue> {
        match self {
            Observation::Value { value } => Some(value),
            Observation::Unknown { .. } => None,
        }
    }
}

impl Default for Observation {
    fn default() -> Self {
        Observation::unknown("not observed")
    }
}
