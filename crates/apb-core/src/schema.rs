use serde::{Deserialize, Serialize};

use crate::profile::{ProfileScope, QualifiedProfileRef};

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("playbook uses schema 1 executors: run `apb migrate` to convert them to profiles")]
    LegacyExecutors,
}

fn default_schema() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Playbook {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub supervisor: Option<Supervisor>,
    /// Structured trigger for agentic matching (spec 8.5). The free-form
    /// `description` is not used for matching - only these canonical fields are.
    #[serde(default)]
    pub trigger: Option<Trigger>,
    /// Project requirements for applicability (spec 5.2), checked by
    /// preflight before a run starts.
    #[serde(default)]
    pub requires: Option<Requires>,
    /// Effects declared by the author (spec 8.5). Not taken on faith:
    /// policy uses effective = inferred ∪ declared (see `effects`).
    #[serde(default)]
    pub effects: Vec<Effect>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Canonical trigger: "when to apply" (spec 8.5). Fields are machine-facing,
/// English, with length limits (validator V17).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Trigger {
    #[serde(default)]
    pub when: Vec<String>,
    #[serde(default)]
    pub avoid_when: Vec<String>,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// Applicability requirements (spec 5.2): presence of files and commands.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Requires {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
}

/// Class of a run effect (spec 8.5). Ord/Hash - so it can be put into a
/// BTreeSet when computing inferred/effective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    FsRead,
    FsWrite,
    Network,
    External,
    Secrets,
    Irreversible,
}

impl Playbook {
    pub fn from_yaml(s: &str) -> Result<Self, SchemaError> {
        let playbook: Playbook = serde_yaml_ng::from_str(s)?;
        // schema 1 executors were removed from the schema (Task 9). serde
        // silently ignores those fields, so we detect them in the raw YAML and
        // route to migration instead of running a playbook without an executor.
        if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(s)
            && has_legacy_executors(&v)
        {
            return Err(SchemaError::LegacyExecutors);
        }
        Ok(playbook)
    }
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

impl Node {
    /// Expected seconds for progress weighting: the parsed `expected_duration`
    /// if present and valid, otherwise the per-kind default (agent_task/script
    /// = `duration::DEFAULT_TASK_SECONDS`, every other kind = 0).
    pub fn expected_seconds(&self) -> u64 {
        if let Some(ed) = &self.expected_duration
            && let Some(s) = ed.parsed()
        {
            return s;
        }
        if self.kind.needs_duration_estimate() {
            crate::duration::DEFAULT_TASK_SECONDS
        } else {
            0
        }
    }
}

impl NodeKind {
    /// Whether executing this node spawns an agent: an `agent_task`, or a
    /// `finish` that composes its answer from the run context via a `prompt`.
    /// The single source of truth for "an agent runs here" (review M3). The
    /// match is exhaustive on purpose: a new `NodeKind` variant is a compile
    /// error until its agent-ness is decided, which keeps the derived
    /// predicates (`takes_workdir_lock`, effects) from silently defaulting a
    /// new acting node to "no agent".
    pub fn runs_agent(&self) -> bool {
        match self {
            NodeKind::AgentTask { .. }
            | NodeKind::Finish {
                prompt: Some(_), ..
            } => true,
            NodeKind::Start
            | NodeKind::Script { .. }
            | NodeKind::Prompt { .. }
            | NodeKind::Condition { .. }
            | NodeKind::HumanReview { .. }
            | NodeKind::Wait { .. }
            | NodeKind::Finish { prompt: None, .. }
            | NodeKind::Playbook { .. } => false,
        }
    }

