//! One judge evaluation for a run, a scenario run and the judge sandbox:
//! render the state from [`JudgeInputs`], ask, apply thresholds and the
//! mode rule, and say what the node would output ([`JudgeEvaluation`]).
//!
//! These are the shared types. The run path renders its inputs from the
//! run state; a scenario run answers from its answer book; the sandbox
//! takes typed inputs or a past run's values. All three go through the
//! same evaluation, so they agree on the state digest, the raw answers and
//! the output.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::decisions::DecisionMode;
use apb_core::judge::JudgeFallback;
use apb_core::schema::{NodeKind, Playbook};
use apb_decide::{Question, UseSite};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::decision::judge::{
    differs_from_fallback, fallback_output, node_questions, output_from_answers,
};
use crate::decision::{
    AnswerSource, DecisionCall, DecisionJournal, DecisionOutcome, DecisionRunner, Judgement, Route,
    StateParts,
};
use crate::error::EngineError;
use crate::event::DecisionAnswer;

pub mod example;
pub(crate) mod inputs;
pub mod sandbox;

pub use inputs::{JudgeInputs, RenderedField, ReviewInput};

/// What the node does with the answers on this machine, at its mode.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JudgeOutcome {
    /// The answers apply: the node outputs this.
    Output {
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        output: Map<String, Value>,
    },
    /// The declared `on_unavailable` fallback applies.
    Fallback {
        reason: String,
        #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
        output: Map<String, Value>,
    },
    /// The node fails.
    Fail { reason: String },
    /// The node hands over to its emulation (the `llm_emulation`
    /// providers, then its profile).
    Emulate { reason: String },
}

/// One judge evaluation.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeEvaluation {
    /// `apb_core::judge::node_digest` of the node's definition.
    pub node_digest: String,
    pub state_digest: String,
    pub questions_digest: String,
    pub fields: Vec<RenderedField>,
    /// The answer per question as asked, an invalid one kept with its
    /// reason.
    pub raw: BTreeMap<String, DecisionAnswer>,
    /// The `judge_node` use's mode; absent without decision settings.
    #[serde(default)]
    pub mode: Option<DecisionMode>,
    /// What the node outputs if the answers applied (`output`), or why
    /// they cannot (`fail`).
    pub at_enforce: JudgeOutcome,
    /// What the node does on this machine at its mode.
    pub as_configured: JudgeOutcome,
    /// Whether applying the answers would change the output, when the mode
    /// keeps them from applying.
    #[serde(default)]
    pub would_change: Option<bool>,
    pub requests: u32,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// One judge node to evaluate: its definition, its inputs, and where a
/// clipped output's note points (the run directory in a run, a scratch
/// directory outside one).
pub(crate) struct JudgeRequest<'a> {
    pub(crate) playbook: &'a Playbook,
    pub(crate) node_id: &'a str,
    pub(crate) inputs: &'a JudgeInputs,
    pub(crate) clip_dir: &'a Path,
}

/// Where the answers come from.
pub(crate) enum Answers<'a> {
    /// Asked through a decision runner (a run, a scenario run, the
    /// sandbox), journaled through `journal`. `None`: the run has no
    /// decision settings, and the reason reads `not_configured`.
    Runner {
        runner: Option<&'a DecisionRunner>,
        journal: &'a dyn DecisionJournal,
    },
    /// Answers already given (a labelled example's observed answers):
    /// no request, no journal. `by` names who gave them.
    Given {
        answers: &'a BTreeMap<String, DecisionAnswer>,
        by: &'a str,
    },
    /// Answers already given, applied as this machine's `judge_node` use
    /// applies an answer it asked for: at enforce they apply, below it the
    /// declared fallback does (reason `mode` or `off`, `not_configured`
    /// when `mode` is absent). No request, no journal.
    GivenAt {
        answers: &'a BTreeMap<String, DecisionAnswer>,
        by: &'a str,
        mode: Option<DecisionMode>,
    },
}

