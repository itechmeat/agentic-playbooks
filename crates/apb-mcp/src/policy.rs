//! The server-side run policy gate (spec 9). Checks what cannot be
//! trusted to the host model's discipline: lifecycle, digest-based trust,
//! the cross-workspace boundary, and applicability preflight. Returns a structural
//! refusal (JSON) that the tool hands back to the agent as-is.

use std::path::Path;

use apb_core::config::program_in_path;
use apb_core::profile::ProfileScope;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::Registry;
use apb_core::schema::{Effect, NodeKind, Playbook};
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Lifecycle, TrustStore, read_lifecycle};
use apb_engine::run_config::ChildExpectation;
use serde_json::{Value, json};

/// String name of an effect, for plans/catalog.
pub fn effect_str(e: &Effect) -> &'static str {
    match e {
        Effect::FsRead => "fs_read",
        Effect::FsWrite => "fs_write",
        Effect::Network => "network",
        Effect::External => "external",
        Effect::Secrets => "secrets",
        Effect::Irreversible => "irreversible",
    }
}

/// Preflight facts for the two-phase contract (spec 7).
pub struct Preflight {
    pub version: String,
    pub digest: String,
    pub effects: Vec<String>,
}

/// Preflight of the definition in a given root: lifecycle (draft/retired are rejected)
/// and `requires` applicability. Without trust- and cross-workspace checks - this
/// is the lower layer shared by the local gate and the cross-workspace plan.
pub fn preflight(root: &Path, id: &str, version: Option<&str>) -> Result<Preflight, Value> {
    let reg = Registry::open(root)
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let loaded = reg
        .load(id, version)
        .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
    let playbook_dir = root.join(".apb/playbooks").join(id);
    match read_lifecycle(&playbook_dir) {
        Lifecycle::Active => {}
        Lifecycle::Draft => return Err(json!({ "policy": "draft_requires_trial", "id": id })),
        Lifecycle::Retired => return Err(json!({ "policy": "retired_not_runnable", "id": id })),
    }
    if let Some(req) = &loaded.playbook.requires {
        check_requires(root, req, id)?;
    }
    // The consent surface shows the WHOLE tree's effects at parent start (spec
    // C): the parent's effective effects UNION every pinned child's, recursively.
    // Reuse the same walk `check_run` uses so both derive the identical union
    // from one resolution. A cross-workspace playbook is always project-scoped
    // here; `acknowledge_untrusted: true` skips trust marking (trust is enforced
    // separately at execute-plan time), keeping preflight read-only.
    let origin = Origin::Project { workspace_id: None };
    let tree = resolve_tree(root, &loaded.playbook, &origin, id, true)?;
    let effects = tree
        .effects
        .iter()
        .map(|e| effect_str(e).to_string())
        .collect();
    Ok(Preflight {
        version: loaded.version.clone(),
        digest: digest_str(&loaded.yaml),
        effects,
    })
}

/// Permission to run, assembled in ONE pass of the trust check: the digest
/// of the definition and the exact map of verified profile bundles. The caller passes
/// EXACTLY this map to the engine (`expected_*`), without recomputing it separately -
/// otherwise editing a profile/skill in the window between the check and the recomputation
/// would give the engine a different set (TOCTOU).
#[derive(Debug, Clone)]
pub struct RunPermit {
    pub playbook_digest: String,
    pub profile_bundles: std::collections::BTreeMap<String, String>,
    /// Verified sub-playbook pins, keyed by THIS playbook's playbook-node id
    /// (spec C). The engine receives it verbatim and rejects drift.
    pub children: std::collections::BTreeMap<String, ChildExpectation>,
}

/// One-pass walk of a playbook's sub-playbook tree (spec C), shared by the local
/// run gate (`check_run`) and the cross-workspace consent surface (`preflight`)
/// so both derive the SAME children pins and recursive effects union from a
/// single resolution instead of duplicating it.
struct TreeResolution {
    /// Node-id -> verified child pin for THIS playbook (recursive).
    children: std::collections::BTreeMap<String, ChildExpectation>,
    /// Union of the parent's effective effects and every pinned child's
    /// effective effects (recursively). Rendered with `effect_str`, matching
    /// `Preflight::effects`.
    effects: std::collections::BTreeSet<Effect>,
    /// `<scope>/<name>` keys of child profile bundles that are not approved
    /// (empty when `acknowledge_untrusted` is set).
    untrusted: Vec<String>,
}

