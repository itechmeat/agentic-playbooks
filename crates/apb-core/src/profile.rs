//! Agent profile: the single executor binding for a node (spec
//! 2026-07-12-agent-profiles, sections 3.1-3.2).
//!
//! A profile encapsulates an agent+model pair with an ordered fallback chain,
//! a delivery requirement for the role's system prompt (SOUL.md), and a set
//! of skills. A playbook node references a profile via `QualifiedProfileRef`
//! (name + scope); the profile's content lives in `profile.yaml` + `SOUL.md`,
//! and its digest is `profile_digest`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Error resolving/loading a profile and its skills. It lives in this
/// neutral types module (rather than in `profile_store`) so that `skills` can
/// use it without importing `profile_store` - otherwise a profile_store <->
/// skills cycle would close. `profile_store` re-exports it as
/// `profile_store::ProfileError`.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("profile `{0}` not found")]
    NotFound(String),
    #[error("scope not allowed: {0}")]
    ScopeForbidden(String),
    #[error("profile name `{name}` does not match directory `{dir}`")]
    NameMismatch { name: String, dir: String },
    #[error("case-fold name collision for `{0}`")]
    CaseFoldCollision(String),
    #[error("skill `{0}` not found")]
    SkillMissing(String),
    #[error("invalid profile `{0}`")]
    Invalid(String),
    #[error("content error: {0}")]
    Content(#[from] crate::content::ContentError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Scope of a profile or skill. `Auto` - resolve according to the resolution
/// rules (project then global for a project playbook; global only for a
/// global one), see spec 3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileScope {
    Project,
    Global,
    #[default]
    Auto,
}

/// A profile's requirement for SOUL delivery (spec 6.3). `Any` - any delivery
/// method; `NativeRequired` - executors without a native system-prompt
/// channel are excluded from the chain during resolution. This is NOT the
/// delivery method itself (that's `SoulDelivery` in the engine), but a
/// requirement of the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoulRequirement {
    #[default]
    Any,
    NativeRequired,
}

/// A reference to a profile: name + scope. Accepted in YAML in two forms -
/// as a string (shorthand, `scope: auto`) or as an object `{ name, scope }`.
/// Always serialized as an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualifiedProfileRef {
    pub name: String,
    pub scope: ProfileScope,
}

/// A reference to a skill: the same two-form representation as
/// `QualifiedProfileRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillRef {
    pub name: String,
    pub scope: ProfileScope,
}

/// The object form of a reference. A separate struct with
/// `deny_unknown_fields`, so that a typo in a key (e.g. `scpoe:`) is an error
/// rather than silently falling back to scope `Auto`. (`deny_unknown_fields`
/// has no effect on the untagged variant - hence the nested struct.)
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefFull {
    name: String,
    #[serde(default)]
    scope: ProfileScope,
}

/// An intermediate form for deserializing "string or object". Shared between
/// profile and skill references.
#[derive(Deserialize)]
#[serde(untagged)]
enum RefForm {
    Short(String),
    Full(RefFull),
}

impl<'de> Deserialize<'de> for QualifiedProfileRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match RefForm::deserialize(d)? {
            RefForm::Short(name) => Self {
                name,
                scope: ProfileScope::Auto,
            },
            RefForm::Full(RefFull { name, scope }) => Self { name, scope },
        })
    }
}

// A copy of the same approach for SkillRef: a macro for just two impls isn't
// worth it (YAGNI).
impl<'de> Deserialize<'de> for SkillRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match RefForm::deserialize(d)? {
            RefForm::Short(name) => Self {
                name,
                scope: ProfileScope::Auto,
            },
            RefForm::Full(RefFull { name, scope }) => Self { name, scope },
        })
    }
}

/// An executor pair: the agent and a model string in exactly that agent's
/// `--model` format.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileFallback {
    pub agent: String,
    pub model: String,
}

/// The profile's primary executor plus an ordered fallback chain. A fallback
/// is the same role, a different executor: SOUL and skills are preserved.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileExecutor {
    pub agent: String,
    pub model: String,
    #[serde(default)]
    pub fallbacks: Vec<ProfileFallback>,
}

