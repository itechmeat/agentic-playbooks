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

/// Renders a param `default` (a free-form YAML scalar) into the plain-string
/// representation the params map (`BTreeMap<String, String>`) uses everywhere:
/// a string yields its bare content, a bool/number its natural text, and any
/// non-scalar its trimmed YAML serialization. This matches how callers already
/// hand params in as strings, so a defaulted param and an explicitly passed one
/// render identically.
/// What this project remembers for playbook `id` of origin `origin_label`,
/// checked against `params`, with the definition's legacy draft as the
/// instruction fallback. The draft is read only when the start passes no
/// instruction. A sub-playbook child (`opts.parent_run`) takes nothing from
/// memory or the draft: the parent's consent and pins cover the declared
/// defaults and the instruction the parent passes, not what someone last
/// typed for the child.
fn recall_input(
    root: &Path,
    origin_label: &str,
    id: &str,
    reg: &Registry,
    params: &[apb_core::schema::Param],
    opts: &RunOptions,
) -> Result<apb_core::run_memory::Recall, EngineError> {
    use apb_core::run_memory::{MemoryError, MemoryKey, recall};
    let to_engine = |e: MemoryError| match e {
        MemoryError::Io(e) => EngineError::Io(e),
        other => EngineError::Invalid(other.to_string()),
    };
    let key = MemoryKey::for_label(origin_label, id).map_err(to_engine)?;
    let child = opts.parent_run.is_some();
    let legacy = match (&opts.instruction, child) {
        (None, false) => reg.read_instruction_draft(id)?,
        _ => None,
    };
    let mut recalled = recall(root, &key, params, legacy).map_err(to_engine)?;
    if child {
        recalled.values.clear();
        recalled.instruction = None;
        recalled.instruction_source = apb_core::run_memory::InstructionSource::None;
    }
    Ok(recalled)
}

/// The run's task type: `quality.task_type` over the start's params (V104
/// keeps it to `params.*`), [`DEFAULT_TASK_TYPE`] when absent or empty.
///
/// [`DEFAULT_TASK_TYPE`]: apb_core::quality::criteria::DEFAULT_TASK_TYPE
fn task_type_at_start(
    quality: &apb_core::quality::QualityBlock,
    root: &Path,
    params: &BTreeMap<String, String>,
) -> String {
    let rendered = quality.task_type.as_deref().map(|t| {
        render(
            t,
            params,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "",
            &BTreeMap::new(),
            &BTreeMap::new(),
            &crate::context::OutputClip {
                run_dir: root,
                max_bytes: 0,
                retro: None,
            },
        )
    });
    rendered
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| apb_core::quality::criteria::DEFAULT_TASK_TYPE.to_string())
}

/// Best-effort: a prepare failure that happens after the run directory (and
/// its event log) already exist would otherwise leave that directory with no
/// explanation - an empty or `RunStarted`-only journal that never reaches
/// `run_finished` at all (issue #42 finding 3; this is the shape a gated
/// sub-playbook child left behind on a missing connector permit before this
/// fix, and `list_runs` silently skips a dir with an empty journal, so the
/// failure was invisible even to `apb runs`). Notes the verbatim reason and
/// closes the run out as failed before the caller ever sees the error, which
/// is returned completely unchanged - this never shadows or replaces it.
fn finalize_prepare_failure(log: &mut EventLog, e: EngineError) -> EngineError {
    let _ = log.append(EventPayload::RunError {
        node: None,
        reason: e.to_string(),
    });
    let _ = log.append(EventPayload::RunFinished {
        outcome: "failed".into(),
    });
    e
}

/// `?`-friendly wrapper around [`finalize_prepare_failure`] for the fallible
/// steps of `prepare_run_target` that run after the run directory's event log
/// is created: `r.map_err(..)` then `?` would also work inline, but this
/// keeps every call site a one-liner.
fn prep_try<T, E: Into<EngineError>>(
    log: &mut EventLog,
    r: Result<T, E>,
) -> Result<T, EngineError> {
    r.map_err(|e| finalize_prepare_failure(log, e.into()))
}

/// What a run that fails before its `run_started` event still records, so
/// it is not an orphan: the journal names the playbook and version, and the
/// directory carries this installation's origin stamp. Without them `apb
/// stats` and `apb decisions report` skip the run as one that did not come
/// from this machine, and a run that never got past its start disappears
/// from the failure counts.
struct Unstarted<'a> {
    run_dir: &'a Path,
    run_id: &'a str,
    playbook: &'a str,
    version: &'a str,
    /// The start's effective external data flag: a failed preparation
    /// records it on the `run_started` it writes, so the run stays flagged.
    external_data: bool,
}

/// [`prep_try`] for the steps before `run_started`: on a failure it first
/// appends `run_started` and stamps the directory (both best-effort; the
/// stamp covers whatever manifest was written), then finalizes as usual.
fn prep_try_unstarted<T, E: Into<EngineError>>(
    log: &mut EventLog,
    u: &Unstarted<'_>,
    r: Result<T, E>,
) -> Result<T, EngineError> {
    r.map_err(|e| {
        let _ = log.append(EventPayload::RunStarted {
            playbook: u.playbook.into(),
            version: u.version.into(),
            definition: None,
            external_data: u.external_data,
        });
        let _ = apb_core::run_origin::stamp(u.run_dir, u.run_id);
        finalize_prepare_failure(log, e.into())
    })
}

pub(crate) fn soul_delivery_str(d: SoulDelivery) -> String {
    match d {
        SoulDelivery::Native => "native".to_string(),
        SoulDelivery::Prefix => "prefix".to_string(),
    }
}

/// Anti-TOCTOU expectation for a single profile's snapshotted bundle digest.
/// `None` skips the check (an ephemeral executor, or a path with no trust gate);
/// `Verify(Some(d))` requires the recomputed digest to equal `d`; `Verify(None)`
/// means a gate ran but never covered this key, which fails closed.
pub(crate) enum ExpectedBundle<'a> {
    None,
    Verify(Option<&'a str>),
}

