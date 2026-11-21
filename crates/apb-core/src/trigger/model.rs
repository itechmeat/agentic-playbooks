//! The data types of a start rule and its store: the rule as a person
//! writes it, the signed per-project rules file, the entry the store
//! keeps, the approval, the digest and the errors. A leaf module: the
//! store logic (`rule`), the checks (`validate`) and the state
//! (`state`) all import from here, and it imports nothing of theirs, so
//! the module graph stays acyclic.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::budget::BudgetRequest;
use crate::connector::event_filter::{BodyCondition, EventFilter};
use crate::scope::Scope;
use crate::validate::Issue;

/// The longest rule name; names are [a-z0-9][a-z0-9-]* like profiles.
pub const NAME_MAX: usize = 40;
pub const MAX_PARAMS: usize = 32;

/// A start rule as a person writes it (YAML at `apb trigger set`, JSON in
/// the store and on the HTTP API).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub playbook: RulePlaybook,
    pub source: RuleSource,
    /// Param name to template (`render.rs`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    pub limits: RuleLimits,
    /// Over the playbook's `budget:` block per field, as a run argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetRequest>,
    /// The run's input is outside data.
    #[serde(default = "yes")]
    pub external_data: bool,
    /// Refuse a start whose readiness report does not pass (preparation
    /// checks it for an unattended start, which a rule's start always is).
    #[serde(default = "yes")]
    pub require_ready: bool,
    /// `None`: the machine's default notify subscription; `Some(false)`:
    /// off; `Some(true)`: the machine's target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<bool>,
}

fn yes() -> bool {
    true
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePlaybook {
    pub id: String,
    /// Always a version: a rule never follows `current` or a candidate.
    pub version: String,
    #[serde(default)]
    pub scope: RuleScope,
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    #[default]
    Project,
    Global,
}

/// What starts the rule (externally tagged: `source: { inbox: { .. } }`).
/// A later source kind is a new variant; every inbox rule written now
/// stays valid as written.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleSource {
    Inbox(InboxSource),
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxSource {
    pub connector: String,
    pub account: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    #[serde(default, rename = "where", skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<BodyCondition>,
    /// The template of the start key: one run per rendered key while the
    /// key lives (`runs.start_key_days`). An event id gives once per
    /// event, an entity id once per entity.
    pub key: String,
}

impl InboxSource {
    /// The filter of `types` and `where` (the same `EventFilter` an inbox
    /// read takes).
    pub fn filter(&self) -> EventFilter {
        EventFilter {
            types: self.types.clone(),
            conditions: self.conditions.clone(),
        }
    }
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleLimits {
    /// Starts in any 60 minutes. Runs at once are the playbook's
    /// `concurrency:` block, not a rule limit.
    pub max_per_hour: u32,
}

/// The rules of one project, signed with the installation key and bound to
/// the project's registered workspace id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesFile {
    pub schema: u32,
    pub scope: String,
    pub id: String,
    /// The registered workspace id of the project the file belongs to,
    /// stamped on every write. The MAC covers it.
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub rules: Vec<RuleEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleEntry {
    pub rule: Rule,
    /// 32 lowercase hex, given at creation and again at every change of the
    /// source: the state, journal and start keys of that creation bind to
    /// it.
    pub incarnation: String,
    /// Only `enable` and `disable` change it.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Approval>,
    pub created_at: u64,
    pub updated_at: u64,
    /// `terminal` or `dashboard_session` (the channel of the change).
    pub updated_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// `rule_digest` of the rule it approved.
    pub rule_digest: String,
    /// The permit's playbook digest when it was approved.
    pub playbook_digest: String,
    /// The one approval binding (the permit digest, the launch policy in
    /// force and the billing decision when approved). Admission compares
    /// it, and only it.
    pub binding: String,
    /// What the person saw, for the surfaces only: the permit digest and
    /// the launch policy. Never compared.
    pub permit_digest: String,
    pub launch_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_nonce: Option<String>,
    pub approved_at: u64,
    pub approved_by: String,
}

/// The SHA-256 over the canonical JSON (every object's keys sorted) of
/// `{ "incarnation": .., "rule": <the rule> }`, as lowercase hex, the full
/// 64 characters (the form `rejection::fingerprint` hashes into, without
/// its truncation). It covers every field that changes whether or how the
/// rule fires, and the incarnation, so an approval never carries over to a
/// rule created again under the same name; `enabled` is not part of `Rule`
/// and so not in the digest.
pub fn rule_digest(entry: &RuleEntry) -> String {
    let value = serde_json::json!({
        "incarnation": entry.incarnation,
        "rule": serde_json::to_value(&entry.rule).expect("a Rule serializes"),
    });
    let mut hash = Sha256::new();
    hash.update(canonical_json(&value));
    crate::content::hex_lower(&hash.finalize())
}

/// `value` as bytes with every object's keys in sorted order, so the
/// digest never depends on whether a crate in the build turned on
/// serde_json's `preserve_order` (the same canonical form
/// `signed_store::bytes_of` uses for MACs).
fn canonical_json(value: &serde_json::Value) -> Vec<u8> {
    fn canonical(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                serde_json::Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), canonical(&map[k])))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(canonical).collect())
            }
            other => other.clone(),
        }
    }
    serde_json::to_vec(&canonical(value)).expect("canonical JSON serializes")
}

