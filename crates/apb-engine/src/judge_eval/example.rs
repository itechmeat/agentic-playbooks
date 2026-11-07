//! Labelled examples of a judge node: a past or typed input with the
//! answers its author expects, kept under
//! `.apb/playbooks/<id>/evals/judge/<node>/<example>.yaml` (outside versions
//! and runs, and committed with the project).
//!
//! These are the file format and the shapes the surfaces share (CLI `apb
//! judge examples` and `save-example`, MCP `judge_examples` and
//! `judge_example_save`, the examples routes). An example holds scrubbed
//! inputs, the expected answers, the observed answers (never the rendered
//! state) and digests. It is never read by a run and never feeds `apb
//! decisions` labels or thresholds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use apb_core::judge_example as files;
use apb_core::registry::is_safe_segment;
use apb_core::schema::{NodeKind, Playbook};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::sandbox::{ObservedAnswers, SECRET_PLACEHOLDER};
use super::sandbox::{RecordedValue, RunValues, SandboxRequest};
use super::{JudgeEvaluation, JudgeInputs, JudgeOutcome, ReviewInput};
use crate::error::EngineError;
use crate::event::{Event, EventPayload};

/// The example file format this apb writes.
pub const EXAMPLE_SCHEMA: u32 = 1;

/// Below a playbook's directory: `<node>/<example>.yaml` live here.
pub const EXAMPLES_DIR: &str = files::EXAMPLES_DIR;

/// The longest note an example keeps, in characters.
pub const NOTE_MAX: usize = 300;

/// Examples kept per judge node; a save past it is refused.
pub const MAX_EXAMPLES_PER_NODE: usize = 200;

/// The answer an author expects to one question: a `noul` question's yes
/// or no, else the option (choice) or the level (score) by name.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExpectedAnswer {
    Yes(bool),
    Value(String),
}

/// Where an example's inputs came from.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExampleSource {
    /// The run, for an example saved from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub run: Option<String>,
    /// The judge node's execution in that run, 1-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub execution: Option<u32>,
    /// The journal seq of that execution's `decision_made`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub decision_seq: Option<u64>,
    /// Typed by hand (in the sandbox), not saved from a run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>", optional))]
    pub hand: bool,
}

/// One labelled example file.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeExample {
    /// [`EXAMPLE_SCHEMA`].
    pub schema: u32,
    /// The file stem, `[a-z0-9][a-z0-9-]*`.
    pub id: String,
    pub playbook: String,
    pub node: String,
    /// `apb_core::judge::node_digest` at labelling time: another one now
    /// means the example is stale (relabel it).
    pub node_digest: String,
    pub questions_digest: String,
    /// The state the inputs rendered to at labelling time: another one now,
    /// under an equal node digest, means the render changed.
    pub state_digest: String,
    /// Scrubbed before writing.
    pub inputs: JudgeInputs,
    /// Per question; an omitted question is unlabelled.
    pub expected: BTreeMap<String, ExpectedAnswer>,
    pub source: ExampleSource,
    pub labelled_by: String,
    /// `YYYY-MM-DD`.
    pub labelled_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub observed: Option<ObservedAnswers>,
    #[serde(default)]
    pub note: String,
}

/// How an example's check went.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExampleStatus {
    /// Evaluated: each labelled question agrees or not.
    Checked,
    /// The node's definition changed since labelling: relabel.
    Stale,
    /// The node is unchanged but the inputs render to another state.
    StateChanged,
    /// Asked live, the node got no answer to compare (the sandbox's daily
    /// cap was spent or every provider failed): no label of it counts. An
    /// answer of an invalid shape is `checked`, its labels disagreeing.
    Unanswered,
}

/// One labelled question of a checked example.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAgreement {
    pub question: String,
    pub expected: ExpectedAnswer,
    /// What the node's output carries for the question now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
    pub observed: Option<serde_json::Value>,
    pub agrees: bool,
}

/// One example, checked.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeExampleResult {
    pub id: String,
    pub status: ExampleStatus,
    /// Absent unless `checked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub evaluation: Option<JudgeEvaluation>,
    #[serde(default)]
    pub questions: Vec<QuestionAgreement>,
}

/// How often one expected answer met one observed answer.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfusionCell {
    pub question: String,
    pub expected: String,
    /// The observed answer as text; `none` when the output carries none.
    pub observed: String,
    pub count: u32,
}

/// The check of every example of one judge node. Nothing is tuned from it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeExamplesCheck {
    pub node: String,
    /// The node's digest now.
    pub node_digest: String,
    pub examples: Vec<JudgeExampleResult>,
    /// Labelled questions of checked examples that agree.
    pub agree: u32,
    pub disagree: u32,
    pub confusion: Vec<ConfusionCell>,
    /// Requests the check sent (`live` only).
    #[serde(default)]
    pub requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cost_usd: Option<f64>,
}

/// A check of a playbook's examples.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExamplesCheckRequest {
    /// One node; every judge node with examples when absent.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub node: Option<String>,
    /// Ask the sandbox again (inside its daily cap) instead of evaluating
    /// the observed answers.
    #[serde(default)]
    pub live: bool,
}

/// Saving a judge execution of a run as an example.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExampleSaveRequest {
    /// The judge node's execution, 1-based; the latest when absent.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub execution: Option<u32>,
    pub expected: BTreeMap<String, ExpectedAnswer>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub note: Option<String>,
}

// --- fill from a run ---------------------------------------------------------

/// A run's judge execution as an example starts from: its inputs, the
/// run's other recorded values and what the model answered, with the
/// definition the run executed.
struct FromRun {
    playbook_id: String,
    playbook: Playbook,
    values: RunValues,
    /// The run tree's redactor the values went through.
    redactor: crate::run_redact::RunRedactor,
    /// The seq of the observed answers' `decision_made`.
    decision_seq: Option<u64>,
}

/// "Fill from a run": the inputs of execution `execution` (1-based, the
/// latest when absent) of judge node `node` in project run `run_id`, as the
/// run had them when that execution started; every other value the run
/// recorded; and what the model answered then. Every text is redacted with
/// the run tree's redactor (it reads [`crate::run_redact::WITHHELD`] when
/// the tree's secrets cannot all be read); a sensitive param reads
/// [`SECRET_PLACEHOLDER`]. Hook paths are capabilities and are left out.
pub fn run_values(
    root: &Path,
    run_id: &str,
    node: &str,
    execution: Option<u32>,
) -> Result<RunValues, EngineError> {
    from_run(root, run_id, node, execution).map(|r| r.values)
}