/// Resolves ONE already-loaded profile into a snapshotted [`ManifestProfile`]:
/// copies profile.yaml/SOUL/skills into `runs/<id>/profiles/<scope>/<name>`,
/// recomputes the bundle digest from that snapshot, checks it against `expected`
/// (anti-TOCTOU), and builds the executor chain. Shared by run-start manifest
/// building and mid-run profile rebinding (issue #45 finding 5) so both verify
/// the new bundle by the exact same code path. `scope`/`name`/`key` are the
/// record identity (`ephemeral/<node>` for an ephemeral executor). `eph`, when
/// set, replaces the chain with a single ad-hoc invocation while SOUL/skills
/// still come from `loaded`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn snapshot_loaded_profile(
    root: &Path,
    global: &GlobalConfig,
    run_dir: &Path,
    scope: &str,
    name: &str,
    key: &str,
    loaded: &profile_store::LoadedProfile,
    eph: Option<&apb_core::overrides::EphemeralExecutor>,
    expected: ExpectedBundle<'_>,
) -> Result<ManifestProfile, EngineError> {
    let limits = apb_core::content::TreeLimits::default();

    // Snapshot of the profile definition (under the record key: for an ephemeral one - `ephemeral/<node>`).
    let dest = run_dir.join("profiles").join(scope).join(name);
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
        let sdigest = apb_core::content::snapshot_tree(&resolved.canonical_path, &sdest, &limits)
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
    // what the trust gate checked. A missing key in expected means the profile
    // did not go through the trust check (fail closed); a different digest means
    // the content changed between the gate and the snapshot (spec 5.1).
    match expected {
        ExpectedBundle::None => {}
        ExpectedBundle::Verify(Some(exp)) if exp == bundle => {}
        ExpectedBundle::Verify(Some(_)) => {
            return Err(EngineError::Invalid(format!(
                "profile `{key}` changed since the trust check (bundle mismatch)"
            )));
        }
        ExpectedBundle::Verify(None) => {
            return Err(EngineError::Invalid(format!(
                "profile `{key}` was not covered by the trust check"
            )));
        }
    }

    let (chain, tiers) = crate::manifest::resolve_executor(loaded, eph, global)?;

    Ok(ManifestProfile {
        scope: scope.to_string(),
        name: name.to_string(),
        profile_digest: loaded.profile_digest.clone(),
        bundle_digest: bundle,
        soul: loaded.soul.clone(),
        soul_requirement: loaded.doc.soul,
        skills: mskills,
        chain,
        ephemeral: eph.is_some(),
        // Snapshot the profile's environment so post-start reads use the
        // run's value, not the live profile (issue #136 item 4).
        hermetic: loaded.doc.environment() == apb_core::profile::AgentEnvironment::Minimal,
        zcode_mode: loaded.doc.zcode_mode,
        // Snapshotted so a resume runs under the policy the run started
        // with, whatever the live profile says by then.
        process_env: apb_core::process_env::ProcessEnvPolicy::stored(
            loaded.doc.process_env.clone(),
        ),
        tiers,
        routed_tier: None,
        cascade: 0,
    })
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
    expected_connectors: &BTreeMap<String, String>,
    expected_accounts: &BTreeMap<String, String>,
    overrides: Option<&apb_core::overrides::RunOverrides>,
    supervised: bool,
) -> Result<RunExecutionManifest, EngineError> {
    let mut bindings: Vec<(String, apb_core::profile::QualifiedProfileRef)> = Vec::new();
    for n in &playbook.nodes {
        if let Some(pref) = n.kind.effective_profile_ref(&playbook.defaults) {
            bindings.push((n.id.clone(), pref));
        }
    }
    // The supervisor binding is created ONLY when the run is actually supervised
    // by an external agent (`supervised` = the mode is agent-supervised). In that case the
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

    // Connectors (spec 6): snapshot the bound connectors + per-node grants
    // into the manifest, verifying the permit maps verbatim. Independent of
    // whether the playbook has any profile bindings, so it runs BEFORE the
    // profile early-return (a connector-only playbook still gets a manifest).
    // When the playbook binds connectors the permit must be present: an empty
    // `expected_connectors` means the run did not go through the gate, which
    // is refused (mirrors the profile-bundle invariant - the CLI/MCP callers
    // must always pass the gate result).
    if playbook.binds_connectors() {
        if expected_connectors.is_empty() {
            return Err(EngineError::Invalid(
                "connector bindings present but no connector permit".into(),
            ));
        }
        let (connectors, grants) = crate::connector::run::snapshot_connectors(
            root,
            run_dir,
            playbook,
            expected_connectors,
            expected_accounts,
        )?;
        manifest.connectors = connectors;
        manifest.connector_grants = grants;
    }

    if bindings.is_empty() {
        return Ok(manifest);
    }

    for (node_id, pref) in &bindings {
        let loaded = profile_store::resolve_profile(root, origin, pref)
            .map_err(|e| EngineError::Invalid(format!("profile `{}`: {e}", pref.name)))?;
        // A run-local ephemeral executor (completion-plan Task 4): we take SOUL/skills
        // from the node's profile and replace the chain with a single invocation. A
        // per-node entry (scope `ephemeral`, name = node id), not deduplicated and not
        // covered by bundle trust. Supervisor overrides do not apply here (nodes are agent nodes).
        let eph = overrides
            .and_then(|o| o.nodes.get(node_id))
            .and_then(|n| n.ephemeral_executor.clone());
        let (scope, name) = match &eph {
            Some(_) => ("ephemeral".to_string(), node_id.clone()),
            None => (
                profile_store::scope_str(loaded.scope).to_string(),
                loaded.name.clone(),
            ),
        };
        let key = format!("{scope}/{name}");
        manifest.node_bindings.insert(node_id.clone(), key.clone());
        if manifest.profiles.iter().any(|p| p.key() == key) {
            continue; // the profile is already recorded (ephemeral keys are unique per-node)
        }

        // An ephemeral executor is ad-hoc and not covered by bundle trust, so it
        // skips the expected-bundle check (there is no `ephemeral/<node>` key in
        // expected). Otherwise the snapshot's recomputed bundle must match the
        // gate's verified digest exactly.
        let expected = if eph.is_some() {
            ExpectedBundle::None
        } else {
            match expected_bundles {
                None => ExpectedBundle::None,
                Some(map) => ExpectedBundle::Verify(map.get(&key).map(String::as_str)),
            }
        };
        let profile = snapshot_loaded_profile(
            root,
            global,
            run_dir,
            &scope,
            &name,
            &key,
            &loaded,
            eph.as_ref(),
            expected,
        )?;
        manifest.profiles.push(profile);
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
    // `route: auto` (issue #165 Part 12) needs a profile with tiers; an
    // ephemeral executor replaces the chain and is never routed.
    for n in &playbook.nodes {
        if n.route == Some(apb_core::schema::RouteSetting::Auto)
            && let Some(p) = manifest.for_node(&n.id)
            && !p.ephemeral
            && p.tiers.is_empty()
        {
            return Err(EngineError::Invalid(format!(
                "node `{}` has `route: auto`, but its profile `{}` declares no tiers",
                n.id,
                p.key()
            )));
        }
    }
    Ok(manifest)
}

/// Unattended mode at a start, before anything is written: a child of an
/// unattended run is unattended too (its parent's manifest says so,
/// whatever the caller passed), a start the mode cannot hold is refused
/// (`unattended_unsupported`, the gate's own check, so no start path skips
/// it), and the execution resolves without a host session, so the CLI to
/// host fallback is off.
/// The external data flag at a start: a child or a `continued_from`
/// successor of a flagged run is flagged, whatever the caller passed (the
/// surfaces refuse an explicit `false` before this, see
/// [`crate::external_data::for_start`]; this is the backstop).
fn external_data_start(place: &Placement, opts: &mut RunOptions) {
    let flagged = |id: &str| {
        apb_core::registry::is_safe_segment(id)
            && crate::external_data::is_flagged(&place.runs.join(id))
    };
    if opts.parent_run.as_deref().is_some_and(flagged)
        || opts.continued_from.as_deref().is_some_and(flagged)
        || opts
            .launch
            .as_ref()
            .and_then(|g| g.record.started_by.as_ref())
            .is_some_and(|by| starter_flagged(&place.runs, by))
    {
        opts.external_data = true;
    }
}

/// Whether the run a step of which started this one is flagged: under this
/// project's runs, or under the registered root of its workspace.
fn starter_flagged(runs: &Path, by: &apb_core::launch::StartedBy) -> bool {
    if !apb_core::registry::is_safe_segment(&by.run_id) {
        return false;
    }
    let here = runs.join(&by.run_id);
    let there = by
        .workspace_id
        .as_deref()
        .and_then(|ws| apb_core::projects::resolve_root(ws).ok())
        .map(|root| root.join(".apb/runs").join(&by.run_id));
    [Some(here), there]
        .into_iter()
        .flatten()
        .any(|d| d.is_dir() && crate::external_data::is_flagged(&d))
}

/// The recall of a start that applies nothing remembered.
fn empty_recall() -> apb_core::run_memory::Recall {
    apb_core::run_memory::Recall {
        instruction: None,
        instruction_source: apb_core::run_memory::InstructionSource::None,
        values: Vec::new(),
        missing: Vec::new(),
        schema_changed: false,
        diagnostics: Vec::new(),
    }
}

fn unattended_start(root: &Path, opts: &mut RunOptions) -> Result<(), EngineError> {
    if let Some(parent) = &opts.parent_run
        && crate::manifest::read(&root.join(".apb/runs").join(parent))
            .ok()
            .flatten()
            .is_some_and(|m| m.unattended)
    {
        opts.unattended = true;
    }
    crate::gate::unattended::check_start(
        root,
        &crate::gate::UnattendedStart {
            unattended: opts.unattended,
            mode: opts.mode,
            execution: &opts.execution,
        },
    )
    .map_err(|refusal| {
        EngineError::Invalid(format!(
            "{}: {}",
            crate::run_config::UNATTENDED_UNSUPPORTED,
            refusal["detail"].as_str().unwrap_or_default()
        ))
    })?;
    opts.execution = crate::gate::unattended::execution_request(&opts.execution, opts.unattended);
    Ok(())
}

/// Claims a fresh run directory `<id>-<ms>` and opens its journal. The
/// directory is created exclusively: two starts of one playbook in the same
/// millisecond (now an ordinary case, since runs over different worktrees no
/// longer wait on each other) used to share one directory, and the loser's
/// start-up failure landed in the winner's journal. A taken id moves on to the
/// next millisecond value instead.
fn allocate_run(runs: &Path, id: &str) -> Result<(String, PathBuf, EventLog), EngineError> {
    std::fs::create_dir_all(runs)?;
    let mut ms = apb_core::clock::now_ms();
    for _ in 0..1000 {
        let run_id = format!("{id}-{ms}");
        let run_dir = runs.join(&run_id);
        match apb_core::fsutil::create_private_dir(&run_dir) {
            Ok(()) => {
                let log = EventLog::open(&run_dir)?;
                return Ok((run_id, run_dir, log));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ms += 1,
            Err(e) => return Err(e.into()),
        }
    }
    Err(EngineError::Invalid(format!(
        "cannot allocate a run id for `{id}`: every candidate is taken"
    )))
}

/// Runs scenario `prep` of playbook `id` (its version `version`, else
/// `current`) of the project at `root` to its end, in this process: the
/// ordinary drive under `.apb/scenario-runs/`, every boundary answered by
/// the scenario's session. Returns the run and how the scenario was used.
/// The caller enters the scenario process mode first (`apb __scenario-run`).
pub fn run_scenario(
    root: &Path,
    id: &str,
    version: Option<&str>,
    prep: &crate::scenario::session::ScenarioPrep,
) -> Result<crate::scenario::session::ScenarioRun, EngineError> {
    run_scenario_of(root, apb_core::scope::Scope::Project, id, version, prep)
}

/// [`run_scenario`] of a playbook of `scope`: a global playbook is read
/// from the global store and its scenario run still lives under the
/// project at `root`, as a global playbook's run does.
pub fn run_scenario_of(
    root: &Path,
    scope: apb_core::scope::Scope,
    id: &str,
    version: Option<&str>,
    prep: &crate::scenario::session::ScenarioPrep,
) -> Result<crate::scenario::session::ScenarioRun, EngineError> {
    let t = match scope {
        apb_core::scope::Scope::Project => PrepareTarget {
            definition_parent: root.join(".apb"),
            execution_root: root.to_path_buf(),
            origin_label: "project",
        },
        apb_core::scope::Scope::Global => PrepareTarget {
            definition_parent: apb_core::store::global_playbooks_parent()
                .ok_or_else(|| EngineError::Invalid("no global config dir".into()))?,
            execution_root: root.to_path_buf(),
            origin_label: "global",
        },
    };
    let place = Placement {
        runs: crate::scenario::RunsDir::Scenario.under(root),
        scenario: Some(prep),
    };
    let mut p = prepare_placed(&t, id, version, RunOptions::default(), &place)?;
    let run_dir = p.run_dir.clone();
    let run_id = p.run_id.clone();
    let outcome = drive(
        p.playbook.clone(),
        &p.run_dir,
        root,
        &mut p.log,
        &p.cfg,
        p.start_node.clone(),
        StartMode::Rerun,
        p.run_id.clone(),
        p.mode,
        &mut p.guard,
    );
    if outcome.is_err() {
        let _ = p.log.append(EventPayload::RunFinished {
            outcome: "failed".into(),
        });
    }
    let session = crate::scenario::session::unregister(&run_dir);
    let outcome = outcome?;
    Ok(crate::scenario::session::ScenarioRun::of(
        run_id,
        run_dir,
        outcome.outcome,
        session.as_deref(),
    ))
}

/// Where a run is placed: the directory its run directory is allocated in
/// (a project's `.apb/runs`, `.apb/scenario-runs`, or for a child run the
/// directory of its parent's), and the script of a scenario run.
pub(crate) struct Placement<'a> {
    pub(crate) runs: PathBuf,
    pub(crate) scenario: Option<&'a crate::scenario::session::ScenarioPrep>,
}

pub(crate) fn prepare_run_target(
    t: &PrepareTarget,
    id: &str,
    version: Option<&str>,
    opts: RunOptions,
) -> Result<Prepared, EngineError> {
    let place = Placement {
        runs: crate::scenario::RunsDir::Production.under(&t.execution_root),
        scenario: None,
    };
    prepare_placed(t, id, version, opts, &place)
}

/// [`prepare_run_target`] at `place`. A scenario run executes nothing
/// outside its run directory: it takes no lock and no working tree, runs
/// no launch, consent, notification, isolation or import check (nothing it
/// runs could act), recalls no remembered input, and binds every agent
/// node to the scripted agent.
pub(crate) fn prepare_placed(
    t: &PrepareTarget,
    id: &str,
    version: Option<&str>,
    mut opts: RunOptions,
    place: &Placement,
) -> Result<Prepared, EngineError> {
    let root = t.execution_root.as_path();
    let scenario = place.scenario;
    if let Some(sp) = scenario {
        opts.unattended = sp.scenario.unattended || opts.unattended;
        opts.external_data = sp.external_data || opts.external_data;
        opts.cache = CacheRunMode::Off;
        opts.params = sp.scenario.params.clone();
        opts.instruction = sp.scenario.instruction.clone();
    }
    unattended_start(root, &mut opts)?;
    external_data_start(place, &mut opts);
    let reg = Registry::open_dir(&t.definition_parent)?;
    let loaded = reg.load(id, version)?;
    // A version a flagged run's patch made stays with runs that process
    // external data.
    if !opts.external_data
        && let Some(run) = apb_core::versioning::read_provenance_in(
            &t.definition_parent.join("playbooks").join(id),
            &loaded.version,
        )
        .and_then(|p| p.external_data_run)
    {
        return Err(EngineError::Invalid(format!(
            "{}: version {} of `{id}` was made by a patch of run {run}, which processed external data; start it with external_data or run another version",
            crate::external_data::RUN,
            loaded.version
        )));
    }
    let digest = loaded.trust_digest()?;
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
    // The import approval rule holds for every start, gated or not (a
    // trial, a sub-playbook child): the gate's own check, so the rule has
    // one copy.
    let playbook_dir = t.definition_parent.join("playbooks").join(id);
    // A scenario run executes none of the imported scripts.
    if scenario.is_none() {
        crate::gate::check_import_approval(&playbook_dir, id, &loaded.version, &digest).map_err(
            |refusal| {
                EngineError::Invalid(format!(
                    "playbook `{id}` {}: {} (digest {digest})",
                    loaded.version,
                    refusal["detail"].as_str().unwrap_or_default()
                ))
            },
        )?;
    }
    let mut playbook = loaded.playbook.clone();
    // A scenario never needs a real secret: a declared secret param holds a
    // placeholder (the scenario may not set one).
    if scenario.is_some() {
        for p in playbook.params.iter().filter(|p| p.secret) {
            opts.params
                .entry(p.name.clone())
                .or_insert_with(|| crate::scenario::session::SECRET_PLACEHOLDER.to_string());
        }
    }

    // The global config is needed to resolve profile invocations (agents, program).
    // A broken config is a start-up error, not a silent default.
    let global = GlobalConfig::load().map_err(EngineError::Invalid)?;
    // Output limits: resolved once and kept in the run config, so every
    // attempt of the run (and a re-drive) applies the same ones.
    let output_limits = global
        .output_limits_for(root)
        .map_err(EngineError::Invalid)?;
    // Host execution mode (0.23.0): resolved once, before anything is
    // written, so a refusal leaves no run behind. The manifest keeps it.
    // A scenario run's executor is the scripted agent: neither host nor
    // supervised.
    let execution = match scenario {
        Some(_) => apb_core::execution::ResolvedExecution {
            mode: apb_core::execution::ExecutionMode::Cli,
            source: apb_core::execution::ExecutionSource::Default,
            fallback_to_host: false,
            client: None,
            notes: Vec::new(),
        },
        None => {
            apb_core::execution::resolve_for(root, &opts.execution).map_err(EngineError::Invalid)?
        }
    };
    if execution.mode == apb_core::execution::ExecutionMode::Host
        && opts.mode.expects_supervisor_agent()
    {
        return Err(EngineError::Invalid(
            "host execution mode spawns no agent CLI, so it cannot start a background supervisor agent: supervise the run from the host session (supervise: self) instead".into(),
        ));
    }

    // List nodes run on the CLI only in this release (their items are
    // agent attempts the engine spawns itself).
    if execution.mode == apb_core::execution::ExecutionMode::Host
        && playbook.nodes.iter().any(|n| n.list().is_some())
    {
        return Err(EngineError::Invalid(
            "list nodes need execution: cli in this release".into(),
        ));
    }

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
    let ctx = ValidationContext::for_registry(&reg, origin);
    let report = validate(&playbook, &ctx);
    let errors: Vec<String> = report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| format!("{} {}", i.code, i.message))
        .collect();
    if !errors.is_empty() {
        return Err(EngineError::Invalid(format!(
            "playbook `{id}` is invalid: {}",
            errors.join("; ")
        )));
    }
    // A child of a run held to `publish_only` is held to it too (decision
    // record 0010): its playbook's own flag cannot lift it.
    if opts.publish_only {
        let open: Vec<String> = apb_core::validate::publish_only_violations(&playbook)
            .iter()
            .map(|o| format!("`{}` ({})", o.node, o.fix))
            .collect();
        if !open.is_empty() {
            return Err(EngineError::Invalid(format!(
                "playbook `{id}` runs under a parent held to `publish_only`, and node(s) {} run an agent without `isolation: full`",
                open.join(", ")
            )));
        }
    }
    // Isolation, with the run's real execution mode: the gate's check again,
    // so no start path (a trial, a child, a resume of a fresh start) runs a
    // `full` node this machine cannot isolate. Nothing is written yet.
    // A scenario run spawns nothing to isolate: not evaluated, and the host
    // is not probed (the probe starts a program the scenario guard refuses).
    let contracts = match scenario {
        Some(_) => Vec::new(),
        None => crate::gate::isolation::node_contracts(
            root,
            &playbook,
            origin,
            execution.mode == apb_core::execution::ExecutionMode::Host,
        ),
    };
    let refused = crate::gate::isolation::refused(&contracts);
    if !refused.is_empty() {
        return Err(EngineError::Invalid(format!(
            "{}: {}",
            apb_core::isolation::contract::UNAVAILABLE,
            crate::gate::isolation::text(&refused)
        )));
    }
    // Cloud catalog models (`gate::cloud`): in cli mode, a model an agent
    // step cannot run on, one the catalog lacks, or a primary executor whose
    // key does not reach it refuses the start before anything is written.
    // Host mode spawns no agent CLI; a scenario run spawns nothing.
    let cloud_warnings =
        if scenario.is_none() && execution.mode == apb_core::execution::ExecutionMode::Cli {
            crate::gate::cloud::check_tree(
                root,
                &playbook,
                origin,
                opts.expected_children.as_ref(),
                crate::gate::cloud::StartBinding {
                    supervised: opts.mode.expects_supervisor_agent(),
                    overrides: opts.overrides.as_ref(),
                },
            )
            .map_err(|r| EngineError::Invalid(crate::gate::cloud::text(&r)))?
        } else {
            Vec::new()
        };
    for w in &cloud_warnings {
        eprintln!("apb: warning: {w}");
    }

    // --- 0.24.0 irreversible consent ---
    // Checked on the effective playbook before anything is written, so a
    // refusal leaves no run behind. The manifest keeps what it returns.
    let consent_origin = if t.origin_label == "global" {
        apb_core::scope::Origin::Global
    } else {
        apb_core::scope::Origin::Project { workspace_id: None }
    };
    let consent = match scenario {
        Some(_) => None,
        None => check_consent(
            root,
            &playbook,
            &consent_origin,
            opts.expected_children.as_ref(),
            opts.consent.as_ref(),
        )?,
    };
    // --- end 0.24.0 irreversible consent ---

    // The launch authorization, checked again against the policy state of
    // what is about to run, before anything is written.
    // Read in the project that owns the definition (a trial's worktree
    // executes elsewhere), as the launch check read it.
    let owner = match consent_origin {
        apb_core::scope::Origin::Global => root.to_path_buf(),
        _ => t
            .definition_parent
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf()),
    };
    if scenario.is_none() {
        check_launch(
            &owner,
            &playbook,
            &consent_origin,
            opts.expected_children.as_ref(),
            opts.launch.as_ref(),
            opts.parent_run.is_some(),
            false,
        )?;
    }

    // The notification subscription: a child inherits its parent's, a
    // top-level run checks its own request against the configs and pins the
    // target. A person's request that cannot be honoured refuses the start
    // before anything is written; the run journals the subscription below.
    let notify_set = match &opts.parent_run {
        _ if scenario.is_some() => None,
        Some(parent) => crate::notify::start::inherited(&place.runs.join(parent)),
        None => {
            let playbook_key = apb_core::policy_file::PlaybookKey {
                id,
                global: t.origin_label == "global",
            };
            let check = crate::notify::check(root, Some(&playbook_key), opts.notify.as_ref());
            if let Some(why) = check.refusal {
                return Err(EngineError::Invalid(format!(
                    "{}: {why}",
                    crate::notify::REFUSED_START
                )));
            }
            check.event()
        }
    };

    let start_node = playbook
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Start))
        .ok_or_else(|| EngineError::Invalid("no start node".into()))?
        .id
        .clone();

    // A run takes the shared workdir lock if any node writes to the workdir
    // (`NodeKind::takes_workdir_lock`): an agent node (agent_task OR
    // finish-with-prompt), a script, or a `playbook` node whose child runs
    // in-process under the parent's lock. The single predicate FIXES review
    // I5: a Start + finish-with-prompt playbook runs an agent and now holds the
    // lock, which the old agent_task|script|playbook match missed.
    //
    // A busy workdir refuses the start outright unless the caller asked for the
    // start to be QUEUED (`RunOptions::workdir_queue_wait`). Queueing splits the
    // acquisition in two: preparation goes ahead without the lock, so the run
    // directory, the snapshot and the caller's parameters are persisted and the
    // run id can be answered immediately, and whoever drives the run claims the
    // lock before the first node (`Prepared::claim_queued_workdir`). Nothing
    // has been written yet at this point, so a refusal here still leaves no
    // trace behind, exactly as before.
    // Schema-default fill (review I6/R1-I2): every declared param that the
    // caller did NOT supply falls back to its `default` from the playbook
    // schema. This is the SINGLE normalization point for ALL runs - top-level
    // and sub-playbook children (which arrive here with an empty params map)
    // alike - so a playbook that relies on a param default renders the default
    // instead of the empty string the template renderer substitutes for a
    // missing param.
    //
    // Before the defaults, remembered run input (`run_memory`): a value the
    // caller did not pass comes from this project's memory of the playbook
    // when it still fits the params (not for a child); the instruction
    // likewise, then the definition's legacy draft. A real IO error fails the start rather than
    // starting with different input; nothing has been written yet.
    let recall = match scenario {
        // A scenario's input is its own, and a run that processes external
        // data recalls nothing: nothing remembered applies.
        Some(_) => empty_recall(),
        _ if opts.external_data => empty_recall(),
        None => recall_input(root, t.origin_label, id, &reg, &playbook.params, &opts)?,
    };
    let applied = apb_core::run_memory::apply(&opts.params, opts.instruction.clone(), &recall);
    let remembered = applied.remembered;
    let mut params = applied.params;
    for p in &playbook.params {
        if p.default.is_some() && !params.contains_key(&p.name) {
            params.insert(p.name.clone(), p.default_text().unwrap_or_default());
        }
    }

    // The launch surfaces refuse a value that does not match its param's
    // pattern; this backstop covers the paths that do not pass the gate:
    // a trial, an eval case, a detached drive. A scenario run's secret
    // placeholder is apb's own and is never checked. Nothing is written
    // yet, so a refusal leaves no run behind.
    for p in &playbook.params {
        if let Some(value) = params.get(&p.name) {
            let placeholder = scenario.is_some()
                && p.secret
                && value == crate::scenario::session::SECRET_PLACEHOLDER;
            if !placeholder && apb_core::param_pattern::check(p, value).is_err() {
                return Err(EngineError::Invalid(format!(
                    "param `{}` does not match its pattern `{}`",
                    p.name,
                    p.pattern.as_deref().unwrap_or("")
                )));
            }
        }
    }

    // The `run.previous_success` window (spec recurring-routines D3), read
    // once here where the params are final and nothing is written yet: a
    // refusal here (unreadable history, a bad `when_none` time) leaves no
    // run behind. Only a definition that reads the namespace looks at all.
    let mut previous_success = if !apb_core::previous_success::reads(&playbook) {
        None
    } else if scenario.is_some() || opts.eval.is_some() {
        // Repeatable by design: no lookup, so every play renders the same.
        Some(crate::previous_success::snapshot(
            root, None, &playbook, &params,
        )?)
    } else if let Some(pred) = opts.continued_from.as_deref() {
        // A continued run keeps its predecessor's window: the predecessor's
        // own record, copied unchanged. When the predecessor recorded none
        // (its definition did not read the namespace), look up as a fresh
        // start does.
        let pred_dir = place.runs.join(pred);
        let pred_events = crate::event::read_all(&pred_dir).map_err(|_| {
            EngineError::Invalid(format!(
                "previous_success_unreadable: run `{pred}`: its journal does not read; repair or remove it, then start again"
            ))
        })?;
        let recorded = crate::previous_success::recorded(&pred_events);
        Some(match recorded {
            Some(ps) => ps,
            None => look_up_previous_success(root, id, t.origin_label, &playbook, &params)?,
        })
    } else if opts.parent_run.is_some() {
        // A child start records the none window: two parent routines that
        // run the same child would otherwise share one history.
        Some(crate::previous_success::snapshot(
            root, None, &playbook, &params,
        )?)
    } else {
        Some(look_up_previous_success(
            root,
            id,
            t.origin_label,
            &playbook,
            &params,
        )?)
    };

    // The run's working tree (issue #67 item 8), when it is known at start:
    // passed by the caller, or the playbook's `worktree` template over params.
    // Resolved before the lock, because the tree is what the lock covers, and
    // before anything is written, so a tree that does not exist refuses the
    // start without leaving a run behind.
    let worktree = match scenario {
        Some(_) => None,
        None => {
            super::worktree::resolve_at_start(&playbook, root, opts.worktree.as_deref(), &params)?
        }
    };

    // The concurrency key (routine health D1): rendered from the final
    // params, checked for sensitivity, recorded for `run_provenance`;
    // nothing is written yet. A refusal here leaves nothing.
    let key = crate::concurrency::key_for_start(
        root,
        &playbook,
        &params,
        &crate::concurrency::KeyScope {
            scenario: scenario.is_some(),
            eval: opts.eval.is_some(),
            child: opts.parent_run.is_some(),
            depth: opts.depth,
        },
    )?;
    // The health hold (routine health D2), authoritative: a hold that
    // appeared after the gate, or a direct engine start, is still refused
    // here, before anything is claimed or written. A trial runs held: it
    // is how a person checks a fix before clearing.
    let top_level =
        scenario.is_none() && opts.eval.is_none() && opts.parent_run.is_none() && opts.depth == 0;
    let trial = opts
        .launch
        .as_ref()
        .is_some_and(|g| g.record.kind == "trial");
    if top_level && !trial {
        let scope = if t.origin_label == "global" {
            apb_core::scope::Scope::Global
        } else {
            apb_core::scope::Scope::Project
        };
        if let Err(refusal) = crate::health::state::hold_refusal(root, scope, &playbook.id) {
            return Err(EngineError::Conflict(format!(
                "playbook_on_hold: {}",
                refusal["detail"]
                    .as_str()
                    .unwrap_or("the playbook is on hold")
            )));
        }
    }

    // The start key is claimed before the tree lock, so a repeat while the
    // first run holds the tree answers that run, not "workdir busy"; the
    // claim stays pending (its owner this preparation) until the run is
    // verifiable.
    let keyed = opts
        .start_key
        .as_ref()
        .filter(|_| scenario.is_none() && opts.eval.is_none() && opts.parent_run.is_none());
    let mut key_claim = match keyed {
        Some(req) => Some(crate::start_key::claim(
            root,
            t.origin_label,
            id,
            req,
            &apb_core::step_env::StepCaller::from_env(),
        )?),
        None => None,
    };
    let start_key_tag = keyed.and_then(crate::start_key::shown_tag);

    let is_write = scenario.is_none() && playbook.nodes.iter().any(|n| n.kind.takes_workdir_lock());
    let mut queued_workdir = None;
    let mut queued_reason = String::new();
    // A keyed start takes the tree only once it holds its key: no
    // `acquire_tree` here, whatever its `on_busy` answers. Two keyed
    // starts that both took the tree first would meet at the tree and the
    // loser would fail before its mode was ever evaluated.
    let guard = if key.is_some() {
        // A start that shares the tree takes no lock, keyed or not.
        if is_write && !opts.allow_shared_workdir {
            queued_workdir = Some(
                opts.workdir_queue_wait
                    .unwrap_or(Duration::ZERO)
                    .max(crate::concurrency::TREE_AFTER_TURN),
            );
            queued_reason = "concurrency: the key is taken before the working tree".into();
        }
        None
    } else if is_write {
        match acquire_tree(
            root,
            worktree.as_ref().map(|w| w.path.as_path()),
            opts.allow_shared_workdir,
        ) {
            Err(EngineError::WorkdirBusy(msg)) if opts.workdir_queue_wait.is_some() => {
                queued_workdir = opts.workdir_queue_wait;
                queued_reason = msg;
                None
            }
            other => other?,
        }
    } else {
        None
    };

    if let Some(ref pred) = opts.continued_from {
        crate::run_lineage::validate_continued_from(root, pred, id)?;
    }
    let (run_id, run_dir, mut log) = allocate_run(&place.runs, id)?;
    if let Some(c) = key_claim.as_mut() {
        c.attach(&run_id)?;
        #[cfg(any(test, feature = "testing"))]
        crate::start_key::test_hooks::after_attach(root);
    }
    // Step 4 of the key's preparation order: the run gets a driver claim
    // of its own (an `apb stop` or `apb pause` of the waiting run is then
    // posted, not written over it), the slot is admitted in one
    // recoverable transaction, and the record says whether it queued.
    let mut slot_owner = None;
    let mut key_queued = false;
    if let Some(record) = &key {
        crate::driver::publish_driver_pid(&run_dir, std::process::id());
        let lineage = crate::concurrency::lineage(
            root,
            opts.launch
                .as_ref()
                .and_then(|g| g.record.started_by.as_ref()),
        );
        let on_busy = playbook
            .concurrency
            .as_ref()
            .map(|c| c.on_busy)
            .unwrap_or_default();
        let scope = apb_core::scope::Scope::parse(t.origin_label)
            .unwrap_or(apb_core::scope::Scope::Project);
        match crate::concurrency::admit(root, scope, id, record, on_busy, &run_id, &lineage) {
            Err(e) => {
                // A refused start leaves no run and frees its start key:
                // the directory holds only the empty journal and
                // `driver.pid`, so it goes away whole, and the `Claim`
                // drop finds no `run_started` and frees its entry.
                crate::driver::let_go_unclaimed(&run_dir);
                if std::fs::symlink_metadata(&run_dir).is_ok_and(|m| m.is_dir()) {
                    let _ = std::fs::remove_dir_all(&run_dir);
                }
                return Err(e);
            }
            Ok(admission) => {
                #[cfg(any(test, feature = "testing"))]
                crate::deadline_hooks::at(crate::deadline_hooks::Hook::KeyAdmitted, &run_dir);
                let holder;
                match admission {
                    crate::concurrency::Admission::Active(owner) => {
                        holder = None;
                        slot_owner = Some(owner);
                    }
                    crate::concurrency::Admission::Queued { holder: h, owner } => {
                        holder = Some(h);
                        slot_owner = Some(owner);
                        key_queued = true;
                    }
                }
                if let Some(h) = holder {
                    queued_reason =
                        format!("concurrency: key `{}` is held by run `{h}`", record.display);
                }
            }
        }
    }
    // Step 5 of the key's order: a queued routine reads its
    // `run.previous_success` window once it got the key, not here, so a
    // run B queued behind A does not keep the success before A.
    if key_queued && opts.continued_from.is_none() {
        previous_success = None;
    }
    // A scenario run's session answers every boundary from here on.
    if let Some(sp) = scenario {
        crate::scenario::session::register(
            &run_dir,
            std::sync::Arc::new(crate::scenario::session::ScenarioSession::new(
                sp, &playbook, &run_dir,
            )),
        );
        apb_core::fsutil::create_dir_under(
            &run_dir,
            &run_dir.join(crate::scenario::session::WORK_DIR),
        )?;
    }
    let run_dir_owned = run_dir.clone();
    let run_id_owned = run_id.clone();
    let unstarted = Unstarted {
        run_dir: &run_dir_owned,
        run_id: &run_id_owned,
        playbook: id,
        version: &loaded.version,
        external_data: opts.external_data,
    };
    // Every run path (CLI, MCP, dashboard, detached driver, child runs) comes
    // through here, and nothing but the event log is in the run yet: make sure
    // `.apb/.gitignore` covers the run directory before any node output or
    // agent transcript lands in it. Best-effort.
    if scenario.is_none() {
        apb_core::workspace::ensure_local_ignored(root);
    }
    // The run snapshot = the effective playbook. Without overrides we write the raw yaml
    // (preserving formatting/comments); with overrides we serialize the
    // effective playbook, so the snapshot honestly reflects what actually ran.
    let snapshot_yaml = if has_overrides {
        prep_try_unstarted(
            &mut log,
            &unstarted,
            yaml_serde::to_string(&playbook).map_err(|e| EngineError::Yaml(e.to_string())),
        )?
    } else {
        loaded.yaml.clone()
    };
    // The exact bytes this start executes, kept as the run's first stored
    // definition; their digest is journaled on `run_started` below, the
    // journal's own evidence of what the start executes (surfaces only).
    let definition = crate::run_definitions::store_best_effort(&run_dir, snapshot_yaml.as_bytes());
    prep_try_unstarted(
        &mut log,
        &unstarted,
        snapshot_playbook(&run_dir, &snapshot_yaml),
    )?;
    // Scripts live in the definition's version directory (<def_parent>/playbooks/<id>/<version>/scripts),
    // not in the run snapshot - we copy them into run_dir, otherwise script nodes would not find
    // their files. The source is definition_parent (for a global playbook this is the
    // config dir, not the project root).
    let version_dir = t
        .definition_parent
        .join("playbooks")
        .join(id)
        .join(&loaded.version);
    prep_try_unstarted(&mut log, &unstarted, copy_scripts(&version_dir, &run_dir))?;
    // The copy is what script nodes execute, so verify THE COPY against the
    // digest checked above (and against the caller's permit through it): a
    // script swapped between loading the definition and copying it must not
    // run under an approval that never covered it.
    let copied = apb_core::scope::definition_digest(&loaded.yaml, &run_dir)
        .map_err(|e| EngineError::Invalid(format!("run scripts cannot be digested: {e}")));
    let copied = prep_try_unstarted(&mut log, &unstarted, copied)?;
    // Pinned only for a playbook with goal `script` criteria, baseline
    // `script` checks or list item `script` checks, the readers of the pin, so every other run's
    // `run_provenance` stays as it was in 0.22.
    let has_goal_scripts = playbook.goal.as_ref().is_some_and(|g| {
        g.criteria
            .iter()
            .any(|c| matches!(c.check, apb_core::schema::GoalCheck::Script { .. }))
    });
    let has_baseline_scripts = playbook.baseline_checks().iter().any(|c| {
        matches!(
            c.source(),
            Some(apb_core::baseline::BaselineSource::Script(_))
        )
    });
    // A list item check script reads the pin the same way.
    let has_item_check_scripts = playbook.nodes.iter().any(|n| {
        n.list()
            .and_then(|l| l.check.as_ref())
            .is_some_and(|c| c.script.is_some())
    });
    let scripts_digest = if has_goal_scripts || has_baseline_scripts || has_item_check_scripts {
        Some(prep_try_unstarted(
            &mut log,
            &unstarted,
            crate::run_scripts::digest(&run_dir),
        )?)
    } else {
        None
    };
    if copied != digest {
        return prep_try_unstarted(
            &mut log,
            &unstarted,
            Err(EngineError::Invalid(format!(
                "playbook `{id}` changed since it was checked (scripts digest mismatch)"
            ))),
        );
    }
    // The run's webhook hook secrets (for wait nodes, spec 6.7).
    prep_try_unstarted(
        &mut log,
        &unstarted,
        crate::hooks::generate_hooks(&run_dir, &playbook),
    )?;

    // Run input precedence (spec A): resolved with the params above, so
    // every surface (MCP, CLI, server, and a sub-playbook child) shares the
    // rule.
    let instruction = applied.instruction;

    let cfg = RunConfig {
        params,
        instruction,
        max_patches_per_run: opts.max_patches_per_run,
        context_max_bytes: opts.context_max_bytes,
        context_compact_model: opts.context_compact_model.clone(),
        overrides: opts.overrides.clone(),
        parent_run: opts.parent_run.clone(),
        continued_from: opts.continued_from.clone(),
        superseded_by: None,
        depth: opts.depth,
        publish_only: playbook.publish_only || opts.publish_only,
        parent_acted: opts.parent_acted,
        expected_children: opts.expected_children.clone(),
        expected_connectors: opts.expected_connectors.clone(),
        expected_connector_accounts: opts.expected_connector_accounts.clone(),
        cache: opts.cache,
        // Persisted so a detached driver process, which learns everything it
        // knows from `runs/<id>`, drives the run in the mode it was started in.
        mode: opts.mode,
        // Same reason as `mode`: the concurrency cap must survive a re-drive.
        max_parallel: opts.max_parallel,
        // Persisted whenever the caller allowed queueing, not only when this
        // start actually queued: a detached driver reads its lock ceiling from
        // here, and the lock can go busy between this preparation and the
        // driver reaching it.
        workdir_queue_wait_ms: opts
            .workdir_queue_wait
            .map(|d| d.as_millis().min(u64::MAX as u128) as u64),
        // Same reason as `mode`: a detached driver must not take the lock
        // this start chose to share.
        shared_workdir: opts.allow_shared_workdir,
        eval: opts.eval.clone(),
        output_limits: Some(output_limits),
        // A copy for the drive; the manifest's is the record.
        unattended: opts.unattended,
    };
    prep_try_unstarted(&mut log, &unstarted, write_run_config(&run_dir, &cfg))?;

    // Resolve profiles into the run snapshot + the immutable manifest (spec 3.6).
    // The executor path (no profiles) does not write a manifest. `origin` was already
    // derived above (used both by the validator and here). This is where a
    // gated sub-playbook child with a missing/drifted connector permit, or a
    // profile bundle mismatch, is refused (issue #42 finding 3b) - the run
    // directory and its journal already exist at this point, hence `prep_try`.
    let mut manifest = match scenario {
        Some(_) => scripted_manifest(&playbook),
        None => prep_try_unstarted(
            &mut log,
            &unstarted,
            build_run_manifest(
                &playbook,
                root,
                origin,
                &global,
                &run_dir,
                opts.expected_profile_bundles.as_ref(),
                &opts.expected_connectors,
                &opts.expected_connector_accounts,
                opts.overrides.as_ref(),
                opts.mode.expects_supervisor_agent(),
            ),
        )?,
    };
    let profiles_prov: Vec<ProfileProvenance> = manifest
        .profiles
        .iter()
        .map(|p| ProfileProvenance {
            scope: p.scope.clone(),
            name: p.name.clone(),
            bundle_digest: p.bundle_digest.clone(),
        })
        .collect();
    // Decision models (issue #165): snapshot the effective settings for a
    // run that has agent nodes (whose uses ask about attempts) or that asks
    // the models itself (a judge node, a judge edge, a list's judge check, a
    // review that decides on its own). Nothing at all without
    // `decisions.yaml`, so such a manifest is unchanged. A file that does
    // not load leaves the layer off; it never fails the run.
    if let Some(sp) = scenario {
        manifest.decisions = decision_settings(root, sp);
    } else if !manifest.profiles.is_empty() || apb_core::validate::asks_decision_models(&playbook) {
        manifest.decisions = crate::decision::snapshot(root);
    }
    // Host execution mode (0.23.0): absent for cli, so a CLI run's manifest
    // stays byte-identical. A run whose definition uses a person-only gate
    // or an approval always writes the block, with the `+guard` suffix, so
    // an apb from 0.23 on refuses to resume it (candidate A of the E5
    // investigation): the manifest is write-once, so the value is decided
    // here, before the origin stamp.
    let guarded = crate::manifest::manifest_guard(&playbook);
    manifest.execution =
        crate::manifest::ManifestExecution::for_run(&execution, guarded, opts.external_data);
    manifest.external_data = opts.external_data;
    // 0.24.0: the irreversible consent, only for a run that needed it.
    manifest.consent = consent;
    // Issue #192: a top-level run (not an eval run) that starts on the
    // playbook's candidate version is a trial of it.
    manifest.candidate_skipped = opts
        .candidate_skipped
        .clone()
        .filter(|_| opts.depth == 0 && opts.parent_run.is_none());
    manifest.unattended = opts.unattended;
    manifest.candidate_trial = scenario.is_none()
        && !opts.external_data
        && opts.depth == 0
        && opts.parent_run.is_none()
        && opts.eval.is_none()
        && crate::candidate::is_trial_start(
            &t.definition_parent.join("playbooks").join(id),
            &loaded.version,
        );
    manifest.task_type = playbook
        .quality
        .as_ref()
        .map(|q| task_type_at_start(q, root, &cfg.params));
    // The run's secret classification, fixed at start: a later patch adds
    // to it and never removes from it (`crate::run_redact`).
    manifest.secret_params = crate::run_redact::declared_secret(&playbook);
    // The final launch recheck, under the floor lock until the record is
    // written: a publication raises a floor under the same lock, so it lands
    // either before this check (and refuses the start) or after the record.
    // The check above refused early, before anything was written; script
    // copies and profile materialization ran since.
    // A scenario run and a repetition of `apb eval` are no history; every
    // other run leaves `origin` out (production).
    manifest.origin = match scenario {
        Some(sp) => Some(crate::manifest::RunOrigin::Scenario {
            level: sp.level,
            scenario: sp.scenario.id.clone(),
            scenario_digest: sp.scenario_digest.clone(),
        }),
        None if opts.eval.is_some() => Some(crate::manifest::RunOrigin::Eval),
        // A run that would be production but processes external data says
        // so in its origin too, for a reader that checks the origin alone.
        None => opts
            .external_data
            .then_some(crate::manifest::RunOrigin::ExternalData),
    };
    let floor_hold = if opts.parent_run.is_none() && scenario.is_none() {
        prep_try_unstarted(
            &mut log,
            &unstarted,
            apb_core::launch::floor::hold().map_err(|e| EngineError::Invalid(e.to_string())),
        )?
    } else {
        None
    };
    if scenario.is_none() {
        manifest.launch = prep_try_unstarted(
            &mut log,
            &unstarted,
            check_launch(
                &owner,
                &playbook,
                &consent_origin,
                opts.expected_children.as_ref(),
                opts.launch.as_ref(),
                opts.parent_run.is_some(),
                true,
            ),
        )?;
    }
    if !manifest.is_empty() {
        prep_try_unstarted(
            &mut log,
            &unstarted,
            crate::manifest::write(&run_dir, &manifest),
        )?;
    }
    drop(floor_hold);
    // Mark the directory as created by this installation (after the
    // write-once manifest, which the stamp covers), so a resume can tell it
    // from a run directory that arrived with the repository.
    prep_try_unstarted(
        &mut log,
        &unstarted,
        apb_core::run_origin::stamp(&run_dir, &run_id),
    )?;

    log.append(EventPayload::RunStarted {
        playbook: id.into(),
        version: loaded.version.clone(),
        definition: Some(definition),
        external_data: opts.external_data,
    })?;
    // Run provenance (spec 3): the definition scope, content fingerprint,
    // execution root, and profiles used. A separate event right after
    // RunStarted.
    // The deadline in force from the start: a top-level production run
    // only (a scenario's timing is scripted, an eval keeps its own, a child
    // is stopped through its parent).
    let deadline =
        (opts.parent_run.is_none() && opts.depth == 0 && scenario.is_none() && opts.eval.is_none())
            .then(|| crate::run_deadline::at_start(&playbook, opts.deadline_ms))
            .flatten();
    log.append(EventPayload::RunProvenance {
        origin: Some(t.origin_label.into()),
        digest: Some(digest),
        execution_root: Some(t.execution_root.to_string_lossy().into_owned()),
        scripts_digest,
        profiles: profiles_prov,
        remembered,
        apb: crate::event::apb_version(),
        limits: run_limits(&manifest),
        previous_success,
        untrusted: apb_core::untrusted::analyze(&playbook)
            .followers
            .iter()
            .map(|f| crate::event::UntrustedNode {
                node: f.node.clone(),
                sources: f.sources.iter().map(|s| s.label()).collect(),
                outward: f.outward.clone(),
                secrets: f.secrets.clone(),
                accepted: f.accepted.clone(),
            })
            .collect(),
        deadline,
        start_key: start_key_tag,
        // The record says whether this start waited for its key: a queued
        // run reads its window once it got the key, at either drive entry.
        concurrency: key.clone().map(|mut record| {
            record.queued = key_queued;
            record
        }),
    })?;
    // A definition using a feature an older apb would drop is guarded from
    // here on: every checkpoint carries the guard, so such an apb refuses
    // to continue the run. A deadline from the run argument alone is named
    // too, and the concurrency feature exactly when the run took a slot.
    if let Some(guard) = run_guard_of(&playbook, deadline.is_some(), key.is_some()) {
        log.append(guard)?;
    }
    // The claim is durable now: an active holder's reservation is cleared
    // (its owner released), a waiter's owner is kept for its turn.
    if let Some(owner) = slot_owner.take() {
        slot_owner = prep_try(&mut log, owner.confirm())?;
    }
    // The run is verifiable now: its key is bound to it.
    if let Some(c) = key_claim.take() {
        c.bind()?;
    }
    // The known problems this run shows its agents, rechecked where their
    // source changed, journaled once (only for a playbook whose
    // `known_problems:` block shows anything).
    if scenario.is_none()
        && let Some(shown) = crate::known_problems::at_start(
            root,
            apb_core::scope::Scope::parse(t.origin_label)
                .unwrap_or(apb_core::scope::Scope::Project),
            &playbook,
            &manifest,
        )
    {
        log.append(shown)?;
    }
    // The estimate snapshot and the budget in force (not for a child).
    if opts.parent_run.is_none() && opts.depth == 0 && scenario.is_none() {
        let recorded = budget_gate::record_start(
            &mut log,
            &budget_gate::StartRecord {
                run_dir: &run_dir,
                root,
                playbook: &playbook,
                manifest: &manifest,
                budget: opts.budget.as_ref(),
                shown: opts.estimate.as_ref(),
                overridden: has_overrides,
            },
        );
        prep_try(&mut log, recorded)?;
    }
    if let Some(set) = notify_set {
        log.append(set)?;
    }
    // Right after provenance, so a reader of the journal alone can tell an
    // admitted-but-waiting run from one whose first node is simply slow.
    if queued_workdir.is_some() {
        log.append(EventPayload::RunQueued {
            reason: queued_reason,
        })?;
    }
    if let Some(w) = &worktree {
        log.append(EventPayload::WorktreeResolved {
            path: w.path.to_string_lossy().into_owned(),
            source: w.source.into(),
            node: None,
        })?;
    }
    if let Some(ref pred) = opts.continued_from {
        prep_try(
            &mut log,
            crate::run_lineage::establish_run_lineage(root, pred, &run_id),
        )?;
    }

    Ok(Prepared {
        playbook,
        run_id,
        run_dir,
        log,
        cfg,
        guard,
        queued_workdir,
        worktree: worktree.map(|w| w.path),
        start_node,
        mode: opts.mode,
        slot_owner,
        key_queued,
    })
}

