//! The decision-model grader (`model/1`): grades the playbook's own
//! criteria of a finished run by asking the standalone decider (use
//! `grade`, off unless `decisions.yaml` sets it, capped per project and
//! UTC day) over whole views of the run ([`crate::decision::bounded`]).
//!
//! Per criterion it asks the `level` (a choice over the five levels, with
//! the criterion's own level texts) and, per determinate level, the record
//! that supports it (`support_<level>`, a choice over the record references
//! plus `none`); the cite is the support of the chosen level. Every view
//! sent is read through the [`Reader`] in full, so the trace is honest and
//! the one evidence validator checks the result like any other grader's:
//!
//! - `none` on a criterion resting on what happened is `not_enough_data`;
//! - an absence is claimed only when every view the criterion reads was
//!   sent whole and is complete;
//! - an answer below `uses.grade.thresholds.min_confidence` is
//!   `not_enough_data`;
//! - its levels count for a promotion only when the use is in `enforce`
//!   and the provider is calibrated.

use std::cell::RefCell;
use std::collections::BTreeMap;

use apb_core::decisions::DecisionMode;
use apb_core::quality::criteria::{CriterionDef, CriterionSource, EvidenceKind};
use apb_core::quality::grade::{CriterionGrade, GraderId, Level};
use apb_decide::{ChoiceCriteria, Question, UseSite};
use serde_json::{Value, json};

use super::Grader;
use super::evidence::Proposal;
use super::reader::Reader;
use super::sources::RunSources;
use crate::decision::bounded::{self, Bounded, BoundedView, NONE};
use crate::decision::standalone::StandaloneDecider;

/// The confidence below which a model's level is `not_enough_data`, when
/// `uses.grade.thresholds.min_confidence` sets none.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.7;

const LEVELS: [Level; 5] = [
    Level::Good,
    Level::Partial,
    Level::Poor,
    Level::NotApplicable,
    Level::NotEnoughData,
];

/// What the grader learned from the decider while grading.
#[derive(Debug, Default)]
struct Asked {
    /// A reason that stops every further ask (`off`, `budget`, ...).
    stopped: Option<&'static str>,
    /// Whether every answer came in `enforce` from a calibrated provider.
    counts: Option<bool>,
}

/// The model grader over one decider.
pub struct ModelGrader<'a> {
    decider: &'a StandaloneDecider,
    asked: RefCell<Asked>,
}

impl<'a> ModelGrader<'a> {
    pub fn new(decider: &'a StandaloneDecider) -> Self {
        ModelGrader {
            decider,
            asked: RefCell::new(Asked::default()),
        }
    }