fn from_run(
    root: &Path,
    run_id: &str,
    node: &str,
    execution: Option<u32>,
) -> Result<FromRun, EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    if !is_safe_segment(node) {
        return Err(EngineError::NotFound(format!("node `{node}`")));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let (playbook_id, _) = crate::scheduler::run_playbook_ref(root, run_id)?;
    let playbook = crate::legacy_snapshot::load_run_playbook(&run_dir)
        .ok_or_else(|| EngineError::NotFound(format!("the playbook snapshot of run `{run_id}`")))?;
    judge_node(&playbook, node)?;
    let events = crate::event::read_all(&run_dir)?;
    let index = crate::identity::index(&events);
    let runs = index.executions(node);
    let exec = match execution {
        Some(n) => runs.iter().find(|e| e.ordinal == n),
        None => runs.last(),
    }
    .ok_or_else(|| {
        EngineError::NotFound(match execution {
            Some(n) => format!("execution {n} of `{node}` in run `{run_id}`"),
            None => format!("an execution of `{node}` in run `{run_id}`"),
        })
    })?;
    let before: Vec<Event> = events
        .iter()
        .filter(|e| e.seq < exec.begin_seq)
        .cloned()
        .collect();
    let cfg = crate::run_config::read_run_config(&run_dir)?;
    let state = crate::state::RunState::fold(&before);
    let context = crate::context::build_context_for_render(
        &run_dir,
        &before,
        cfg.instruction.as_deref(),
        &playbook.context_budget(node),
    )?;
    let redactor = crate::run_redact::RunRedactor::for_run(root, &run_dir);
    let text = |t: &str| redactor.redact_str(t);
    let inputs = JudgeInputs {
        params: cfg
            .params
            .iter()
            .map(|(k, v)| (k.clone(), text(v)))
            .collect(),
        instruction: cfg.instruction.as_deref().map(text),
        outputs: state
            .outputs
            .iter()
            .map(|(k, v)| (k.clone(), text(v)))
            .collect(),
        reviews: state
            .reviews
            .iter()
            .map(|(k, r)| {
                (
                    k.clone(),
                    ReviewInput {
                        decision: r.decision.clone(),
                        note: Some(text(&r.note)).filter(|n| !n.is_empty()),
                    },
                )
            })
            .collect(),
        rejected_outputs: state
            .rejected_outputs
            .iter()
            .map(|(k, v)| (k.clone(), text(v)))
            .collect(),
        hooks: BTreeMap::new(),
        seen: crate::loop_seen::seen_texts(&playbook, &state)
            .into_iter()
            .map(|(k, v)| (k, text(&v)))
            .collect(),
        previous_success: state
            .previous_success
            .as_ref()
            .map(crate::previous_success::record_values)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, text(&v)))
            .collect(),
        context: text(&context),
    };
    let inputs = super::sandbox::without_secrets(&playbook, &inputs);
    let observed = observed_in(&events, node, exec.begin_seq, exec.end_seq);
    let values = RunValues {
        run_id: run_id.to_string(),
        node: node.to_string(),
        execution: exec.ordinal,
        recorded: recorded(&playbook, &events, &inputs, &text),
        inputs,
        observed: observed.as_ref().map(|(_, o)| o.clone()),
    };
    Ok(FromRun {
        playbook_id,
        playbook,
        values,
        redactor,
        decision_seq: observed.map(|(seq, _)| seq),
    })
}

/// The first answered `judge_node` decision of `node` within one
/// execution's span, with its seq.
fn observed_in(
    events: &[Event],
    node: &str,
    begin: u64,
    end: Option<u64>,
) -> Option<(u64, ObservedAnswers)> {
    events
        .iter()
        .filter(|e| e.seq >= begin && end.is_none_or(|end| e.seq <= end))
        .find_map(|e| match &e.payload {
            EventPayload::DecisionMade {
                use_site,
                node: Some(n),
                provider: Some(provider),
                model,
                answers,
                error: None,
                ..
            } if use_site == "judge_node" && n == node && !answers.is_empty() => Some((
                e.seq,
                ObservedAnswers {
                    provider: provider.clone(),
                    model: model.clone().unwrap_or_default(),
                    answers: answers.clone(),
                },
            )),
            _ => None,
        })
}

/// Every value the run recorded, to place into any input: each node output
/// and review note as the run ended, each param (sensitive ones as the
/// placeholder), and each baseline observation (the latest per check and
/// phase, as JSON text). Run outputs are not listed: they are node outputs.
fn recorded(
    playbook: &Playbook,
    events: &[Event],
    inputs: &JudgeInputs,
    text: &dyn Fn(&str) -> String,
) -> Vec<RecordedValue> {
    let end = crate::state::RunState::fold(events);
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (node, output) in &end.outputs {
        out.insert(format!("outputs.{node}"), text(output));
    }
    for (node, review) in &end.reviews {
        if !review.note.is_empty() {
            out.insert(format!("reviews.{node}.note"), text(&review.note));
        }
    }
    let params = super::sandbox::without_secrets(
        playbook,
        &JudgeInputs {
            params: inputs.params.clone(),
            ..Default::default()
        },
    );
    for (name, value) in &params.params {
        out.insert(format!("params.{name}"), value.clone());
    }
    for e in events {
        if let EventPayload::BaselineObserved {
            check,
            phase,
            observations,
            ..
        } = &e.payload
        {
            let json = serde_json::to_string(observations).unwrap_or_default();
            out.insert(format!("baseline.{check}.{phase}"), text(&json));
        }
    }
    out.into_iter()
        .map(|(label, text)| RecordedValue { label, text })
        .collect()
}

/// The judge node `node` of `playbook`, or why it is none.
fn judge_node<'a>(playbook: &'a Playbook, node: &str) -> Result<&'a NodeKind, EngineError> {
    match playbook.node(node).map(|n| &n.kind) {
        Some(kind @ NodeKind::Judge { .. }) => Ok(kind),
        Some(_) => Err(EngineError::Invalid(format!(
            "node `{node}` is not a judge"
        ))),
        None => Err(EngineError::NotFound(format!("node `{node}`"))),
    }
}

// --- saving ------------------------------------------------------------------

/// The lock a save holds in the node's examples directory while it picks
/// an id and writes the file.
const SAVE_LOCK: &str = ".save.lock";

/// The project directory of playbook `id`, refused when it is missing or a
/// link: examples are kept for project playbooks only.
fn playbook_dir(root: &Path, id: &str) -> Result<PathBuf, EngineError> {
    let apb = root.join(".apb");
    let dir = apb.join("playbooks").join(id);
    if !is_safe_segment(id) || !dir.is_dir() {
        return Err(EngineError::NotFound(format!("project playbook `{id}`")));
    }
    files::check_no_link(&apb, &dir).map_err(|e| EngineError::Invalid(e.to_string()))?;
    Ok(dir)
}