    /// Whether executing this node writes to the shared working directory and
    /// therefore must hold the workdir lock: any agent node (`runs_agent`), a
    /// `script`, or a `playbook` node (its child runs in-process under the
    /// parent's lock, so the parent must hold it). FIXES review I5: a
    /// finish-with-prompt runs an agent in the shared workdir, which the old
    /// `AgentTask | Script | Playbook` predicate missed, so a Start +
    /// Finish-with-prompt playbook ran an agent WITHOUT the lock.
    pub fn takes_workdir_lock(&self) -> bool {
        self.runs_agent() || matches!(self, NodeKind::Script { .. } | NodeKind::Playbook { .. })
    }

    /// Whether progress weighting estimates a duration for this node: the V19
    /// nudge and the `Node::expected_seconds` default arm. Same set as
    /// `takes_workdir_lock` (agent_task | script | finish-with-prompt |
    /// playbook), identical to prior behavior.
    pub fn needs_duration_estimate(&self) -> bool {
        self.takes_workdir_lock()
    }

    /// Whether executing this node renders the full run context (so context
    /// compaction must run before the render): an `agent_task`, a `prompt`, a
    /// finish-with-prompt, or a `playbook` node with an explicit `instruction`
    /// template (review R1-M3 - the old trigger covered only agent_task and
    /// prompt).
    pub fn renders_context(&self) -> bool {
        matches!(
            self,
            NodeKind::AgentTask { .. }
                | NodeKind::Prompt { .. }
                | NodeKind::Finish {
                    prompt: Some(_),
                    ..
                }
                | NodeKind::Playbook {
                    instruction: Some(_),
                    ..
                }
        )
    }

