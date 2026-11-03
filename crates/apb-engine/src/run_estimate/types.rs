//! The estimate's types: what a run of a playbook version is expected to
//! take, and what the estimate is for.

use std::collections::BTreeMap;

use apb_core::schema::Playbook;
use serde::{Deserialize, Serialize};

/// How many of the newest comparable runs (same version and execution
/// mode) the estimate reads.
pub const ESTIMATE_WINDOW: usize = 50;

/// How many of a project's newest run directories the estimate reads at
/// most, whatever their playbook, before it picks the comparable ones.
pub const ESTIMATE_SCAN: usize = 200;
/// Below this many comparable runs the estimate is indicative only.
pub const MIN_COMPARABLE: u32 = 3;

/// A distribution: the median and a range around it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spread<T> {
    /// How many samples; 0 for a declared figure.
    pub n: u32,
    pub p50: T,
    pub low: T,
    pub high: T,
    pub kind: SpreadKind,
}

/// What `low` and `high` are.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpreadKind {
    /// p25 to p75 (4 samples or more).
    Iqr,
    /// The smallest and the largest (2 or 3 samples).
    MinMax,
    /// One value.
    Single,
}

/// Where a run figure comes from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimateBasis {
    /// Comparable past runs.
    History,
    /// The nodes' declared `expected_duration`.
    Declared,
    /// Nothing to go on.
    None,
}

/// The key a node's history is matched by since the executor digest: the
/// profile, the bundle and the executor of the binding.
pub const ESTIMATE_KEY_VERSION: u32 = 2;
/// The key of an estimate written before [`ESTIMATE_KEY_VERSION`]: the
/// profile digest only.
pub const PROFILE_ONLY_KEY: u32 = 1;

fn profile_only_key() -> u32 {
    PROFILE_ONLY_KEY
}

/// The counts behind an estimate.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparable {
    /// How a node's history was matched: [`ESTIMATE_KEY_VERSION`] (profile,
    /// bundle and executor), or [`PROFILE_ONLY_KEY`] for a snapshot written
    /// before it, which has no such field.
    #[serde(default = "profile_only_key")]
    pub key_version: u32,
    /// Runs of the same version and execution mode within the window.
    pub runs: u32,
    pub succeeded: u32,
    pub window: u32,
    /// Runs whose attempts reported no usage at all.
    pub runs_without_usage: u32,
    pub attempts_without_usage: u32,
    pub attempts_without_cost: u32,
    /// Per node, the comparable runs that bound it to another profile.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<BTreeMap<String, u32>>"))]
    pub profile_mismatch: BTreeMap<String, u32>,
    /// Per node, the comparable runs that bound it to the same profile with
    /// other skills (another bundle digest).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<BTreeMap<String, u32>>"))]
    pub bundle_mismatch: BTreeMap<String, u32>,
    /// Per node, the comparable runs that bound it to the same profile and
    /// skills with another executor (chain, tiers, or an override).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<BTreeMap<String, u32>>"))]
    pub executor_mismatch: BTreeMap<String, u32>,
}

impl Default for Comparable {
    fn default() -> Self {
        Comparable {
            key_version: ESTIMATE_KEY_VERSION,
            runs: 0,
            succeeded: 0,
            window: 0,
            runs_without_usage: 0,
            attempts_without_usage: 0,
            attempts_without_cost: 0,
            profile_mismatch: BTreeMap::new(),
            bundle_mismatch: BTreeMap::new(),
            executor_mismatch: BTreeMap::new(),
        }
    }
}

impl Comparable {
    /// The runs of `node` counted under `part` of the key.
    pub(super) fn mismatch_of(&mut self, part: KeyPart) -> &mut BTreeMap<String, u32> {
        match part {
            KeyPart::Profile => &mut self.profile_mismatch,
            KeyPart::Bundle => &mut self.bundle_mismatch,
            KeyPart::Executor => &mut self.executor_mismatch,
        }
    }

    /// The part of the key a history of `node` differed in, outermost
    /// first; `None` when every comparable run matched it.
    pub fn mismatch_part(&self, node: &str) -> Option<KeyPart> {
        [
            (KeyPart::Profile, &self.profile_mismatch),
            (KeyPart::Bundle, &self.bundle_mismatch),
            (KeyPart::Executor, &self.executor_mismatch),
        ]
        .into_iter()
        .find(|(_, m)| m.contains_key(node))
        .map(|(p, _)| p)
    }
}

/// A part of the key a node's history is matched by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPart {
    Profile,
    Bundle,
    Executor,
}

impl KeyPart {
    /// What the estimate text names: `profile`, `skill set` or `executor`.
    pub fn word(self) -> &'static str {
        match self {
            KeyPart::Profile => "profile",
            KeyPart::Bundle => "skill set",
            KeyPart::Executor => "executor",
        }
    }
}

