//! Run preparation: resolve profiles, build the immutable run manifest, stage the run dir.
//! Split out of `scheduler` for navigability; shares the parent module's imports via `use super::*`.

use super::*;

/// Where the engine looks for the definition and where it writes the execution
/// (spec 3). Previously this was a single `root`; the split allows running a
/// global playbook (definition in the config dir) at the project root.
pub(crate) struct PrepareTarget {
    /// The directory with `playbooks/` (the definition): the project's `.apb` or the
    /// global config dir.
    pub(crate) definition_parent: PathBuf,
    /// The project root where `.apb/runs` and the workdir lock live, and where the
    /// agent writes.
    pub(crate) execution_root: PathBuf,
    /// The origin label for provenance: `"project"` / `"global"`.
    pub(crate) origin_label: &'static str,
}

pub(crate) fn prepare_run(
    root: &Path,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<Prepared, EngineError> {
    // The regular project path: definition and execution share one root.
    let t = PrepareTarget {
        definition_parent: root.join(".apb"),
        execution_root: root.to_path_buf(),
        origin_label: "project",
    };
    prepare_run_target(&t, id, version, opts)
}

pub(crate) fn soul_delivery_str(d: SoulDelivery) -> String {
    match d {
        SoulDelivery::Native => "native".to_string(),
        SoulDelivery::Prefix => "prefix".to_string(),
    }
}

/// Resolves node (and supervisor) profiles into the run snapshot: copies
/// profile.yaml, SOUL.md, and skill contents into `runs/<id>/profiles/<scope>/<name>/`,
/// builds the invocation chain and bundle_digest, and returns the immutable
/// manifest (spec 3.4-3.6). An empty manifest (no profiles) is not written - the executor path.
// An internal builder with a cohesive set of parameters (resolution context +
// anti-TOCTOU expected + mode flags); splitting it into a struct wrapper is not worthwhile.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_run_manifest(
    playbook: &Playbook,
    root: &Path,
    origin: PlaybookOrigin,
    global: &GlobalConfig,
    run_dir: &Path,
    expected_bundles: Option<&BTreeMap<String, String>>,
    overrides: Option<&apb_core::overrides::RunOverrides>,
    supervised: bool,
) -> Result<RunExecutionManifest, EngineError> {
    let mut bindings: Vec<(String, apb_core::profile::QualifiedProfileRef)> = Vec::new();
    for n in &playbook.nodes {
        if let NodeKind::AgentTask { profile, .. } = &n.kind
            && let Some(pref) = profile
                .clone()
                .or_else(|| playbook.defaults.profile.clone())
        {
            bindings.push((n.id.clone(), pref));
        }
    }
    // The supervisor binding is created ONLY when the run is actually supervised
    // by an external agent (`supervised` = supervisor_expected). In that case the
    // executor is supervisor.profile OR defaults.profile, EVEN without a `supervisor:`
    // section (so `--supervise` with just defaults.profile brings up an agent). For
    // an autonomous (and self-supervised) run there is no binding: otherwise the run
    // would be forced to resolve/trust a profile that is never spawned (review P2).
    // The gate (collect_profile_refs) uses the same `supervised` flag, so the
    // permit's key set matches the snapshot (anti-TOCTOU).
    if supervised
        && let Some(pref) = playbook
            .supervisor
            .as_ref()
            .and_then(|s| s.profile.clone())
            .or_else(|| playbook.defaults.profile.clone())
    {
        bindings.push(("supervisor".to_string(), pref));
    }

    let mut manifest = RunExecutionManifest::default();
    if bindings.is_empty() {
        return Ok(manifest);
    }
    let limits = apb_core::content::TreeLimits::default();

    for (node_id, pref) in bindings {
        let loaded = profile_store::resolve_profile(root, origin, &pref)
            .map_err(|e| EngineError::Invalid(format!("profile `{}`: {e}", pref.name)))?;
        // A run-local ephemeral executor (completion-plan Task 4): we take SOUL/skills
        // from the node's profile and replace the chain with a single invocation. A
        // per-node entry (scope `ephemeral`, name = node id), not deduplicated and not
        // covered by bundle trust. Supervisor overrides do not apply here (nodes are agent nodes).
        let eph = overrides
            .and_then(|o| o.nodes.get(&node_id))
            .and_then(|n| n.ephemeral_executor.clone());
        let (scope, name) = match &eph {
            Some(_) => ("ephemeral".to_string(), node_id.clone()),
            None => (
                profile_store::scope_str(loaded.scope).to_string(),
                loaded.name.clone(),
            ),
        };
        let key = format!("{scope}/{name}");
        manifest.node_bindings.insert(node_id, key.clone());
        if manifest.profiles.iter().any(|p| p.key() == key) {
            continue; // the profile is already recorded (ephemeral keys are unique per-node)
        }

        // Snapshot of the profile definition (under the record key: for an ephemeral one - `ephemeral/<node>`).
        let dest = run_dir.join("profiles").join(&scope).join(&name);
        std::fs::create_dir_all(&dest)?;
        apb_core::fsutil::atomic_write(&dest.join("profile.yaml"), loaded.profile_yaml.as_bytes())?;
        apb_core::fsutil::atomic_write(&dest.join("SOUL.md"), loaded.soul.as_bytes())?;

        let mut mskills = Vec::new();
        let mut skill_pairs = Vec::new();
        for skill in &loaded.doc.skills {
            let resolved = apb_core::skills::resolve_skill(root, loaded.scope, skill)
                .map_err(|e| EngineError::Invalid(format!("skill `{}`: {e}", skill.name)))?;
            let sscope = profile_store::scope_str(resolved.scope).to_string();
            // The snapshot path includes scope: same-named project/global skills in
            // one profile would otherwise collide in `skills/<name>` (snapshot_tree
            // would reject the already-existing dest).
            let sdest = dest.join("skills").join(&sscope).join(&resolved.name);
            let sdigest =
                apb_core::content::snapshot_tree(&resolved.canonical_path, &sdest, &limits)
                    .map_err(|e| {
                        EngineError::Invalid(format!("skill snapshot `{}`: {e}", resolved.name))
                    })?;
            skill_pairs.push((format!("{sscope}/{}", resolved.name), sdigest.clone()));
            mskills.push(ManifestSkill {
                name: resolved.name,
                scope: sscope,
                digest: sdigest,
            });
        }
        let bundle = apb_core::content::bundle_digest(&loaded.profile_digest, &skill_pairs);

        // Anti-TOCTOU: the bundle recomputed from the snapshot must EXACTLY match
        // what the bundle gate checked. A missing key in expected means the
        // profile did not go through the trust check (fail closed); a different digest
        // means the content changed between the gate and the snapshot (spec 5.1).
        // An ephemeral executor is ad-hoc and not covered by bundle trust - we do not
        // apply the expected check to it (there is no `ephemeral/<node>` key in expected).
        if eph.is_none()
            && let Some(expected) = expected_bundles
        {
            match expected.get(&key) {
                Some(exp) if exp == &bundle => {}
                Some(_) => {
                    return Err(EngineError::Invalid(format!(
                        "profile `{key}` changed since the trust check (bundle mismatch)"
                    )));
                }
                None => {
                    return Err(EngineError::Invalid(format!(
                        "profile `{key}` was not covered by the trust check"
                    )));
                }
            }
        }

        let mut chain = Vec::new();
        if let Some(e) = &eph {
            let program = crate::invocation::program_for(&e.agent, global);
            chain.push(crate::invocation::resolve_invocation(
                &e.agent, &e.model, &program, global,
            )?);
        } else {
            let ex = &loaded.doc.executor;
            let program = crate::invocation::program_for(&ex.agent, global);
            chain.push(crate::invocation::resolve_invocation(
                &ex.agent, &ex.model, &program, global,
            )?);
            for f in &ex.fallbacks {
                let p = crate::invocation::program_for(&f.agent, global);
                chain.push(crate::invocation::resolve_invocation(
                    &f.agent, &f.model, &p, global,
                )?);
            }
        }
        let soul_empty = loaded.soul.trim().is_empty();
        let chain = crate::invocation::filter_chain(chain, loaded.doc.soul, soul_empty)?;

        manifest.profiles.push(ManifestProfile {
            scope,
            name,
            profile_digest: loaded.profile_digest.clone(),
            bundle_digest: bundle,
            soul: loaded.soul.clone(),
            soul_requirement: loaded.doc.soul,
            skills: mskills,
            chain,
            ephemeral: eph.is_some(),
        });
    }

    // Exact set equality: expected must not have any leftover keys that are
    // not among the snapshotted profiles (otherwise the gate checked a profile that
    // the run does not use - a set mismatch, fail closed). Ephemeral entries are
    // not bundle-trusted - they do not take part in the set comparison.
    //
    // INVARIANT (do not break when adding surfaces): `overrides`
    // (ephemeral executor) and `expected_bundles` (the trust gate) are NEVER combined
    // in one run. The `policy::collect_profile_refs` gate does not know about ephemeral
    // and would return the node's original profile key; if that key ended up in
    // expected while ephemeral excluded it from resolved, we would get a false key set
    // mismatch. Currently all surfaces (apb-mcp tools, apb-server, apb-cli) set
    // overrides and expected_profile_bundles mutually exclusively, so this is unreachable.
    if let Some(expected) = expected_bundles {
        let resolved_keys: std::collections::BTreeSet<String> = manifest
            .profiles
            .iter()
            .filter(|p| !p.ephemeral)
            .map(|p| p.key())
            .collect();
        let expected_keys: std::collections::BTreeSet<String> = expected.keys().cloned().collect();
        if resolved_keys != expected_keys {
            return Err(EngineError::Invalid(
                "profile set changed since the trust check (key set mismatch)".into(),
            ));
        }
    }
    Ok(manifest)
}