/// Seeds the effects union and cycle path with the parent itself, then walks and
/// verifies its sub-playbook tree once. `parent_id`/`origin` identify the parent
/// for cycle detection and `auto` scope resolution of its children.
fn resolve_tree(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    parent_id: &str,
    acknowledge_untrusted: bool,
) -> Result<TreeResolution, Value> {
    let mut effects: std::collections::BTreeSet<Effect> = apb_core::effects::effective(playbook);
    let parent_scope = if matches!(origin, Origin::Global) {
        "global"
    } else {
        "project"
    };
    let mut path: Vec<(String, String)> = vec![(parent_scope.to_string(), parent_id.to_string())];
    let mut untrusted: Vec<String> = Vec::new();
    let children = collect_children(
        root,
        playbook,
        origin,
        acknowledge_untrusted,
        &mut path,
        &mut untrusted,
        &mut effects,
    )?;
    Ok(TreeResolution {
        children,
        effects,
        untrusted,
    })
}

/// Checks whether a run is permitted. `Ok(RunPermit)` - the run may proceed (digest +
/// verified bundle map); `Err(value)` - a structural policy refusal.
/// `supervised` - whether the run will actually spawn an EXTERNAL supervisor
/// agent (CLI `--supervise`). Only then does the supervisor profile enter the
/// verified bundle set (and the engine snapshot), matching the manifest. All
/// current MCP paths (autonomous, supervise:"self") do not spawn an external supervisor
/// agent - they pass `false`.
pub fn check_run(
    root: &Path,
    wref: &PlaybookRef,
    acknowledge_untrusted: bool,
    supervised: bool,
) -> Result<RunPermit, Value> {
    // Cross-workspace: a direct run in a foreign workspace is forbidden, only through
    // the two-phase contract (spec 7). This path bypasses it, hence the refusal.
    if matches!(
        wref.origin,
        Origin::Project {
            workspace_id: Some(_)
        }
    ) {
        return Err(json!({
            "policy": "cross_workspace_requires_plan",
            "detail": "use playbook_prepare_run / playbook_execute_plan for another workspace",
        }));
    }

    let definition_parent = match &wref.origin {
        Origin::Global => match apb_core::store::global_playbooks_parent() {
            Some(p) => p,
            None => return Err(json!({ "policy": "not_found", "detail": "no global config dir" })),
        },
        Origin::Project { .. } => root.join(".apb"),
    };
    let reg = match Registry::open_dir(&definition_parent) {
        Ok(r) => r,
        Err(e) => return Err(json!({ "policy": "not_found", "detail": e.to_string() })),
    };
    let loaded = match reg.load(&wref.id, wref.version.as_deref()) {
        Ok(l) => l,
        Err(e) => return Err(json!({ "policy": "not_found", "detail": e.to_string() })),
    };

    // Lifecycle: draft/retired does not run through the normal path - only via trial.
    let playbook_dir = definition_parent.join("playbooks").join(&wref.id);
    match read_lifecycle(&playbook_dir) {
        Lifecycle::Active => {}
        Lifecycle::Draft => {
            return Err(json!({ "policy": "draft_requires_trial", "id": wref.id }));
        }
        Lifecycle::Retired => {
            return Err(json!({ "policy": "retired_not_runnable", "id": wref.id }));
        }
    }

    // Digest-based trust: unapproved content requires an explicit acknowledge.
    let digest = digest_str(&loaded.yaml);
    if !acknowledge_untrusted && !TrustStore::load().is_approved(&digest) {
        return Err(json!({
            "policy": "untrusted_requires_acknowledge",
            "id": wref.id,
            "digest": digest,
            "detail": "run again with acknowledge_untrusted: true after user confirmation",
        }));
    }

    // Profile bundle trust (spec 5.1): the profile plus the actual content of its
    // skills are trusted as a unit. An unapproved bundle requires acknowledge.
    // The returned map is exactly what was verified, and it is what goes to the engine.
    let profile_bundles = check_profile_bundles(
        root,
        &loaded.playbook,
        &wref.origin,
        acknowledge_untrusted,
        supervised,
    )?;

    // Sub-playbook pins (spec C): walk the reference tree in the same gate pass,
    // detect cycles, and trust-check each child's bundles alongside the parent's.
    // The recursive effects union that this walk also accumulates is the user's
    // consent surface, exposed through `preflight` (which shares this walk); the
    // local run gate only needs the verified pins.
    let tree = resolve_tree(
        root,
        &loaded.playbook,
        &wref.origin,
        &wref.id,
        acknowledge_untrusted,
    )?;
    if !tree.untrusted.is_empty() {
        return Err(json!({
            "policy": "untrusted_profile_requires_acknowledge",
            "profiles": tree.untrusted,
            "detail": "a sub-playbook binds an untrusted profile bundle; run again with acknowledge_untrusted: true after user confirmation",
        }));
    }

    // Applicability preflight (spec 5.2), in the execution root (current project).
    if let Some(req) = &loaded.playbook.requires {
        check_requires(root, req, &wref.id)?;
    }

    Ok(RunPermit {
        playbook_digest: digest,
        profile_bundles,
        children: tree.children,
    })
}

