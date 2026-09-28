//! Request, question, answer and response types shared by every provider.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where in APB a decision is asked. Recorded with every decision so the
/// report can group by use; providers never see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UseSite {
    JudgeNode,
    JudgeEdge,
    CompletionCheck,
    RetryAdvice,
    SupervisorTriage,
    ReviewTriage,
    Routing,
    CatalogRank,
}

impl UseSite {
    /// The snake_case name, as written in `decisions.yaml` and the journal.
    pub fn as_str(self) -> &'static str {
        match self {
            UseSite::JudgeNode => "judge_node",
            UseSite::JudgeEdge => "judge_edge",
            UseSite::CompletionCheck => "completion_check",
            UseSite::RetryAdvice => "retry_advice",
            UseSite::SupervisorTriage => "supervisor_triage",
            UseSite::ReviewTriage => "review_triage",
            UseSite::Routing => "routing",
            UseSite::CatalogRank => "catalog_rank",
        }
    }
}

/// The optional descriptions of the two `noul` outcomes. The wire keys are
/// exactly `"true"` and `"false"`; any other key would be dropped silently by
/// the provider, so the type admits no other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: Value,
    #[serde(rename = "false")]
    pub no: Value,
}

/// The options of a `choice` question, in the order they are sent: a JSON
/// object on the wire, whose key order a model may be sensitive to, so it is
/// kept as given rather than sorted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChoiceCriteria(Vec<(String, Option<Value>)>);

impl ChoiceCriteria {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an option (a repeated name replaces the earlier description).
    pub fn with(mut self, name: impl Into<String>, description: Option<Value>) -> Self {
        let name = name.into();
        match self.0.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = description,
            None => self.0.push((name, description)),
        }
        self
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The option names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    /// The options with their descriptions, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&Value>)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_ref()))
    }
}

impl<K: Into<String>> FromIterator<(K, Option<Value>)> for ChoiceCriteria {
    fn from_iter<I: IntoIterator<Item = (K, Option<Value>)>>(iter: I) -> Self {
        iter.into_iter()
            .fold(ChoiceCriteria::new(), |c, (k, v)| c.with(k, v))
    }
}

impl Serialize for ChoiceCriteria {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ChoiceCriteria {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ChoiceCriteria;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of option names to descriptions")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut m: A,
            ) -> Result<Self::Value, A::Error> {
                let mut c = ChoiceCriteria::new();
                while let Some((k, v)) = m.next_entry::<String, Option<Value>>()? {
                    c = c.with(k, v);
                }
                Ok(c)
            }
        }
        d.deserialize_map(Visitor)
    }
}

/// One typed question. Serializes to the `/v1/systemone` wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// One of up to 255 named options; a criterion value describes its option
    /// (`None` leaves the name to speak for itself).
    Choice {
        instructions: Value,
        criteria: ChoiceCriteria,
    },
    /// A position on 2 to 10 ordered levels, lowest first.
    Score {
        instructions: Value,
        #[serde(rename = "criteria")]
        levels: Vec<Value>,
    },
    /// The probability of "yes".
    Noul {
        instructions: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

impl Question {
    /// The wire `type` tag.
    pub fn type_str(&self) -> &'static str {
        match self {
            Question::Choice { .. } => "choice",
            Question::Score { .. } => "score",
            Question::Noul { .. } => "noul",
        }
    }
}

/// A decision request: a state and a map of questions (a list is refused by
/// the providers). The use site travels with it for accounting only.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRequest {
    pub use_site: UseSite,
    pub state: Value,
    /// For an object state: the keys to send first, in this order (a model
    /// reads the serialized state, so key order is part of the prompt). Keys
    /// not listed follow in sorted order. Not part of any digest.
    pub state_order: Vec<String>,
    pub questions: BTreeMap<String, Question>,
}

/// One validated answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        value: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// `value` is the expected level index (0 = lowest); `probabilities` is
    /// keyed by level index.
    Score {
        value: f64,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Noul {
        p: f64,
    },
    /// The provider's item failed validation (or was missing); the rest of
    /// the batch stands.
    Invalid {
        reason: String,
    },
}

/// Token counts and cost as reported by the provider. `cost_usd` is only
/// the provider's own figure (OpenRouter reports one); a price-table estimate
/// is the caller's business.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// A validated response.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionResponse {
    /// The id of the provider that answered (the configured id, not a vendor).
    pub provider: String,
    /// The model that answered, as the reply names it.
    pub model: String,
    /// Whether the provider's probabilities are calibrated (every
    /// `/v1/systemone` model is; an LLM emulation would not be).
    pub calibrated: bool,
    pub answers: BTreeMap<String, Answer>,
    pub usage: Usage,
    pub latency_ms: u64,
    /// Served from the caller's per-run cache: no request was made.
    pub cached: bool,
    /// Reply items whose id was not asked (ignored and counted).
    pub ignored_items: usize,
}

/// Client-side limits checked before anything is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most options a `choice` question may carry.
    pub max_options: usize,
    /// Most levels a `score` question may carry.
    pub max_levels: usize,
    /// The token budget for the state plus the longest question.
    pub max_state_tokens: usize,
}

impl Default for Limits {
    /// The lowest limits across the known gateways: 255 options, 10 levels,
    /// 32k tokens for the state plus the longest question.
    fn default() -> Self {
        Limits {
            max_options: 255,
            max_levels: 10,
            max_state_tokens: 32_000,
        }
    }
}
