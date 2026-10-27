//! Rechecking a problem whose source changed: by a decision model through
//! the bounded path (use `problem_recheck`, off unless `decisions.yaml`
//! sets it, capped per project and UTC day), by a host, or by a person.
//! Every proposal goes through [`apb_core::known_problems::recheck::settle`].
//!
//! A model never types a quote: it chooses among the changed passages of
//! the new source (numbered, plus `none`) and apb copies the chosen passage
//! as the quote.
//!
//! A model recheck sends the problem's symptom and workaround (what agents
//! wrote) and changed passages of the source, so it needs `privacy.send` to
//! allow both `outputs` and `diffs`. Its close stands in `enforce` only on
//! a calibrated answer (or `allow_uncalibrated`) at or above
//! `uses.problem_recheck.thresholds.min_confidence`; any other close is a
//! proposal for a person.

use std::collections::BTreeMap;

use apb_core::decisions::{DecisionMode, SendClass};
use apb_core::known_problems::recheck::{
    CheckMeta, ProposedVerdict, RecheckProposal, changed_passages, settle,
};
use apb_core::known_problems::{KnownProblem, RecheckBasis, RecheckRecord};
use apb_decide::{ChoiceCriteria, Question, UseSite};
use serde_json::{Value, json};

use super::source::Current;
use crate::decision::bounded::{self, Bounded, BoundedView, NONE};
use crate::decision::standalone::StandaloneDecider;

/// The most model rechecks one run start asks; the other changed problems
/// are hidden as `source_changed_unchecked`.
pub const MAX_MODEL_RECHECKS_PER_START: usize = 2;

/// The confidence below which a model's close is only proposed, when
/// `uses.problem_recheck.thresholds.min_confidence` sets none.
pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.8;

/// Why a model recheck cannot be asked at all.
pub const MODEL_OFF: &str = "no decision model is on for problem_recheck in this project (uses.problem_recheck, privacy.send with outputs and diffs, and a provider)";
/// Why a model recheck that could be asked gave nothing.
pub const MODEL_SILENT: &str =
    "the decision model did not answer (off, over the daily cap, busy, or failed)";
/// Why a model was not asked about one problem: nothing in it to judge.
pub const NOTHING_TO_ASK: &str = "nothing to ask the decision model about this problem: its source has no new passage to judge, or still looks like it holds a credential; a person rechecks it";

/// Why [`by_model`] made no record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotRechecked {
    /// Nothing was asked: this problem gives the model nothing to judge.
    NothingToAsk,
    /// The model was asked and did not answer (off, over the daily cap,
    /// busy, failed): no other problem will be answered either.
    Silent,
}

impl NotRechecked {
    pub fn message(self) -> &'static str {
        match self {
            NotRechecked::NothingToAsk => NOTHING_TO_ASK,
            NotRechecked::Silent => MODEL_SILENT,
        }
    }
}

/// Whether `e` says the decision model did not answer (as opposed to a
/// reason tied to one problem).
pub fn is_model_silent(e: &crate::error::EngineError) -> bool {
    matches!(e, crate::error::EngineError::Conflict(m) if m == MODEL_SILENT)
}

/// Who rechecks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecheckBy {
    /// The project's decision model, when its `problem_recheck` use is on.
    Model,
    /// The host agent's proposal (a close becomes a proposal for a person).
    Host(RecheckProposal),
    /// A person's verdict (a close needs a quote of new text).
    Person(RecheckProposal),
}

/// The project's decider for rechecks, when the use is on, `privacy.send`
/// allows what a recheck sends (agents' texts and changed source
/// passages), and a provider resolves.
pub fn decider(root: &std::path::Path) -> Option<StandaloneDecider> {
    StandaloneDecider::for_project(root)
        .filter(|d| d.mode_for(UseSite::ProblemRecheck) != DecisionMode::Off)
        .filter(|d| d.sends(SendClass::Outputs) && d.sends(SendClass::Diffs))
        .filter(StandaloneDecider::available)
}

/// The mode a model's answer counts in: an `enforce` answer that is
/// uncalibrated (without `allow_uncalibrated`) or below the confidence
/// threshold counts as `advise`, so its close is only proposed.
fn counted_mode(
    decider: &StandaloneDecider,
    mode: DecisionMode,
    calibrated: bool,
    confidence: f64,
) -> DecisionMode {
    let site = UseSite::ProblemRecheck;
    let threshold = decider
        .threshold(site, "min_confidence")
        .unwrap_or(DEFAULT_MIN_CONFIDENCE);
    let trusted = (calibrated || decider.allows_uncalibrated(site)) && confidence >= threshold;
    if mode == DecisionMode::Enforce && !trusted {
        DecisionMode::Advise
    } else {
        mode
    }
}