/// Saves execution `req.execution` of judge node `node` in project run
/// `run_id` as a labelled example of the run's playbook, labelled by `by`.
/// The inputs are redacted with the run tree's redactor, scrubbed with the
/// workspace's scrubber, and a sensitive param is kept as
/// [`SECRET_PLACEHOLDER`]; the save is refused when a credential-shaped
/// value or a configured secret value is still left, when the run's
/// secrets could not all be read, when the run's judge node differs from
/// the one in `current` (a check reads that one), and past
/// [`MAX_EXAMPLES_PER_NODE`]. The note goes through the same scrubber and
/// the same refusal. Returns the example as stored.
pub fn save_from_run(
    root: &Path,
    run_id: &str,
    node: &str,
    req: &ExampleSaveRequest,
    by: &str,
) -> Result<JudgeExample, EngineError> {
    let from = from_run(root, run_id, node, req.execution)?;
    // A check reads the node in `current`: an example of another
    // definition of it would read `stale` before its labels were checked.
    let current = apb_core::registry::Registry::open(root)?.load(&from.playbook_id, None)?;
    if super::node_digest(&current.playbook, node) != super::node_digest(&from.playbook, node) {
        return Err(EngineError::Conflict(format!(
            "run `{run_id}` executed another definition of judge node `{node}` than version {} (current); save from a run of the current version",
            current.version
        )));
    }
    // The run tree's scrubber (the workspace's secrets and every value of
    // the run and its ancestors) checks and scrubs every text kept.
    let scrubber = from.redactor.scrubber().map_err(|_| {
        EngineError::Invalid(
            "the run's secret values could not all be read; the example is not stored".into(),
        )
    })?;
    let source = ExampleSource {
        run: Some(run_id.to_string()),
        execution: Some(from.values.execution),
        decision_seq: from.decision_seq,
        hand: false,
    };
    save(
        root,
        scrubber,
        &from.playbook_id,
        &from.playbook,
        node,
        from.values.inputs,
        &req.expected,
        source,
        from.values.observed,
        req.note.as_deref(),
        by,
    )
}

/// Writes one example: checks the labels against the node's questions,
/// scrubs and checks the inputs, digests them as a check will, and writes
/// the file privately under the playbook's examples directory.
#[allow(clippy::too_many_arguments)]
fn save(
    root: &Path,
    scrubber: &apb_core::scrub::Scrubber,
    playbook_id: &str,
    playbook: &Playbook,
    node: &str,
    inputs: JudgeInputs,
    expected: &BTreeMap<String, ExpectedAnswer>,
    source: ExampleSource,
    observed: Option<ObservedAnswers>,
    note: Option<&str>,
    by: &str,
) -> Result<JudgeExample, EngineError> {
    check_labels(playbook, node, expected)?;
    let dir_of = playbook_dir(root, playbook_id)?;
    let inputs = scrubbed(scrubber, inputs)?;
    let note = match note {
        Some(n) if !n.trim().is_empty() => {
            let line = n.split_whitespace().collect::<Vec<_>>().join(" ");
            scrubber
                .stored_text(&line, NOTE_MAX)
                .filter(|kept| scrubber.refuses(kept).is_none())
                .ok_or_else(|| {
                    EngineError::Invalid(
                        "the note holds a credential-shaped or secret value; it is not stored"
                            .into(),
                    )
                })?
        }
        _ => String::new(),
    };
    let observed = observed.map(|o| scrubbed(scrubber, o)).transpose()?;
    let evaluation = given(root, playbook, node, &inputs, observed.as_ref())?;
    let dir = files::node_dir(&dir_of, node)
        .ok_or_else(|| EngineError::NotFound(format!("node `{node}`")))?;
    // Checked before anything is created: a linked `evals` must not get a
    // directory made through it.
    files::check_no_link(&dir_of, &dir).map_err(|e| EngineError::Invalid(e.to_string()))?;
    std::fs::create_dir_all(&dir)?;
    files::check_no_link(&dir_of, &dir).map_err(|e| EngineError::Invalid(e.to_string()))?;
    // The id is the next free one and the cap is checked under the node's
    // lock, held until the file is written: two saves at once never pick
    // the same id, and the second never replaces the first's label.
    let _lock = apb_core::fsutil::lock_dir(&dir, SAVE_LOCK).map_err(|_| {
        EngineError::Conflict(format!(
            "another example of judge node `{node}` is being saved; try again shortly"
        ))
    })?;
    let ids = files::example_ids(&dir_of, &dir).map_err(|e| EngineError::Invalid(e.to_string()))?;
    if ids.len() >= MAX_EXAMPLES_PER_NODE {
        return Err(EngineError::Conflict(format!(
            "judge node `{node}` already has {MAX_EXAMPLES_PER_NODE} examples; remove some first"
        )));
    }
    let example = JudgeExample {
        schema: EXAMPLE_SCHEMA,
        id: files::next_id(&ids),
        playbook: playbook_id.to_string(),
        node: node.to_string(),
        node_digest: evaluation.node_digest,
        questions_digest: evaluation.questions_digest,
        state_digest: evaluation.state_digest,
        inputs,
        expected: expected.clone(),
        source,
        labelled_by: by.to_string(),
        labelled_at: files::labelled_on(apb_core::clock::now_ms_u64()),
        observed,
        note,
    };
    let yaml = yaml_serde::to_string(&example).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write_private_under(
        &dir_of,
        &files::example_path(&dir, &example.id),
        yaml.as_bytes(),
    )?;
    Ok(example)
}

/// Refuses labels that name no question of the node, or an answer the
/// question cannot give.
fn check_labels(
    playbook: &Playbook,
    node: &str,
    expected: &BTreeMap<String, ExpectedAnswer>,
) -> Result<(), EngineError> {
    let NodeKind::Judge {
        questions,
        thresholds,
        ..
    } = judge_node(playbook, node)?
    else {
        unreachable!("judge_node returns a judge")
    };
    if expected.is_empty() {
        return Err(EngineError::Invalid(
            "an example needs at least one expected answer".into(),
        ));
    }
    for (id, answer) in expected {
        let q = questions.get(id).ok_or_else(|| {
            EngineError::Invalid(format!("`{id}` is not a question of judge node `{node}`"))
        })?;
        match (
            apb_core::judge::expected_names(q, thresholds.get(id)),
            answer,
        ) {
            (None, ExpectedAnswer::Yes(_)) => {}
            (Some(names), ExpectedAnswer::Value(v)) if names.contains(v) => {}
            (None, _) => {
                return Err(EngineError::Invalid(format!(
                    "`{id}` is a yes or no question: expect true or false"
                )));
            }
            (Some(names), _) => {
                return Err(EngineError::Invalid(format!(
                    "`{id}` expects one of: {}",
                    names.join(", ")
                )));
            }
        }
    }
    Ok(())
}

/// A stored part of an example (the inputs, the observed answers) as
/// stored: refused when it holds what the storage rule refuses, else every
/// string scrubbed with the run tree's scrubber (secret values, the project
/// path, the home directory), and refused when anything is still left.
fn scrubbed<T: Serialize + serde::de::DeserializeOwned>(
    scrubber: &apb_core::scrub::Scrubber,
    part: T,
) -> Result<T, EngineError> {
    let value = serde_json::to_value(&part).map_err(|e| EngineError::Invalid(e.to_string()))?;
    // Checked before the scrub rewrites a credential-shaped value into a
    // marker: an example that silently lost part of its input would label
    // another input than the run had.
    let value = match flagged(scrubber, &value) {
        None => scrubber.redact_value(value),
        found => return Err(refused(found)),
    };
    if let found @ Some(_) = flagged(scrubber, &value) {
        return Err(refused(found));
    }
    serde_json::from_value(value).map_err(|e| EngineError::Invalid(e.to_string()))
}

