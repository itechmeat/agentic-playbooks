//! The `quality:` block of a playbook: the task type a run is compared
//! under, whether the run is graded by rules when it ends, the built-in
//! criteria (all on unless switched off with a reason) and at most
//! [`MAX_CRITERIA`] criteria of the playbook's own.
//!
//! Every criterion has a digest over its definition: two grades are
//! comparable only under the same digest, so an edited criterion starts a
//! new history instead of mixing with the old one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The most criteria a playbook declares besides the built-ins.
pub const MAX_CRITERIA: usize = 3;
/// The task type of a run whose playbook declares none.
pub const DEFAULT_TASK_TYPE: &str = "default";
/// The longest criterion id.
pub const ID_MAX: usize = 40;
/// The longest level description or `applies_when` text.
pub const TEXT_MAX: usize = 500;

/// The built-in criteria, graded deterministically from the journal and
/// the transcripts.
pub const OUTCOME: &str = "outcome";
pub const LOOPS: &str = "loops";
pub const SCOPE: &str = "scope";
pub const CONSENT: &str = "consent";
pub const SKILL_USE: &str = "skill_use";
pub const BUILTIN_IDS: [&str; 5] = [OUTCOME, LOOPS, SCOPE, CONSENT, SKILL_USE];

/// The `quality:` block. Additive: an older apb ignores it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityBlock {
    /// A template over `params.*` naming the kind of task a run does; runs
    /// are compared only within one task type. Absent: [`DEFAULT_TASK_TYPE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    /// Whether the run is graded by the rules grader as it ends.
    #[serde(default)]
    pub grade_on_finish: GradeOnFinish,
    /// Per built-in settings; a built-in not named here is on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub builtins: BTreeMap<String, BuiltinSetting>,
    /// The playbook's own criteria, graded by a host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<Criterion>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeOnFinish {
    #[default]
    Rules,
    Off,
}

/// One built-in's setting: `off` switches it off and says why.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BuiltinSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<String>,
}

/// What a criterion is about: the whole run, or one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Subject {
    Node {
        node: String,
    },
    /// `run`; any other word is a V102.
    Named(String),
}

impl Default for Subject {
    fn default() -> Self {
        Subject::Named("run".into())
    }
}

impl Subject {
    /// `run` or `node:<id>`, as a grade records it.
    pub fn key(&self) -> String {
        match self {
            Subject::Node { node } => format!("node:{node}"),
            Subject::Named(word) => word.clone(),
        }
    }
}

/// Whether a criterion's levels rest on something that happened
/// (`presence`), on something that did not (`absence`), or on either.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    #[default]
    Presence,
    Absence,
    Both,
}

/// The parts of a run a criterion is graded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadKind {
    Journal,
    Transcripts,
    Outputs,
    ConnectorCalls,
    Findings,
}

impl ReadKind {
    pub const ALL: [ReadKind; 5] = [
        ReadKind::Journal,
        ReadKind::Transcripts,
        ReadKind::Outputs,
        ReadKind::ConnectorCalls,
        ReadKind::Findings,
    ];
}

/// What each determinate level means, in the author's words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Levels {
    #[serde(default)]
    pub good: String,
    #[serde(default)]
    pub partial: String,
    #[serde(default)]
    pub poor: String,
}

impl Levels {
    /// `(name, text)` of every level, in order.
    pub fn named(&self) -> [(&'static str, &str); 3] {
        [
            ("good", &self.good),
            ("partial", &self.partial),
            ("poor", &self.poor),
        ]
    }
}

/// One criterion of the playbook's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Criterion {
    pub id: String,
    #[serde(default)]
    pub subject: Subject,
    /// When the criterion applies at all; a run it does not apply to is
    /// graded `not_applicable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_when: Option<String>,
    #[serde(default)]
    pub evidence: EvidenceKind,
    /// Empty reads as every part of the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<ReadKind>,
    #[serde(default)]
    pub levels: Levels,
}

/// Where a criterion comes from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionSource {
    Builtin,
    Playbook,
}

/// A criterion in force for a run: a built-in that is on, or one of the
/// playbook's own, with its digest.
#[derive(Debug, Clone, PartialEq)]
pub struct CriterionDef {
    pub source: CriterionSource,
    pub digest: String,
    pub criterion: Criterion,
}

impl CriterionDef {
    pub fn id(&self) -> &str {
        &self.criterion.id
    }