    /// The effective profile binding for a node that runs an agent: the node's
    /// own `profile`, else `defaults.profile`. `None` for any node that does
    /// not run an agent (a plain finish, script, prompt, ...). The single
    /// source used by both the run-manifest builder and the policy gate so
    /// their key sets cannot diverge (anti-TOCTOU).
    pub fn effective_profile_ref(&self, defaults: &Defaults) -> Option<QualifiedProfileRef> {
        match self {
            NodeKind::AgentTask { profile, .. }
            | NodeKind::Finish {
                prompt: Some(_),
                profile,
                ..
            } => profile.clone().or_else(|| defaults.profile.clone()),
            _ => None,
        }
    }
}

/// Whether the raw YAML has traces of schema-1 executors: a top-level
/// `executors`, `defaults.executor`, `supervisor.executor`, or `executor` on
/// any node.
fn has_legacy_executors(v: &serde_yaml_ng::Value) -> bool {
    // A non-empty `executors` (a mapping with entries, or any other non-null
    // value) is legacy; an empty/null `executors:` has nothing to migrate (V18
    // will catch a node without a binding).
    if v.get("executors")
        .is_some_and(|e| !e.is_null() && e.as_mapping().map(|m| !m.is_empty()).unwrap_or(true))
    {
        return true;
    }
    let has_exec = |m: Option<&serde_yaml_ng::Value>| m.and_then(|x| x.get("executor")).is_some();
    if has_exec(v.get("defaults")) || has_exec(v.get("supervisor")) {
        return true;
    }
    v.get("nodes")
        .and_then(|n| n.as_sequence())
        .is_some_and(|nodes| nodes.iter().any(|n| n.get("executor").is_some()))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String, // text | enum | int | bool
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub default: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Defaults {
    /// Default profile for nodes without their own (schema 2). Executor
    /// selection for a node: node.profile -> defaults.profile.
    #[serde(default)]
    pub profile: Option<QualifiedProfileRef>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Supervisor {
    #[serde(default)]
    pub profile: Option<QualifiedProfileRef>,
    #[serde(default)]
    pub policy: Option<serde_yaml_ng::Value>, // details land in phase 3A
}

/// Estimated wall time of ONE execution of a node (spec 2026-07-17). Accepts
/// an integer count of seconds or a string with a single unit suffix; the
/// parse is validated (V20) and the value is read via `parsed()`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ExpectedDuration {
    Seconds(u64),
    Text(String),
    /// Any other scalar the author wrote (a float, a negative number, a
    /// boolean, ...). Kept verbatim so the playbook still loads and the
    /// validator can emit a clean V20 diagnostic instead of the load failing
    /// at deserialization. MUST stay the LAST untagged variant so it only
    /// catches values that match neither `Seconds` nor `Text`. `parsed()`
    /// returns None, so callers fall back to the per-kind default.
    Invalid(serde_yaml_ng::Value),
}

impl ExpectedDuration {
    /// Seconds if the value parses, else None (an invalid value is caught by
    /// validator V20; callers fall back to the per-kind default).
    pub fn parsed(&self) -> Option<u64> {
        match self {
            ExpectedDuration::Seconds(n) => Some(*n),
            ExpectedDuration::Text(s) => crate::duration::parse_duration_str(s),
            ExpectedDuration::Invalid(_) => None,
        }
    }
}

/// A reference to a playbook (spec C): id + scope. Two YAML forms - a bare
/// string (shorthand, `scope: auto`) or an object `{ id, scope }`. Always
/// serialized as an object. Mirrors `QualifiedProfileRef` but keyed by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualifiedPlaybookRef {
    pub id: String,
    pub scope: ProfileScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybookRefFull {
    id: String,
    #[serde(default)]
    scope: ProfileScope,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PlaybookRefForm {
    Short(String),
    Full(PlaybookRefFull),
}

impl<'de> Deserialize<'de> for QualifiedPlaybookRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match PlaybookRefForm::deserialize(d)? {
            PlaybookRefForm::Short(id) => Self {
                id,
                scope: ProfileScope::Auto,
            },
            PlaybookRefForm::Full(PlaybookRefFull { id, scope }) => Self { id, scope },
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Estimated time of ONE execution (spec 2026-07-17). Absent -> the per-kind
    /// default (see `expected_seconds`). Additive to schema 2; no migration.
    #[serde(default)]
    pub expected_duration: Option<ExpectedDuration>,
    #[serde(flatten)]
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    Start,
    AgentTask {
        prompt: String,
        /// Profile reference (schema 2) - the only executor binding. The
        /// string YAML form (`profile: name`) is parsed as a ref with
        /// `scope: auto`; a missing profile falls back to `defaults.profile`.
        #[serde(default)]
        profile: Option<QualifiedProfileRef>,
        #[serde(default)]
        max_retries: Option<u32>,
        #[serde(default)]
        timeout_seconds: Option<u64>,
        #[serde(default)]
        workdir: Option<String>,
        #[serde(default)]
        isolation: Option<Isolation>,
        /// Deterministic script check of the result on top of the agent's
        /// self-assessment (spec 6.2): script path (sh) relative to the
        /// version's scripts/. A non-zero exit code makes the node Failed even
        /// if the agent reported success. `None` - no check is performed.
        #[serde(default)]
        success_check: Option<String>,
    },
    Script {
        script: String,
        runner: String,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    Prompt {
        prompt: String,
    },
    Condition {
        #[serde(default)]
        max_loops: Option<u32>,
    },
    HumanReview {
        options: Vec<String>,
    },
    Wait {
        wait_for: WaitFor,
        timeout_seconds: u64,
        #[serde(default)]
        scope: Option<String>,
    },
    Finish {
        outcome: Outcome,
        /// Optional finish prompt (spec B). When set, an agent composes the run
        /// answer from the accumulated run context; the agent's output becomes
        /// this node's output. Absent -> instant, free, empty output (unchanged).
        #[serde(default)]
        prompt: Option<String>,
        /// Profile binding for the finish agent (spec B). Meaningful only with
        /// `prompt`; falls back to `defaults.profile`. Validator V21 errors on a
        /// profile without a prompt (a binding that can never execute).
        #[serde(default)]
        profile: Option<QualifiedProfileRef>,
    },
    Playbook {
        /// The child playbook to run (spec C). `scope: auto` resolves the
        /// parent's origin registry first, then global.
        playbook: QualifiedPlaybookRef,
        /// Template rendered with the parent run's context; the result becomes
        /// the child run's `instruction` (Part A precedence: this explicit value
        /// wins over the child's draft). Absent -> the child falls back to its
        /// own draft.
        #[serde(default)]
        instruction: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WaitFor {
    Timer { seconds: u64 },
    Webhook { key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
}

/// Required isolation level for node execution (spec 7.3/8.3): `full` -
/// a fully isolated sandbox, `best_effort` - isolation to the extent the
/// adapter supports it, `none` - a shared working directory (default).
/// Currently the field is declarative and shown in the web node form; the
/// engine does NOT enforce it yet (real enforcement via worktree is future
/// work, spec 8.3), so the validator warns about isolation that is declared
/// but not enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    Full,
    BestEffort,
    #[default]
    None,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: Option<EdgeCondition>,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub join: Option<String>, // all | any; executed in phase 2, but parsed already now
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EdgeCondition {
    NodeStatus { node: String, equals: StatusEq },
    ReviewStatus { equals: String },
    OutputMatch { node: String, pattern: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusEq {
    Success,
    Failure,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_seconds_uses_value_or_kind_default() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: a, type: agent_task, prompt: hi, profile: x }
  - { id: b, type: agent_task, prompt: hi, profile: x, expected_duration: 5m }
  - { id: c, type: agent_task, prompt: hi, profile: x, expected_duration: 90 }
  - { id: p1, type: prompt, prompt: hi }
  - { id: f, type: finish, outcome: success }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        assert_eq!(pb.node("a").unwrap().expected_seconds(), 120);
        assert_eq!(pb.node("b").unwrap().expected_seconds(), 300);
        assert_eq!(pb.node("c").unwrap().expected_seconds(), 90);
        assert_eq!(pb.node("p1").unwrap().expected_seconds(), 0);
    }

    #[test]
    fn finish_with_prompt_defaults_to_task_seconds() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: f1, type: finish, outcome: success }
  - { id: f2, type: finish, outcome: success, prompt: "x" }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        assert_eq!(pb.node("f1").unwrap().expected_seconds(), 0);
        assert_eq!(pb.node("f2").unwrap().expected_seconds(), 120);
    }

    #[test]
    fn playbook_node_parses_both_ref_forms() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: c1, type: playbook, playbook: child, instruction: "go" }
  - { id: c2, type: playbook, playbook: { id: child, scope: global } }
  - { id: f, type: finish, outcome: success }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        match &pb.node("c1").unwrap().kind {
            NodeKind::Playbook {
                playbook,
                instruction,
            } => {
                assert_eq!(playbook.id, "child");
                assert_eq!(playbook.scope, crate::profile::ProfileScope::Auto);
                assert_eq!(instruction.as_deref(), Some("go"));
            }
            _ => panic!("c1 not a playbook node"),
        }
        match &pb.node("c2").unwrap().kind {
            NodeKind::Playbook { playbook, .. } => {
                assert_eq!(playbook.scope, crate::profile::ProfileScope::Global);
            }
            _ => panic!("c2 not a playbook node"),
        }
        assert_eq!(pb.node("c1").unwrap().expected_seconds(), 120);
    }

    #[test]
    fn finish_parses_with_and_without_prompt_profile() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: f1, type: finish, outcome: success }
  - { id: f2, type: finish, outcome: success, prompt: "compose", profile: writer }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        let f1 = &pb.node("f1").unwrap().kind;
        let f2 = &pb.node("f2").unwrap().kind;
        assert!(matches!(
            f1,
            NodeKind::Finish {
                prompt: None,
                profile: None,
                ..
            }
        ));
        match f2 {
            NodeKind::Finish {
                prompt: Some(p),
                profile: Some(pr),
                ..
            } => {
                assert_eq!(p, "compose");
                assert_eq!(pr.name, "writer");
            }
            _ => panic!("expected finish with prompt+profile"),
        }
    }
}