/// The refusal of inputs that still hold `flag`.
fn refused(flag: Option<apb_core::scrub::ScrubFlag>) -> EngineError {
    let what = flag.map_or("secret", |f| f.as_str());
    EngineError::Invalid(format!(
        "the run's values or answers still hold a {what} after redaction; remove it from the run's values or type the example by hand"
    ))
}

/// The first string of `v` (object keys included) the storage rule
/// refuses: a credential shape, a configured secret value, or any keyed
/// value such as `password: ...`.
fn flagged(scrubber: &apb_core::scrub::Scrubber, v: &Value) -> Option<apb_core::scrub::ScrubFlag> {
    scrubber.refuses_value(v)
}

/// The evaluation of `inputs` over the given answers (none: rendering
/// only), in the scratch place the sandbox renders in, so a save and a
/// check digest the state alike.
fn given(
    root: &Path,
    playbook: &Playbook,
    node: &str,
    inputs: &JudgeInputs,
    observed: Option<&ObservedAnswers>,
) -> Result<JudgeEvaluation, EngineError> {
    let scratch = super::sandbox::scratch(root)?;
    let empty = BTreeMap::new();
    let (answers, by) = match observed {
        Some(o) => (&o.answers, format!("{}/{}", o.provider, o.model)),
        None => (&empty, String::new()),
    };
    super::evaluate_given(playbook, node, inputs, &scratch, answers, &by)
}

// --- listing and checking ----------------------------------------------------

/// The examples of project playbook `id`, per node (one node, or every
/// node with an examples directory), each file read as written. A file
/// that does not read, or names another node or id than its place, is
/// refused by name: a label never vanishes silently.
pub fn list(
    root: &Path,
    id: &str,
    node: Option<&str>,
) -> Result<Vec<(String, Vec<JudgeExample>)>, EngineError> {
    let dir_of = playbook_dir(root, id)?;
    let nodes = match node {
        Some(n) => vec![n.to_string()],
        None => files::example_nodes(&dir_of).map_err(|e| EngineError::Invalid(e.to_string()))?,
    };
    let mut out = Vec::new();
    for node in nodes {
        let dir = files::node_dir(&dir_of, &node)
            .ok_or_else(|| EngineError::NotFound(format!("node `{node}`")))?;
        let mut examples = Vec::new();
        for ex in
            files::example_ids(&dir_of, &dir).map_err(|e| EngineError::Invalid(e.to_string()))?
        {
            let text = files::read_example(&dir_of, &dir, &ex)
                .map_err(|e| EngineError::Invalid(format!("example `{node}/{ex}`: {e}")))?;
            let example: JudgeExample = yaml_serde::from_str(&text)
                .map_err(|e| EngineError::Invalid(format!("example `{node}/{ex}`: {e}")))?;
            if example.id != ex || example.node != node || example.playbook != id {
                return Err(EngineError::Invalid(format!(
                    "example `{node}/{ex}` names another place (`{}`/`{}`/`{}`)",
                    example.playbook, example.node, example.id
                )));
            }
            examples.push(example);
        }
        out.push((node, examples));
    }
    Ok(out)
}

/// Checks the examples of project playbook `id` against its `current`
/// version: `stale` when the node's digest moved since labelling,
/// `state_changed` when the inputs now render to another state, else each
/// labelled question agrees or not with what the node outputs now (from
/// the observed answers, or asked again through the sandbox when
/// `req.live`, inside its daily cap). Counts and a confusion table per
/// node; nothing is tuned.
pub fn check(
    root: &Path,
    id: &str,
    req: &ExamplesCheckRequest,
) -> Result<Vec<NodeExamplesCheck>, EngineError> {
    let current = apb_core::registry::Registry::open(root)?.load(id, None)?;
    let playbook = &current.playbook;
    let mut out = Vec::new();
    for (node, examples) in list(root, id, req.node.as_deref())? {
        let now = super::node_digest(playbook, &node);
        let mut check = NodeExamplesCheck {
            node: node.clone(),
            node_digest: now.clone(),
            examples: Vec::new(),
            agree: 0,
            disagree: 0,
            confusion: Vec::new(),
            requests: 0,
            cost_usd: None,
        };
        let mut cells: BTreeMap<(String, String, String), u32> = BTreeMap::new();
        for ex in examples {
            let result = check_one(root, id, playbook, &node, &now, &ex, req.live, &mut check)?;
            for q in &result.questions {
                if q.agrees {
                    check.agree += 1;
                } else {
                    check.disagree += 1;
                }
                let key = (
                    q.question.clone(),
                    expected_text(&q.expected),
                    q.observed
                        .as_ref()
                        .map_or_else(|| "none".to_string(), value_text),
                );
                *cells.entry(key).or_default() += 1;
            }
            check.examples.push(result);
        }
        check.confusion = cells
            .into_iter()
            .map(|((question, expected, observed), count)| ConfusionCell {
                question,
                expected,
                observed,
                count,
            })
            .collect();
        out.push(check);
    }
    Ok(out)
}

/// One example's check.
#[allow(clippy::too_many_arguments)]
fn check_one(
    root: &Path,
    id: &str,
    playbook: &Playbook,
    node: &str,
    digest_now: &str,
    ex: &JudgeExample,
    live: bool,
    totals: &mut NodeExamplesCheck,
) -> Result<JudgeExampleResult, EngineError> {
    let unchecked = |status| JudgeExampleResult {
        id: ex.id.clone(),
        status,
        evaluation: None,
        questions: Vec::new(),
    };
    if ex.node_digest != digest_now {
        return Ok(unchecked(ExampleStatus::Stale));
    }
    let rendered = given(root, playbook, node, &ex.inputs, ex.observed.as_ref())?;
    if rendered.state_digest != ex.state_digest {
        return Ok(unchecked(ExampleStatus::StateChanged));
    }
    let evaluation = if live {
        let asked = super::sandbox::evaluate(
            root,
            id,
            node,
            &SandboxRequest {
                inputs: ex.inputs.clone(),
                ..Default::default()
            },
        )?;
        totals.requests += asked.evaluation.requests;
        if let Some(c) = asked.evaluation.cost_usd {
            totals.cost_usd = Some(totals.cost_usd.unwrap_or(0.0) + c);
        }
        // An ask that got no answer says nothing about the labels: what
        // the node then outputs is its fallback, not a disagreement. An
        // answer of an invalid shape is an answer, and its labels disagree.
        if asked.evaluation.raw.is_empty() {
            return Ok(JudgeExampleResult {
                evaluation: Some(asked.evaluation),
                ..unchecked(ExampleStatus::Unanswered)
            });
        }
        asked.evaluation
    } else {
        rendered
    };
    let NodeKind::Judge {
        questions,
        thresholds,
        ..
    } = judge_node(playbook, node)?
    else {
        unreachable!("judge_node returns a judge")
    };
    let output = match &evaluation.at_enforce {
        JudgeOutcome::Output { output } => Some(output),
        _ => None,
    };
    let agreement = ex
        .expected
        .iter()
        .map(|(qid, expected)| {
            let observed = match (questions.get(qid), output) {
                (Some(q), Some(out)) => {
                    apb_core::judge::labelled_value(q, thresholds.get(qid), qid, out)
                }
                _ => None,
            };
            QuestionAgreement {
                question: qid.clone(),
                expected: expected.clone(),
                agrees: observed.as_ref() == Some(&expected_value(expected)),
                observed,
            }
        })
        .collect();
    Ok(JudgeExampleResult {
        id: ex.id.clone(),
        status: ExampleStatus::Checked,
        evaluation: Some(evaluation),
        questions: agreement,
    })
}

