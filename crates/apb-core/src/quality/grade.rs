//! A grade: what one grader concluded about one finished run, criterion by
//! criterion, with the records each conclusion cites and a trace of what
//! the grader read. A grade holds no source text: citations are references
//! into the run (see [`Cite`]) and the trace counts characters.

use serde::{Deserialize, Serialize};

use super::criteria::CriterionSource;

/// The grade file format this apb writes.
pub const GRADE_SCHEMA: u32 = 1;

/// The level a criterion was graded at. Only `good`, `partial` and `poor`
/// are determinate; the other two say why there is no level.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Good,
    Partial,
    Poor,
    NotApplicable,
    NotEnoughData,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Good => "good",
            Level::Partial => "partial",
            Level::Poor => "poor",
            Level::NotApplicable => "not_applicable",
            Level::NotEnoughData => "not_enough_data",
        }
    }

    pub fn is_determinate(self) -> bool {
        matches!(self, Level::Good | Level::Partial | Level::Poor)
    }
}

/// A citation: a reference to a record the grader read in full, and
/// optionally a character span inside it. The reference grammar:
/// `j:<seq>` a journal event, `o:<node>` a node output, `s:<stem>` a status
/// file, `t:<stem>/<session|stdout|stderr>#<record>` a transcript record.
/// Outline references (`n:<id>`) route a reader and are never citable.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cite {
    #[serde(rename = "ref")]
    pub r#ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub span: Option<[u64; 2]>,
}

impl Cite {
    pub fn to(r: impl Into<String>) -> Self {
        Cite {
            r#ref: r.into(),
            span: None,
        }
    }
}

/// A view that a claim of absence rests on: it must be complete and read
/// to its end.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewEnd {
    pub view: String,
    pub complete: bool,
    pub ended: bool,
}

/// One criterion's grade.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionGrade {
    pub id: String,
    pub source: CriterionSource,
    /// The criterion definition's digest at grading time.
    pub digest: String,
    /// `run` or `node:<id>`.
    pub subject: String,
    pub level: Level,
    /// A short machine slug for why (`run_failed`, `no_signal`, ...).
    pub reason_code: String,
    /// One sentence for a person; never source text.
    pub why: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cites: Vec<Cite>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absence_basis: Vec<ViewEnd>,
    /// Whether the level counts in a promotion decision.
    pub counts_for_promotion: bool,
}

/// One reading operation of the trace: what was asked for and how many
/// characters came back. Never the text itself.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadOp {
    pub op: String,
    #[serde(rename = "ref")]
    pub r#ref: String,
    pub chars: u64,
}

/// How far one view was read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewRead {
    pub view: String,
    /// The view holds everything the run produced (nothing was cut).
    pub complete: bool,
    /// Every record of the view was read in full.
    pub ended: bool,
    /// The share of its records read in full, 0 to 1.
    pub read_share: f64,
}

/// What a grader read, without the text.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadTrace {
    pub budget_chars: u64,
    pub read_chars: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<ReadOp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<ViewRead>,
}

/// Who graded.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraderKind {
    /// apb's deterministic rules.
    Rules,
    /// The host agent in a bounded reading session.
    Host,
}

/// A grader and its version: grades of different graders or versions are
/// never compared.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GraderId {
    pub kind: GraderKind,
    pub version: u32,
}

impl GraderId {
    pub const RULES: GraderId = GraderId {
        kind: GraderKind::Rules,
        version: 1,
    };
    pub const HOST: GraderId = GraderId {
        kind: GraderKind::Host,
        version: 1,
    };
}

impl std::fmt::Display for GraderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            GraderKind::Rules => "rules",
            GraderKind::Host => "host",
        };
        write!(f, "{kind}/{}", self.version)
    }
}

/// Whether a skill an attempt was given was read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillReadState {
    Yes,
    No,
    /// The attempt's records cannot tell (an agent without a structured
    /// transcript, an incomplete one).
    Unknown,
}

/// One skill of a node's profile and whether the node's attempts read it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRead {
    pub node: String,
    pub profile: String,
    pub skill: String,
    pub bundle_digest: String,
    pub read: SkillReadState,
    /// The record that shows the read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub at: Option<String>,
}

/// One stored grade (`runs/<id>/grades/<grade_id>.json`, write-once).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grade {
    pub schema: u32,
    pub grade_id: String,
    pub run_id: String,
    pub playbook: String,
    pub version: String,
    pub task_type: String,
    pub grader: GraderId,
    /// Unix milliseconds.
    pub created_at: u64,
    pub criteria: Vec<CriterionGrade>,
    pub reading: ReadTrace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillRead>,
    /// The installation's MAC over the rest of the grade: only a grade apb
    /// stored on this machine counts in a promotion. Absent when the
    /// installation has no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub mac: Option<String>,
}

/// The purpose a grade's MAC is made for.
pub const GRADE_MAC_DOMAIN: &str = "apb-grade-v1";

impl Grade {
    /// The bytes the MAC covers: the grade without its MAC; `None` when it
    /// does not serialize (there is then nothing to sign or check).
    pub fn signed_bytes(&self) -> Option<Vec<u8>> {
        let unsigned = Grade {
            mac: None,
            ..self.clone()
        };
        serde_json::to_vec(&unsigned).ok()
    }

    /// Signs the grade with this installation's key. Returns whether it is
    /// signed: it stays unsigned when the installation has no key (one is
    /// made by an `apb` entry point, never here), and an unsigned grade
    /// never counts in a promotion.
    pub fn sign(&mut self) -> bool {
        self.mac = self
            .signed_bytes()
            .and_then(|b| crate::run_origin::tag_if_keyed(GRADE_MAC_DOMAIN, &b));
        self.mac.is_some()
    }

    /// Whether this installation signed the grade as it is.
    pub fn signed_here(&self) -> bool {
        match (self.mac.as_deref(), self.signed_bytes()) {
            (Some(mac), Some(bytes)) => {
                crate::run_origin::verify_tag(GRADE_MAC_DOMAIN, &bytes, mac)
            }
            _ => false,
        }
    }

    /// The grade of criterion `id`, if this grade has one.
    pub fn criterion(&self, id: &str) -> Option<&CriterionGrade> {
        self.criteria.iter().find(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grader_reads_as_kind_and_version() {
        assert_eq!(GraderId::RULES.to_string(), "rules/1");
        assert_eq!(GraderId::HOST.to_string(), "host/1");
        assert!(Level::Partial.is_determinate());
        assert!(!Level::NotEnoughData.is_determinate());
    }
}
