//! The run's working tree (issue #67 item 8): the directory agent_task and
//! script nodes without their own `workdir` run in, and the tree the run's
//! busy lock covers. Resolved once per run, either at start (a tree the
//! caller passes, or the playbook's `worktree` template over params) or when
//! the node the template reads succeeds; journaled as `worktree_resolved`,
//! so every later drive (a resume, a detached driver) reads it back from the
//! journal instead of resolving it again.

use super::*;
use crate::workdir::WorkdirGuard;

/// A resolved working tree and where it came from (`caller`, `playbook`,
/// `node`), the `source` of its `worktree_resolved` event.
pub(crate) struct ResolvedTree {
    pub(crate) path: PathBuf,
    pub(crate) source: &'static str,
}

/// Resolves `raw` against the execution root: a blank value, or one that
/// names no existing directory, is an error naming the value, never a silent
/// fallback to the execution root (running in the wrong tree is the failure
/// this value exists to prevent).
fn resolve_dir(root: &Path, raw: &str, from: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("the run worktree ({from}) is empty"));
    }
    let dir = match root.join(raw).canonicalize() {
        Ok(dir) if dir.is_dir() => dir,
        _ => {
            return Err(format!(
                "the run worktree `{raw}` ({from}) is not an existing directory"
            ));
        }
    };
    if belongs_to_project(root, &dir) {
        Ok(dir)
    } else {
        Err(format!(
            "the run worktree `{raw}` ({from}) is neither inside the project nor a git worktree of its repository"
        ))
    }
}

/// Whether `dir` may serve as a working tree of the project at `root`: a
/// directory inside the project, or a git worktree of the project's own
/// repository (the two share one git common directory). Anything else would
/// let a start request point every agent of the run at an unrelated part of
/// the disk under the project's lock and trust.
fn belongs_to_project(root: &Path, dir: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if dir.starts_with(&root) {
        return true;
    }
    let common = |d: &Path| crate::workdir::git_path(d, "--git-common-dir");
    match (common(&root), common(dir)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The tree known before the run starts: the caller's (which wins), else the
/// playbook's `worktree` template when it reads only params. `None` when
/// there is neither, or the template waits for a node's output. An
/// unresolvable tree refuses the start.
pub(crate) fn resolve_at_start(
    playbook: &Playbook,
    root: &Path,
    caller: Option<&str>,
    params: &BTreeMap<String, String>,
) -> Result<Option<ResolvedTree>, EngineError> {
    if let Some(raw) = caller.filter(|c| !c.trim().is_empty()) {
        let path = resolve_dir(root, raw, "passed at start").map_err(EngineError::Invalid)?;
        return Ok(Some(ResolvedTree {
            path,
            source: "caller",
        }));
    }
    let Some(template) = playbook.worktree_template() else {
        return Ok(None);
    };
    if playbook.worktree_source_node().is_some() {
        return Ok(None);
    }
    let rendered = render(
        template,
        params,
        None,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        "",
        &crate::context::OutputClip {
            run_dir: root,
            max_bytes: 0,
        },
    );
    let path = resolve_dir(root, &rendered, &format!("from `{template}`"))
        .map_err(EngineError::Invalid)?;
    Ok(Some(ResolvedTree {
        path,
        source: "playbook",
    }))
}

/// The tree a node's output names, once that node has succeeded: `Ok(None)`
/// while the run already has a tree, the playbook has no node-sourced
/// template, or the node has not succeeded yet. The template is rendered
/// with the prompt renderer, unclipped, so it reads exactly what the node
/// published. An unresolvable tree is an error: the nodes after it must not
/// run in the execution root believing they are in the tree.
pub(crate) fn resolve_from_node(
    playbook: &Playbook,
    root: &Path,
    run_dir: &Path,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
) -> Result<Option<(PathBuf, String)>, EngineError> {
    if state.worktree.is_some() {
        return Ok(None);
    }
    let (Some(template), Some(node)) = (
        playbook.worktree_template(),
        playbook.worktree_source_node(),
    ) else {
        return Ok(None);
    };
    if state.nodes.get(&node) != Some(&NodeStatus::Succeeded) {
        return Ok(None);
    }
    let rendered = render_node_prompt(
        run_dir,
        run_id,
        state,
        cfg,
        template,
        &apb_core::schema::ContextBudget::UNLIMITED,
    )?;
    let path = resolve_dir(
        root,
        &rendered,
        &format!("from `{template}` after node `{node}` succeeded"),
    )
    .map_err(EngineError::Invalid)?;
    Ok(Some((path, node)))
}

/// Moves a run's busy lock onto the tree a node just resolved: the new
/// tree's lock is taken first (waiting up to the run's queue ceiling when it
/// has one, else refusing at once), and only then is the old lock released,
/// so the run is never unlocked in between. A run that holds no lock (it
/// writes nothing, shares the workdir by request, or is a sub-playbook child
/// under its parent's lock) has nothing to move.
pub(crate) fn move_lock(
    root: &Path,
    run_dir: &Path,
    tree: &Path,
    cfg: &RunConfig,
    lock: &mut Option<WorkdirGuard>,
) -> Result<(), EngineError> {
    if lock.is_none() {
        return Ok(());
    }
    if crate::workdir::tree_lock_path(root, Some(tree))
        == crate::workdir::tree_lock_path(root, None)
    {
        return Ok(());
    }
    let next = match cfg.workdir_queue_wait_ms {
        Some(ms) => acquire_queued_tree(root, Some(tree), Duration::from_millis(ms), &mut || {
            matches!(crate::control::pending_stop_seq(run_dir), Ok(Some(_)))
        })?,
        None => acquire_tree(root, Some(tree), false)?,
    };
    *lock = next;
    Ok(())
}