/// An expected answer as the output carries it.
fn expected_value(e: &ExpectedAnswer) -> Value {
    match e {
        ExpectedAnswer::Yes(b) => Value::Bool(*b),
        ExpectedAnswer::Value(s) => Value::String(s.clone()),
    }
}

fn expected_text(e: &ExpectedAnswer) -> String {
    value_text(&expected_value(e))
}

/// A label value as text, without JSON quotes.
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYBOOK: &str = "schema: 2\nid: j\nname: j\nversion: 1.0.0\nparams:\n  - { name: api_token, type: string, secret: true }\nnodes:\n  - { id: start, type: start }\n  - id: triage\n    type: judge\n    title: Triage\n    state: { task: \"{{run.instruction}}\", key: \"{{params.api_token}}\", before: \"{{nodes.triage.output}}\" }\n    questions:\n      verdict: { type: choice, instructions: \"Which?\", criteria: { clean: a, needs_fix: b } }\n    thresholds:\n      verdict: { min_confidence: 0.5, below: needs_fix }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: triage }\n  - { from: triage, to: done }\n";

    const TOKEN: &str = "sk-live-abcdefghijklmnopqrstuvwx";

    fn payload(v: serde_json::Value) -> EventPayload {
        serde_json::from_value(v).unwrap()
    }

    /// A project with playbook `j` 1.0.0 as `current` and run `r1` of it:
    /// a baseline observation, then one judge execution whose provider
    /// answered `clean`, with `instruction` as the run's instruction.
    fn project(instruction: &str) -> tempfile::TempDir {
        project_answering(
            instruction,
            serde_json::json!({ "verdict": { "value": "clean", "p": 0.9, "confidence": 0.9 } }),
        )
    }

    /// [`project`] whose judge execution the provider answered `answers`.
    fn project_answering(instruction: &str, answers: serde_json::Value) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let pb = root.path().join(".apb/playbooks/j");
        std::fs::create_dir_all(pb.join("1.0.0")).unwrap();
        std::fs::write(pb.join("1.0.0/playbook.yaml"), PLAYBOOK).unwrap();
        std::fs::write(pb.join("current"), "1.0.0\n").unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("playbook.yaml"), PLAYBOOK).unwrap();
        crate::run_config::write_run_config(
            &run,
            &crate::run_config::RunConfig {
                params: BTreeMap::from([("api_token".to_string(), TOKEN.to_string())]),
                instruction: Some(instruction.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let unknown =
            serde_json::to_value(apb_core::baseline::Observation::unknown("offline")).unwrap();
        let mut log = crate::event::EventLog::create(&run).unwrap();
        for e in [
            serde_json::json!({ "type": "run_started", "playbook": "j", "version": "1.0.0" }),
            serde_json::json!({ "type": "baseline_observed", "check": "open_issues", "phase": "before", "status": "unknown", "observations": { "count": unknown } }),
            serde_json::json!({ "type": "node_started", "node": "triage", "attempt": 1 }),
            serde_json::json!({ "type": "decision_made", "use_site": "judge_node", "node": "triage", "provider": "fake", "model": "fake-1", "mode": "enforce", "answers": answers }),
            serde_json::json!({ "type": "node_finished", "node": "triage", "status": "succeeded", "attempt": 1, "output": "{\"verdict\":\"clean\"}" }),
        ] {
            log.append(payload(e)).unwrap();
        }
        root
    }

    fn expect(pairs: &[(&str, &str)]) -> ExampleSaveRequest {
        ExampleSaveRequest {
            expected: pairs
                .iter()
                .map(|(q, v)| (q.to_string(), ExpectedAnswer::Value(v.to_string())))
                .collect(),
            ..Default::default()
        }
    }

    /// "Fill from a run" gives the judge execution's inputs as they were
    /// when it started (its own output not yet there), the run's other
    /// values, and the model's answer; the run's secret value never
    /// appears, in the instruction or as the param.
    #[test]
    fn filling_from_a_run_keeps_its_values_and_never_its_secret() {
        let root = project(&format!("Fix the parser, key {TOKEN}"));
        let v = run_values(root.path(), "r1", "triage", None).unwrap();
        assert_eq!(v.execution, 1);
        assert!(v.inputs.outputs.is_empty(), "{:?}", v.inputs.outputs);
        assert_eq!(v.inputs.params["api_token"], SECRET_PLACEHOLDER);
        let instruction = v.inputs.instruction.as_deref().unwrap();
        assert!(instruction.starts_with("Fix the parser"), "{instruction}");
        let labels: Vec<&str> = v.recorded.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "baseline.open_issues.before",
                "outputs.triage",
                "params.api_token"
            ]
        );
        let observed = v.observed.as_ref().unwrap();
        assert_eq!(
            (observed.provider.as_str(), observed.model.as_str()),
            ("fake", "fake-1")
        );
        let all = serde_json::to_string(&v).unwrap();
        assert!(!all.contains(TOKEN), "{all}");
        assert!(run_values(root.path(), "r1", "triage", Some(2)).is_err());
        assert!(run_values(root.path(), "../r1", "triage", None).is_err());
    }

    /// A save keeps exactly what a check needs (scrubbed inputs, the
    /// labels, the observed answers, digests and the source) and refuses
    /// labels the node cannot give and an input still shaped like a
    /// credential.
    #[test]
    fn a_save_stores_scrubbed_inputs_and_refuses_what_it_must_not_keep() {
        let root = project("Fix the parser");
        let saved = save_from_run(
            root.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        )
        .unwrap();
        assert_eq!(saved.id, "ex-0001");
        assert_eq!(saved.inputs.params["api_token"], SECRET_PLACEHOLDER);
        assert_eq!(saved.source.run.as_deref(), Some("r1"));
        assert_eq!(saved.source.decision_seq, Some(3));
        assert!(saved.node_digest.starts_with("j1:"));
        assert_eq!(saved.labelled_by, "cli");
        assert_eq!(saved.labelled_at.len(), 10);
        let path = root
            .path()
            .join(".apb/playbooks/j/evals/judge/triage/ex-0001.yaml");
        let back: JudgeExample =
            yaml_serde::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, saved);
        assert!(!std::fs::read_to_string(&path).unwrap().contains(TOKEN));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        for bad in [
            expect(&[("verdict", "maybe")]),
            expect(&[("risky", "yes")]),
            expect(&[]),
        ] {
            assert!(
                matches!(
                    save_from_run(root.path(), "r1", "triage", &bad, "cli"),
                    Err(EngineError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        let shaped = project("deploy with ghp_abcdefghijklmnopqrstuvwxyz0123456789");
        let refused = save_from_run(
            shaped.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("credential-shaped"),
            "{refused}"
        );
        assert!(!shaped.path().join(".apb/playbooks/j/evals").exists());
    }

    /// A run's own secret value never reaches the stored example, in the
    /// note or anywhere else, even when it is not shaped like a credential;
    /// a keyed password the storage rule refuses is refused, not stored.
    #[test]
    fn a_save_keeps_no_run_secret_and_refuses_a_keyed_password() {
        let root = project("Fix the parser");
        let run = root.path().join(".apb/runs/r1");
        crate::run_config::write_run_config(
            &run,
            &crate::run_config::RunConfig {
                params: BTreeMap::from([("api_token".to_string(), "violet hedgehog".to_string())]),
                instruction: Some("Fix the parser".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let req = ExampleSaveRequest {
            note: Some("Fails with violet hedgehog".into()),
            ..expect(&[("verdict", "clean")])
        };
        let saved = save_from_run(root.path(), "r1", "triage", &req, "cli").unwrap();
        let stored = std::fs::read_to_string(
            root.path()
                .join(".apb/playbooks/j/evals/judge/triage")
                .join(format!("{}.yaml", saved.id)),
        )
        .unwrap();
        assert!(!saved.note.contains("violet hedgehog"), "{}", saved.note);
        assert!(!stored.contains("violet hedgehog"), "{stored}");

        let keyed = project("deploy, password: correcthorsebatterystaple");
        let refused = save_from_run(
            keyed.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        );
        assert!(
            matches!(&refused, Err(EngineError::Invalid(_))),
            "{refused:?}"
        );
        assert!(!keyed.path().join(".apb/playbooks/j/evals").exists());
        let noted = save_from_run(
            root.path(),
            "r1",
            "triage",
            &ExampleSaveRequest {
                note: Some("password: correcthorsebatterystaple".into()),
                ..expect(&[("verdict", "clean")])
            },
            "cli",
        );
        assert!(matches!(&noted, Err(EngineError::Invalid(_))), "{noted:?}");
    }

    /// A run whose judge node differs from the one in `current` cannot be
    /// saved from: its example would read `stale` on its first check, and
    /// its labels were checked against a node `current` no longer has.
    #[test]
    fn a_run_of_another_node_definition_is_refused_by_name() {
        let root = project("Fix the parser");
        let pb = root.path().join(".apb/playbooks/j");
        std::fs::create_dir_all(pb.join("1.1.0")).unwrap();
        std::fs::write(
            pb.join("1.1.0/playbook.yaml"),
            PLAYBOOK
                .replace("version: 1.0.0", "version: 1.1.0")
                .replace("min_confidence: 0.5", "min_confidence: 0.7"),
        )
        .unwrap();
        std::fs::write(pb.join("current"), "1.1.0\n").unwrap();
        let refused = save_from_run(
            root.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        );
        assert!(
            matches!(&refused, Err(EngineError::Conflict(m)) if m.contains("current")),
            "{refused:?}"
        );
        assert!(!pb.join("evals").exists());
    }

    /// What the provider answered is stored only after the same scrub and
    /// refusal as the inputs: an invalid answer quoting a credential is
    /// refused, a run's secret value in it never reaches the file.
    #[test]
    fn the_observed_answers_are_scrubbed_and_refused_like_the_inputs() {
        let shaped = project_answering(
            "Fix the parser",
            serde_json::json!({ "verdict": { "invalid": "choice `ghp_abcdefghijklmnopqrstuvwxyz0123456789` is not an option" } }),
        );
        let refused = save_from_run(
            shaped.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        );
        assert!(
            matches!(&refused, Err(EngineError::Invalid(_))),
            "{refused:?}"
        );
        assert!(!shaped.path().join(".apb/playbooks/j/evals").exists());

        let valued = project_answering(
            "Fix the parser",
            serde_json::json!({ "verdict": { "invalid": format!("choice `{TOKEN}` is not an option") } }),
        );
        let saved = save_from_run(
            valued.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        );
        if let Ok(saved) = &saved {
            let stored = std::fs::read_to_string(
                valued
                    .path()
                    .join(".apb/playbooks/j/evals/judge/triage")
                    .join(format!("{}.yaml", saved.id)),
            )
            .unwrap();
            assert!(!stored.contains(TOKEN), "{stored}");
        }
    }

    /// An `evals` that is a link out of the project is refused before
    /// anything is created through it.
    #[cfg(unix)]
    #[test]
    fn a_linked_evals_directory_is_refused_and_left_untouched() {
        let root = project("Fix the parser");
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".apb/playbooks/j/evals"))
            .unwrap();
        let refused = save_from_run(
            root.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        );
        assert!(
            matches!(&refused, Err(EngineError::Invalid(_))),
            "{refused:?}"
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    /// Saves at the same time each get their own id: none overwrites
    /// another's label.
    #[test]
    fn saves_at_the_same_time_never_overwrite_each_other() {
        let root = project("Fix the parser");
        let saves = 12;
        let start = std::sync::Barrier::new(saves);
        let ids: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..saves)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        save_from_run(
                            root.path(),
                            "r1",
                            "triage",
                            &expect(&[("verdict", "clean")]),
                            "cli",
                        )
                        .unwrap()
                        .id
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let distinct: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(distinct.len(), saves, "{ids:?}");
        assert_eq!(
            list(root.path(), "j", Some("triage")).unwrap()[0].1.len(),
            saves
        );
    }

    /// A check evaluates the observed answers under the current version:
    /// agreement per label and a confusion table; `state_changed` when the
    /// stored inputs render another state; `stale` once a threshold of the
    /// node changed.
    #[test]
    fn a_check_counts_agreement_and_says_stale_after_a_threshold_edit() {
        let root = project("Fix the parser");
        save_from_run(
            root.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        )
        .unwrap();
        save_from_run(
            root.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "needs_fix")]),
            "cli",
        )
        .unwrap();
        let checked = check(root.path(), "j", &ExamplesCheckRequest::default()).unwrap();
        assert_eq!(checked.len(), 1);
        let node = &checked[0];
        assert_eq!((node.agree, node.disagree, node.requests), (1, 1, 0));
        assert!(
            node.examples
                .iter()
                .all(|e| e.status == ExampleStatus::Checked)
        );
        let cell = |expected: &str| {
            node.confusion
                .iter()
                .find(|c| c.expected == expected)
                .map(|c| (c.observed.as_str(), c.count))
        };
        assert_eq!(cell("clean"), Some(("clean", 1)));
        assert_eq!(cell("needs_fix"), Some(("clean", 1)));

        // The stored inputs of the second example are edited by hand.
        let second = root
            .path()
            .join(".apb/playbooks/j/evals/judge/triage/ex-0002.yaml");
        let text = std::fs::read_to_string(&second).unwrap();
        std::fs::write(&second, text.replace("Fix the parser", "Fix the lexer")).unwrap();
        let checked = check(root.path(), "j", &ExamplesCheckRequest::default()).unwrap();
        assert_eq!(checked[0].examples[1].status, ExampleStatus::StateChanged);
        assert_eq!((checked[0].agree, checked[0].disagree), (1, 0));

        // A threshold edit of the current version: every example is stale.
        let current = root.path().join(".apb/playbooks/j/1.0.0/playbook.yaml");
        std::fs::write(&current, PLAYBOOK.replace("0.5", "0.95")).unwrap();
        let checked = check(root.path(), "j", &ExamplesCheckRequest::default()).unwrap();
        assert!(
            checked[0]
                .examples
                .iter()
                .all(|e| e.status == ExampleStatus::Stale)
        );
        assert_eq!((checked[0].agree, checked[0].disagree), (0, 0));
        // A title edit is not a change of the node.
        std::fs::write(&current, PLAYBOOK.replace("title: Triage", "title: Sort")).unwrap();
        let checked = check(root.path(), "j", &ExamplesCheckRequest::default()).unwrap();
        assert_eq!(checked[0].examples[0].status, ExampleStatus::Checked);
    }

    /// A live check whose ask got no answer (the day's sandbox requests are
    /// spent) has nothing to compare: the example reads `unanswered` and
    /// none of its labels counts as a disagreement.
    #[test]
    fn a_live_check_without_an_answer_counts_no_disagreement() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::example::tests::a_live_check_without_an_answer_counts_no_disagreement",
            || {
                let cfg = tempfile::tempdir().unwrap();
                std::fs::write(
                    cfg.path().join(apb_core::decisions::DECISIONS_FILE),
                    "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: { type: choice, choice: clean, probabilities: { clean: 0.9, needs_fix: 0.1 }, confidence: 0.9 }\nuses:\n  judge_node: { mode: enforce }\n",
                )
                .unwrap();
                // SAFETY: a plain child process runs this one test on one thread.
                unsafe {
                    std::env::set_var("APB_CONFIG_DIR", cfg.path());
                    std::env::set_var("APB_DECISIONS_ALLOW_FAKE", "1");
                }
                let root = project("Fix the parser");
                save_from_run(
                    root.path(),
                    "r1",
                    "triage",
                    &expect(&[("verdict", "clean")]),
                    "cli",
                )
                .unwrap();
                // The day's sandbox requests are spent.
                let now = apb_core::clock::now_ms_u64();
                let line = serde_json::json!({ "ts_ms": now, "use_site": "judge_node", "provider": "fake", "cached": false, "use": super::super::sandbox::USE_JUDGE_SANDBOX });
                let log = root
                    .path()
                    .join(".apb")
                    .join(crate::decision::standalone::DECISIONS_LOG);
                let body: String = (0..super::super::sandbox::SANDBOX_DAILY_CAP)
                    .map(|_| format!("{line}\n"))
                    .collect();
                std::fs::write(&log, body).unwrap();
                let live = check(
                    root.path(),
                    "j",
                    &ExamplesCheckRequest {
                        node: Some("triage".into()),
                        live: true,
                    },
                )
                .unwrap();
                assert_eq!(live[0].examples[0].status, ExampleStatus::Unanswered);
                assert!(live[0].examples[0].questions.is_empty());
                assert_eq!((live[0].agree, live[0].disagree), (0, 0));
                assert!(live[0].confusion.is_empty());
            },
        );
    }

    /// A live ask the provider answered with an invalid shape did get an
    /// answer: the example is checked, and its label counts as a
    /// disagreement rather than `unanswered`.
    #[test]
    fn a_live_check_with_an_invalid_answer_counts_a_disagreement() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::example::tests::a_live_check_with_an_invalid_answer_counts_a_disagreement",
            || {
                let cfg = tempfile::tempdir().unwrap();
                std::fs::write(
                    cfg.path().join(apb_core::decisions::DECISIONS_FILE),
                    "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: { type: choice, choice: bogus, probabilities: { bogus: 0.9 }, confidence: 0.9 }\nuses:\n  judge_node: { mode: enforce }\n",
                )
                .unwrap();
                // SAFETY: a plain child process runs this one test on one thread.
                unsafe {
                    std::env::set_var("APB_CONFIG_DIR", cfg.path());
                    std::env::set_var("APB_DECISIONS_ALLOW_FAKE", "1");
                }
                let root = project("Fix the parser");
                save_from_run(
                    root.path(),
                    "r1",
                    "triage",
                    &expect(&[("verdict", "clean")]),
                    "cli",
                )
                .unwrap();
                let live = check(
                    root.path(),
                    "j",
                    &ExamplesCheckRequest {
                        node: Some("triage".into()),
                        live: true,
                    },
                )
                .unwrap();
                let ex = &live[0].examples[0];
                let raw = &ex.evaluation.as_ref().unwrap().raw;
                assert!(raw["verdict"].invalid.is_some(), "{raw:?}");
                assert_eq!(ex.status, ExampleStatus::Checked);
                assert_eq!((live[0].agree, live[0].disagree), (0, 1));
            },
        );
    }

    /// `live` asks the sandbox again inside its daily cap: the request is
    /// counted, and the labels are checked against the new answer.
    #[test]
    fn a_live_check_asks_again_through_the_sandbox() {
        apb_core::process_mode::testing::in_plain_child(
            "judge_eval::example::tests::a_live_check_asks_again_through_the_sandbox",
            || {
                let cfg = tempfile::tempdir().unwrap();
                std::fs::write(
                    cfg.path().join(apb_core::decisions::DECISIONS_FILE),
                    "version: 1\nmode: enforce\nproviders:\n  - id: fake\n    kind: fake\n    answers:\n      verdict: { type: choice, choice: needs_fix, probabilities: { clean: 0.1, needs_fix: 0.9 }, confidence: 0.9 }\nuses:\n  judge_node: { mode: enforce }\n",
                )
                .unwrap();
                // SAFETY: a plain child process runs this one test on one thread.
                unsafe {
                    std::env::set_var("APB_CONFIG_DIR", cfg.path());
                    std::env::set_var("APB_DECISIONS_ALLOW_FAKE", "1");
                }
                let root = project("Fix the parser");
                save_from_run(
                    root.path(),
                    "r1",
                    "triage",
                    &expect(&[("verdict", "clean")]),
                    "cli",
                )
                .unwrap();
                let observed = check(root.path(), "j", &ExamplesCheckRequest::default()).unwrap();
                assert_eq!((observed[0].agree, observed[0].requests), (1, 0));
                let live = check(
                    root.path(),
                    "j",
                    &ExamplesCheckRequest {
                        node: Some("triage".into()),
                        live: true,
                    },
                )
                .unwrap();
                assert_eq!(
                    (live[0].agree, live[0].disagree, live[0].requests),
                    (0, 1, 1)
                );
                // The day counts the ask's worst case.
                assert_eq!(
                    super::super::sandbox::requests_today(root.path()),
                    crate::decision::PROVIDER_ATTEMPTS
                );
            },
        );
    }

    /// A file that does not read, or that names another place than where it
    /// sits, fails the listing by name instead of dropping its labels.
    #[test]
    fn an_unreadable_or_misplaced_example_is_refused_by_name() {
        let root = project("Fix the parser");
        save_from_run(
            root.path(),
            "r1",
            "triage",
            &expect(&[("verdict", "clean")]),
            "cli",
        )
        .unwrap();
        let dir = root.path().join(".apb/playbooks/j/evals/judge/triage");
        std::fs::write(dir.join("ex-0009.yaml"), "schema: [").unwrap();
        let err = list(root.path(), "j", None).unwrap_err();
        assert!(err.to_string().contains("triage/ex-0009"), "{err}");
        let moved = std::fs::read_to_string(dir.join("ex-0001.yaml")).unwrap();
        std::fs::write(dir.join("ex-0009.yaml"), moved).unwrap();
        let err = list(root.path(), "j", Some("triage")).unwrap_err();
        assert!(err.to_string().contains("names another place"), "{err}");
    }

    const WORKED: &str = r#"schema: 1