/// [`evaluate_judge`] over answers already given (a labelled example's
/// observed answers, a scenario's typed ones): no request, no journal.
/// `by` names who gave them.
pub fn evaluate_given(
    playbook: &Playbook,
    node_id: &str,
    inputs: &JudgeInputs,
    clip_dir: &Path,
    answers: &BTreeMap<String, DecisionAnswer>,
    by: &str,
) -> Result<JudgeEvaluation, EngineError> {
    evaluate_judge(
        &JudgeRequest {
            playbook,
            node_id,
            inputs,
            clip_dir,
        },
        Answers::Given { answers, by },
    )
}

/// [`evaluate_judge`] with the rendered state and questions a run's
/// emulation steps ask again.
pub(crate) struct Evaluated {
    pub(crate) evaluation: JudgeEvaluation,
    pub(crate) parts: StateParts,
    pub(crate) wire: BTreeMap<String, Question>,
}

/// Evaluates one judge node over `req.inputs`: renders the state, asks (or
/// takes the given answers), applies the thresholds, the mode rule and the
/// declared `on_unavailable`, and says what the node outputs at enforce and
/// on this machine. The one place every judge evaluation goes through.
pub(crate) fn evaluate_judge(
    req: &JudgeRequest,
    answers: Answers,
) -> Result<JudgeEvaluation, EngineError> {
    evaluate(req, answers).map(|e| e.evaluation)
}

/// [`evaluate_judge`], keeping the rendered state and questions.
pub(crate) fn evaluate(req: &JudgeRequest, answers: Answers) -> Result<Evaluated, EngineError> {
    let node = req
        .playbook
        .node(req.node_id)
        .ok_or_else(|| EngineError::NotFound(req.node_id.into()))?;
    let NodeKind::Judge {
        questions,
        thresholds,
        on_unavailable,
        ..
    } = &node.kind
    else {
        return Err(EngineError::Invalid(format!(
            "node `{}` is not a judge",
            req.node_id
        )));
    };
    let fallback = on_unavailable.as_ref();
    let (parts, fields) =
        inputs::render_state(req.playbook, req.node_id, req.inputs, req.clip_dir)?;
    let wire = node_questions(questions);
    let questions_digest = apb_decide::digest::questions_digest(&wire);
    // Whether the answers would act, given whether the use is at enforce.
    let judge = |answers: &BTreeMap<String, DecisionAnswer>, enforce: bool| {
        let derived = output_from_answers(questions, thresholds, answers, "").ok();
        Judgement {
            applied: enforce && derived.is_some(),
            would_change: match (&derived, enforce) {
                (_, true) => None,
                (Some(d), false) => differs_from_fallback(d, fallback, questions, thresholds),
                (None, false) => None,
            },
        }
    };
    let (asked, state_digest, mode, requests, cost_usd) = match answers {
        Answers::Runner { runner, journal } => {
            let before = runner.map(DecisionRunner::spent);
            let asked = ask(runner, journal, req.node_id, &parts, &wire, &judge);
            let after = runner.map(DecisionRunner::spent);
            let (requests, cost) = match (before, after) {
                (Some(b), Some(a)) => (a.0.saturating_sub(b.0), Some(a.1 - b.1)),
                _ => (0, None),
            };
            (
                asked,
                crate::decision::state_digest(runner, &parts),
                runner.map(|r| r.mode_for(UseSite::JudgeNode)),
                requests,
                cost.filter(|c| *c > 0.0),
            )
        }
        Answers::Given { answers, by } => (
            Asked {
                verdict: Verdict::Answered,
                raw: answers.clone(),
                by: Some(by.to_string()),
            },
            crate::decision::state_digest(None, &parts),
            None,
            0,
            None,
        ),
        Answers::GivenAt { answers, by, mode } => (
            Asked {
                verdict: match mode {
                    None => Verdict::Unavailable("not_configured".into()),
                    Some(DecisionMode::Off) => Verdict::Unavailable("off".into()),
                    Some(DecisionMode::Enforce) => Verdict::Answered,
                    Some(_) => Verdict::Unavailable("mode".into()),
                },
                raw: answers.clone(),
                by: Some(by.to_string()),
            },
            crate::decision::state_digest(None, &parts),
            mode,
            0,
            None,
        ),
    };
    let by = asked.by.clone().unwrap_or_else(|| "unknown".to_string());
    let derived = output_from_answers(questions, thresholds, &asked.raw, &by);
    let at_enforce = match &derived {
        Ok(output) => JudgeOutcome::Output {
            output: output.clone(),
        },
        Err(reason) => JudgeOutcome::Fail {
            reason: reason.clone(),
        },
    };
    let reason = match (&asked.verdict, &derived) {
        (Verdict::Answered, Ok(_)) => None,
        (Verdict::Answered, Err(_)) => Some("invalid".to_string()),
        (Verdict::Unavailable(reason), _) => Some(reason.clone()),
    };
    let as_configured = match reason {
        None => at_enforce.clone(),
        Some(reason) => match fallback_output(fallback, questions, thresholds, &reason) {
            Some(output) => JudgeOutcome::Fallback { reason, output },
            None if matches!(fallback, Some(JudgeFallback::Emulate)) => {
                JudgeOutcome::Emulate { reason }
            }
            None => JudgeOutcome::Fail {
                reason: format!(
                    "judge node `{}` has no usable answer ({reason}) and on_unavailable is fail",
                    req.node_id
                ),
            },
        },
    };
    let would_change = match (&mode, &derived) {
        (Some(m), Ok(d)) if *m != DecisionMode::Enforce && !asked.raw.is_empty() => {
            differs_from_fallback(d, fallback, questions, thresholds)
        }
        _ => None,
    };
    Ok(Evaluated {
        evaluation: JudgeEvaluation {
            node_digest: node_digest(req.playbook, req.node_id),
            state_digest,
            questions_digest,
            fields,
            raw: asked.raw,
            mode,
            at_enforce,
            as_configured,
            would_change,
            requests,
            cost_usd,
        },
        parts,
        wire,
    })
}

