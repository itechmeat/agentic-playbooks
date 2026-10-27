//! The one evidence validator every grader's output goes through, so a
//! level always rests on what the grader actually read:
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

use apb_core::quality::criteria::{CriterionDef, EvidenceKind, ReadKind};
use apb_core::quality::grade::{Cite, CriterionGrade, Level, ViewEnd};
use serde::{Deserialize, Serialize};

use super::reader::ReadState;
use super::sources::RunSources;

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

/// The problems of one citation for a criterion that reads `kinds`, if
/// any.
fn cite_problem(
    cite: &Cite,
    kinds: &[ReadKind],
    sources: &RunSources,
    state: &ReadState,
) -> Option<String> {
    let r = cite.r#ref.as_str();
    if r.starts_with("n:") {
        return Some(format!(
            "`{r}` is an outline entry: outline entries never prove"
        ));
    }
    let Some(rec) = sources.record(r) else {
        return Some(format!("`{r}` names no record of this run"));
    };
    if !sources.views_holding(r).any(|v| kinds.contains(&v.kind)) {
        return Some(format!("`{r}` is outside the views this criterion reads"));
    }
    if !state.full.contains(r) {
        return Some(if state.routed.contains(r) {
            format!("`{r}` was only seen in a listing or a search: read it before citing it")
        } else {
            format!("`{r}` was not read in full")
        });
    }
    match cite.span {
        Some([a, b]) if a >= b || b > rec.text.chars().count() as u64 => {
            Some(format!("the span of `{r}` is outside the record"))
        }
        _ => None,
    }
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
    let mut problems: Vec<String> = p
        .cites
        .iter()
        .filter_map(|c| cite_problem(c, &kinds, sources, state))
        .collect();
    let mut level = p.level;
    let mut reason_code = reason_code_of(&p.reason_code, p.level);
    let mut absence_basis = Vec::new();
    if p.absence {
        if def.criterion.evidence == EvidenceKind::Presence {
            problems.push("this criterion rests on what happened, not on an absence".into());
        }
        for v in sources.views_of(&kinds) {
            let ended = state.ended(v);
            if v.complete && !ended {
                problems.push(format!("an absence needs `{}` read to its end", v.id));
            }
            absence_basis.push(ViewEnd {
                view: v.id.clone(),
                complete: v.complete,
                ended,
            });
        }
        if absence_basis.is_empty() {
            // Nothing of the kinds the criterion reads exists: an absence
            // resting on no view rests on nothing.
            problems.push(
                "the run has no view this criterion reads: an absence needs one read to its end"
                    .into(),
            );
        }
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