/// The content of `profile.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDoc {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub executor: ProfileExecutor,
    #[serde(default)]
    pub soul: SoulRequirement,
    #[serde(default)]
    pub skills: Vec<SkillRef>,
}

impl ProfileDoc {
    pub fn from_yaml(s: &str) -> Result<Self, String> {
        serde_yaml_ng::from_str(s).map_err(|e| e.to_string())
    }
}

/// Profile name rules (spec 3.1): `[a-z0-9][a-z0-9-]*`, at most 64
/// characters. Matching the directory name and rejecting case-fold
/// collisions are the resolver's job (Task 3); this only validates the name
/// format itself.
pub fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("profile name is empty".into());
    }
    if name.len() > 64 {
        return Err(format!("profile name `{name}` exceeds 64 chars"));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!("profile name `{name}` must start with [a-z0-9]"));
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(format!("profile name `{name}` allows only [a-z0-9-]"));
        }
    }
    Ok(())
}

/// Digest of a profile's content (spec 3.1): sha256 of the canonical
/// concatenation of `profile.yaml` + `\0` + `SOUL.md`. A missing SOUL.md is
/// equivalent to an empty one. Format is `sha256:<hex>` (as with
/// `scope::digest_str`).
pub fn profile_digest(profile_yaml: &str, soul_md: &str) -> String {
    let mut h = Sha256::new();
    h.update(profile_yaml.as_bytes());
    h.update([0u8]);
    h.update(soul_md.as_bytes());
    format!("sha256:{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "name: architect\ndescription: d\nexecutor:\n  agent: claude\n  model: claude-opus-4-8\n  fallbacks:\n    - { agent: opencode, model: opencode/claude-opus-4-8 }\nskills:\n  - coding-standards\n  - { name: writing-plans, scope: global }\n";

    #[test]
    fn parses_profile_and_skill_ref_forms() {
        let p = ProfileDoc::from_yaml(P).unwrap();
        assert_eq!(p.executor.fallbacks.len(), 1);
        assert_eq!(
            p.skills[0],
            SkillRef {
                name: "coding-standards".into(),
                scope: ProfileScope::Auto
            }
        );
        assert_eq!(p.skills[1].scope, ProfileScope::Global);
        assert_eq!(p.soul, SoulRequirement::Any); // default
    }

    #[test]
    fn profile_ref_accepts_string_and_object() {
        let s: QualifiedProfileRef = serde_yaml_ng::from_str("architect").unwrap();
        assert_eq!(s.scope, ProfileScope::Auto);
        let o: QualifiedProfileRef =
            serde_yaml_ng::from_str("{ name: reviewer, scope: project }").unwrap();
        assert_eq!(o.scope, ProfileScope::Project);
        assert_eq!(o.name, "reviewer");
    }

    #[test]
    fn name_rules() {
        assert!(validate_profile_name("architect").is_ok());
        assert!(validate_profile_name("a1-b2").is_ok());
        assert!(validate_profile_name("Architect").is_err());
        assert!(validate_profile_name("-x").is_err());
        assert!(validate_profile_name(&"a".repeat(65)).is_err());
        assert!(validate_profile_name("").is_err());
    }

    #[test]
    fn profile_digest_stable_and_covers_soul() {
        let d1 = profile_digest(P, "role text");
        assert!(d1.starts_with("sha256:"));
        assert_eq!(d1, profile_digest(P, "role text"));
        assert_ne!(d1, profile_digest(P, "other soul"));
        assert_ne!(d1, profile_digest(P, ""));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = format!("{P}bogus: 1\n");
        assert!(ProfileDoc::from_yaml(&bad).is_err());
    }

    #[test]
    fn misspelled_scope_key_in_ref_is_rejected() {
        // A typo in the reference object's key is an error, not a silent
        // fallback to scope: auto.
        let r: Result<QualifiedProfileRef, _> =
            serde_yaml_ng::from_str("{ name: architect, scpoe: project }");
        assert!(r.is_err(), "misspelled `scpoe` must be rejected");
        let s: Result<SkillRef, _> = serde_yaml_ng::from_str("{ name: x, scpoe: global }");
        assert!(s.is_err(), "misspelled `scpoe` must be rejected for skills");
    }
}
