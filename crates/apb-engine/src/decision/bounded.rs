//! The one bounded ask over records outside a run, shared by the model
//! grader and the known-problem recheck: views of numbered records go to
//! the standalone decider whole or not at all.
//!
//! [`ask`] scrubs every record first, then packs the views, required ones
//! first, while the state still fits `privacy.max_state_bytes` ([`pack`]). A view is
//! never clipped: one that does not fit is left out (and reported as not
//! sent), and a required view that does not fit skips the ask with
//! `nothing_fits`. A model answers only with choices, so anything it cites
//! is a record reference apb offered ([`refs_choice`]).

use std::collections::BTreeMap;

use apb_core::decisions::DecisionMode;
use apb_decide::{Answer, ChoiceCriteria, Question, UseSite};
use serde_json::{Map, Value, json};

use super::standalone::{StandaloneDecider, StandaloneOutcome};

/// The option a reference choice adds for "no record".
pub const NONE: &str = "none";
/// The most references one choice offers (the gateways take 255 options,
/// one of which is [`NONE`]).
pub const MAX_REFS: usize = 254;

/// One view of records.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedView {
    pub id: String,
    /// `(reference, text)`, in order.
    pub records: Vec<(String, String)>,
    /// The view holds everything there is (nothing was cut upstream).
    pub complete: bool,
    /// The ask is pointless without this view.
    pub required: bool,
}

/// An answered bounded ask.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedOutcome {
    pub answers: BTreeMap<String, Answer>,
    /// The views sent, each whole.
    pub sent_whole: Vec<String>,
    pub mode: DecisionMode,
    pub calibrated: bool,
}

/// What a bounded ask came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Bounded {
    Answered(BoundedOutcome),
    /// Nothing asked: `nothing_fits`, or the decider's own reason (`off`,
    /// `send`, `budget`, `busy`, `state_too_large`).
    Skipped {
        reason: &'static str,
    },
    Failed {
        error_kind: String,
    },
}

/// The serialized size of `state`.
fn bytes_of(state: &Value) -> usize {
    serde_json::to_string(state).map_or(usize::MAX, |s| s.len())
}

/// One view as the state carries it, its records scrubbed.
fn view_value(decider: &StandaloneDecider, v: &BoundedView) -> Value {
    let records: Map<String, Value> = v
        .records
        .iter()
        .map(|(r, text)| (r.clone(), Value::String(decider.redact(text))))
        .collect();
    json!({ "complete": v.complete, "records": records })
}

/// The views that fit one request, each whole: what [`ask_packed`] sends.
#[derive(Debug, Clone, PartialEq)]
pub struct Packed {
    views: Map<String, Value>,
    /// The ids of the views packed, each whole.
    pub sent_whole: Vec<String>,
}

impl Packed {
    /// Whether view `id` is sent.
    pub fn sends(&self, id: &str) -> bool {
        self.sent_whole.iter().any(|v| v == id)
    }
}

/// Packs as many whole `views` as fit `privacy.max_state_bytes`: the
/// required views first (an optional view never takes the room a required
/// one needs), then the others in the order given. `Err("nothing_fits")`
/// when a required view does not fit or nothing does.
pub fn pack(
    decider: &StandaloneDecider,
    subject: &Value,
    views: &[BoundedView],
) -> Result<Packed, &'static str> {
    let max = decider.max_state_bytes();
    let mut packed = Map::new();
    let mut sent_whole = Vec::new();
    let required_first = views
        .iter()
        .filter(|v| v.required)
        .chain(views.iter().filter(|v| !v.required));
    for v in required_first {
        let mut trial = packed.clone();
        trial.insert(v.id.clone(), view_value(decider, v));
        let state = json!({ "subject": subject, "views": trial });
        if bytes_of(&state) <= max {
            packed = trial;
            sent_whole.push(v.id.clone());
        } else if v.required {
            return Err("nothing_fits");
        }
    }
    if sent_whole.is_empty() && !views.is_empty() {
        return Err("nothing_fits");
    }
    Ok(Packed {
        views: packed,
        sent_whole,
    })
}