/// What asking came to: the answers as given (empty when nothing came
/// back), who gave them, and whether they apply.
pub(crate) struct Asked {
    pub(crate) verdict: Verdict,
    pub(crate) raw: BTreeMap<String, DecisionAnswer>,
    pub(crate) by: Option<String>,
}

/// Whether answers apply.
pub(crate) enum Verdict {
    /// Answered with the `judge_node` use at enforce.
    Answered,
    /// No usable answer, and why: `not_configured`, `off`, `mode` (below
    /// enforce), a skip reason or an error kind.
    Unavailable(String),
}

/// Asks a judge's questions over `parts` through the run's decision runner
/// (journal first, replay on resume): the one path a judge node and a list
/// item's judge check share. `judge` tells the runner whether the answers
/// would act, given whether the use is at enforce.
pub(crate) fn ask(
    decisions: Option<&DecisionRunner>,
    journal: &dyn DecisionJournal,
    node_id: &str,
    parts: &StateParts,
    wire: &BTreeMap<String, Question>,
    judge: &dyn Fn(&BTreeMap<String, DecisionAnswer>, bool) -> Judgement,
) -> Asked {
    let unavailable = |reason: &str| Asked {
        verdict: Verdict::Unavailable(reason.to_string()),
        raw: BTreeMap::new(),
        by: None,
    };
    let Some(runner) = decisions else {
        return unavailable("not_configured");
    };
    let mode = runner.mode_for(UseSite::JudgeNode);
    if mode == DecisionMode::Off {
        return unavailable("off");
    }
    let enforce = mode == DecisionMode::Enforce;
    let judged = |answers: &BTreeMap<String, DecisionAnswer>| judge(answers, enforce);
    let (outcome, source) =
        runner.decide_routed(journal, call(node_id, parts, wire, &judged), Route::Native);
    let by = source.as_ref().map(decided_by);
    match outcome {
        DecisionOutcome::Answered { answers, .. } => Asked {
            verdict: match enforce {
                true => Verdict::Answered,
                false => Verdict::Unavailable("mode".into()),
            },
            raw: answers,
            by,
        },
        DecisionOutcome::Skipped { reason } => unavailable(reason),
        DecisionOutcome::Failed { error_kind } => unavailable(&error_kind),
    }
}

/// `<provider>/<model>`: who answered, as a judge's output names it.
pub(crate) fn decided_by(s: &AnswerSource) -> String {
    format!("{}/{}", s.provider, s.model)
}

