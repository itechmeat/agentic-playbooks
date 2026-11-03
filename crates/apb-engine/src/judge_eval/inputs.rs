//! The inputs a judge's state renders from, and the one render every
//! template of a run goes through.
//!
//! A run gathers its [`JudgeInputs`] from the journal and the run state
//! ([`JudgeInputs::from_state`]); a scenario run does the same; the sandbox
//! takes them typed or from a past run. [`JudgeInputs::render`] is
//! `context::render` over them, so a node prompt, a judge state field and a
//! cache key render alike everywhere.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::schema::{ContextBudget, NodeKind, Playbook};

use crate::context::OutputClip;
use crate::decision::{FieldClass, StateField, StateParts, intern};
use crate::error::EngineError;
use crate::event::read_all;
use crate::run_config::RunConfig;
use crate::state::{ReviewDecision, RunState};
use serde::{Deserialize, Serialize};

/// Everything a judge node's state renders from, field by field: what the
/// template context reads in a run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeInputs {
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub instruction: Option<String>,
    /// Node outputs by node id.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    /// Review decisions by review node id.
    #[serde(default)]
    pub reviews: BTreeMap<String, ReviewInput>,
    /// The output a rejection sent back, by node id.
    #[serde(default)]
    pub rejected_outputs: BTreeMap<String, String>,
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
    /// `run.context` as it would render.
    #[serde(default)]
    pub context: String,
}

/// A review's decision and note, as a template reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    pub decision: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// One rendered state field: what was sent for it. Never journaled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedField {
    pub name: String,
    /// The field's material class: `prompt`, or `output` when it carries
    /// what a node produced.
    pub class: String,
    /// Bytes sent after redaction and the clip.
    pub bytes: usize,
    pub clipped: bool,
    /// Template references that rendered as nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unfilled: Vec<String>,
}

/// Each state field's own clip, head and tail kept, before the share of
/// `privacy.max_state_bytes` it may take.
pub(crate) const FIELD_HEAD: usize = 12 * 1024;
pub(crate) const FIELD_TAIL: usize = 12 * 1024;

impl JudgeInputs {
    /// What a template of node budget `budget` reads in the run of
    /// `run_dir` now: the run's params and instruction, the folded outputs
    /// and reviews, the hook paths and the run context as it renders.
    pub(crate) fn from_state(
        run_dir: &Path,
        run_id: &str,
        state: &RunState,
        cfg: &RunConfig,
        budget: &ContextBudget,
    ) -> Result<Self, EngineError> {
        let context = crate::context::build_context_for_render(
            run_dir,
            &read_all(run_dir)?,
            cfg.instruction.as_deref(),
            budget,
        )?;
        let hooks = crate::hooks::read_hooks(run_dir)?.paths(run_id);
        Ok(JudgeInputs {
            params: cfg.params.clone(),
            instruction: cfg.instruction.clone(),
            outputs: state.outputs.clone(),
            reviews: state
                .reviews
                .iter()
                .map(|(k, r)| {
                    (
                        k.clone(),
                        ReviewInput {
                            decision: r.decision.clone(),
                            note: Some(r.note.clone()).filter(|n| !n.is_empty()),
                        },
                    )
                })
                .collect(),
            rejected_outputs: state.rejected_outputs.clone(),
            hooks,
            context,
        })
    }

    /// `template` rendered over these inputs; outputs over `max_bytes` are
    /// clipped with a note pointing into `clip_dir`.
    pub(crate) fn render(&self, template: &str, clip_dir: &Path, max_bytes: usize) -> String {
        let reviews: BTreeMap<String, ReviewDecision> = self
            .reviews
            .iter()
            .map(|(k, r)| {
                (
                    k.clone(),
                    ReviewDecision {
                        decision: r.decision.clone(),
                        note: r.note.clone().unwrap_or_default(),
                    },
                )
            })
            .collect();
        crate::context::render(
            template,
            &self.params,
            self.instruction.as_deref(),
            &self.outputs,
            &reviews,
            &self.rejected_outputs,
            &self.hooks,
            &self.context,
            &OutputClip {
                run_dir: clip_dir,
                max_bytes,
                retro: Some(crate::run_retro::text::prompt_text),
            },
        )
    }

    /// The template references of `template` that render as nothing over
    /// these inputs, each with why: `never ran` (no output of that node),
    /// `field absent` or `not JSON` (a field selector over an output), or
    /// `empty` (anything else that is empty). Each reference is rendered
    /// as the template is, against the run directory `run_dir` (a run
    /// reference such as `{{run.id}}` reads it).
    pub(crate) fn unfilled(&self, template: &str, run_dir: &Path) -> Vec<(String, &'static str)> {
        apb_core::template::refs(template)
            .into_iter()
            .filter_map(|r| {
                let rendered = self.render(&format!("{{{{{r}}}}}"), run_dir, 0);
                if !rendered.is_empty() {
                    return None;
                }
                let why = match r.split('.').collect::<Vec<_>>().as_slice() {
                    ["nodes", node, ..] if !self.outputs.contains_key(*node) => "never ran",
                    ["nodes", node, "output", _, ..] => {
                        match serde_json::from_str::<serde_json::Value>(&self.outputs[*node]) {
                            Ok(serde_json::Value::Object(_)) => "field absent",
                            _ => "not JSON",
                        }
                    }
                    _ => "empty",
                };
                Some((r, why))
            })
            .collect()
    }
}

/// The judge node `node_id`'s state, rendered over `inputs`: each field a
/// template; a field reading a node's output or the run context is
/// `output` material, any other `prompt`.
pub(crate) fn render_state(
    playbook: &Playbook,
    node_id: &str,
    inputs: &JudgeInputs,
    clip_dir: &Path,
) -> Result<(StateParts, Vec<RenderedField>), EngineError> {
    let node = playbook
        .node(node_id)
        .ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    let NodeKind::Judge { state: fields, .. } = &node.kind else {
        return Err(EngineError::Invalid(format!(
            "node `{node_id}` is not a judge"
        )));
    };
    let max_bytes = playbook.context_budget(node_id).output_max_bytes;
    let mut parts = StateParts::default();
    let mut rendered = Vec::new();
    for (name, template) in fields.iter() {
        let text = inputs.render(template, clip_dir, max_bytes);
        let reads_output = apb_core::template::refs(template)
            .iter()
            .any(|r| r.starts_with("nodes.") || r == "run.context");
        let field = state_field(name, text, reads_output);
        rendered.push(RenderedField {
            name: name.to_string(),
            class: match field.class {
                FieldClass::Output => "output",
                FieldClass::Prompt => "prompt",
            }
            .to_string(),
            bytes: field.text.len().min(field.head + field.tail),
            clipped: field.text.len() > field.head + field.tail,
            unfilled: inputs
                .unfilled(template, clip_dir)
                .into_iter()
                .map(|(r, _)| r)
                .collect(),
        });
        parts.fields.push(field);
    }
    Ok((parts, rendered))
}

/// One rendered state field: `output` material when it carries what a node
/// produced, `prompt` otherwise.
pub(crate) fn state_field(name: &str, text: String, reads_output: bool) -> StateField {
    StateField {
        name: intern(name),
        class: if reads_output {
            FieldClass::Output
        } else {
            FieldClass::Prompt
        },
        text,
        head: FIELD_HEAD,
        tail: FIELD_TAIL,
    }
}
