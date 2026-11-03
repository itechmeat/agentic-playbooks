//! The one evidence validator every grader's output goes through, so a
//! level always rests on what the grader actually read. The citations and
//! the absence rule are checked by `crate::citation`; this module adds
//! what is a grade's own (levels, reason codes, correction rounds):
//!
//! - every citation names a record the grader read in full (an outline
//!   entry, a child listing or a search hit routes, it never proves), in a
//!   view of a kind the criterion reads;
//! - a determinate level cites what it rests on, or claims an absence;
//! - "this did not happen" (an absence) needs at least one view of the
//!   kinds the criterion reads, each complete and read to its end; an
//!   incomplete view turns the level into `not_enough_data`;
//! - one level per criterion; a `reason_code` is a short slug
//!   (`[a-z0-9_]`, at most [`REASON_CODE_MAX`] characters) or the level's
//!   name;
//! - one correction round: what is still wrong after it becomes
//!   `not_enough_data`, and so does a level that changed without a record
//!   newly read in full.

use std::collections::BTreeMap;

use apb_core::quality::criteria::{CriterionDef, EvidenceKind};
use apb_core::quality::grade::{Cite, CriterionGrade, Level};
use serde::{Deserialize, Serialize};

use super::reader::ReadState;
use super::sources::RunSources;
use crate::citation::{self, Checked, Claim};

/// The longest `why` a grade keeps.
pub const WHY_MAX: usize = 300;
/// The longest `reason_code` a grade keeps.
pub const REASON_CODE_MAX: usize = 40;

/// `code` when it is a short slug, else the level's own name: a reason
/// code never carries run text or terminal escapes into a grade file.
fn reason_code_of(code: &str, level: Level) -> String {
    let slug = (1..=REASON_CODE_MAX).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if slug {
        code.to_string()
    } else {
        level.as_str().to_string()
    }
}

/// What a grader proposes for one criterion.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub criterion: String,
    pub level: Level,
    #[serde(default)]
    pub reason_code: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub cites: Vec<Cite>,
    /// Set to claim that something did not happen: the claim rests on every
    /// view the criterion reads.
    #[serde(default)]
    pub absence: bool,
}

/// Why a proposal was refused.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub criterion: String,
    pub problem: String,
}

/// The levels of a first round that was refused, and how many records had
/// been read in full by then: a level may change in the correction round
/// only after something more was read in full (a listing, a search or an
/// outline routes and never counts).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub levels: BTreeMap<String, Level>,
    #[serde(default)]
    pub full_at: usize,
}

impl Round {
    pub fn of(proposals: &[Proposal], state: &ReadState) -> Self {
        Round {
            levels: proposals
                .iter()
                .map(|p| (p.criterion.clone(), p.level))
                .collect(),
            full_at: state.full.len(),
        }
    }
}

/// The outcome of validating one round.
#[derive(Debug, Clone, PartialEq)]
pub enum Validated {
    Accepted(Vec<CriterionGrade>),
    Rejected(Vec<Rejection>),
}

/// Validates one proposal against its criterion: the grade it stands for,
/// or the problems found.
fn check(
    p: &Proposal,
    def: &CriterionDef,
    sources: &RunSources,
    state: &ReadState,
) -> Result<CriterionGrade, Vec<String>> {
    let kinds = def.reads();
    let claim = Claim {
        cites: &p.cites,
        absence: p.absence,
        absence_allowed: def.criterion.evidence != EvidenceKind::Presence,
    };
    let Checked {
        mut problems,
        absence_basis,
    } = citation::check(&claim, &kinds, &sources.views, state);
    let mut level = p.level;
    let mut reason_code = reason_code_of(&p.reason_code, p.level);
    if p.absence {
        if level != Level::NotEnoughData && absence_basis.iter().any(|v| !v.complete) {
            level = Level::NotEnoughData;
            reason_code = "incomplete_view".into();
        }
    } else if p.level.is_determinate() && p.cites.is_empty() {
        problems.push("cite the records the level rests on, or claim an absence".into());
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(CriterionGrade {
        id: def.id().to_string(),
        source: def.source,
        digest: def.digest.clone(),
        subject: def.criterion.subject.key(),
        level,
        reason_code,
        why: p.why.chars().take(WHY_MAX).collect(),
        cites: p.cites.clone(),
        absence_basis,
        counts_for_promotion: true,
    })
}

/// A criterion graded `not_enough_data` because its evidence did not hold.
fn not_enough(def: &CriterionDef, reason_code: &str, why: String) -> CriterionGrade {
    CriterionGrade {
        id: def.id().to_string(),
        source: def.source,
        digest: def.digest.clone(),
        subject: def.criterion.subject.key(),
        level: Level::NotEnoughData,
        reason_code: reason_code.to_string(),
        why: why.chars().take(WHY_MAX).collect(),
        cites: Vec::new(),
        absence_basis: Vec::new(),
        counts_for_promotion: true,
    }
}

/// Validates the proposals for `defs`. A first round (`prior: None`) with
/// any problem is refused whole; a correction round is always accepted,
/// with each criterion that still fails, or that changed its level without
/// new reading, graded `not_enough_data`.
pub fn validate(
    proposals: &[Proposal],
    defs: &[CriterionDef],
    sources: &RunSources,
    state: &ReadState,
    prior: Option<&Round>,
) -> Validated {
    let mut grades = Vec::new();
    let mut rejections = Vec::new();
    for p in proposals {
        if !defs.iter().any(|d| d.id() == p.criterion) {
            rejections.push(Rejection {
                criterion: p.criterion.clone(),
                problem: "no such criterion in this grading".into(),
            });
        }
    }
    for def in defs {
        let mut of_def = proposals.iter().filter(|p| p.criterion == def.id());
        let Some(p) = of_def.next() else {
            match prior {
                None => rejections.push(Rejection {
                    criterion: def.id().to_string(),
                    problem: "no level proposed".into(),
                }),
                Some(_) => grades.push(not_enough(
                    def,
                    "not_graded",
                    "no level was proposed".into(),
                )),
            }
            continue;
        };
        if of_def.next().is_some() {
            // Two levels for one criterion: which one stands is never left
            // to the order they were sent in.
            match prior {
                None => rejections.push(Rejection {
                    criterion: def.id().to_string(),
                    problem: "more than one level proposed".into(),
                }),
                Some(_) => grades.push(not_enough(
                    def,
                    "more_than_one_level",
                    "more than one level was proposed".into(),
                )),
            }
            continue;
        }
        let changed_unread = prior.is_some_and(|r| {
            r.levels.get(def.id()).is_some_and(|l| *l != p.level) && state.full.len() <= r.full_at
        });
        match (check(p, def, sources, state), prior) {
            (Ok(_), Some(_)) if changed_unread => grades.push(not_enough(
                def,
                "changed_without_reading",
                "the level changed in the correction round without anything new being read".into(),
            )),
            (Ok(g), _) => grades.push(g),
            (Err(problems), None) => {
                rejections.extend(problems.into_iter().map(|problem| Rejection {
                    criterion: def.id().to_string(),
                    problem,
                }))
            }
            (Err(problems), Some(_)) => {
                grades.push(not_enough(def, "evidence_rejected", problems.join("; ")))
            }
        }
    }
    if prior.is_none() && !rejections.is_empty() {
        Validated::Rejected(rejections)
    } else {
        Validated::Accepted(grades)
    }
}
