//! Profile skill resolution and the bridge into the claude convention (spec
//! 2026-07-12, section 6.4).
//!
//! The canon is `.agents/skills/<name>/` (project) and `~/.agents/skills/<name>/`
//! (global, the ecosystem's standard location). For claude, the same skill is
//! visible via the symlink `.claude/skills/<name>` -> canon.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::profile::ProfileError;
use crate::profile::{ProfileScope, SkillRef};

/// A resolved skill path with its actual scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSkillPath {
    pub name: String,
    pub scope: ProfileScope, // Project | Global
    pub canonical_path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct ProjectConfigSkills {
    #[serde(default)]
    skills_dir: Option<String>,
}

/// Project skills directory: `skills_dir` from `.apb/config.yaml`, otherwise
/// `<root>/.agents/skills`.
pub fn project_skills_dir(root: &Path) -> PathBuf {
    let cfg = root.join(".apb/config.yaml");
    if let Ok(raw) = std::fs::read_to_string(&cfg)
        && let Ok(parsed) = serde_yaml_ng::from_str::<ProjectConfigSkills>(&raw)
        && let Some(dir) = parsed.skills_dir
    {
        let p = PathBuf::from(&dir);
        return if p.is_absolute() { p } else { root.join(p) };
    }
    root.join(".agents/skills")
}

/// Global skills directory: `~/.agents/skills`. None if HOME is not set.
pub fn global_skills_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".agents/skills"))
}

fn scope_dir(root: &Path, scope: ProfileScope) -> Option<PathBuf> {
    match scope {
        ProfileScope::Project => Some(project_skills_dir(root)),
        ProfileScope::Global => global_skills_dir(),
        ProfileScope::Auto => None,
    }
}

/// Resolves a skill per the rules in 6.4: a global profile sees only global
/// skills; a project profile sees project then global (project shadows
/// global); an explicit scope settles the question.
pub fn resolve_skill(
    root: &Path,
    profile_scope: ProfileScope,
    s: &SkillRef,
) -> Result<ResolvedSkillPath, ProfileError> {
    let candidates: Vec<ProfileScope> = match profile_scope {
        ProfileScope::Global => {
            if s.scope == ProfileScope::Project {
                return Err(ProfileError::ScopeForbidden(format!(
                    "global profile cannot use project skill `{}`",
                    s.name
                )));
            }
            vec![ProfileScope::Global]
        }
        ProfileScope::Project => match s.scope {
            ProfileScope::Project => vec![ProfileScope::Project],
            ProfileScope::Global => vec![ProfileScope::Global],
            ProfileScope::Auto => vec![ProfileScope::Project, ProfileScope::Global],
        },
        ProfileScope::Auto => vec![ProfileScope::Project, ProfileScope::Global],
    };

    // The skill name must be a single safe segment (no `/`, `..`, absolute
    // paths): otherwise `dir.join(name)` could escape the skills root and an
    // arbitrary directory could be read/removed/overwritten.
    crate::profile::validate_profile_name(&s.name)
        .map_err(|e| ProfileError::Invalid(format!("skill name `{}`: {e}", s.name)))?;

    for scope in candidates {
        if let Some(dir) = scope_dir(root, scope) {
            let cand = dir.join(&s.name);
            if cand.is_dir() {
                let canonical_path = std::fs::canonicalize(&cand)?;
                // Defense-in-depth: even with a valid name, a symlink inside
                // the skills root could lead outside it - check containment.
                let canonical_root = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
                if !canonical_path.starts_with(&canonical_root) {
                    return Err(ProfileError::ScopeForbidden(format!(
                        "skill `{}` resolves outside its skills root",
                        s.name
                    )));
                }
                return Ok(ResolvedSkillPath {
                    name: s.name.clone(),
                    scope,
                    canonical_path,
                });
            }
        }
    }
    Err(ProfileError::SkillMissing(s.name.clone()))
}

/// Idempotently bridges `<claude_parent>/<name>` -> `<skills_parent>/<name>`
/// with symlinks for each skill. Real (non-symlink) directories with the same
/// name are left untouched - a diagnostic is returned instead.
pub fn ensure_claude_bridge(skills_parent: &Path, claude_parent: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    let Ok(entries) = std::fs::read_dir(skills_parent) else {
        return notes;
    };
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let target = entry.path();
        let link = claude_parent.join(&name);
        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let points_here = std::fs::read_link(&link)
                    .map(|t| t == target)
                    .unwrap_or(false);
                if points_here {
                    // Correct bridge - idempotent, nothing to do.
                } else if std::fs::metadata(&link).is_err() {
                    // A dangling link - this is our own orphaned bridge:
                    // fix it by pointing it back at the canon.
                    let _ = std::fs::remove_file(&link);
                    if let Err(e) = make_symlink(&target, &link) {
                        notes.push(format!(
                            "could not repair dangling bridge for skill `{}`: {e}",
                            name.to_string_lossy()
                        ));
                    }
                } else {
                    // A valid link, but pointing elsewhere - not ours, leave it alone.
                    notes.push(format!(
                        "skill `{}` is bridged elsewhere in {}; left as is",
                        name.to_string_lossy(),
                        claude_parent.display()
                    ));
                }
            }
            Ok(_) => {
                notes.push(format!(
                    "skill `{}` already exists as a real entry in {}; not bridged",
                    name.to_string_lossy(),
                    claude_parent.display()
                ));
            }
            Err(_) => {
                if let Err(e) = std::fs::create_dir_all(claude_parent)
                    .and_then(|_| make_symlink(&target, &link))
                {
                    notes.push(format!(
                        "could not bridge skill `{}`: {e}",
                        name.to_string_lossy()
                    ));
                }
            }
        }
    }
    notes
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn make_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "symlink bridge is only supported on unix",
    ))
}