    /// The parts of the run the criterion reads (empty means all).
    pub fn reads(&self) -> Vec<ReadKind> {
        if self.criterion.reads.is_empty() {
            ReadKind::ALL.to_vec()
        } else {
            self.criterion.reads.clone()
        }
    }

    fn of(source: CriterionSource, criterion: Criterion) -> Self {
        CriterionDef {
            source,
            digest: digest(source, &criterion),
            criterion,
        }
    }
}

/// The digest of a criterion definition: what it is about, when it applies,
/// what it reads and what each level means.
fn digest(source: CriterionSource, c: &Criterion) -> String {
    let canonical = serde_json::json!({
        "source": source,
        "criterion": c,
    });
    crate::content::sha256_hex(canonical.to_string().as_bytes())
}

/// The definition of a built-in criterion; `None` for any other id.
pub fn builtin(id: &str) -> Option<CriterionDef> {
    use ReadKind::*;
    let (evidence, reads, good, partial, poor) = match id {
        OUTCOME => (
            EvidenceKind::Presence,
            vec![Journal],
            "the run succeeded and no node failed on the way",
            "the run succeeded after a node failed and the run recovered",
            "the run failed",
        ),
        LOOPS => (
            EvidenceKind::Both,
            vec![Journal, Transcripts, ConnectorCalls],
            "no sign of going in circles",
            "one sign of going in circles: the same tool call repeated, a retry failing the same way, a bounded loop spent, or the same connector call failing again",
            "two or more signs of going in circles",
        ),
        SCOPE => (
            EvidenceKind::Both,
            vec![Journal, Transcripts, ConnectorCalls],
            "nothing was changed outside the task: no write outside the working directory and no protected path",
            "a write outside the working directory",
            "a protected path was changed",
        ),
        CONSENT => (
            EvidenceKind::Both,
            vec![Journal, Transcripts, ConnectorCalls],
            "every irreversible action ran with the run's consent or after an approving review gate right before its node",
            "a destructive shell command ran under the run's consent without an approving review gate right before its node",
            "an irreversible action ran without consent",
        ),
        SKILL_USE => (
            EvidenceKind::Presence,
            vec![Journal, Transcripts],
            "every skill an attempt read served a node that succeeded",
            "a skill was read and its node succeeded only after a failure",
            "a skill was read and its node failed",
        ),
        _ => return None,
    };
    Some(CriterionDef::of(
        CriterionSource::Builtin,
        Criterion {
            id: id.to_string(),
            subject: Subject::default(),
            applies_when: None,
            evidence,
            reads,
            levels: Levels {
                good: good.into(),
                partial: partial.into(),
                poor: poor.into(),
            },
        },
    ))
}

impl QualityBlock {
    /// Every criterion in force: the built-ins that are on, then the
    /// playbook's own. An unknown built-in name selects nothing (a V105).
    pub fn criteria_defs(&self) -> Vec<CriterionDef> {
        let builtins = BUILTIN_IDS
            .iter()
            .filter(|id| self.builtins.get(**id).is_none_or(|s| s.off.is_none()))
            .filter_map(|id| builtin(id));
        let own = self
            .criteria
            .iter()
            .map(|c| CriterionDef::of(CriterionSource::Playbook, c.clone()));
        builtins.chain(own).collect()
    }

    /// Whether the rules grader grades a run of this playbook as it ends.
    pub fn grades_on_finish(&self) -> bool {
        self.grade_on_finish == GradeOnFinish::Rules
    }
}

/// The criteria in force for `quality` (absent: the built-ins alone, so a
/// run of any playbook can still be graded on request).
pub fn criteria_of(quality: Option<&QualityBlock>) -> Vec<CriterionDef> {
    match quality {
        Some(q) => q.criteria_defs(),
        None => QualityBlock::default().criteria_defs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_builtin_switched_off_leaves_the_others_on() {
        let block: QualityBlock =
            yaml_serde::from_str("builtins:\n  scope: { off: \"reorganizing the repository\" }\n")
                .unwrap();
        let ids: Vec<String> = block
            .criteria_defs()
            .iter()
            .map(|d| d.id().to_string())
            .collect();
        assert_eq!(ids, ["outcome", "loops", "consent", "skill_use"]);
        assert!(block.grades_on_finish());
    }

    #[test]
    fn the_digest_follows_the_definition() {
        let a = builtin(LOOPS).unwrap();
        assert_eq!(a.digest, builtin(LOOPS).unwrap().digest);
        assert_ne!(a.digest, builtin(SCOPE).unwrap().digest);
        assert!(builtin("nope").is_none());
    }
}