/// Recursively collects and verifies the sub-playbook pins of `playbook`.
/// `origin` is where THIS playbook's definition came from (drives `scope: auto`
/// resolution of its children: parent origin first, then global, mirroring
/// profile scope resolution). `path` holds the `(scope, id)` pairs on the
/// current branch for cycle detection; a repeated pair is a cycle. On an
/// untrusted child bundle the key is pushed to `untrusted` (the caller turns a
/// non-empty list into the standard refusal). `effects` accumulates the union of
/// every pinned child's effective effects. Returns the node-id -> ChildExpectation
/// map for `playbook`.
#[allow(clippy::too_many_arguments)]
fn collect_children(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    acknowledge_untrusted: bool,
    path: &mut Vec<(String, String)>,
    untrusted: &mut Vec<String>,
    effects: &mut std::collections::BTreeSet<Effect>,
) -> Result<std::collections::BTreeMap<String, ChildExpectation>, Value> {
    let mut out = std::collections::BTreeMap::new();
    for n in &playbook.nodes {
        let NodeKind::Playbook { playbook: pref, .. } = &n.kind else {
            continue;
        };
        // Scope resolution: an explicit scope pins the origin; `auto` prefers the
        // parent's origin, then global (mirrors profile scope: auto ordering).
        let candidates: Vec<Origin> = match pref.scope {
            ProfileScope::Global => vec![Origin::Global],
            ProfileScope::Project => vec![Origin::Project { workspace_id: None }],
            ProfileScope::Auto => match origin {
                Origin::Global => vec![Origin::Global],
                Origin::Project { .. } => {
                    vec![Origin::Project { workspace_id: None }, Origin::Global]
                }
            },
        };
        // First candidate scope in which the child resolves wins.
        let mut resolved_opt = None;
        for cand in &candidates {
            let cref = PlaybookRef {
                origin: cand.clone(),
                id: pref.id.clone(),
                version: None,
            };
            if let Ok(r) = apb_core::store::resolve(root, &cref) {
                resolved_opt = Some((cand.clone(), r));
                break;
            }
        }
        let Some((child_origin, resolved)) = resolved_opt else {
            return Err(json!({
                "policy": "not_found",
                "detail": format!(
                    "sub-playbook `{}` (node `{}`) did not resolve in any candidate scope",
                    pref.id, n.id
                ),
            }));
        };
        let scope_str = if matches!(child_origin, Origin::Global) {
            "global"
        } else {
            "project"
        };
        let pair = (scope_str.to_string(), resolved.id.clone());
        if path.contains(&pair) {
            let mut cycle: Vec<String> = path.iter().map(|(s, i)| format!("{s}/{i}")).collect();
            cycle.push(format!("{scope_str}/{}", resolved.id));
            return Err(json!({ "policy": "sub_playbook_cycle", "cycle": cycle }));
        }
        // Load the child definition to walk its own children + collect bundles.
        let reg = Registry::open_dir(&resolved.definition_parent)
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        let loaded = reg
            .load(&resolved.id, Some(&resolved.version))
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        // Fold this child's effective effects into the consented union.
        effects.extend(apb_core::effects::effective(&loaded.playbook));
        let worigin = if matches!(child_origin, Origin::Global) {
            PlaybookOrigin::Global
        } else {
            PlaybookOrigin::Project
        };
        // Child profile bundles (nodes + finish-with-prompt), trust-checked.
        let mut bundles = std::collections::BTreeMap::new();
        let store = TrustStore::load();
        for r in collect_profile_refs(&loaded.playbook, false) {
            match profile_store::compute_bundle(root, worigin, &r) {
                Ok((lp, _pairs, bundle)) => {
                    let key = format!("{}/{}", profile_store::scope_str(lp.scope), lp.name);
                    if !acknowledge_untrusted
                        && !store.is_approved(&bundle)
                        && !untrusted.contains(&key)
                    {
                        untrusted.push(key.clone());
                    }
                    bundles.insert(key, bundle);
                }
                Err(e) => {
                    return Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() }));
                }
            }
        }
        // Recurse into the child's own sub-playbook nodes on the current branch.
        path.push(pair);
        let grand = collect_children(
            root,
            &loaded.playbook,
            &child_origin,
            acknowledge_untrusted,
            path,
            untrusted,
            effects,
        )?;
        path.pop();

        out.insert(
            n.id.clone(),
            ChildExpectation {
                id: resolved.id.clone(),
                scope: scope_str.to_string(),
                version: resolved.version.clone(),
                playbook_digest: resolved.digest.clone(),
                profile_bundles: bundles,
                children: grand,
            },
        );
    }
    Ok(out)
}