/// A judge node's own call to the runner.
pub(crate) fn call<'a>(
    node_id: &'a str,
    parts: &StateParts,
    wire: &BTreeMap<String, Question>,
    judge: &'a dyn Fn(&BTreeMap<String, DecisionAnswer>) -> Judgement,
) -> DecisionCall<'a> {
    DecisionCall {
        edge_addresses: BTreeMap::new(),
        enforce: None,
        join: BTreeMap::new(),
        join_from: None,
        site: UseSite::JudgeNode,
        node: Some(node_id),
        attempt: None,
        state: parts.clone(),
        questions: wire.clone(),
        baseline: None,
        judge,
    }
}

/// The edges a node's output `out` would take, by edge address: the
/// routing rule over a state holding only that output, the node
/// succeeded. Edge budgets, joins and judge edges are not modelled.
pub fn route_of(playbook: &Playbook, node_id: &str, out: &Map<String, Value>) -> Vec<String> {
    let mut state = crate::state::RunState::default();
    state
        .nodes
        .insert(node_id.to_string(), crate::state::NodeStatus::Succeeded);
    state
        .outputs
        .insert(node_id.to_string(), crate::decision::judge::render(out));
    state.finished_counts.insert(node_id.to_string(), 1);
    let addresses = apb_core::edge_address::edge_addresses(playbook);
    crate::parallel::evaluate_route(playbook, node_id, &state, |_| true)
        .chosen
        .into_iter()
        .filter_map(|i| addresses.iter().find(|a| a.index == i))
        .map(|a| a.rendered.clone())
        .collect()
}