    /// The reason the decider stopped every ask, if it did.
    pub fn stopped(&self) -> Option<&'static str> {
        self.asked.borrow().stopped
    }

    /// Whether the decider answered at least one criterion.
    pub fn answered_any(&self) -> bool {
        self.asked.borrow().counts.is_some()
    }

    fn min_confidence(&self) -> f64 {
        self.decider
            .threshold(UseSite::Grade, "min_confidence")
            .unwrap_or(DEFAULT_MIN_CONFIDENCE)
    }

    fn propose_one(
        &self,
        sources: &RunSources,
        def: &CriterionDef,
        reader: &mut Reader<'_>,
    ) -> Proposal {
        let kinds = def.reads();
        let views: Vec<BoundedView> = sources
            .views_of(&kinds)
            .map(|v| BoundedView {
                id: v.id.clone(),
                records: v
                    .records
                    .iter()
                    .map(|r| (r.r#ref.clone(), r.text.clone()))
                    .collect(),
                complete: v.complete,
                required: false,
            })
            .collect();
        let subject = json!({
            "criterion": def.id(),
            "about": def.criterion.subject.key(),
            "applies_when": def.criterion.applies_when,
            "outcome": sources.outcome,
        });
        // Pack first: the model is offered only references of the views it
        // is sent, each once.
        let packed = match bounded::pack(self.decider, &subject, &views) {
            Ok(p) => p,
            Err(reason) => return not_enough(def, reason),
        };
        let sent: Vec<&BoundedView> = views.iter().filter(|v| packed.sends(&v.id)).collect();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let refs: Vec<String> = sent
            .iter()
            .flat_map(|v| &v.records)
            .filter(|(r, _)| seen.insert(r.as_str()))
            .map(|(r, _)| r.clone())
            .collect();
        let answered = match bounded::ask_packed(
            self.decider,
            UseSite::Grade,
            subject,
            packed,
            questions(def, &refs),
        ) {
            Bounded::Answered(a) => a,
            Bounded::Skipped {
                reason: "nothing_fits",
            } => return not_enough(def, "nothing_fits"),
            Bounded::Skipped { reason } => {
                self.asked.borrow_mut().stopped = Some(reason);
                return not_enough(def, reason);
            }
            Bounded::Failed { .. } => return not_enough(def, "model_failed"),
        };
        {
            let mut asked = self.asked.borrow_mut();
            let counts = answered.mode == DecisionMode::Enforce && answered.calibrated;
            asked.counts = Some(asked.counts.unwrap_or(true) && counts);
        }
        // Every record sent counts as read in full, through the reader; a
        // record an earlier criterion read is not charged again. A record
        // the reader could not take whole leaves the criterion without the
        // evidence its answer rests on.
        for r in &refs {
            if reader.state.full.contains(r) {
                continue;
            }
            let read = reader.read(r, None);
            if read.is_err() || !reader.state.full.contains(r) {
                // The budget is spent: a later criterion would pay for an
                // answer it could not use either, so nothing more is asked.
                self.asked.borrow_mut().stopped = Some("read_budget");
                return not_enough(def, "read_budget");
            }
        }
        let Some((level, confidence)) = bounded::chosen(&answered.answers, "level") else {
            return not_enough(def, "no_answer");
        };
        let Some(level) = LEVELS.iter().copied().find(|l| l.as_str() == level) else {
            return not_enough(def, "no_answer");
        };
        if confidence < self.min_confidence() {
            return not_enough(def, "low_confidence");
        }
        if !level.is_determinate() {
            return proposal(def, level, "model", Vec::new(), false);
        }
        let support = bounded::chosen(&answered.answers, &format!("support_{}", level.as_str()))
            .map(|(r, _)| r)
            .filter(|r| r != NONE);
        if let Some(r) = support {
            return proposal(
                def,
                level,
                "model",
                vec![apb_core::quality::grade::Cite::to(r)],
                false,
            );
        }
        // An absence needs every view sent whole and complete, and every
        // record offered as a cite: past [`bounded::MAX_REFS`] the model
        // could not name the record that shows it.
        let all_sent_whole = sources
            .views_of(&kinds)
            .all(|v| v.complete && answered.sent_whole.contains(&v.id));
        let all_offered = refs.len() <= bounded::MAX_REFS;
        if def.criterion.evidence != EvidenceKind::Presence && all_sent_whole && all_offered {
            proposal(def, level, "model_absence", Vec::new(), true)
        } else {
            not_enough(def, "no_support")
        }
    }
}

fn proposal(
    def: &CriterionDef,
    level: Level,
    reason_code: &str,
    cites: Vec<apb_core::quality::grade::Cite>,
    absence: bool,
) -> Proposal {
    Proposal {
        criterion: def.id().to_string(),
        level,
        reason_code: reason_code.to_string(),
        why: format!("graded by a decision model: {}", level.as_str()),
        cites,
        absence,
    }
}

fn not_enough(def: &CriterionDef, reason_code: &str) -> Proposal {
    proposal(def, Level::NotEnoughData, reason_code, Vec::new(), false)
}

/// The questions of one criterion.
fn questions(def: &CriterionDef, refs: &[String]) -> BTreeMap<String, Question> {
    let mut level_texts: BTreeMap<&str, String> = def
        .criterion
        .levels
        .named()
        .iter()
        .map(|(name, text)| (*name, text.to_string()))
        .collect();
    level_texts.insert(
        Level::NotApplicable.as_str(),
        match &def.criterion.applies_when {
            Some(when) => format!("the criterion applies only when: {when}; it does not here"),
            None => "the criterion does not apply to this run".into(),
        },
    );
    level_texts.insert(
        Level::NotEnoughData.as_str(),
        "the records sent do not show which level fits".into(),
    );
    let criteria: ChoiceCriteria = LEVELS
        .iter()
        .map(|l| {
            (
                l.as_str().to_string(),
                level_texts.get(l.as_str()).cloned().map(Value::String),
            )
        })
        .collect();
    let mut out = BTreeMap::from([(
        "level".to_string(),
        Question::Choice {
            instructions: json!({
                "question": "Which level of this criterion did the run reach? Judge only by the records in the state.",
                "criterion": def.id(),
            }),
            criteria,
        },
    )]);
    for (name, text) in def.criterion.levels.named() {
        out.insert(
            format!("support_{name}"),
            bounded::refs_choice(
                json!({
                    "question": "If the level is this one, which single record shows it best?",
                    "level": name,
                    "meaning": text,
                }),
                refs,
            ),
        );
    }
    out
}

impl Grader for ModelGrader<'_> {
    fn id(&self) -> GraderId {
        GraderId::MODEL
    }

    fn grades(&self, def: &CriterionDef) -> bool {
        def.source == CriterionSource::Playbook
    }

    fn propose(
        &self,
        sources: &RunSources,
        defs: &[CriterionDef],
        reader: &mut Reader<'_>,
    ) -> Vec<Proposal> {
        defs.iter()
            .map(|def| match self.stopped() {
                Some(reason) => not_enough(def, reason),
                None => self.propose_one(sources, def, reader),
            })
            .collect()
    }

    /// A model's levels count for a promotion only in `enforce` with a
    /// calibrated provider; its "does not apply" never does, and neither
    /// does a grade stopped part way (the criteria it did not ask would
    /// count as `not_enough_data` until a later request regrades it).
    fn settle(&self, criteria: &mut [CriterionGrade]) {
        let asked = self.asked.borrow();
        let counts = asked.counts.unwrap_or(false) && asked.stopped.is_none();
        for c in criteria {
            c.counts_for_promotion = counts && c.level != Level::NotApplicable;
        }
    }
}