impl crate::signed_store::SignedDoc for RulesFile {
    const MAC_DOMAIN: &'static str = "apb-trigger-rules-v1";
    const SCHEMA: u32 = 1;
    const DIR: &'static str = "triggers";
    const ENTRIES_KEY: &'static str = "rules";

    fn fresh(scope: Scope, playbook: &str) -> Self {
        RulesFile {
            schema: Self::SCHEMA,
            scope: scope.as_str().to_string(),
            id: playbook.to_string(),
            project: String::new(),
            rules: Vec::new(),
            mac: None,
        }
    }

    const NO_LINKS: bool = true;

    fn identity(&self) -> (&str, &str) {
        (&self.scope, &self.id)
    }

    fn belongs_to(&self, root: &Path) -> bool {
        crate::projects::registered_id(root).is_some_and(|id| id == self.project)
    }

    fn mac(&self) -> Option<&str> {
        self.mac.as_deref()
    }

    fn set_mac(&mut self, mac: Option<String>) {
        self.mac = mac;
    }

    fn set_schema(&mut self, schema: u32) {
        self.schema = schema;
    }
}

/// Why a rule read or write failed.
#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error(transparent)]
    Store(#[from] crate::signed_store::StoreError),
    /// The rule has errors (every issue, errors and warnings).
    #[error("the rule has errors")]
    Invalid(Vec<Issue>),
    #[error("no rule `{0}`")]
    NotFound(String),
    #[error(transparent)]
    Inbox(#[from] crate::connector::inbox::InboxError),
    /// A rule lock stayed held for 60 s.
    #[error("rule `{0}` is being changed or swept; try again")]
    Busy(String),
    /// The project has no registered workspace id, so no listener would
    /// sweep a rule saved here.
    #[error("trigger_project_unregistered")]
    Unregistered,
    /// The rule document names `enabled`.
    #[error("trigger_enabled_not_settable")]
    EnabledNotSettable,
    /// A rules write the store's read cap refused: the signed rules file
    /// would grow past what its reader reads, so every rule in it would
    /// stop reading. Nothing was written; the limit is the reader's
    /// constant.
    #[error("rules_store_full: the rules file would exceed {0} bytes; remove a rule first")]
    StoreFull(u64),
}

/// The code of a rules file that does not verify or does not belong.
pub const RULES_UNVERIFIED: &str = "trigger_rules_unreadable";

/// The one sentence every surface (the CLI, the engine's list and doctor
/// lines, the HTTP and MCP refusals) gives for a rules file that does not
/// verify or does not belong, after its code: no write can repair it
/// (every write refuses it), so the remedy is to move it aside.
pub fn rules_unverified_detail(path: &std::path::Path) -> String {
    format!(
        "{} does not verify (changed outside apb, written by another installation or for another project); move it aside to write rules again",
        path.display()
    )
}

/// [`rules_unverified_detail`] after its code, as one line.
pub fn rules_unverified_text(path: &std::path::Path) -> String {
    format!("{RULES_UNVERIFIED}: {}", rules_unverified_detail(path))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOutcome {
    Created,
    Updated { approval_dropped: bool },
    Unchanged,
}
