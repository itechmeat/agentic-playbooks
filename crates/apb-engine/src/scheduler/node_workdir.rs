//! A node's own working directory (issue #67 item 4): the `workdir` template
//! of an agent_task or a script, rendered with the prompt template engine
//! and resolved against the execution root, so a later node can run in a git
//! worktree an earlier node published as a named output.

use super::*;

/// The directory `node_id` runs in, its `workdir` template rendered by
/// `render` (the prompt renderer): `Ok(root)` when the node declares no
/// `workdir`, the resolved directory when it does, and `Err(message)` when the
/// template renders empty or names no existing directory. The error is a node
/// failure, never a silent fallback to the execution root: running a gate in
/// the wrong tree and reporting it green is the failure this field prevents.
pub(crate) fn resolve(
    playbook: &Playbook,
    node_id: &str,
    root: &Path,
    render: impl FnOnce(&str) -> Result<String, EngineError>,
) -> Result<Result<PathBuf, String>, EngineError> {
    let Some(template) = playbook
        .node(node_id)
        .and_then(|n| n.kind.workdir_template())
    else {
        return Ok(Ok(root.to_path_buf()));
    };
    let rendered = render(template)?;
    let rendered = rendered.trim();
    if rendered.is_empty() {
        return Ok(Err(format!(
            "node `{node_id}` workdir `{template}` rendered empty: the value it reads was never published, so the node was not started"
        )));
    }
    let path = root.join(rendered);
    match path.canonicalize() {
        Ok(dir) if dir.is_dir() => Ok(Ok(dir)),
        _ => Ok(Err(format!(
            "node `{node_id}` workdir `{rendered}` (from `{template}`) is not an existing directory, so the node was not started"
        ))),
    }
}
