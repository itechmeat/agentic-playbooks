//! The one entry point the drive executes a node through: a list node runs
//! its visit (`super::list`), every other node its own execution
//! (`super::node::execute_node`), which is also what a list item runs.

use super::outcome::{AttemptOutcome, ExecOptions, NodeCtx};
use crate::error::EngineError;

/// Executes `node_id`.
pub(crate) fn execute(
    ctx: &NodeCtx<'_, '_>,
    node_id: &str,
    opts: ExecOptions<'_>,
) -> Result<AttemptOutcome, EngineError> {
    if ctx
        .playbook
        .node(node_id)
        .is_some_and(|n| n.list().is_some())
    {
        return super::list::execute(ctx, node_id, &opts);
    }
    super::node::execute_node(ctx, node_id, opts)
}