/// [`apb_core::judge::node_digest`] of the judge node `node_id` of
/// `playbook`; an id that names no judge node digests an empty definition.
pub(crate) fn node_digest(playbook: &Playbook, node_id: &str) -> String {
    match playbook.node(node_id).map(|n| &n.kind) {
        Some(NodeKind::Judge {
            state,
            questions,
            thresholds,
            on_unavailable,
            ..
        }) => apb_core::judge::node_digest(state, questions, thresholds, on_unavailable.as_ref()),
        _ => apb_core::judge::node_digest(
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playbook(on_unavailable: &str) -> Playbook {
        Playbook::from_yaml(&format!(
            "schema: 2\nid: j\nname: j\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: triage\n    type: judge\n    title: Triage\n    state: {{ task: \"{{{{run.instruction}}}}\" }}\n    questions:\n      verdict: {{ type: choice, instructions: \"Which?\", criteria: {{ clean: a, needs_fix: b }} }}\n    thresholds:\n      verdict: {{ min_confidence: 0.5, below: needs_fix }}\n{on_unavailable}  - {{ id: fix, type: finish, outcome: success }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: triage }}\n  - {{ id: to-fix, from: triage, to: fix, condition: {{ type: output_field, node: triage, field: verdict, equals: needs_fix }} }}\n  - {{ from: triage, to: done, fallback: true }}\n"
        ))
        .unwrap()
    }

    fn answer(value: &str, confidence: f64) -> BTreeMap<String, DecisionAnswer> {
        let a: DecisionAnswer = serde_json::from_value(serde_json::json!({
            "value": value, "p": 0.9, "confidence": confidence
        }))
        .unwrap();
        BTreeMap::from([("verdict".to_string(), a)])
    }

    fn invalid() -> BTreeMap<String, DecisionAnswer> {
        let a: DecisionAnswer =
            serde_json::from_value(serde_json::json!({ "invalid": "not an option" })).unwrap();
        BTreeMap::from([("verdict".to_string(), a)])
    }

    fn eval(pb: &Playbook, answers: &BTreeMap<String, DecisionAnswer>) -> JudgeEvaluation {
        let inputs = JudgeInputs {
            instruction: Some("Fix the parser".into()),
            ..Default::default()
        };
        evaluate_given(
            pb,
            "triage",
            &inputs,
            Path::new("/nonexistent"),
            answers,
            "fake/fake-1",
        )
        .unwrap()
    }

    /// Valid answers apply: the output carries the thresholded answer and
    /// who gave it, at enforce and as configured alike.
    #[test]
    fn valid_answers_are_the_output() {
        let e = eval(&playbook(""), &answer("clean", 0.9));
        let JudgeOutcome::Output { output } = &e.as_configured else {
            panic!("{:?}", e.as_configured)
        };
        assert_eq!(output["verdict"], "clean");
        assert_eq!(output["decided_by"], "fake/fake-1");
        assert_eq!(e.at_enforce, e.as_configured);
        // The threshold applies: below `min_confidence` reads `below`.
        let low = eval(&playbook(""), &answer("clean", 0.2));
        assert!(
            matches!(&low.as_configured, JudgeOutcome::Output { output } if output["verdict"] == "needs_fix")
        );
    }

    /// An invalid answer falls to the declared `default`, with the reason.
    #[test]
    fn an_invalid_answer_takes_the_default() {
        let e = eval(
            &playbook("    on_unavailable: { default: { verdict: clean } }\n"),
            &invalid(),
        );
        assert!(
            matches!(&e.as_configured, JudgeOutcome::Fallback { reason, output } if reason == "invalid" && output["verdict"] == "clean"),
            "{:?}",
            e.as_configured
        );
        assert!(matches!(e.at_enforce, JudgeOutcome::Fail { .. }));
    }

    /// Without a usable fallback the node fails, or hands over to its
    /// emulation.
    #[test]
    fn fail_and_emulate_say_why() {
        let e = eval(&playbook("    on_unavailable: fail\n"), &invalid());
        assert!(
            matches!(&e.as_configured, JudgeOutcome::Fail { reason } if reason.contains("(invalid)")),
            "{:?}",
            e.as_configured
        );
        let e = eval(
            &playbook("    on_unavailable: emulate\n    profile: main\n"),
            &invalid(),
        );
        assert!(
            matches!(&e.as_configured, JudgeOutcome::Emulate { reason } if reason == "invalid"),
            "{:?}",
            e.as_configured
        );
    }

    /// The node digest ignores the title and changes with a threshold.
    #[test]
    fn the_node_digest_follows_the_definition_not_the_title() {
        let pb = playbook("");
        let retitled = Playbook::from_yaml(
            &serde_json::to_string(&pb)
                .unwrap()
                .replace("\"Triage\"", "\"Another title\""),
        )
        .unwrap();
        let rethresholded =
            Playbook::from_yaml(&serde_json::to_string(&pb).unwrap().replace("0.5", "0.6"))
                .unwrap();
        assert!(node_digest(&pb, "triage").starts_with("j1:sha256:"));
        assert_eq!(node_digest(&pb, "triage"), node_digest(&retitled, "triage"));
        assert_ne!(
            node_digest(&pb, "triage"),
            node_digest(&rethresholded, "triage")
        );
    }

    /// The route preview evaluates the node's edges over the output alone.
    #[test]
    fn the_route_preview_follows_the_output() {
        let pb = playbook("");
        let fix = eval(&pb, &answer("needs_fix", 0.9));
        let JudgeOutcome::Output { output } = fix.as_configured else {
            panic!()
        };
        assert_eq!(route_of(&pb, "triage", &output), ["id:to-fix"]);
        let clean = eval(&pb, &answer("clean", 0.9));
        let JudgeOutcome::Output { output } = clean.as_configured else {
            panic!()
        };
        assert_eq!(route_of(&pb, "triage", &output), ["e:triage>done#1"]);
    }

    /// The evaluation carries the core judge digest of the node: the order
    /// of the state as written moves it, since a model reads it in order.
    #[test]
    fn the_node_digest_is_the_core_digest_of_the_node() {
        let pb = playbook("");
        let Some(NodeKind::Judge {
            state,
            questions,
            thresholds,
            on_unavailable,
            ..
        }) = pb.node("triage").map(|n| &n.kind)
        else {
            panic!("triage is a judge")
        };
        assert_eq!(
            node_digest(&pb, "triage"),
            apb_core::judge::node_digest(state, questions, thresholds, on_unavailable.as_ref())
        );
        let two = |order: &str| {
            Playbook::from_yaml(&format!(
                "schema: 2\nid: j\nname: j\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - id: triage\n    type: judge\n    state: {order}\n    questions:\n      verdict: {{ type: choice, instructions: \"Which?\", criteria: {{ clean: a, needs_fix: b }} }}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: triage }}\n  - {{ from: triage, to: done }}\n"
            ))
            .unwrap()
        };
        assert_ne!(
            node_digest(&two("{ a: \"x\", b: \"y\" }"), "triage"),
            node_digest(&two("{ b: \"y\", a: \"x\" }"), "triage")
        );
    }
}
