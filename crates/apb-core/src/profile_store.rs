//! Profile resolution by scope, and bundle trust (spec 2026-07-12, sections
//! 3.3, 4.1, 5.1).
//!
//! Profile selection is determined by filesystem content and does NOT depend
//! on trust: first we pick the object by scope, then (by calling code) we
//! check trust for its bundle. Bundle = profile + the actual content of its
//! skills, so editing any skill changes the bundle_digest and drops trust.

use std::path::{Path, PathBuf};

use crate::content::{self, TreeLimits};
use crate::profile::{ProfileDoc, ProfileScope, QualifiedProfileRef};
// ProfileError lives in the neutral `profile` type module so that `skills`
// (which uses it) does not import `profile_store` - that would close a
// profile_store <-> skills cycle (code-ranker CYC). The re-export keeps the
// external path `profile_store::ProfileError`.
pub use crate::profile::ProfileError;

/// Origin of the playbook - determines the `scope: auto` rules (spec 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybookOrigin {
    #[default]
    Project,
    Global,
}

/// A loaded profile with its actual (resolved) scope.
#[derive(Debug, Clone)]
pub struct LoadedProfile {
    pub scope: ProfileScope, // Project | Global (never Auto)
    pub name: String,
    pub dir: PathBuf,
    pub doc: ProfileDoc,
    pub profile_yaml: String,
    pub soul: String,
    pub profile_digest: String,
}

pub fn project_dir(root: &Path) -> PathBuf {
    root.join(".apb/profiles")
}

/// Directory of global profiles (`<config_dir>/profiles`). None in a
/// no-config environment.
pub fn global_dir() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("profiles"))
}

pub fn scope_str(scope: ProfileScope) -> &'static str {
    match scope {
        ProfileScope::Project => "project",
        ProfileScope::Global => "global",
        ProfileScope::Auto => "auto",
    }
}

/// Resolves a profile reference to a concrete loaded profile per the rules in 3.3.
pub fn resolve_profile(
    root: &Path,
    origin: PlaybookOrigin,
    r: &QualifiedProfileRef,
) -> Result<LoadedProfile, ProfileError> {
    // scope: project is forbidden in a global playbook (a global playbook must
    // be self-contained).
    if origin == PlaybookOrigin::Global && r.scope == ProfileScope::Project {
        return Err(ProfileError::ScopeForbidden(format!(
            "global playbook cannot reference project profile `{}`",
            r.name
        )));
    }

    let candidates: Vec<ProfileScope> = match r.scope {
        ProfileScope::Project => vec![ProfileScope::Project],
        ProfileScope::Global => vec![ProfileScope::Global],
        ProfileScope::Auto => match origin {
            PlaybookOrigin::Project => vec![ProfileScope::Project, ProfileScope::Global],
            PlaybookOrigin::Global => vec![ProfileScope::Global],
        },
    };

    for scope in candidates {
        if let Some(dir) = scope_profiles_dir(root, scope)
            && let Some(profile_dir) = find_in_scope(&dir, &r.name)?
        {
            return load_profile(scope, &r.name, &profile_dir);
        }
    }
    Err(ProfileError::NotFound(r.name.clone()))
}

fn scope_profiles_dir(root: &Path, scope: ProfileScope) -> Option<PathBuf> {
    match scope {
        ProfileScope::Project => Some(project_dir(root)),
        ProfileScope::Global => global_dir(),
        ProfileScope::Auto => None,
    }
}

/// Looks for profile `name` in a scope directory, while also detecting a
/// case-fold collision.
fn find_in_scope(scope_dir: &Path, name: &str) -> Result<Option<PathBuf>, ProfileError> {
    if !scope_dir.is_dir() {
        return Ok(None);
    }
    let target_lower = name.to_lowercase();
    let mut matches_ci = 0u32;
    let mut exact: Option<PathBuf> = None;
    for entry in std::fs::read_dir(scope_dir)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let entry_name = entry.file_name().to_string_lossy().to_string();
        if entry_name.to_lowercase() == target_lower {
            matches_ci += 1;
            if entry_name == name {
                exact = Some(entry.path());
            }
        }
    }
    if matches_ci > 1 {
        return Err(ProfileError::CaseFoldCollision(name.to_string()));
    }
    Ok(exact)
}

fn load_profile(
    scope: ProfileScope,
    name: &str,
    dir: &Path,
) -> Result<LoadedProfile, ProfileError> {
    crate::profile::validate_profile_name(name).map_err(ProfileError::Invalid)?;
    let yaml = std::fs::read_to_string(dir.join("profile.yaml"))?;
    let soul = std::fs::read_to_string(dir.join("SOUL.md")).unwrap_or_default();
    let doc = ProfileDoc::from_yaml(&yaml).map_err(ProfileError::Invalid)?;
    if doc.name != name {
        return Err(ProfileError::NameMismatch {
            name: doc.name.clone(),
            dir: name.to_string(),
        });
    }
    let profile_digest = crate::profile::profile_digest(&yaml, &soul);
    Ok(LoadedProfile {
        scope,
        name: name.to_string(),
        dir: dir.to_path_buf(),
        doc,
        profile_yaml: yaml,
        soul,
        profile_digest,
    })
}

/// Result of `compute_bundle`: the loaded profile, pairs of `("<scope>/<name>",
/// skill_digest)`, and the resulting bundle_digest.
pub type ComputedBundle = (LoadedProfile, Vec<(String, String)>, String);

/// Resolves the profile and its skills, computes the content digest of each
/// skill from the LIVE tree, and returns the bundle_digest (for the policy
/// gate; the run snapshot will recompute the digest from a copy). Pairs are
/// `("<scope>/<name>", digest)`.
pub fn compute_bundle(
    root: &Path,
    origin: PlaybookOrigin,
    r: &QualifiedProfileRef,
) -> Result<ComputedBundle, ProfileError> {
    let profile = resolve_profile(root, origin, r)?;
    let limits = TreeLimits::default();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for skill in &profile.doc.skills {
        let resolved = crate::skills::resolve_skill(root, profile.scope, skill)?;
        let digest = content::tree_digest(&resolved.canonical_path, &limits)?;
        let qualified = format!("{}/{}", scope_str(resolved.scope), resolved.name);
        pairs.push((qualified, digest));
    }
    let bundle = content::bundle_digest(&profile.profile_digest, &pairs);
    Ok((profile, pairs, bundle))
}
