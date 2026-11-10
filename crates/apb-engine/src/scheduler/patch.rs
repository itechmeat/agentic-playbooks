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
    // The `run.previous_success` snapshot was taken at start over the
    // definition in force then: a patch may add no read of it the start
    // never snapshotted.
    if let Some(reason) =
        crate::previous_success::patch_refusal(playbook, &loaded.playbook, &events)
    {
        return append_patch_rejected(log, reason);
    }
    // The deadline is the run's contract: only a person changes it, at a
    // resume.
    if let Some(reason) = crate::run_deadline::patch_refusal(playbook, &loaded.playbook) {
        return append_patch_rejected(log, reason);
    }
    // A person-only gate and an approval are a person's constraints: a
    // supervisor's patch never weakens them, nor continues past a gate no
    // decision answered.
    if let Some(reason) =
        super::approval::patch_refusal(playbook, &loaded.playbook, &events, &continue_from)
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
    // The run guard follows the definition in force. A patched definition
    // that uses a guarded feature is guarded before its snapshot replaces
    // the running one (a resume executes `playbook.yaml` whatever the
    // journal says, so the guard must already stand when it does). A
    // supervisor's patch never drops such a feature (`approval::
    // patch_refusal` above); a guarded run whose patched definition names
    // none all the same ends its guard only once the patch is journaled.
    let guard = super::prepare::run_guard(&loaded.playbook, &events);
    let was_guarded = latest_guard(&events).is_some_and(|r| !r.is_empty());
    if let Some(guard) = guard.clone() {
        log.append(guard)?;
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
    if guard.is_none() && was_guarded {
        log.append(EventPayload::RunGuard { requires: vec![] })?;
    }
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

/// The `requires` of the latest `run_guard` of `events`, `None` when the
/// run never journaled one.
pub(super) fn latest_guard(events: &[Event]) -> Option<&[String]> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunGuard { requires } => Some(requires.as_slice()),
        _ => None,
    })
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
    // A run that processes external data keeps its patch versions to
    // itself: they never become `current`.
    if crate::external_data::is_flagged(run_dir) {
        return Ok(());
    }
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
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::RunGuard { .. })),
            "a definition without a guarded feature needs no guard"
        );
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

    const GATED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: gate, type: human_review, options: [approve, reject] }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: gate }\n  - { from: gate, to: done }\n";

    fn person_only() -> String {
        GATED.replace(
            "options: [approve, reject] }",
            "options: [approve, reject], person_only: true }",
        )
    }

    /// Patches a run of `running` (its journal guarded by `guard`, if any)
    /// to `patched`, continuing from `continue_from`, appends the next
    /// checkpoint when the patch applied, and returns whether it applied
    /// with the event types journaled from the patch on.
    fn patch_run(
        running: &str,
        patched: &str,
        guard: Option<&[&str]>,
        continue_from: &str,
    ) -> (bool, Vec<String>) {
        patch_run_with(running, patched, guard, continue_from, &[])
    }

    /// [`patch_run`] with `extra` journaled after `run_started` (a
    /// `run_provenance` that records a run argument).
    fn patch_run_with(
        running: &str,
        patched: &str,
        guard: Option<&[&str]>,
        continue_from: &str,
        extra: &[EventPayload],
    ) -> (bool, Vec<String>) {
        let root = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(root.path()).unwrap();
        let next = patched.replace("version: 1.0.0", "version: 1.0.1");
        for (version, yaml) in [("1.0.0", running), ("1.0.1", next.as_str())] {
            let dir = root.path().join(format!(".apb/playbooks/g/{version}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("playbook.yaml"), yaml).unwrap();
        }
        std::fs::write(root.path().join(".apb/playbooks/g/current"), "1.0.0").unwrap();
        let run_dir = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"), running).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "g".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        })
        .unwrap();
        for p in extra {
            log.append(p.clone()).unwrap();
        }
        if let Some(requires) = guard {
            log.append(EventPayload::RunGuard {
                requires: requires.iter().map(|s| s.to_string()).collect(),
            })
            .unwrap();
        }
        log.append(EventPayload::node_finished("start", "succeeded", 1, ""))
            .unwrap();
        let before = read_all(&run_dir).unwrap().len();
        let mut playbook = Playbook::from_yaml(running).unwrap();
        let mut current = "gate".to_string();
        let result = apply_patch(
            root.path(),
            &run_dir,
            &mut log,
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
        let applied = matches!(result, PatchResult::Applied(_));
        if applied {
            // The next checkpoint, as the drive appends it after the patch.
            log.append(EventPayload::node_finished("gate", "succeeded", 1, ""))
                .unwrap();
        } else {
            assert_eq!(
                std::fs::read_to_string(run_dir.join("playbook.yaml")).unwrap(),
                running,
                "a refused patch leaves the running definition"
            );
        }
        let events = read_all(&run_dir).unwrap()[before..]
            .iter()
            .map(|e| match &e.payload {
                EventPayload::RunGuard { requires } => format!("run_guard{requires:?}"),
                EventPayload::PatchRejected { reason } => format!("patch_rejected {reason}"),
                p => serde_json::to_value(p).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            })
            .collect();
        (applied, events)
    }

    fn patch_gate(running: &str, patched: &str, guard: Option<&[&str]>) -> Vec<String> {
        let (applied, events) = patch_run(running, patched, guard, "gate");
        assert!(applied, "{events:?}");
        events
    }

    /// The one line a refused patch journals.
    fn refused(running: &str, patched: &str, continue_from: &str) -> String {
        let (applied, events) = patch_run(running, patched, Some(&["person_only"]), continue_from);
        assert!(!applied, "{events:?}");
        assert_eq!(events.len(), 1, "{events:?}");
        events[0].clone()
    }

    /// A patch that brings a guarded feature guards the run before the
    /// patched definition replaces the running one, and the next
    /// checkpoint carries the guard.
    #[test]
    fn a_patch_that_adds_a_guarded_feature_guards_the_run_before_its_snapshot() {
        assert_eq!(
            patch_gate(GATED, &person_only(), None),
            [
                r#"run_guard["person_only"]"#,
                "patch_applied",
                "run_migrated",
                "node_finished",
                r#"run_guard["person_only"]"#,
            ]
        );
    }

    /// A run whose deadline is a run argument (no `deadline:` in the
    /// definition) keeps its `deadline` guard through a supervisor's
    /// patch, and a patch that adds a `deadline:` is refused.
    #[test]
    fn a_patch_keeps_the_guard_and_refuses_a_deadline_change() {
        let provenance: EventPayload = serde_json::from_value(serde_json::json!({
            "type": "run_provenance",
            "deadline": { "total_ms": 600_000, "source": "run" }
        }))
        .unwrap();
        let edited = GATED.replace("name: g\n", "name: g2\n");
        assert_ne!(edited, GATED, "the fixture changed");
        let (applied, events) = patch_run_with(
            GATED,
            &edited,
            Some(&["deadline"]),
            "gate",
            std::slice::from_ref(&provenance),
        );
        assert!(applied, "{events:?}");
        assert_eq!(
            events,
            [
                r#"run_guard["deadline"]"#,
                "patch_applied",
                "run_migrated",
                "node_finished",
                r#"run_guard["deadline"]"#,
            ]
        );
        let with_deadline = GATED.replace("version: 1.0.0\n", "version: 1.0.0\ndeadline: 1h\n");
        assert_ne!(with_deadline, GATED);
        let (applied, events) = patch_run_with(
            GATED,
            &with_deadline,
            Some(&["deadline"]),
            "gate",
            &[provenance],
        );
        assert!(!applied, "{events:?}");
        assert!(
            events[0].starts_with("patch_rejected deadline_changed"),
            "{events:?}"
        );
    }

    /// A supervisor's patch never drops `person_only`: the run keeps its
    /// definition and its guard.
    #[test]
    fn a_patch_that_drops_person_only_is_refused() {
        let line = refused(&person_only(), GATED, "gate");
        assert!(
            line.starts_with("patch_rejected approval_weakened: node `gate` drops `person_only`"),
            "{line}"
        );
    }

    /// Nor does it continue past a person-only gate no decision answered,
    /// nor open a way around one.
    #[test]
    fn a_patch_that_skips_or_bypasses_an_undecided_person_only_gate_is_refused() {
        let line = refused(&person_only(), &person_only(), "done");
        assert!(
            line.starts_with("patch_rejected approval_weakened: `done` skips gate `gate`"),
            "{line}"
        );
        let bypass = person_only().replace(
            "  - { from: gate, to: done }\n",
            "  - { from: gate, to: done }\n  - { from: start, to: done, condition: { type: node_status, node: start, equals: success } }\n",
        );
        let line = refused(&person_only(), &bypass, "gate");
        assert!(
            line.starts_with(
                "patch_rejected approval_weakened: node `done` becomes reachable without passing gate `gate`"
            ),
            "{line}"
        );
    }

    /// Patches a run whose journal holds a `run_provenance` with a
    /// `previous_success` record over `record_keys` (empty: none recorded),
    /// continuing from `start`, and returns the one line the patch
    /// journals.
    fn patch_prev(running: &str, patched: &str, record_keys: &[&str]) -> String {
        let root = tempfile::tempdir().unwrap();
        apb_core::registry::init_project(root.path()).unwrap();
        let next = patched.replace("version: 1.0.0", "version: 1.0.1");
        for (version, yaml) in [("1.0.0", running), ("1.0.1", next.as_str())] {
            let dir = root.path().join(format!(".apb/playbooks/p/{version}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("playbook.yaml"), yaml).unwrap();
        }
        std::fs::write(root.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
        let run_dir = root.path().join(".apb/runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("playbook.yaml"), running).unwrap();
        let mut log = EventLog::create(&run_dir).unwrap();
        log.append(EventPayload::RunStarted {
            playbook: "p".into(),
            version: "1.0.0".into(),
            definition: None,
            external_data: false,
        })
        .unwrap();
        log.append(EventPayload::RunProvenance {
            deadline: None,
            start_key: None,
            origin: Some("project".into()),
            digest: None,
            execution_root: None,
            scripts_digest: None,
            profiles: Vec::new(),
            remembered: Vec::new(),
            apb: None,
            limits: Vec::new(),
            untrusted: Vec::new(),
            previous_success: (!record_keys.is_empty()).then(|| crate::event::PreviousSuccess {
                run_id: Some("r0".into()),
                outputs: record_keys
                    .iter()
                    .map(|k| ((*k).to_string(), "v".into()))
                    .collect(),
                ..Default::default()
            }),
        })
        .unwrap();
        log.append(EventPayload::node_finished("start", "succeeded", 1, ""))
            .unwrap();
        let before = read_all(&run_dir).unwrap().len();
        let mut playbook = Playbook::from_yaml(running).unwrap();
        let mut current = "start".to_string();
        let _result = apply_patch(
            root.path(),
            &run_dir,
            &mut log,
            &RunConfig::default(),
            &mut playbook,
            &mut current,
            PatchCommand {
                version: "1.0.1".into(),
                classification: "improvement".into(),
                continue_from: "start".into(),
            },
        )
        .unwrap();
        let events = read_all(&run_dir).unwrap()[before..]
            .iter()
            .map(|e| match &e.payload {
                EventPayload::PatchRejected { reason } => format!("patch_rejected {reason}"),
                p => serde_json::to_value(p).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            })
            .collect::<Vec<_>>();
        events.first().cloned().unwrap_or_else(|| "applied".into())
    }

    /// A patch that adds a read of the namespace to a run that recorded
    /// none is refused.
    #[test]
    fn a_patch_adding_the_namespace_to_a_run_that_recorded_none_is_refused() {
        let line = patch_prev(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: x }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.x}}\" }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            &[],
        );
        assert!(
            line.starts_with("patch_rejected previous_success_unrecorded: the run recorded no run.previous_success at start"),
            "{line}"
        );
    }

    /// A patch that adds a read of an output key the start did not read
    /// is refused, naming the key.
    #[test]
    fn a_patch_adding_an_unsnapshotted_output_key_is_refused() {
        let line = patch_prev(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.x}}\" }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: b, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.x}} {{run.previous_success.nodes.b.output.y}}\" }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: review }\n",
            &["nodes.a.output.x"],
        );
        assert!(
            line.starts_with("patch_rejected previous_success_unrecorded: run.previous_success.nodes.b.output.y was not read at start"),
            "{line}"
       );
    }

    /// A read the start snapshotted, removed by an earlier patch, may come
    /// back: the running definition no longer reads it, the record does.
    #[test]
    fn a_patch_restoring_a_snapshotted_read_applies() {
        let line = patch_prev(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: x }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.x}}\" }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            &["nodes.a.output.x"],
        );
        assert!(!line.starts_with("patch_rejected"), "{line}");
    }

    /// A patch that keeps the start's reads applies.
    #[test]
    fn a_patch_keeping_the_starts_reads_applies() {
        let line = patch_prev(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.x}}\" }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh }\n  - { id: review, type: agent_task, prompt: \"{{run.previous_success.nodes.a.output.x}}\" }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: review }\n",
            &["nodes.a.output.x"],
        );
        // The patched definition reads the namespace, so the guard is
        // written before the snapshot replaces the running one.
        assert_eq!(line, "run_guard");
    }

    /// Every way an approval weakens, judged on the definitions alone (an
    /// in-run and a forward patch share it).
    #[test]
    fn every_weakening_of_an_approval_is_named() {
        let base = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: a, type: script, script: s, runner: sh, results: [{ path: a.md }] }\n  - { id: b, type: script, script: s, runner: sh, results: [{ path: b.md }] }\n  - { id: gate, type: human_review, options: [approve, ok, reject], approves: { results: [a, b] } }\n  - { id: ship, type: script, script: s, runner: sh, requires_approval: { gate: gate, of: [a] } }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: a }\n  - { from: a, to: b }\n  - { from: b, to: gate }\n  - { from: gate, to: ship }\n  - { from: ship, to: done }\n";
        let running = Playbook::from_yaml(base).unwrap();
        for (from, to, want) in [
            (
                "approves: { results: [a, b] }",
                "approves: { results: [a] }",
                "node `gate` binds fewer results in `approves`",
            ),
            (
                "approves: { results: [a, b] }",
                "approves: { results: [a, b], options: [approve, ok] }",
                "node `gate` counts more options as approval",
            ),
            (
                ", approves: { results: [a, b] }",
                "",
                "node `gate` drops `approves`",
            ),
            (
                ", requires_approval: { gate: gate, of: [a] }",
                "",
                "node `ship` drops `requires_approval`",
            ),
            (
                "requires_approval: { gate: gate, of: [a] }",
                "requires_approval: { gate: gate, of: [a], live: ignore }",
                "node `ship` stops checking the live files (`live: ignore`)",
            ),
            (
                "requires_approval: { gate: gate, of: [a] }",
                "requires_approval: { gate: gate, of: [b] }",
                "node `ship` requires the approval of fewer results",
            ),
        ] {
            assert!(base.contains(from), "{from}");
            let patched = Playbook::from_yaml(&base.replace(from, to)).unwrap();
            assert_eq!(
                super::super::approval::patch_weakening(&running, &patched).as_deref(),
                Some(want)
            );
        }
        // Stricter or equal is not weaker: all results required, the
        // default approving option named.
        for (from, to) in [
            (
                "requires_approval: { gate: gate, of: [a] }",
                "requires_approval: { gate: gate }",
            ),
            (
                "approves: { results: [a, b] }",
                "approves: { results: [a, b, a], options: [approve] }",
            ),
        ] {
            let patched = Playbook::from_yaml(&base.replace(from, to)).unwrap();
            assert_eq!(
                super::super::approval::patch_weakening(&running, &patched),
                None,
                "{to}"
            );
        }
    }

    /// A gate whose options route somewhere else after the patch: a
    /// person's `reject` would lead to what `approve` led to.
    #[test]
    fn a_patch_that_reroutes_a_person_only_gate_is_refused() {
        let running = person_only()
            .replace(
                "  - { id: done, type: finish, outcome: success }\n",
                "  - { id: done, type: finish, outcome: success }\n  - { id: stop, type: finish, outcome: failure }\n",
            )
            .replace(
                "  - { from: gate, to: done }\n",
                "  - { from: gate, to: done, condition: { type: review_status, equals: approve } }\n  - { from: gate, to: stop, condition: { type: review_status, equals: reject } }\n",
            );
        let swapped = running
            .replace("equals: approve } }", "equals: tmp } }")
            .replace("equals: reject } }", "equals: approve } }")
            .replace("equals: tmp } }", "equals: reject } }");
        assert_ne!(running, swapped);
        let line = refused(&running, &swapped, "gate");
        assert!(
            line.starts_with(
                "patch_rejected approval_weakened: node `gate` changes where its options route"
            ),
            "{line}"
        );
        // An added route on `reject` toward the protected node too.
        let added = running.replace(
            "  - { from: start, to: gate }\n",
            "  - { from: start, to: gate }\n  - { from: gate, to: done, condition: { type: review_status, equals: reject } }\n",
        );
        assert!(refused(&running, &added, "gate").contains("changes where its options route"));
    }

    /// `start -> w -> g (person-only) -> ship -> check -> done`, `reject`
    /// routes to `fix`, which leads back into `g`, and so does `check`.
    const LOOPED: &str = "schema: 2\nid: g\nname: g\nversion: 1.0.0\nnodes:\n  - { id: start, type: start }\n  - { id: w, type: script, script: s, runner: sh }\n  - { id: g, type: human_review, options: [approve, reject], person_only: true }\n  - { id: fix, type: script, script: s, runner: sh }\n  - { id: ship, type: script, script: s, runner: sh }\n  - { id: check, type: script, script: s, runner: sh }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: w }\n  - { from: w, to: g }\n  - { from: g, to: ship, condition: { type: review_status, equals: approve } }\n  - { from: g, to: fix, condition: { type: review_status, equals: reject } }\n  - { from: fix, to: g, max_traversals: 3 }\n  - { from: ship, to: check }\n  - { from: check, to: fix, max_traversals: 3 }\n  - { from: check, to: done }\n";

    /// A journal from `lines`, one `"type":...` body per event, numbered
    /// from 1.
    fn journal(lines: &[&str]) -> Vec<crate::event::Event> {
        lines
            .iter()
            .enumerate()
            .map(|(i, body)| {
                serde_json::from_str(&format!("{{\"seq\":{},\"ts\":0,{body}}}", i + 1)).unwrap()
            })
            .collect()
    }

    const REQ: &str = r#""type":"review_requested","node":"g","options":["approve","reject"]"#;

    fn decided(option: &str) -> String {
        format!(r#""type":"review_decided","node":"g","decision":"{option}","note":"""#)
    }

    fn started(node: &str) -> String {
        format!(r#""type":"node_started","node":"{node}","attempt":1"#)
    }

    /// The one check every move shares: a gate counts as passed toward the
    /// target only by a passing decision of its current visit.
    #[test]
    fn only_a_passing_decision_of_this_visit_lets_a_move_past_a_gate() {
        use super::super::approval::skips_gate;
        let pb = Playbook::from_yaml(LOOPED).unwrap();
        let (approve, reject) = (decided("approve"), decided("reject"));
        let (ship, check, fix) = (started("ship"), started("check"), started("fix"));
        // Undecided, and rejected: the run is at `fix`.
        assert!(skips_gate(&pb, &journal(&[REQ]), "ship").is_some());
        let rejected = journal(&[REQ, &reject, &fix]);
        let why = skips_gate(&pb, &rejected, "ship").unwrap();
        assert!(why.starts_with("`ship` skips gate `g`"), "{why}");
        // Approved in this visit: on to `ship` and past it.
        let approved = journal(&[REQ, &approve, &ship]);
        assert_eq!(skips_gate(&pb, &approved, "ship"), None);
        assert_eq!(skips_gate(&pb, &approved, "check"), None);
        // Approved in the earlier loop: the run left for `fix`, back toward
        // the gate, before it asked again.
        let earlier = journal(&[REQ, &approve, &ship, &check, &fix]);
        assert!(skips_gate(&pb, &earlier, "ship").is_some());
        assert!(skips_gate(&pb, &earlier, "check").is_some());
        // A request since the decision is a new visit, open or withdrawn.
        assert!(skips_gate(&pb, &journal(&[REQ, &approve, &ship, REQ]), "ship").is_some());
        // Nodes before the gate, and the gate itself, are never skipped.
        assert_eq!(skips_gate(&pb, &rejected, "w"), None);
        assert_eq!(skips_gate(&pb, &rejected, "g"), None);
    }

    /// For a gate that approves results the decision must be an approving
    /// option on the binding of its current request.
    #[test]
    fn an_approving_gate_passes_only_on_its_current_binding() {
        use super::super::approval::skips_gate;
        let pb =
            Playbook::from_yaml(&LOOPED.replace("person_only: true", "approves: { results: [w] }"))
                .unwrap();
        let req = |nonce: &str| {
            format!(
                r#""type":"review_requested","node":"g","options":["approve","reject"],"binding":{{"nonce":"{nonce}","snapshots":[]}}"#
            )
        };
        let bound = |option: &str, nonce: &str| {
            format!(
                r#""type":"review_decided","node":"g","decision":"{option}","note":"","binding":"{nonce}""#
            )
        };
        let ship = started("ship");
        let ok = journal(&[&req("n1"), &bound("approve", "n1"), &ship]);
        assert_eq!(skips_gate(&pb, &ok, "ship"), None);
        for events in [
            journal(&[&req("n1"), &bound("approve", "n0"), &ship]),
            journal(&[&req("n1"), &decided("approve"), &ship]),
            journal(&[&req("n1"), &bound("reject", "n1"), &ship]),
        ] {
            assert!(skips_gate(&pb, &events, "ship").is_some());
        }
    }
}