/// The launch authorization a start records (see [`crate::gate::launch`]).
/// A child keeps the grant it inherited through its pin. A top-level start
/// with a grant recomputes the policy state of the tree it is about to run
/// and refuses (`launch_policy_changed`) when a floor or a settings file
/// changed since the check; one without a grant (a start that bypassed
/// every launch surface) runs only a playbook agents may launch freely, and
/// records nothing. With `commit` (the final check, right before the record
/// is written) the grant's billing challenge is consumed.
fn check_launch(
    root: &Path,
    playbook: &apb_core::schema::Playbook,
    origin: &apb_core::scope::Origin,
    pins: Option<&BTreeMap<String, crate::run_config::ChildExpectation>>,
    grant: Option<&crate::gate::launch::LaunchGrant>,
    child: bool,
    commit: bool,
) -> Result<Option<apb_core::launch::LaunchRecord>, EngineError> {
    use crate::gate::launch;
    if child {
        return Ok(grant.map(|g| g.record.clone()));
    }
    let empty = BTreeMap::new();
    let tree = launch::Tree {
        root,
        playbook,
        origin,
        children: pins.unwrap_or(&empty),
        // A node without a pin fails closed at its spawn; here it only
        // leaves the tree's policy unread for that node.
        require_pins: false,
    };
    let refused = |v: serde_json::Value| {
        EngineError::Invalid(format!(
            "{}: {}",
            v["policy"].as_str().unwrap_or("launch_refused"),
            v["detail"].as_str().unwrap_or_default()
        ))
    };
    match grant {
        Some(g) => {
            if let Some(state) = &g.policy_state {
                let (now, _) = launch::effective(tree, g.tolerant).map_err(refused)?;
                if launch::policy_state(&now) != *state {
                    return Err(EngineError::Invalid(format!(
                        "{}: the launch policy of playbook `{}` changed since the launch was checked (now `{}`: {}); launch it again",
                        launch::POLICY_CHANGED,
                        playbook.id,
                        now.policy,
                        now.sources.join(", ")
                    )));
                }
            }
            // Backstop of the chain bound (the grant may come from a
            // handoff file): a record past it never starts.
            if let Some(by) = &g.record.started_by
                && by.chain > apb_core::launch::MAX_START_CHAIN
            {
                return Err(EngineError::Invalid(format!(
                    "{}: run `{}` starts a chain of {} runs started from run steps; at most {} are allowed",
                    crate::step_start::CHAIN_TOO_DEEP,
                    by.run_id,
                    by.chain,
                    apb_core::launch::MAX_START_CHAIN
                )));
            }
            // The final check commits the start: a billing confirmation is
            // spent now, not by a check that refused for another reason.
            if commit {
                g.consume_challenge().map_err(EngineError::Invalid)?;
            }
            Ok(Some(g.record.clone()))
        }
        None => {
            // Strict: an unchecked start never reads past a damaged
            // settings or floor file, which an agent can write; it runs
            // only a playbook agents may launch freely.
            let (now, _) = launch::effective(tree, false).map_err(refused)?;
            if now.policy != apb_core::launch::LaunchPolicy::Allow {
                return Err(EngineError::Invalid(format!(
                    "launch_unchecked: playbook `{}` is `{}` for agents ({}), and this start passed no launch check; start it through apb run, MCP playbook_run or the dashboard",
                    playbook.id,
                    now.policy,
                    now.sources.join(", ")
                )));
            }
            Ok(None)
        }
    }
}

