//! MCP tools for managing profiles (spec 2026-07-12, section 9.1). The logic lives
//! here and in `apb_core`; the server methods are thin wrappers. Cross-workspace
//! profile mutations are forbidden in the first version (spec 9.4) - the tools work
//! only with the local `root`.

use std::path::{Path, PathBuf};

use apb_core::profile::{
    ProfileDoc, ProfileExecutor, ProfileFallback, ProfileScope, SkillRef, SoulRequirement,
};
use apb_core::profile_store::{self, PlaybookOrigin, ProfileError};
use apb_core::trust::{Kind, OriginKind, TrustStore};
use serde_json::{Value, json};

use crate::tools::ToolError;

fn scope_dir(root: &Path, scope: ProfileScope) -> Result<PathBuf, ToolError> {
    match scope {
        ProfileScope::Project => Ok(profile_store::project_dir(root)),
        ProfileScope::Global => profile_store::global_dir()
            .ok_or_else(|| ToolError::Engine("no global config dir".into())),
        ProfileScope::Auto => Err(ToolError::Engine("scope must be project or global".into())),
    }
}

fn parse_scope(s: &str) -> Result<ProfileScope, ToolError> {
    match s {
        "project" => Ok(ProfileScope::Project),
        "global" => Ok(ProfileScope::Global),
        other => Err(ToolError::Engine(format!("unknown scope `{other}`"))),
    }
}

fn origin_for(scope: ProfileScope) -> PlaybookOrigin {
    match scope {
        ProfileScope::Global => PlaybookOrigin::Global,
        _ => PlaybookOrigin::Project,
    }
}

/// Computes a profile's bundle_digest from the live skill tree (for trust).
fn compute_bundle_for(
    root: &Path,
    scope: ProfileScope,
    name: &str,
) -> Result<String, ProfileError> {
    let r = apb_core::profile::QualifiedProfileRef {
        name: name.to_string(),
        scope,
    };
    let (_loaded, _pairs, bundle) = profile_store::compute_bundle(root, origin_for(scope), &r)?;
    Ok(bundle)
}

/// A list of profiles in both scopes with bundle trust status.
pub fn profile_list(root: &Path) -> Result<Value, ToolError> {
    let mut out = Vec::new();
    let store = TrustStore::load();
    for scope in [ProfileScope::Project, ProfileScope::Global] {
        let Ok(dir) = scope_dir(root, scope) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for name in names {
            let r = apb_core::profile::QualifiedProfileRef {
                name: name.clone(),
                scope,
            };
            let Ok(loaded) = profile_store::resolve_profile(root, origin_for(scope), &r) else {
                continue;
            };
            let trusted = compute_bundle_for(root, scope, &name)
                .map(|b| store.is_approved(&b))
                .unwrap_or(false);
            out.push(json!({
                "name": name,
                "scope": profile_store::scope_str(scope),
                "description": loaded.doc.description,
                "trusted": trusted,
                "profile_digest": loaded.profile_digest,
                "agent": loaded.doc.executor.agent,
                "model": loaded.doc.executor.model,
                "skills": loaded.doc.skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            }));
        }
    }
    Ok(json!({ "profiles": out }))
}

pub fn profile_get(root: &Path, name: &str, scope: &str) -> Result<Value, ToolError> {
    // Validate the name BEFORE any path construction: `..`/`/`/an absolute path would otherwise
    // steer the read outside the profiles root.
    apb_core::profile::validate_profile_name(name).map_err(ToolError::Engine)?;
    let scope = parse_scope(scope)?;
    let r = apb_core::profile::QualifiedProfileRef {
        name: name.to_string(),
        scope,
    };
    let loaded = profile_store::resolve_profile(root, origin_for(scope), &r)
        .map_err(|e| ToolError::NotFound(e.to_string()))?;
    let bundle = compute_bundle_for(root, scope, name).ok();
    Ok(json!({
        "name": loaded.name,
        "scope": profile_store::scope_str(loaded.scope),
        "profile_yaml": loaded.profile_yaml,
        "soul_md": loaded.soul,
        "profile_digest": loaded.profile_digest,
        "bundle_digest": bundle,
    }))
}