/// Asks `questions` about `subject` over the views [`pack`] packed. A
/// caller that offers record references builds them from the packed views
/// only, so a model never cites a record it was not sent.
pub fn ask_packed(
    decider: &StandaloneDecider,
    site: UseSite,
    subject: Value,
    packed: Packed,
    questions: BTreeMap<String, Question>,
) -> Bounded {
    let Packed { views, sent_whole } = packed;
    let state = json!({ "subject": subject, "views": views });
    let order = vec!["subject".to_string(), "views".to_string()];
    match decider.decide(site, state, order, questions) {
        StandaloneOutcome::Answered(a) => Bounded::Answered(BoundedOutcome {
            answers: a.answers,
            sent_whole,
            mode: a.mode,
            calibrated: a.calibrated,
        }),
        StandaloneOutcome::Skipped { reason } => Bounded::Skipped { reason },
        StandaloneOutcome::Failed { error_kind } => Bounded::Failed { error_kind },
    }
}

/// Asks `questions` about `subject` over as many whole `views` as fit
/// ([`pack`], then [`ask_packed`]).
pub fn ask(
    decider: &StandaloneDecider,
    site: UseSite,
    subject: Value,
    views: &[BoundedView],
    questions: BTreeMap<String, Question>,
) -> Bounded {
    match pack(decider, &subject, views) {
        Ok(packed) => ask_packed(decider, site, subject, packed, questions),
        Err(reason) => Bounded::Skipped { reason },
    }
}

/// A choice over at most [`MAX_REFS`] references plus [`NONE`].
pub fn refs_choice(instructions: Value, refs: &[String]) -> Question {
    let criteria: ChoiceCriteria = refs
        .iter()
        .take(MAX_REFS)
        .map(|r| (r.clone(), None))
        .chain(std::iter::once((
            NONE.to_string(),
            Some(Value::String("no record fits".into())),
        )))
        .collect();
    Question::Choice {
        instructions,
        criteria,
    }
}

/// The chosen option of a choice answer and its confidence.
pub fn chosen(answers: &BTreeMap<String, Answer>, question: &str) -> Option<(String, f64)> {
    match answers.get(question)? {
        Answer::Choice {
            value, confidence, ..
        } => Some((value.clone(), *confidence)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decider(root: &std::path::Path) -> StandaloneDecider {
        let cfg = tempfile::tempdir().unwrap();
        std::fs::write(
            cfg.path().join(apb_core::decisions::DECISIONS_FILE),
            "mode: advise\nproviders: [{ id: a, kind: systemone, base_url: 'http://127.0.0.1:1', model: m }]\nprivacy: { max_state_bytes: 1024 }\nuses:\n  grade: { mode: advise }\n",
        )
        .unwrap();
        let settings = apb_core::decisions::load_file(cfg.path()).unwrap().unwrap();
        StandaloneDecider::with_settings(settings, root)
    }

    fn view(id: &str, chars: usize, required: bool) -> BoundedView {
        BoundedView {
            id: id.into(),
            records: vec![(format!("{id}:1"), "word ".repeat(chars / 5))],
            complete: true,
            required,
        }
    }

    #[test]
    fn a_required_view_is_packed_before_an_optional_one_that_would_crowd_it_out() {
        let root = tempfile::tempdir().unwrap();
        let d = decider(root.path());
        let subject = json!({ "s": 1 });
        // Each fits alone; both together do not.
        let views = [view("optional", 600, false), view("needed", 600, true)];
        let packed = pack(&d, &subject, &views).unwrap();
        assert_eq!(packed.sent_whole, vec!["needed".to_string()]);
        assert!(packed.sends("needed") && !packed.sends("optional"));
        // A required view that cannot fit at all still skips the ask.
        assert_eq!(
            pack(&d, &subject, &[view("huge", 4000, true)]).unwrap_err(),
            "nothing_fits"
        );
    }
}
