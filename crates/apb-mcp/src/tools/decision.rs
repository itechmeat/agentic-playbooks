//! `decision_ask` (issue #193): a bounded decision from the configured
//! decision providers, for a host task or a script. The engine function
//! ([`apb_engine::decision::host_task::ask`]) is shared with `apb decide`.

use std::path::Path;

use apb_engine::decision::host_task::{AskKind, AskRequest, ask};
use serde_json::{Value, json};

use super::ToolError;

/// What `decision_ask` receives, already unpacked from its arguments.
#[derive(Debug, Clone, Default)]
pub struct DecisionAskInput {
    pub kind: String,
    pub question: String,
    pub options: Vec<String>,
    pub items: Vec<String>,
    pub criteria: Option<String>,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
}

/// Asks one decision. A malformed request or an unknown run is a tool error;
/// a refusal (no provider, budget spent, switched off) is an answer with
/// `answered: false` and its `reason`.
pub fn decision_ask(root: &Path, input: DecisionAskInput) -> Result<Value, ToolError> {
    let Some(kind) = AskKind::parse(&input.kind) else {
        return Ok(json!({
            "error": "unknown_kind",
            "detail": format!(
                "kind must be choose, rank, filter, map, is or score, got `{}`",
                input.kind
            ),
        }));
    };
    let req = AskRequest {
        kind,
        question: input.question,
        options: input.options,
        items: input.items,
        criteria: input.criteria,
        run_id: input.run_id.filter(|r| !r.trim().is_empty()),
        node_id: input.node_id,
    };
    match ask(root, &req) {
        Ok(outcome) => Ok(outcome.to_json()),
        Err(apb_engine::EngineError::Invalid(m)) => {
            Ok(json!({ "error": "invalid_request", "detail": m }))
        }
        Err(e) => Err(e.into()),
    }
}