#[derive(Default)]
pub struct ExecutorInput {
    pub agent: String,
    pub model: String,
    pub fallbacks: Vec<(String, String)>,
}

/// Everything needed to create or update a profile. Grouping the fields into a
/// single struct keeps the CLI, MCP, and HTTP surfaces (which each build one of
/// these from their own input type) calling the shared logic with named fields
/// rather than a long, transposition-prone positional argument list.
#[derive(Default)]
pub struct ProfileWrite {
    pub name: String,
    pub scope: String,
    pub description: String,
    pub soul_md: String,
    pub skills: Vec<SkillRef>,
    pub executor: ExecutorInput,
    /// The current profile_digest for an update (optimistic concurrency); `None`
    /// creates a new profile.
    pub expected_digest: Option<String>,
    pub soul_requirement: SoulRequirement,
}

/// Create/update a profile (CAS under a per-profile lock, spec 9.1).
pub fn profile_write(root: &Path, req: ProfileWrite) -> Result<Value, ToolError> {
    let ProfileWrite {
        name,
        scope,
        description,
        soul_md,
        skills,
        executor,
        expected_digest,
        soul_requirement,
    } = req;
    apb_core::profile::validate_profile_name(&name).map_err(ToolError::Engine)?;
    let scope_enum = parse_scope(&scope)?;
    let parent = scope_dir(root, scope_enum)?;
    std::fs::create_dir_all(&parent).map_err(|e| ToolError::Engine(e.to_string()))?;

    let doc = ProfileDoc {
        name: name.clone(),
        description,
        executor: ProfileExecutor {
            agent: executor.agent,
            model: executor.model,
            fallbacks: executor
                .fallbacks
                .into_iter()
                .map(|(agent, model)| ProfileFallback { agent, model })
                .collect(),
        },
        soul: soul_requirement,
        skills,
    };
    let yaml = serde_yaml_ng::to_string(&doc).map_err(|e| ToolError::Engine(e.to_string()))?;

    // Validation: the agent is known (builtin or config) - both the primary and EVERY
    // fallback; an unknown agent is a refusal, not a warning (spec 9.1).
    // A broken global config is NOT swallowed into a default: otherwise writing the profile
    // (and the auto-approve) would succeed, and a later run would load that
    // same config strictly and fail; for a custom agent it would produce a false
    // `no known invocation`. We return the load error to the caller before writing.
    let global = apb_core::config::GlobalConfig::load()
        .map_err(|e| ToolError::Engine(format!("global config is invalid: {e}")))?;
    let warnings: Vec<String> = Vec::new();
    let mut agents = vec![&doc.executor.agent];
    agents.extend(doc.executor.fallbacks.iter().map(|f| &f.agent));
    for agent in agents {
        if apb_engine::invocation::spec_for(agent, &global).is_err() {
            return Err(ToolError::Engine(format!(
                "agent `{agent}` has no known invocation"
            )));
        }
    }
    for s in &doc.skills {
        if apb_core::skills::resolve_skill(root, scope_enum, s).is_err() {
            return Err(ToolError::Engine(format!(
                "skill `{}` not found in scope",
                s.name
            )));
        }
    }

    let dir = parent.join(&name);
    // CAS under the scope directory's lock: create requires absence, update -
    // a match between expected_digest and the current content.
    let _lock = apb_core::fsutil::lock_dir(&parent, &format!("{name}.lock"))
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let exists = dir.join("profile.yaml").is_file();
    match expected_digest {
        None => {
            if exists {
                return Err(ToolError::Conflict(
                    "profile exists; pass expected_digest to update".into(),
                ));
            }
        }
        Some(expected) => {
            if !exists {
                return Err(ToolError::Conflict("profile does not exist".into()));
            }
            let cur_yaml = std::fs::read_to_string(dir.join("profile.yaml")).unwrap_or_default();
            let cur_soul = std::fs::read_to_string(dir.join("SOUL.md")).unwrap_or_default();
            let cur = apb_core::profile::profile_digest(&cur_yaml, &cur_soul);
            if cur != expected {
                return Err(ToolError::Conflict(
                    "expected_digest does not match current".into(),
                ));
            }
        }
    }

    // Publish as a whole directory (not two independent files): assemble into
    // staging, then swap under the already-held lock (writers are serialized by
    // the lock, so there is no concurrent write to the same profile). A torn read of the
    // profile.yaml/SOUL.md pair is impossible - the directory appears atomically.
    //
    // HONESTLY about the limits (see review): on UPDATE there is a brief window between
    // rename(dir->trash) and rename(staging->dir) during which a reader WITHOUT a lock
    // sees NotFound; and a crash between the two renames leaves the profile only in the
    // hidden trash. Full crash-safety (versioned dirs + an atomic pointer,
    // like the registry's `current`, or a platform atomic-exchange + reader-retry) is
    // a separate redesign of the on-disk profile layout, deferred out of this batch.
    // Staging/trash paths are unique per-call (pid + uuid) so that even bypassing
    // the lock, two writers in the same process would not share one staging dir.
    let uniq = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
    let staging = parent.join(format!(".staging-{uniq}-{name}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| ToolError::Engine(e.to_string()))?;
    apb_core::fsutil::atomic_write(&staging.join("profile.yaml"), yaml.as_bytes())
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    apb_core::fsutil::atomic_write(&staging.join("SOUL.md"), soul_md.as_bytes())
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    if dir.exists() {
        let trash = parent.join(format!(".trash-{uniq}-{name}"));
        let _ = std::fs::remove_dir_all(&trash);
        std::fs::rename(&dir, &trash).map_err(|e| ToolError::Engine(e.to_string()))?;
        if let Err(e) = std::fs::rename(&staging, &dir) {
            let _ = std::fs::rename(&trash, &dir); // rollback
            return Err(ToolError::Engine(e.to_string()));
        }
        let _ = std::fs::remove_dir_all(&trash);
    } else {
        std::fs::rename(&staging, &dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    }

    let profile_digest = apb_core::profile::profile_digest(&yaml, &soul_md);
    // Bundle from the live tree (just written) + auto-approve.
    let bundle = compute_bundle_for(root, scope_enum, &name)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let mut trust = TrustStore::load();
    let mut trust_write_failed = false;
    if trust
        .approve_kind(
            &bundle,
            &name,
            Kind::ProfileBundle,
            OriginKind::AgentGenerated,
        )
        .is_err()
    {
        trust_write_failed = true;
    }
    Ok(json!({
        "name": name,
        "scope": scope,
        "profile_digest": profile_digest,
        "bundle_digest": bundle,
        "warnings": warnings,
        "trust_write_failed": trust_write_failed,
    }))
}

/// Moving a profile between scopes: copy semantics (spec 4.2). The source
/// remains; deletion is a separate `profile_delete`.
pub fn profile_move(root: &Path, name: &str, from: &str, to: &str) -> Result<Value, ToolError> {
    apb_core::profile::validate_profile_name(name).map_err(ToolError::Engine)?;
    let from_s = parse_scope(from)?;
    let to_s = parse_scope(to)?;
    if from_s == to_s {
        return Err(ToolError::Engine("from and to scopes are the same".into()));
    }
    let src_parent = scope_dir(root, from_s)?;
    let dst_parent = scope_dir(root, to_s)?;
    std::fs::create_dir_all(&dst_parent).map_err(|e| ToolError::Engine(e.to_string()))?;

    // Both per-profile locks in a deterministic order (by parent path) -
    // a concurrent move cannot deadlock. We hold both for the whole duration of
    // read/validate/conflict/copy so the source cannot change under us.
    let lock_name = format!("{name}.lock");
    let (first, second) = if src_parent <= dst_parent {
        (&src_parent, &dst_parent)
    } else {
        (&dst_parent, &src_parent)
    };
    let _l1 = apb_core::fsutil::lock_dir(first, &lock_name)
        .map_err(|e| ToolError::Engine(e.to_string()))?;
    let _l2 = apb_core::fsutil::lock_dir(second, &lock_name)
        .map_err(|e| ToolError::Engine(e.to_string()))?;

    let src = src_parent.join(name);
    if !src.join("profile.yaml").is_file() {
        return Err(ToolError::NotFound(format!("profile `{name}` in `{from}`")));
    }
    let dst = dst_parent.join(name);
    // A move from project -> global with references to project skills is forbidden.
    if to_s == ProfileScope::Global {
        let yaml = std::fs::read_to_string(src.join("profile.yaml"))
            .map_err(|e| ToolError::Engine(e.to_string()))?;
        if let Ok(doc) = ProfileDoc::from_yaml(&yaml) {
            for s in &doc.skills {
                if s.scope == ProfileScope::Project {
                    return Err(ToolError::Engine(format!(
                        "profile references project skill `{}`; move it, drop it, or keep the profile in project scope",
                        s.name
                    )));
                }
            }
        }
    }
    if dst.exists() {
        return Err(ToolError::Engine(
            json!({ "error": "conflict", "detail": "target scope already has this profile" })
                .to_string(),
        ));
    }
    copy_dir(&src, &dst).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "name": name, "from": from, "to": to, "copied": true }))
}