// --- 0.24.0 irreversible consent ---
/// The engine's check at run start: `Ok(Some(consent))` to record in the
/// manifest (with the sources it covers) when the run needs it and has it, `Ok(None)` when it needs none,
/// an error naming what is irreversible when it needs one and has none.
fn check_consent(
    root: &Path,
    playbook: &Playbook,
    origin: &apb_core::scope::Origin,
    pins: Option<&BTreeMap<String, crate::run_config::ChildExpectation>>,
    granted: Option<&crate::consent::RunConsent>,
) -> Result<Option<crate::consent::RunConsent>, EngineError> {
    let sources = crate::gate::consent_sources(root, playbook, origin, pins);
    if sources.is_empty() {
        return Ok(None);
    }
    match granted {
        // The manifest records what the consent covered, so a later patch
        // cannot widen it (see `scheduler::patch`).
        Some(c) if c.irreversible => Ok(Some(crate::consent::RunConsent {
            sources,
            ..c.clone()
        })),
        _ => Err(EngineError::Invalid(format!(
            "{}: {}",
            crate::consent::REFUSAL_POLICY,
            crate::consent::refusal_detail(&playbook.id, &sources)
        ))),
    }
}
// --- end 0.24.0 irreversible consent ---

/// The manifest of a scenario run: every node that binds a profile is
/// bound to the scripted agent (one chain step, nothing to start), so the
/// drive's attempt loop runs as for a real agent.
pub(super) fn scripted_manifest(playbook: &Playbook) -> crate::manifest::RunExecutionManifest {
    let mut manifest = crate::manifest::RunExecutionManifest::default();
    let has_profile = playbook
        .nodes
        .iter()
        .any(|n| n.kind.effective_profile_ref(&playbook.defaults).is_some());
    if !has_profile {
        return manifest;
    }
    let spec: apb_core::config::InvocationDef =
        serde_json::from_value(serde_json::json!({ "argv": ["{prompt}"] }))
            .expect("the scripted invocation form parses");
    let profile = crate::manifest::ManifestProfile {
        scope: crate::scenario::session::SCENARIO_AGENT.into(),
        name: crate::scenario::session::SCENARIO_AGENT.into(),
        profile_digest: String::new(),
        bundle_digest: String::new(),
        soul: String::new(),
        soul_requirement: Default::default(),
        skills: Vec::new(),
        chain: vec![crate::invocation::ResolvedInvocation {
            agent_id: crate::scenario::session::SCENARIO_AGENT.into(),
            model: String::new(),
            spec,
            soul_delivery: Default::default(),
            canonical_executable: std::path::PathBuf::new(),
            executable_fingerprint: String::new(),
            cloud: None,
        }],
        ephemeral: false,
        hermetic: false,
        zcode_mode: None,
        process_env: None,
        tiers: Vec::new(),
        routed_tier: None,
        cascade: 0,
    };
    let key = profile.key();
    manifest.profiles.push(profile);
    for n in &playbook.nodes {
        if n.kind.effective_profile_ref(&playbook.defaults).is_some() {
            manifest.node_bindings.insert(n.id.clone(), key.clone());
        }
    }
    manifest
}