pub(crate) fn prepare_run_target(
    t: &PrepareTarget,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<Prepared, EngineError> {
    let root = t.execution_root.as_path();
    let reg = Registry::open_dir(&t.definition_parent)?;
    let loaded = reg.load(id, version)?;
    let digest = apb_core::scope::digest_str(&loaded.yaml);
    // Anti-TOCTOU: if the caller checked trust against a specific digest, it
    // must match the actually loaded content - otherwise the file was swapped
    // between the check and the run (spec 9).
    if let Some(expected) = opts.expected_digest.as_deref()
        && expected != digest
    {
        return Err(EngineError::Invalid(format!(
            "playbook `{id}` changed since it was checked (digest mismatch)"
        )));
    }
    let mut playbook = loaded.playbook.clone();

    // The global config is needed to resolve profile invocations (agents, program).
    // A broken config is a start-up error, not a silent default.
    let global = GlobalConfig::load().map_err(EngineError::Invalid)?;

    // Run-level overrides (spec 11): produce an "effective playbook" = version +
    // overrides. All the code afterward works only with it. Empty overrides
    // change nothing.
    let has_overrides = opts.overrides.as_ref().is_some_and(|o| !o.is_empty());
    if let Some(ov) = opts.overrides.as_ref() {
        ov.apply(&mut playbook).map_err(EngineError::Invalid)?;
    }

    // Gate: do not run an invalid playbook. We derive origin HERE (not just
    // for the manifest below) - otherwise the validator would work with the default
    // Project and would not catch a scope:project reference in a global playbook (V14).
    let origin = if t.origin_label == "global" {
        PlaybookOrigin::Global
    } else {
        PlaybookOrigin::Project
    };
    let ctx = ValidationContext {
        profiles: reg.profiles(),
        playbook_origin: origin,
    };
    let report = validate(&playbook, &ctx);
    if report.issues.iter().any(|i| i.severity == Severity::Error) {
        return Err(EngineError::Invalid(format!("playbook `{id}` is invalid")));
    }

    let start_node = playbook
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Start))
        .ok_or_else(|| EngineError::Invalid("no start node".into()))?
        .id
        .clone();

    let is_write = playbook
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }));
    let guard = if is_write {
        acquire(root, opts.allow_shared_workdir)?
    } else {
        None
    };

    let run_id = format!("{id}-{}", now_millis());
    let run_dir = root.join(".apb/runs").join(&run_id);
    let mut log = EventLog::create(&run_dir)?;
    // The run snapshot = the effective playbook. Without overrides we write the raw yaml
    // (preserving formatting/comments); with overrides we serialize the
    // effective playbook, so the snapshot honestly reflects what actually ran.
    let snapshot_yaml = if has_overrides {
        serde_yaml_ng::to_string(&playbook).map_err(|e| EngineError::Yaml(e.to_string()))?
    } else {
        loaded.yaml.clone()
    };
    snapshot_playbook(&run_dir, &snapshot_yaml)?;
    // Scripts live in the definition's version directory (<def_parent>/playbooks/<id>/<version>/scripts),
    // not in the run snapshot - we copy them into run_dir, otherwise script nodes would not find
    // their files. The source is definition_parent (for a global playbook this is the
    // config dir, not the project root).
    let version_dir = t
        .definition_parent
        .join("playbooks")
        .join(id)
        .join(&loaded.version);
    copy_scripts(&version_dir, &run_dir)?;
    // The run's webhook hook secrets (for wait nodes, spec 6.7).
    crate::hooks::generate_hooks(&run_dir, &playbook)?;
    let cfg = RunConfig {
        params: opts.params.clone(),
        instruction: opts.instruction.clone(),
        supervisor_expected: opts.supervisor_expected,
        max_patches_per_run: opts.max_patches_per_run,
        context_max_bytes: opts.context_max_bytes,
        context_compact_model: opts.context_compact_model.clone(),
        overrides: opts.overrides.clone(),
    };
    write_run_config(&run_dir, &cfg)?;

    // Resolve profiles into the run snapshot + the immutable manifest (spec 3.6).
    // The executor path (no profiles) does not write a manifest. `origin` was already
    // derived above (used both by the validator and here).
    let manifest = build_run_manifest(
        &playbook,
        root,
        origin,
        &global,
        &run_dir,
        opts.expected_profile_bundles.as_ref(),
        opts.overrides.as_ref(),
        opts.supervisor_expected,
    )?;
    let profiles_prov: Vec<ProfileProvenance> = manifest
        .profiles
        .iter()
        .map(|p| ProfileProvenance {
            scope: p.scope.clone(),
            name: p.name.clone(),
            bundle_digest: p.bundle_digest.clone(),
        })
        .collect();
    if !manifest.is_empty() {
        crate::manifest::write(&run_dir, &manifest)?;
    }

    log.append(EventPayload::RunStarted {
        playbook: id.into(),
        version: loaded.version.clone(),
    })?;
    // Run provenance (spec 3): the definition scope, content fingerprint,
    // execution root, and profiles used. A separate event right after
    // RunStarted.
    log.append(EventPayload::RunProvenance {
        origin: Some(t.origin_label.into()),
        digest: Some(digest),
        execution_root: Some(t.execution_root.to_string_lossy().into_owned()),
        profiles: profiles_prov,
    })?;

    Ok(Prepared {
        playbook,
        run_id,
        run_dir,
        log,
        cfg,
        guard,
        start_node,
        mode: opts.mode,
        supervisor_expected: opts.supervisor_expected,
    })
}