fn old_text(p: &KnownProblem) -> Option<&str> {
    p.source.as_ref().and_then(|s| s.excerpt.as_deref())
}

fn now_ms() -> u64 {
    apb_core::clock::now_ms_u64()
}

/// A host's or a person's recheck of `p` against `current`.
pub fn settle_by(
    p: &KnownProblem,
    current: &Current,
    proposal: &RecheckProposal,
    basis: RecheckBasis,
    by: &str,
) -> RecheckRecord {
    settle(
        proposal,
        old_text(p),
        &current.text,
        CheckMeta {
            digest: current.digest.clone(),
            basis,
            mode: None,
            by: by.to_string(),
            at_ms: now_ms(),
        },
    )
}

/// A model's recheck of `p` against `current`, or why there is none (the
/// problem stays unchecked). Nothing is asked when the source has no new
/// passage to judge (its old text is unknown, or the change only removed
/// lines) or still looks like it holds a credential after scrubbing: that
/// is for a person.
pub fn by_model(
    decider: &StandaloneDecider,
    p: &KnownProblem,
    current: &Current,
) -> Result<RecheckRecord, NotRechecked> {
    if current.flagged {
        return Err(NotRechecked::NothingToAsk);
    }
    let passages = changed_passages(old_text(p), &current.text);
    if passages.is_empty() {
        return Err(NotRechecked::NothingToAsk);
    }
    let refs: Vec<String> = passages.iter().map(|q| format!("p{}", q.index)).collect();
    let problem_text = match &p.workaround {
        Some(w) => format!("{}\nWorkaround reported: {w}", p.symptom),
        None => p.symptom.clone(),
    };
    let views = vec![
        BoundedView {
            id: "problem".into(),
            records: vec![("problem".into(), problem_text)],
            complete: true,
            required: true,
        },
        BoundedView {
            id: "changed_passages".into(),
            records: refs
                .iter()
                .cloned()
                .zip(passages.iter().map(|q| q.text.clone()))
                .collect(),
            complete: true,
            required: true,
        },
    ];
    let verdicts: ChoiceCriteria = [
        ("holds", "the changed source does not fix the problem"),
        (
            "closed",
            "one changed passage shows the problem is fixed or no longer applies",
        ),
        ("undetermined", "the passages do not show either way"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), Some(Value::String(v.into()))))
    .collect();
    let questions = BTreeMap::from([
        (
            "verdict".to_string(),
            Question::Choice {
                instructions: json!({
                    "question": "The source of this problem changed. Does the problem still hold after the change? Judge only by the problem and the changed passages.",
                    "source": p.scope.key(),
                }),
                criteria: verdicts,
            },
        ),
        (
            "quote".to_string(),
            bounded::refs_choice(
                json!({ "question": "If the problem is fixed, which changed passage shows the fix?" }),
                &refs,
            ),
        ),
    ]);
    let subject = json!({ "problem": p.id, "scope": p.scope.key() });
    let Bounded::Answered(answered) =
        bounded::ask(decider, UseSite::ProblemRecheck, subject, &views, questions)
    else {
        return Err(NotRechecked::Silent);
    };
    let (verdict, verdict_confidence) =
        bounded::chosen(&answered.answers, "verdict").ok_or(NotRechecked::Silent)?;
    let verdict = match verdict.as_str() {
        "holds" => ProposedVerdict::Holds,
        "closed" => ProposedVerdict::Closed,
        _ => ProposedVerdict::Undetermined,
    };
    let chosen_quote = bounded::chosen(&answered.answers, "quote");
    let confidence = chosen_quote
        .as_ref()
        .map_or(verdict_confidence, |(_, c)| verdict_confidence.min(*c));
    let quote = chosen_quote
        .map(|(r, _)| r)
        .filter(|r| r != NONE)
        .and_then(|r| refs.iter().position(|x| *x == r))
        .map(|i| passages[i].text.clone());
    let mode = counted_mode(decider, answered.mode, answered.calibrated, confidence);
    Ok(settle(
        &RecheckProposal {
            verdict,
            quote,
            why: String::new(),
        },
        old_text(p),
        &current.text,
        CheckMeta {
            digest: current.digest.clone(),
            basis: RecheckBasis::Model,
            mode: Some(mode),
            by: "model".into(),
            at_ms: now_ms(),
        },
    ))
}