/// The decision settings a scenario run starts with. Quick: the judge uses
/// at the scenario's modes (default `enforce`), answered by the scenario's
/// provider, every other use off. Smoke: this machine's settings, the
/// request budget lowered to the cap (never raised), every engine aid use
/// off; nothing when the machine has none.
pub(super) fn decision_settings(
    root: &Path,
    prep: &crate::scenario::session::ScenarioPrep,
) -> Option<apb_core::decisions::EffectiveDecisions> {
    use apb_core::decisions::{DecisionMode, EffectiveDecisions};
    let judge_uses = ["judge_node", "judge_edge"];
    match prep.level {
        apb_core::scenario::ScenarioLevel::Quick => {
            let modes = &prep.scenario.decisions;
            let mut uses = std::collections::BTreeMap::new();
            for (name, mode) in [
                ("judge_node", modes.judge_node),
                ("judge_edge", modes.judge_edge),
            ] {
                let mode = mode.unwrap_or(DecisionMode::Enforce);
                uses.insert(
                    name.to_string(),
                    serde_json::from_value(serde_json::json!({ "mode": mode.as_str() }))
                        .expect("a use with a mode parses"),
                );
            }
            Some(EffectiveDecisions {
                mode: DecisionMode::Enforce,
                timeout_ms: 3000,
                providers: Vec::new(),
                budget: Default::default(),
                privacy: Default::default(),
                uses,
            })
        }
        apb_core::scenario::ScenarioLevel::Smoke => {
            let mut settings = crate::decision::snapshot(root)?;
            settings
                .uses
                .retain(|k, _| judge_uses.contains(&k.as_str()));
            let cap = prep
                .max_requests
                .min(crate::scenario::session::SMOKE_MAX_REQUESTS);
            settings.budget.max_requests_per_run = settings.budget.max_requests_per_run.min(cap);
            Some(settings)
        }
    }
}