/// Deleting a profile with a scan of playbook references (spec 4.2).
pub fn profile_delete(
    root: &Path,
    name: &str,
    scope: &str,
    force: bool,
) -> Result<Value, ToolError> {
    apb_core::profile::validate_profile_name(name).map_err(ToolError::Engine)?;
    let scope_enum = parse_scope(scope)?;
    let dir = scope_dir(root, scope_enum)?.join(name);
    if !dir.join("profile.yaml").is_file() {
        return Err(ToolError::NotFound(format!(
            "profile `{name}` in `{scope}`"
        )));
    }
    let refs = referencing_playbooks(root, name);
    if !refs.is_empty() && !force {
        return Err(ToolError::Engine(
            json!({ "error": "referenced", "playbooks": refs, "detail": "pass force: true to delete anyway" }).to_string(),
        ));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "name": name, "scope": scope, "deleted": true }))
}

/// Finds project playbooks that reference the profile by name (across all versions).
fn referencing_playbooks(root: &Path, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let apb_dir = root.join(".apb/playbooks");
    let Ok(ids) = std::fs::read_dir(&apb_dir) else {
        return out;
    };
    for id_entry in ids.filter_map(Result::ok) {
        if !id_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let id = id_entry.file_name().to_string_lossy().to_string();
        let Ok(versions) = std::fs::read_dir(id_entry.path()) else {
            continue;
        };
        for v in versions.filter_map(Result::ok) {
            let playbook_yaml = v.path().join("playbook.yaml");
            let Ok(raw) = std::fs::read_to_string(&playbook_yaml) else {
                continue;
            };
            if let Ok(playbook) = apb_core::schema::Playbook::from_yaml(&raw)
                && playbook_references(&playbook, name)
            {
                out.push(format!("{id}@{}", v.file_name().to_string_lossy()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn playbook_references(playbook: &apb_core::schema::Playbook, name: &str) -> bool {
    use apb_core::schema::NodeKind;
    let matches = |p: &Option<apb_core::profile::QualifiedProfileRef>| {
        p.as_ref().is_some_and(|r| r.name == name)
    };
    if matches(&playbook.defaults.profile) {
        return true;
    }
    if let Some(s) = &playbook.supervisor
        && matches(&s.profile)
    {
        return true;
    }
    playbook
        .nodes
        .iter()
        .any(|n| matches!(&n.kind, NodeKind::AgentTask { profile, .. } if matches(profile)))
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Skills for profile_write from a plain list of strings (scope auto).
pub fn skill_refs(names: &[String]) -> Vec<SkillRef> {
    names
        .iter()
        .map(|n| SkillRef {
            name: n.clone(),
            scope: ProfileScope::Auto,
        })
        .collect()
}

/// Parses the SOUL requirement from a string (`any` | `native_required`, default any).
/// An unknown value is an error, not a silent `any` (spec 9.1).
pub fn parse_soul_requirement(s: Option<&str>) -> Result<SoulRequirement, String> {
    match s {
        None | Some("any") => Ok(SoulRequirement::Any),
        Some("native_required") => Ok(SoulRequirement::NativeRequired),
        Some(other) => Err(format!(
            "unknown soul requirement `{other}` (use any | native_required)"
        )),
    }
}