/// How long the run takes, active time (paused time left out).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeEstimate {
    pub basis: EstimateBasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub spread: Option<Spread<u64>>,
    /// The median time succeeded runs waited for people (gates and
    /// questions), included in the spread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub human_wait_p50_ms: Option<u64>,
}

/// A run's spend in one dimension.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpendEstimate<T> {
    pub basis: EstimateBasis,
    pub spread: Spread<T>,
}

/// A node's spend and duration in each dimension.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeSpread {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tokens: Option<Spread<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<Spread<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub ms: Option<Spread<u64>>,
}

/// The median spend of one attempt of a node: what an attempt that reported
/// nothing is counted as.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct AttemptMedian {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<f64>,
}

/// Where a list node's item count comes from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemsBasis {
    /// The inline list, after its filter.
    Inline,
    /// The median item count of earlier visits.
    History,
    /// Not known before the run.
    Unknown,
}

/// A list node's figures.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ListEstimate {
    /// One item's spend over all its attempts (rounds and checks).
    pub per_item: NodeSpread,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub items: Option<u32>,
    pub items_basis: ItemsBasis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub batch_ms_p50: Option<u64>,
    pub batch_size: u32,
}

impl NodeEstimate {
    /// What the budget gate weighs before the node: one item for a list
    /// node (times the batch's items), one visit otherwise.
    pub fn gate_figure(&self) -> &NodeSpread {
        match &self.list {
            Some(l) => &l.per_item,
            None => &self.per_visit,
        }
    }
}

/// One gated node's figures.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEstimate {
    pub node: String,
    /// The median number of visits per run.
    pub visits_p50: f64,
    /// One visit: what the budget gate weighs before the node.
    pub per_visit: NodeSpread,
    /// All visits of a run: loops and retries as they happened.
    pub per_run: NodeSpread,
    pub per_attempt_p50: AttemptMedian,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub list: Option<ListEstimate>,
}

/// What a run of a playbook version is expected to take.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEstimate {
    pub playbook: String,
    pub version: String,
    /// `cli` or `host`.
    pub execution: String,
    pub comparable: Comparable,
    pub time: TimeEstimate,
    /// Tokens over the estimated nodes; `None` when no node is estimated.
    pub tokens: Option<SpendEstimate<u64>>,
    /// What the estimated nodes cost when billed: the reported figures
    /// whatever their basis (a subscription equivalent is priced like billed
    /// work, so subscription history never admits billed work as free);
    /// `None` when no comparable attempt reported a cost.
    pub cost_usd: Option<SpendEstimate<f64>>,
    pub nodes: Vec<NodeEstimate>,
    /// Gated nodes with no spend sample: the run figures leave them out.
    pub unestimated: Vec<String>,
    /// Gated nodes the gate has no token figure for (those of
    /// `unestimated` and those with cost samples only): a token limit
    /// cannot weigh them, and the run's tokens leave them out.
    #[serde(default)]
    pub unestimated_tokens: Vec<String>,
    /// Gated nodes the gate has no cost figure for: a dollar limit cannot
    /// weigh them, and the run's cost leaves them out.
    #[serde(default)]
    pub unestimated_cost: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub note: Option<String>,
}

impl RunEstimate {
    /// The figures of `node`, when it is a gated node of the playbook.
    pub fn node(&self, node: &str) -> Option<&NodeEstimate> {
        self.nodes.iter().find(|n| n.node == node)
    }

    /// The gated nodes the gate cannot weigh in a dimension `limit` limits,
    /// in playbook order: before each of them `on_no_estimate` decides.
    pub fn unestimated_for(&self, limit: &apb_core::budget::BudgetLimit) -> Vec<String> {
        let missing =
            |list: &[String], limited: bool, node: &str| limited && list.iter().any(|n| n == node);
        self.nodes
            .iter()
            .map(|n| n.node.as_str())
            .filter(|n| {
                missing(&self.unestimated_tokens, limit.tokens.is_some(), n)
                    || missing(&self.unestimated_cost, limit.usd.is_some(), n)
            })
            .map(str::to_string)
            .collect()
    }
}

/// What an estimate is for: a playbook version, the execution mode, and
/// each node's key (profile, bundle and executor digest).
#[derive(Debug, Clone)]
pub struct EstimateTarget {
    pub playbook: Playbook,
    pub version: String,
    /// `cli` or `host`.
    pub execution: String,
    /// Node id to the key its history must match.
    pub node_keys: BTreeMap<String, crate::execution_identity::digest::NodeExecKey>,
    /// A run left out of the history (the run being estimated).
    pub exclude_run: Option<String>,
}
