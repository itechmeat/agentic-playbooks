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
        log.append(crate::pause::engine_paused(format!(
            "max patches per run exhausted: {handled} patches handled (limit {limit})"
        )))?;
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
        .filter_map(|(node, status)| status.has_executed().then_some(node.clone()))
        .collect();
    if let Err(error) = validate_migration(playbook, &loaded.playbook, &executed, &continue_from) {
        return append_patch_rejected(log, error.to_string());
    }
    // 0.24.0 irreversible consent: a patch may not bring irreversible steps
    // the run's consent does not cover. A run started without consent may
    // not gain any; a consented run may not gain sources beyond the ones the
    // person consented to (the manifest records them).
    if let Some(reason) = uncovered_consent_sources(root, run_dir, playbook, &loaded.playbook) {
        return append_patch_rejected(log, reason);
    }
    if let Some(reason) = super::baseline::patch_refusal(playbook, &loaded.playbook, &events) {
        return append_patch_rejected(log, reason);
    }
    if let Some(reason) =
        super::judge::patch_refusal(playbook, &loaded.playbook, &events, &continue_from)
    {
        return append_patch_rejected(log, reason);
    }

    let from_version = playbook.version.clone();
    let base = playbook.clone();
    // The run's secret set only grows: the declarations of the snapshot the
    // patch replaces and of the patched playbook both stay recorded, before
    // the new snapshot can drop one.
    crate::run_redact::keep_secret_declarations(run_dir, &[playbook, &loaded.playbook])?;
    // The launch policy is approved content: a supervisor's patch never
    // loosens what the run's snapshot declares.
    if loaded.playbook.agent_launch.unwrap_or_default() < playbook.agent_launch.unwrap_or_default()
    {
        return append_patch_rejected(
            log,
            format!(
                "{}: the patch declares agent_launch `{}`, looser than the run's `{}`; only a person may loosen a launch policy",
                apb_core::launch::floor::LOOSENED,
                loaded.playbook.agent_launch.unwrap_or_default(),
                playbook.agent_launch.unwrap_or_default()
            ),
        );
    }
    // Store the definition, replace the snapshot, journal: in this order,
    // so a journaled digest names stored bytes whenever the store worked.
    // The digest is for surfaces only; a resume executes `playbook.yaml`
    // whatever it says.
    let definition = crate::run_definitions::store_best_effort(run_dir, loaded.yaml.as_bytes());
    snapshot_playbook(run_dir, &loaded.yaml)?;
    log.append(EventPayload::PatchApplied {
        version: version.clone(),
        classification: classification.clone(),
        continue_from: continue_from.clone(),
        definition: Some(definition),
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

/// The rejection reason when `patched` declares consent sources that the
/// run's recorded consent does not cover, `None` when it adds none. A
/// sub-playbook node that keeps its id but now runs another playbook is a
/// new source too: the consent covered the child it named.
fn uncovered_consent_sources(
    root: &Path,
    run_dir: &Path,
    running: &Playbook,
    patched: &Playbook,
) -> Option<String> {
    // Children resolve from the run's own origin, as the run spawns them: a
    // global playbook's `auto` child is the global one.
    let origin = super::parent_run_origin(run_dir);
    let sources = crate::gate::consent_sources(root, patched, &origin, None);
    let consent = crate::manifest::read(run_dir)
        .ok()
        .flatten()
        .and_then(|m| m.consent)
        .filter(|c| c.irreversible);
    let covered: &[String] = consent.as_ref().map_or(&[], |c| &c.sources);
    let child_ref = |p: &Playbook, id: &str| {
        p.nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| match &n.kind {
                apb_core::schema::NodeKind::Playbook { playbook, .. } => Some(playbook.clone()),
                _ => None,
            })
    };
    let retargeted = |s: &String| {
        patched.nodes.iter().any(|n| {
            crate::consent::sub_playbook_source(&n.id) == *s
                && child_ref(running, &n.id) != child_ref(patched, &n.id)
        })
    };
    let added: Vec<&str> = sources
        .iter()
        .filter(|s| !covered.contains(s) || retargeted(s))
        .map(String::as_str)
        .collect();
    if added.is_empty() {
        return None;
    }
    Some(format!(
        "{}: the patched version adds irreversible steps ({}) that {}; start a new run to ask the person",
        crate::consent::REFUSAL_POLICY,
        added.join(", "),
        if consent.is_some() {
            "the run's consent does not cover"
        } else {
            "a run started without consent may not gain"
        }
    ))
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
    // An in-run patch is judged by this run alone: no earlier success counts
    // for it (`prior_successes` 0), so `after_n_successes` above 1 and a
    // quality policy never promote it here; a forward patch's trials do.
    if promote_policy(playbook).should_promote(
        &applied.classification,
        true,
        changed_nodes_succeeded(applied, playbook, &state),
        0,
    ) {
        let run_id = run_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        current::move_current(
            &apb_core::candidate::playbook_dir(root, &playbook.id),
            &current::MoveCurrent::new(
                &applied.version,
                current::MovedBy::PatchPromotion { run_id },
            ),
        )
        .map_err(|e| e.into_versioning(&playbook.id))?;
        log.append(EventPayload::VersionPromoted {
            version: applied.version.clone(),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: review, type: script, script: s, runner: sh }\n  - { id: fix, type: finish, outcome: success }\n  - { id: ship, type: finish, outcome: success }\nedges:\n  - { from: start, to: review }\n  - { from: review, to: fix, condition: { type: judge, question: q, min_p: 0.7, on_unavailable: false } }\n  - { from: review, to: ship, fallback: true }\n";

    /// The running definition with an edge inserted before the judge edge,
    /// so the judge edge's rank moves.
    fn patched() -> String {
        RUNNING
            .replace("version: 1.0.0", "version: 1.0.1")
            .replace(
                "  - { from: review, to: fix, condition",
                "  - { from: review, to: ship, condition: { type: node_status, node: review, equals: failure } }\n  - { from: review, to: fix, condition",
            )
    }

    /// A project with both versions and a run held between the judge-edge
    /// answer of `review` and its routing.
    fn held_run() -> (tempfile::TempDir, PathBuf, EventLog) {
        let root = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(root.path()).unwrap();
        for (version, yaml) in [("1.0.0", RUNNING.to_string()), ("1.0.1", patched())] {
            let dir = root.path().join(format!(".apb/playbooks/p/{version}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("playbook.yaml"), yaml).unwrap();
        }
        std::fs::write(root.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
        let run_dir = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"), RUNNING).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        for body in [
            serde_json::json!({"type": "run_started", "playbook": "p", "version": "1.0.0"}),
            serde_json::json!({"type": "node_started", "node": "start", "attempt": 1}),
            serde_json::json!({"type": "node_finished", "node": "start", "status": "succeeded", "attempt": 1, "output": "", "artifacts": []}),
            serde_json::json!({"type": "edge_traversed", "from": "start", "to": "review", "uncounted": true}),
            serde_json::json!({"type": "node_started", "node": "review", "attempt": 1}),
            serde_json::json!({"type": "node_finished", "node": "review", "status": "succeeded", "attempt": 1, "output": "", "artifacts": []}),
            serde_json::json!({"type": "decision_made", "use_site": "judge_edge", "node": "review", "attempt": 1, "applied": true, "answers": {"edge_0": {"value": "true", "p": 0.9}}}),
        ] {
            let mut v = body;
            v["seq"] = 0.into();
            v["ts"] = 0.into();
            log.append(serde_json::from_value::<Event>(v).unwrap().payload)
                .unwrap();
        }
        (root, run_dir, log)
    }

    fn patch(
        root: &Path,
        run_dir: &Path,
        log: &mut EventLog,
        continue_from: &str,
    ) -> (PatchResult, Playbook) {
        let mut playbook = Playbook::from_yaml(RUNNING).unwrap();
        let mut current = "review".to_string();
        let result = apply_patch(
            root,
            run_dir,
            log,
            &RunConfig::default(),
            &mut playbook,
            &mut current,
            PatchCommand {
                version: "1.0.1".into(),
                classification: "improvement".into(),
                continue_from: continue_from.into(),
            },
        )
        .unwrap();
        (result, playbook)
    }

    #[test]
    fn a_pending_judge_answer_refuses_the_patch_and_changes_nothing() {
        let (root, run_dir, mut log) = held_run();
        let (result, playbook) = patch(root.path(), &run_dir, &mut log, "start");
        assert!(matches!(result, PatchResult::Rejected));
        let events = read_all(&run_dir).unwrap();
        let last = events.last().unwrap();
        assert!(
            matches!(&last.payload, EventPayload::PatchRejected { reason } if reason.starts_with("judge_edge_answers_pending") && reason.contains("[review]")),
            "{last:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::PatchApplied { .. }))
        );
        assert_eq!(
            std::fs::read_to_string(run_dir.join("playbook.yaml")).unwrap(),
            RUNNING
        );
        assert!(!run_dir.join(crate::run_definitions::DIR).exists());
        assert_eq!(playbook.version, "1.0.0");
    }

    #[test]
    fn continue_from_the_judged_node_applies_the_patch_in_order() {
        let (root, run_dir, mut log) = held_run();
        let (result, playbook) = patch(root.path(), &run_dir, &mut log, "review");
        assert!(matches!(result, PatchResult::Applied(_)));
        let events = read_all(&run_dir).unwrap();
        let definition = events.iter().find_map(|e| match &e.payload {
            EventPayload::PatchApplied { definition, .. } => definition.clone(),
            _ => None,
        });
        let want = crate::run_definitions::digest(patched().as_bytes());
        assert_eq!(definition.as_deref(), Some(want.as_str()));
        // Stored, then the snapshot replaced, then journaled.
        assert_eq!(
            crate::run_definitions::read_stored(&run_dir, &want).unwrap(),
            patched().as_bytes()
        );
        assert_eq!(
            std::fs::read_to_string(run_dir.join("playbook.yaml")).unwrap(),
            patched()
        );
        assert_eq!(playbook.version, "1.0.1");
    }

    #[test]
    fn a_dead_end_after_the_answer_lets_the_patch_through() {
        let (root, run_dir, mut log) = held_run();
        log.append(EventPayload::RunFinished {
            outcome: "failed".into(),
        })
        .unwrap();
        let (result, _) = patch(root.path(), &run_dir, &mut log, "start");
        assert!(matches!(result, PatchResult::Applied(_)));
    }
}