/// The `run_guard` a run of `playbook` journals, `None` when it uses no
/// guarded feature (`apb_core::validate::guarded_features`).
pub(crate) fn run_guard(
    playbook: &Playbook,
    events: &[crate::event::Event],
) -> Option<EventPayload> {
    let keyed = events.iter().any(|e| {
        matches!(
            &e.payload,
            EventPayload::RunProvenance {
                concurrency: Some(_),
                ..
            }
        )
    });
    run_guard_of(
        playbook,
        crate::run_deadline::in_force(events).is_some(),
        keyed,
    )
}

/// [`run_guard`] with the run's contract given: the definition's features,
/// with `deadline` named exactly when a deadline is in force for the run
/// (a scenario, an eval and a child ignore the playbook's) and
/// `concurrency` named exactly when the run took a slot (a child, a
/// scenario and an eval take none).
fn run_guard_of(playbook: &Playbook, deadline: bool, concurrency: bool) -> Option<EventPayload> {
    use apb_core::validate::GuardedFeature;
    let mut features: Vec<GuardedFeature> = apb_core::validate::guarded_features(playbook)
        .into_iter()
        .filter(|f| *f != GuardedFeature::Deadline && *f != GuardedFeature::Concurrency)
        .collect();
    if deadline {
        features.push(GuardedFeature::Deadline);
    }
    if concurrency {
        features.push(GuardedFeature::Concurrency);
    }
    features.sort();
    let requires: Vec<String> = features
        .into_iter()
        .map(|f| f.as_str().to_string())
        .collect();
    (!requires.is_empty()).then_some(EventPayload::RunGuard { requires })
}