id: ex-0007
playbook: task-check-first
node: check_task
node_digest: j1:sha256:aa
questions_digest: sha256:bb
state_digest: sha256:cc
inputs:
  instruction: "Fix the parser"
  outputs: { clarify: "" }
expected: { sufficient: missing_detail, risky: false }
source: { run: task-check-first-1793200094570, execution: 1, decision_seq: 6 }
labelled_by: owner
labelled_at: 2026-10-28
observed: { provider: fake, model: fake-1, answers: { sufficient: { value: clear, p: 0.62, confidence: 0.41 } } }
note: ""
"#;

    /// The documented example file reads as written (a yes/no and a named
    /// answer, a run source, the date as text) and writes back to the same
    /// value; a typo anywhere in it is refused rather than dropped, since
    /// an author's label must never vanish silently.
    #[test]
    fn an_example_file_reads_as_documented_and_refuses_a_misspelt_key() {
        let ex: JudgeExample = yaml_serde::from_str(WORKED).unwrap();
        assert_eq!(
            ex.expected,
            BTreeMap::from([
                ("risky".to_string(), ExpectedAnswer::Yes(false)),
                (
                    "sufficient".to_string(),
                    ExpectedAnswer::Value("missing_detail".into())
                ),
            ])
        );
        assert_eq!(ex.source.decision_seq, Some(6));
        assert!(!ex.source.hand);
        assert_eq!(ex.labelled_at, "2026-10-28");
        assert_eq!(ex.inputs.outputs["clarify"], "");
        let observed = ex.observed.as_ref().unwrap();
        assert_eq!(observed.answers["sufficient"].confidence, Some(0.41));
        let back: JudgeExample =
            yaml_serde::from_str(&yaml_serde::to_string(&ex).unwrap()).unwrap();
        assert_eq!(back, ex);
        let by_hand: JudgeExample = yaml_serde::from_str(&WORKED.replace(
            "{ run: task-check-first-1793200094570, execution: 1, decision_seq: 6 }",
            "{ hand: true }",
        ))
        .unwrap();
        assert_eq!(
            by_hand.source,
            ExampleSource {
                hand: true,
                ..Default::default()
            }
        );
        for (from, to) in [
            ("note: \"\"", "notes: \"\""),
            ("decision_seq:", "decision_sq:"),
            ("  instruction:", "  instructions:"),
            ("model: fake-1,", "model: fake-1, by: owner,"),
        ] {
            let text = WORKED.replacen(from, to, 1);
            assert!(
                yaml_serde::from_str::<JudgeExample>(&text).is_err(),
                "accepted `{to}`"
            );
        }
    }

    /// `JudgeInputs` is `deny_unknown_fields` and lives inside stored
    /// examples: an empty seen map must not be written at all, or every
    /// example of a playbook without a `no_new_items` condition would stop
    /// reading back (spec loops-and-graphs D2).
    #[test]
    fn an_empty_seen_map_is_serialized_as_absent() {
        let inputs = JudgeInputs::default();
        let json = serde_json::to_value(&inputs).unwrap();
        assert!(json.get("seen").is_none(), "{json}");
        let with_seen = JudgeInputs {
            seen: BTreeMap::from([("hunt.findings".to_string(), "a\nb".to_string())]),
            ..Default::default()
        };
        let json = serde_json::to_value(&with_seen).unwrap();
        assert_eq!(
            json.get("seen"),
            Some(&serde_json::json!({"hunt.findings": "a\nb"}))
        );
    }

    /// "Fill from a run" carries the seen text of the playbook's
    /// `no_new_items` conditions, folded from the successful visits before
    /// the judge execution started, redacted like the outputs.
    #[test]
    fn filling_from_a_run_fills_the_seen_text_of_the_loop() {
        const LOOPED: &str = "schema: 2\nid: j\nname: j\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: hunt, type: script, script: 'true', runner: sh }\n  - id: triage\n    type: judge\n    title: Triage\n    state: { task: \"{{run.instruction}}\" }\n    questions:\n      verdict: { type: choice, instructions: \"Which?\", criteria: { clean: a, needs_fix: b } }\n    thresholds:\n      verdict: { min_confidence: 0.5, below: needs_fix }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: hunt }\n  - { from: hunt, to: triage }\n  - { from: triage, to: done, condition: { type: no_new_items, node: hunt, field: findings, rounds: 2 } }\n";
        let root = tempfile::tempdir().unwrap();
        let pb = root.path().join(".apb/playbooks/j");
        std::fs::create_dir_all(pb.join("1.0.0")).unwrap();
        std::fs::write(pb.join("1.0.0/playbook.yaml"), LOOPED).unwrap();
        std::fs::write(pb.join("current"), "1.0.0\n").unwrap();
        let run = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("playbook.yaml"), LOOPED).unwrap();
        let mut log = crate::event::EventLog::create(&run).unwrap();
        for e in [
            serde_json::json!({ "type": "run_started", "playbook": "j", "version": "1.0.0" }),
            serde_json::json!({ "type": "node_started", "node": "hunt", "attempt": 1 }),
            serde_json::json!({ "type": "node_finished", "node": "hunt", "status": "succeeded", "attempt": 1, "output": "{\"findings\":[\"a\",\"b\"]}" }),
            serde_json::json!({ "type": "node_started", "node": "triage", "attempt": 1 }),
            serde_json::json!({ "type": "decision_made", "use_site": "judge_node", "node": "triage", "provider": "fake", "model": "fake-1", "mode": "enforce", "answers": serde_json::json!({ "verdict": { "value": "clean", "p": 0.9, "confidence": 0.9 } }) }),
        ] {
            log.append(payload(e)).unwrap();
        }
        let v = run_values(root.path(), "r1", "triage", None).unwrap();
        assert_eq!(
            v.inputs.seen.get("hunt.findings").map(String::as_str),
            Some("a\nb"),
            "{:?}",
            v.inputs.seen
        );
    }
}
