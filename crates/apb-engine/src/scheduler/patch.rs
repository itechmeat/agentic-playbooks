//! Supervisor patch application: validate, apply, and promote playbook patches.
//! Split out of `scheduler` for navigability; shares the parent module's imports via `use super::*`.

use super::*;

#[derive(Debug)]
pub(crate) struct AppliedPatch {
    version: String,
    classification: String,
    continue_from: String,
    base: Playbook,
}

pub(crate) struct PatchCommand {
    pub(crate) version: String,
    pub(crate) classification: String,
    pub(crate) continue_from: String,
}

pub(crate) enum PatchResult {
    Applied(Box<AppliedPatch>),
    Rejected,
    Paused,
}

pub(crate) fn append_patch_rejected(
    log: &mut EventLog,
    reason: String,
) -> Result<PatchResult, EngineError> {
    log.append(EventPayload::PatchRejected { reason })?;
    Ok(PatchResult::Rejected)
}

pub(crate) fn is_terminal_node_status(status: NodeStatus) -> bool {
    !matches!(
        status,
        NodeStatus::Pending | NodeStatus::Ready | NodeStatus::Running
    )
}

pub(crate) fn apply_patch(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    cfg: &RunConfig,
    playbook: &mut Playbook,
    current: &mut String,
    command: PatchCommand,
) -> Result<PatchResult, EngineError> {
    let PatchCommand {
        version,
        classification,
        continue_from,
    } = command;
    let events = read_all(run_dir)?;
    let handled = events
        .iter()
        .filter(|event| {
            matches!(
                event.payload,
                EventPayload::PatchApplied { .. } | EventPayload::PatchRejected { .. }
            )
        })
        .count();
    let limit = cfg.max_patches_per_run.unwrap_or(5);
    if handled >= usize::try_from(limit).unwrap_or(usize::MAX) {
        log.append(EventPayload::RunPaused {
            reason: format!(
                "max patches per run exhausted: {handled} patches handled (limit {limit})"
            ),
        })?;
        return Ok(PatchResult::Paused);
    }

    if !is_safe_segment(&version) || !is_safe_segment(&continue_from) {
        return append_patch_rejected(log, "invalid patch path segment".into());
    }
    if !matches!(classification.as_str(), "improvement" | "workaround") {
        return append_patch_rejected(log, "invalid patch classification".into());
    }

    let Some(playbook_id) = events.iter().find_map(|event| match &event.payload {
        EventPayload::RunStarted { playbook, .. } => Some(playbook.clone()),
        _ => None,
    }) else {
        return append_patch_rejected(log, "run has no playbook identity".into());
    };
    if !is_safe_segment(&playbook_id) {
        return append_patch_rejected(log, "invalid playbook id".into());
    }

    let loaded = match Registry::open(root)
        .and_then(|registry| registry.load(&playbook_id, Some(&version)))
    {
        Ok(loaded) => loaded,
        Err(error) => return append_patch_rejected(log, error.to_string()),
    };
    let state = RunState::fold(&events);
    let executed: Vec<String> = state
        .nodes
        .iter()
        .filter_map(|(node, status)| is_terminal_node_status(*status).then_some(node.clone()))
        .collect();
    if let Err(error) = validate_migration(playbook, &loaded.playbook, &executed, &continue_from) {
        return append_patch_rejected(log, error.to_string());
    }

    let from_version = playbook.version.clone();
    let base = playbook.clone();
    snapshot_playbook(run_dir, &loaded.yaml)?;
    log.append(EventPayload::PatchApplied {
        version: version.clone(),
        classification: classification.clone(),
        continue_from: continue_from.clone(),
    })?;
    log.append(EventPayload::RunMigrated {
        from_version,
        to_version: version.clone(),
        continue_from: continue_from.clone(),
    })?;
    *playbook = loaded.playbook;
    *current = continue_from.clone();

    Ok(PatchResult::Applied(Box::new(AppliedPatch {
        version,
        classification,
        continue_from,
        base,
    })))
}

pub(crate) fn nodes_differ(
    base: &apb_core::schema::Node,
    patched: &apb_core::schema::Node,
) -> bool {
    match (serde_json::to_value(base), serde_json::to_value(patched)) {
        (Ok(base), Ok(patched)) => base != patched,
        _ => true,
    }
}

pub(crate) fn changed_nodes_succeeded(
    applied: &AppliedPatch,
    patched: &Playbook,
    state: &RunState,
) -> bool {
    let mut changed = BTreeSet::new();
    changed.insert(applied.continue_from.as_str());
    for node in &patched.nodes {
        if applied
            .base
            .node(&node.id)
            .is_none_or(|base| nodes_differ(base, node))
        {
            changed.insert(node.id.as_str());
        }
    }
    changed
        .into_iter()
        .all(|node| state.nodes.get(node) == Some(&NodeStatus::Succeeded))
}

pub(crate) fn promote_applied_patch(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    playbook: &Playbook,
    applied: &AppliedPatch,
) -> Result<(), EngineError> {
    let state = RunState::fold(&read_all(run_dir)?);
    // prior_successes is hardcoded to 0: there is no cross-run version success
    // counter yet in 6a. The default OnSuccess policy (and Always) do not depend
    // on it; AfterNSuccesses(n>1) therefore does not promote - the counter will be
    // added in Phase 6b.
    if should_promote(
        promote_policy(playbook),
        &applied.classification,
        true,
        changed_nodes_succeeded(applied, playbook, &state),
        0,
    ) {
        promote_version(root, &playbook.id, &applied.version)?;
        log.append(EventPayload::VersionPromoted {
            version: applied.version.clone(),
        })?;
    }
    Ok(())
}