/// The limits a run journals on `run_provenance`, from the profiles its
/// manifest resolved: `same_agent_reviewer` when it binds both `apb-main`
/// and `apb-reviewer` and the reviewer's primary executor runs on the main
/// role's agent ([`apb_core::bundled::same_agent_reviewer`]; fallbacks are
/// not compared). Recorded at start only: a later
/// `supervisor_rebind_profile` does not update it.
fn run_limits(manifest: &crate::manifest::RunExecutionManifest) -> Vec<crate::event::RunLimit> {
    use apb_core::bundled::{self, ROLE_MAIN, ROLE_REVIEWER};
    let agent = |name: &str| {
        manifest
            .profiles
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| p.chain.first())
            .map(|i| i.agent_id.as_str())
    };
    match (agent(ROLE_MAIN), agent(ROLE_REVIEWER)) {
        (Some(main), Some(reviewer)) if bundled::same_agent_reviewer(main, reviewer) => {
            vec![crate::event::RunLimit {
                code: bundled::SAME_AGENT_REVIEWER.to_string(),
                detail: bundled::SAME_AGENT_REVIEWER_DETAIL.to_string(),
            }]
        }
        _ => Vec::new(),
    }
}

/// The `run.previous_success` window of a top-level start: the latest
/// earlier success of `id` in scope `scope_label` (spec recurring-routines
/// D2), snapshotted, or the none window. A run directory that cannot be
/// classified refuses the start.
pub(crate) fn look_up_previous_success(
    root: &std::path::Path,
    id: &str,
    scope_label: &str,
    playbook: &Playbook,
    params: &std::collections::BTreeMap<String, String>,
) -> Result<crate::event::PreviousSuccess, EngineError> {
    let found = crate::previous_success::find(root, id, scope_label).map_err(|h| {
        EngineError::Invalid(format!(
            "previous_success_unreadable: run `{}`: its {} does not read; repair or remove it, then start again",
            h.run, h.what
        ))
    })?;
    crate::previous_success::snapshot(root, found.as_deref(), playbook, params)
}

#[cfg(test)]
mod run_guard_tests {
    use super::*;

    /// A playbook with the block names `concurrency` only when asked: the
    /// run's slot decides, not the definition alone.
    #[test]
    fn run_guard_of_names_concurrency_only_when_asked() {
        let pb = Playbook::from_yaml(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nconcurrency: { on_busy: refuse }\nnodes:\n  - { id: start, type: start }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: done }\n",
        )
        .unwrap();
        let none = run_guard_of(&pb, false, false).map(|p| match p {
            EventPayload::RunGuard { requires } => requires,
            _ => Vec::new(),
        });
        assert_eq!(none, None);
        let keyed = run_guard_of(&pb, false, true).map(|p| match p {
            EventPayload::RunGuard { requires } => requires,
            _ => Vec::new(),
        });
        assert_eq!(keyed, Some(vec!["concurrency".to_string()]));
    }
}
