//! Review gate recommendation (issue #165 Part 11, Part 14.4).
//!
//! Asked once per gate visit, before `review_requested`. The state is the
//! gate (title and literal `prompt`) and the outputs of its direct
//! predecessors that ran (6 kB tail each, by node id); the one question,
//! `decision`, is a choice over the gate's effective options with the
//! `option_descriptions` as criteria.
//!
//! Shadow journals only. Advise puts `recommendation` on `review_requested`
//! and one sentence in its instruction; the gate still waits for a person
//! and nothing is preselected. Enforce, the one fail-closed use: with
//! `auto_decide` on the gate, a stored threshold and no refusal, a
//! recommendation of an allowed option (`needs_changes` only) at the
//! higher of the stored threshold and `auto_decide.min_confidence` is posted
//! through the review channel as the decision, noted `auto: <provider>/<model>
//! p=<p>`, so it is journaled as an ordinary `review_decided`. A decision a
//! person posted first wins (the channel refuses a second one). Every other
//! outcome, a provider failure included, waits for the person.

use std::collections::BTreeMap;

use apb_core::decisions::DecisionMode;
use apb_core::schema::AutoDecide;
use apb_decide::{Question, UseSite};
use serde_json::{Value, json};

use super::{
    DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, Enforce, FieldClass,
    Judgement, StateField, StateParts,
};
use crate::event::{DecisionAnswer, ReviewRecommendation};

const GATE_HEAD: usize = 4 * 1024;
/// Each predecessor's output tail.
pub(crate) const INPUT_TAIL: usize = 6 * 1024;

/// The gate visit, as the drive knows it.
pub(crate) struct Gate<'a> {
    pub(crate) node: &'a str,
    pub(crate) title: Option<&'a str>,
    pub(crate) prompt: Option<&'a str>,
    /// The effective options, in order.
    pub(crate) options: &'a [String],
    pub(crate) descriptions: &'a BTreeMap<String, String>,
    /// `(node id, output)` of each direct predecessor that ran.
    pub(crate) inputs: Vec<(String, String)>,
    /// 1 for the first visit of the gate in this run.
    pub(crate) visit: u32,
    pub(crate) auto_decide: Option<&'a AutoDecide>,
    /// Why an automatic decision is refused here (effects, a shipping step).
    pub(crate) refusal: Option<&'static str>,
}

pub(crate) fn questions(g: &Gate) -> BTreeMap<String, Question> {
    BTreeMap::from([(
        "decision".to_string(),
        Question::Choice {
            instructions: json!(
                "Given the material in `inputs` and the guidance in `gate`, which option would the reviewer most likely choose?"
            ),
            criteria: g
                .options
                .iter()
                .map(|o| (o.clone(), g.descriptions.get(o).map(|d| json!(d))))
                .collect(),
        },
    )])
}

fn decision(answers: &BTreeMap<String, DecisionAnswer>) -> Option<(&str, f64, f64)> {
    let a = answers.get("decision")?;
    Some((
        a.value.as_ref()?.as_str()?,
        a.p.unwrap_or(0.0),
        a.confidence.unwrap_or(0.0),
    ))
}

/// The recommendation and, when enforced, the decision to post.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Recommendation {
    pub(crate) recommendation: Option<ReviewRecommendation>,
    /// Enforced: `(option, note)` to post through the review channel.
    pub(crate) auto: Option<(String, String)>,
}

/// The instruction sentence of an advisory recommendation.
pub(crate) fn instruction_line(r: &ReviewRecommendation) -> String {
    format!(" Advisory recommendation: {} (p={:.2}).", r.option, r.p)
}

/// The predecessors' outputs as one text, each tail-clipped.
fn inputs_text(inputs: &[(String, String)]) -> String {
    inputs
        .iter()
        .map(|(id, out)| {
            let chars = out.chars().count();
            let tail: String = out.chars().skip(chars.saturating_sub(INPUT_TAIL)).collect();
            format!("## {id}\n{tail}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Asks once for this visit. Fail-open for the recommendation, fail-closed
/// for the automatic decision: anything but an enforced answer waits.
pub(crate) fn recommend(
    runner: &DecisionRunner,
    journal: &dyn DecisionJournal,
    g: Gate,
) -> Recommendation {
    let mode = runner.mode_for(UseSite::ReviewTriage);
    if mode == DecisionMode::Off || g.options.is_empty() {
        return Recommendation::default();
    }
    let gate_text = [g.title.unwrap_or(g.node), g.prompt.unwrap_or("")]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n");
    let inputs = inputs_text(&g.inputs);
    let input_bytes = inputs.len();
    let mut meta = serde_json::Map::new();
    meta.insert(
        "inputs".into(),
        Value::Array(g.inputs.iter().map(|(id, _)| json!(id)).collect()),
    );
    let judge = |_: &BTreeMap<String, DecisionAnswer>| Judgement::default();
    let (allow, min_confidence) = match g.auto_decide {
        Some(ad) => (ad.allow.clone(), ad.min_confidence),
        None => (Vec::new(), 1.0),
    };
    let acts = move |answers: &BTreeMap<String, DecisionAnswer>, threshold: f64| {
        matches!(decision(answers), Some((o, _, c))
            if allow.iter().any(|a| a == o) && o != "approve" && c >= threshold.max(min_confidence))
    };
    let outcome = runner.decide(
        journal,
        DecisionCall {
            site: UseSite::ReviewTriage,
            node: Some(g.node),
            attempt: Some(g.visit),
            state: StateParts {
                fields: vec![
                    StateField {
                        name: "gate",
                        class: FieldClass::Prompt,
                        text: gate_text,
                        head: GATE_HEAD,
                        tail: 0,
                    },
                    StateField {
                        name: "inputs",
                        class: FieldClass::Output,
                        text: inputs,
                        head: 0,
                        tail: input_bytes.max(1),
                    },
                ],
                meta,
            },
            questions: questions(&g),
            baseline: None,
            judge: &judge,
            join_from: None,
            join: BTreeMap::from([("gate_visit".to_string(), Value::from(g.visit))]),
            enforce: Some(Enforce {
                opted_in: g.auto_decide.is_some(),
                refused: g.refusal,
                acts: &acts,
            }),
        },
    );
    let DecisionOutcome::Answered {
        answers,
        mode,
        meta,
        ..
    } = outcome
    else {
        return Recommendation::default();
    };
    if mode < DecisionMode::Advise {
        return Recommendation::default();
    }
    let Some((option, p, confidence)) = decision(&answers) else {
        return Recommendation::default();
    };
    if !g.options.iter().any(|o| o == option) {
        return Recommendation::default();
    }
    let provider = meta.provider.clone().unwrap_or_default();
    let model = meta.model.clone().unwrap_or_default();
    let auto = meta
        .applied
        .then(|| (option.to_string(), format!("auto: {provider}/{model} p={p:.2}")));
    Recommendation {
        recommendation: Some(ReviewRecommendation {
            option: option.to_string(),
            p,
            confidence,
            provider,
            model,
            calibrated: meta.calibrated,
            applied: meta.applied,
        }),
        auto,
    }
}