/// Checks trust for the bundle of every profile the playbook references
/// (nodes + supervisor, accounting for defaults). An unapproved bundle without acknowledge -
/// refusal `untrusted_profile_requires_acknowledge` with the list of `<scope>/<name>`.
/// Collects the playbook's profile references (nodes + supervisor, accounting for defaults).
///
/// Does not account for the run-local ephemeral executor (`overrides`): the gate only sees
/// the playbook definition. This is safe only because the surfaces do NOT
/// combine `overrides` with the trust gate (`expected_profile_bundles`) - see
/// the invariant in `build_run_manifest`. Otherwise a node's profile key with an ephemeral
/// override would end up in the permit but not in the snapshot, producing a false key-set mismatch.
///
/// `supervised` - whether the run will spawn an external supervisor agent. The supervisor profile
/// (supervisor.profile OR defaults.profile, even without a section) enters the set
/// ONLY when `supervised: true` - the same rule as `build_run_manifest`,
/// otherwise the permit's key set would diverge from the snapshot.
pub fn collect_profile_refs(
    playbook: &Playbook,
    supervised: bool,
) -> Vec<apb_core::profile::QualifiedProfileRef> {
    let mut refs = Vec::new();
    for n in &playbook.nodes {
        let pref = match &n.kind {
            NodeKind::AgentTask { profile, .. } => profile
                .clone()
                .or_else(|| playbook.defaults.profile.clone()),
            NodeKind::Finish {
                prompt: Some(_),
                profile,
                ..
            } => profile
                .clone()
                .or_else(|| playbook.defaults.profile.clone()),
            _ => None,
        };
        if let Some(p) = pref {
            refs.push(p);
        }
    }
    if supervised
        && let Some(p) = playbook
            .supervisor
            .as_ref()
            .and_then(|s| s.profile.clone())
            .or_else(|| playbook.defaults.profile.clone())
    {
        refs.push(p);
    }
    refs
}

