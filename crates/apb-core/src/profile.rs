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
    /// What the executor loads of the operator's own agent setup (see
    /// [`AgentEnvironment`]). Absent means [`AgentEnvironment::Minimal`];
    /// `full` is the explicit opt-in to the whole personal environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<AgentEnvironment>,
    /// Deprecated, read only so older profile.yaml files still parse. It no
    /// longer selects anything: every write path used to emit `hermetic:
    /// false` whether or not anyone chose it, so it cannot stand for the
    /// full-environment opt-in (that is `environment: full`), and `true` is
    /// what the default does anyway. New writes omit it.
    #[serde(default, skip_serializing)]
    pub hermetic: Option<bool>,
    /// The ZCode permission mode the profile's zcode steps get in a run that
    /// grants autonomy (see [`ZcodeMode`]). Absent means `yolo`, the
    /// historical grant; only zcode reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zcode_mode: Option<ZcodeMode>,
}

impl ProfileDoc {
    /// The environment the executor runs with: the declared one, else
    /// [`AgentEnvironment::Minimal`].
    pub fn environment(&self) -> AgentEnvironment {
        self.environment.unwrap_or_default()
    }
}

/// What an executor loads of the operator's own agent setup (issue #136
/// item 4). A node agent is a batch worker: loading every plugin, MCP server,
/// user skill and personal instruction file the operator installed costs
/// thousands of tokens per spawn and changes what the node does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEnvironment {
    /// The default. claude gets apb's own settings (no hooks, no plugins),
    /// only the project and local setting sources (no user CLAUDE.md, user
    /// skills or user settings), and only the MCP servers apb passes itself.
    /// The project's own CLAUDE.md and `.claude/skills` still load, and the
    /// profile's declared skills are delivered.
    #[default]
    Minimal,
    /// The operator's whole personal environment, as an interactive session
    /// would have it. For a profile that depends on a user-scope plugin,
    /// skill or MCP server.
    Full,
}

impl AgentEnvironment {
    /// The spelling in profile.yaml and on every surface.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentEnvironment::Minimal => "minimal",
            AgentEnvironment::Full => "full",
        }
    }

    /// Parses `minimal` / `full`.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "minimal" => Ok(AgentEnvironment::Minimal),
            "full" => Ok(AgentEnvironment::Full),
            other => Err(format!(
                "environment `{other}`: expected `minimal` or `full`"
            )),
        }
    }

    /// The environment a write surface asked for: `environment` wins; the
    /// deprecated `hermetic` flag maps `true` to minimal and `false` to full
    /// (on a write surface `false` is a caller's explicit choice). `None`
    /// when neither was given, which keeps the stored value on an update.
    pub fn from_surface(
        environment: Option<&str>,
        hermetic: Option<bool>,
    ) -> Result<Option<Self>, String> {
        match (environment, hermetic) {
            (Some(e), _) => Self::parse(e).map(Some),
            (None, Some(true)) => Ok(Some(AgentEnvironment::Minimal)),
            (None, Some(false)) => Ok(Some(AgentEnvironment::Full)),
            (None, None) => Ok(None),
        }
    }
}

/// ZCode's `--mode` for a run that grants autonomy. In a headless run every
/// approval request is denied, so the mode is the whole permission set:
/// `yolo` allows everything (files, shell, network), `edit` allows file edits
/// and nothing that needs an approval (no shell commands). A run that grants
/// no autonomy always runs zcode in `build`, which refuses writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZcodeMode {
    #[default]
    Yolo,
    Edit,
}

impl ZcodeMode {
    /// The value ZCode's `--mode` flag takes.
    pub fn as_str(self) -> &'static str {
        match self {
            ZcodeMode::Yolo => "yolo",
            ZcodeMode::Edit => "edit",
        }
    }

    /// Parses `yolo` / `edit` (the surfaces' spelling).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "yolo" => Ok(ZcodeMode::Yolo),
            "edit" => Ok(ZcodeMode::Edit),
            other => Err(format!("zcode_mode `{other}`: expected `edit` or `yolo`")),
        }
    }
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
    validate_slug("profile name", name)
}

/// The apb slug rule (spec 3.1), shared by every identifier the user types
/// and apb later joins into a path or a URL segment: `[a-z0-9][a-z0-9-]*`,
/// at most 64 characters. `label` names the thing being validated so the
/// message reads naturally on each surface (profile name, suggestion
/// pattern). Keeping one implementation means a slug accepted by one surface
/// can never be rejected as unroutable by another.
pub(crate) fn validate_slug(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} is empty"));
    }
    if value.len() > 64 {
        return Err(format!("{label} `{value}` exceeds 64 chars"));
    }
    let mut chars = value.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!("{label} `{value}` must start with [a-z0-9]"));
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-' {
            return Err(format!("{label} `{value}` allows only [a-z0-9-]"));
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
    format!("sha256:{}", crate::content::hex_lower(&h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "name: architect\ndescription: d\nexecutor:\n  agent: claude\n  model: claude-opus-5-5\n  fallbacks:\n    - { agent: opencode, model: opencode/claude-opus-5-5 }\nskills:\n  - coding-standards\n  - { name: writing-plans, scope: global }\n";

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

    /// Issue #136 item 4: the cheap environment is the default, the full one
    /// an explicit opt-in, and the deprecated `hermetic` key (which every old
    /// write path emitted as `false`) opts into nothing.
    #[test]
    fn environment_defaults_to_minimal_and_full_is_explicit() {
        let p = ProfileDoc::from_yaml(P).unwrap();
        assert_eq!(p.environment(), AgentEnvironment::Minimal);
        for legacy in ["hermetic: false", "hermetic: true"] {
            let doc = ProfileDoc::from_yaml(&format!("{P}{legacy}\n")).unwrap();
            assert_eq!(doc.environment(), AgentEnvironment::Minimal, "{legacy}");
            let written = serde_yaml_ng::to_string(&doc).unwrap();
            assert!(
                !written.contains("hermetic"),
                "a rewrite drops the key: {written}"
            );
        }
        let full = ProfileDoc::from_yaml(&format!("{P}environment: full\n")).unwrap();
        assert_eq!(full.environment(), AgentEnvironment::Full);
        assert!(ProfileDoc::from_yaml(&format!("{P}environment: all\n")).is_err());
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
