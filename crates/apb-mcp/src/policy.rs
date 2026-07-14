//! The server-side run policy gate (spec 9). Checks what cannot be
//! trusted to the host model's discipline: lifecycle, digest-based trust,
//! the cross-workspace boundary, and applicability preflight. Returns a structural
//! refusal (JSON) that the tool hands back to the agent as-is.

use std::path::Path;

use apb_core::config::program_in_path;
use apb_core::profile_store::{self, PlaybookOrigin};
use apb_core::registry::Registry;
use apb_core::schema::{Effect, NodeKind, Playbook};
use apb_core::scope::{Origin, PlaybookRef, digest_str};
use apb_core::trust::{Lifecycle, TrustStore, read_lifecycle};
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
    let effects = apb_core::effects::effective(&loaded.playbook)
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

    // Applicability preflight (spec 5.2), in the execution root (current project).
    if let Some(req) = &loaded.playbook.requires {
        check_requires(root, req, &wref.id)?;
    }

    Ok(RunPermit {
        playbook_digest: digest,
        profile_bundles,
    })
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
        if let NodeKind::AgentTask { profile, .. } = &n.kind
            && let Some(p) = profile
                .clone()
                .or_else(|| playbook.defaults.profile.clone())
        {
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