/// Pairs of `(<scope>/<name>, bundle_digest)` for a project-scope playbook's profiles.
/// Best-effort is safe: a skipped profile causes a key-set mismatch
/// in the engine (exact-match), i.e. a refusal. For the local-project and foreign-
/// project paths.
pub fn playbook_profile_bundles(
    root: &Path,
    id: &str,
    version: Option<&str>,
    supervised: bool,
) -> Vec<(String, String)> {
    playbook_profile_bundles_for(
        &root.join(".apb"),
        root,
        id,
        version,
        PlaybookOrigin::Project,
        supervised,
    )
}

/// Origin-aware variant: the definition is read from `def_parent`, profiles
/// are resolved with the given origin (a global playbook sees only global
/// profiles). `exec_root` is the execution root (for project skills).
pub fn playbook_profile_bundles_for(
    def_parent: &Path,
    exec_root: &Path,
    id: &str,
    version: Option<&str>,
    origin: PlaybookOrigin,
    supervised: bool,
) -> Vec<(String, String)> {
    let Ok(reg) = Registry::open_dir(def_parent) else {
        return vec![];
    };
    let Ok(loaded) = reg.load(id, version) else {
        return vec![];
    };
    let mut out = Vec::new();
    for r in collect_profile_refs(&loaded.playbook, supervised) {
        if let Ok((lp, _pairs, bundle)) = profile_store::compute_bundle(exec_root, origin, &r) {
            out.push((
                format!("{}/{}", profile_store::scope_str(lp.scope), lp.name),
                bundle,
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn check_profile_bundles(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    acknowledge_untrusted: bool,
    supervised: bool,
) -> Result<std::collections::BTreeMap<String, String>, Value> {
    let worigin = match origin {
        Origin::Global => PlaybookOrigin::Global,
        _ => PlaybookOrigin::Project,
    };
    let refs = collect_profile_refs(playbook, supervised);
    let mut verified: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if refs.is_empty() {
        return Ok(verified);
    }
    let store = TrustStore::load();
    let mut untrusted: Vec<String> = Vec::new();
    for r in refs {
        match profile_store::compute_bundle(root, worigin, &r) {
            Ok((loaded, _pairs, bundle)) => {
                let key = format!("{}/{}", profile_store::scope_str(loaded.scope), loaded.name);
                if !acknowledge_untrusted
                    && !store.is_approved(&bundle)
                    && !untrusted.contains(&key)
                {
                    untrusted.push(key.clone());
                }
                // The map VERIFIED by this same pass - the engine will receive it as
                // expected (permit), not a freshly recomputed one (closes TOCTOU).
                verified.insert(key, bundle);
            }
            Err(e) => {
                return Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() }));
            }
        }
    }
    if !untrusted.is_empty() {
        return Err(json!({
            "policy": "untrusted_profile_requires_acknowledge",
            "profiles": untrusted,
            "detail": "run again with acknowledge_untrusted: true after user confirmation",
        }));
    }
    Ok(verified)
}

/// A safe relative path name: not absolute and without `..` components.
/// Protection against `requires.files` serving as an existence oracle for
/// arbitrary files (especially in foreign prepare_run before trust is
/// confirmed) - see spec 5.2.
fn is_safe_relative(p: &str) -> bool {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return false;
    }
    path.components().all(|c| {
        !matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::Prefix(_)
                | std::path::Component::RootDir
        )
    })
}

/// Checks `requires` applicability: files - only safe relative
/// paths inside the root; commands - only program names (no path separators).
fn check_requires(root: &Path, req: &apb_core::schema::Requires, id: &str) -> Result<(), Value> {
    let mut missing: Vec<String> = Vec::new();
    for f in &req.files {
        if !is_safe_relative(f) {
            return Err(json!({ "policy": "requires_unsafe_path", "id": id, "path": f }));
        }
        if !root.join(f).exists() {
            missing.push(format!("file:{f}"));
        }
    }
    for c in &req.commands {
        if c.contains('/') || c.contains('\\') {
            return Err(json!({ "policy": "requires_unsafe_command", "id": id, "command": c }));
        }
        if !program_in_path(c) {
            missing.push(format!("command:{c}"));
        }
    }
    if !missing.is_empty() {
        return Err(json!({ "policy": "requires_unmet", "id": id, "missing": missing }));
    }
    Ok(())
}
