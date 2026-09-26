//! Playbook scope, origin, and addressing (spec section 3).
//!
//! Before this module, a playbook definition's source and its execution
//! location were the same project root. This introduces the independent
//! concepts of `Scope` (where the definition is stored) and `PlaybookRef` (how
//! to reference it), as well as `digest_str` - the content fingerprint that
//! trust is bound to (spec 3.1).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::profile::ProfileScope;

/// Storage scope for a definition (spec 5.1): the project's
/// `.apb/playbooks/` or the global `<config_dir>/playbooks/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Project,
    Global,
}

/// Origin of a definition. `Project { workspace_id: None }` means "the
/// caller's current workspace" - the id is filled in by calling code via the
/// registry (spec 3, 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Global,
    Project {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
    },
}

impl Origin {
    pub fn scope(&self) -> Scope {
        match self {
            Origin::Global => Scope::Global,
            Origin::Project { .. } => Scope::Project,
        }
    }
}

/// Address of a playbook definition (spec 3). `version: None` means the
/// current version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookRef {
    pub origin: Origin,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// The `project`/`global` label of an origin. The single source for turning
/// an `Origin` into the scope string the gate and engine both use (review I2).
pub fn origin_scope_label(origin: &Origin) -> &'static str {
    match origin {
        Origin::Global => "global",
        Origin::Project { .. } => "project",
    }
}

/// The ordered candidate origins in which to resolve a scoped reference, the
/// shared semantics of the policy gate (`collect_children`) and the engine
/// (`run_playbook_node`) (review M4): an explicit `Global` pins global; an
/// explicit `Project` pins the current project; `Auto` prefers the parent's
/// origin, then falls back to global (a global parent has no project to fall
/// back to, so it stays global-only). The first candidate in which the child
/// resolves wins.
pub fn scope_candidates(scope: ProfileScope, parent: &Origin) -> Vec<Origin> {
    match scope {
        ProfileScope::Global => vec![Origin::Global],
        ProfileScope::Project => vec![Origin::Project { workspace_id: None }],
        ProfileScope::Auto => match parent {
            Origin::Global => vec![Origin::Global],
            Origin::Project { .. } => {
                vec![Origin::Project { workspace_id: None }, Origin::Global]
            }
        },
    }
}

/// Content fingerprint of a definition; the basis for trust binding (spec
/// 3.1). Format is `sha256:<hex>`, so the algorithm is visible in the data
/// itself and can change without ambiguity.
pub fn digest_str(yaml: &str) -> String {
    let mut h = Sha256::new();
    h.update(yaml.as_bytes());
    format!("sha256:{}", crate::content::hex_lower(&h.finalize()))
}

/// The trust digest of a playbook version: what an approval covers and what a
/// run permit pins. It covers `playbook.yaml` AND every file under the
/// version's `scripts/` (the code script nodes and `success_check` run), so a
/// changed script, or the same YAML shipped with other scripts, is content
/// nobody approved.
///
/// A version without scripts (no `scripts/` directory, or an empty one)
/// digests to exactly [`digest_str`] of its YAML, so approvals recorded before
/// scripts were covered keep holding for those playbooks. With scripts the
/// digest is a domain-tagged hash over the YAML and the
/// [`crate::content::tree_digest`] of `scripts/` (sorted, length-prefixed,
/// symlinks by their in-tree target; a symlink leaving the tree or an
/// unsupported entry is an error, never a silently partial digest).
///
/// `version_dir` is the directory holding `scripts/`: the definition's
/// `<id>/<version>` directory, or a run directory holding the copy a run
/// starts with.
pub fn definition_digest(
    yaml: &str,
    version_dir: &std::path::Path,
) -> Result<String, crate::content::ContentError> {
    let scripts = version_dir.join("scripts");
    let meta = match std::fs::symlink_metadata(&scripts) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(digest_str(yaml)),
        Err(e) => return Err(e.into()),
    };
    if !meta.file_type().is_dir() {
        return Err(crate::content::ContentError::Unsupported(scripts));
    }
    if std::fs::read_dir(&scripts)?.next().is_none() {
        return Ok(digest_str(yaml));
    }
    let tree = crate::content::tree_digest(&scripts, &crate::content::TreeLimits::default())?;
    let mut h = Sha256::new();
    h.update(b"apb-definition-v1\0");
    for field in [yaml.as_bytes(), tree.as_bytes()] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field);
    }
    Ok(format!(
        "sha256:{}",
        crate::content::hex_lower(&h.finalize())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_and_prefixed() {
        let d = digest_str("id: x\n");
        assert!(d.starts_with("sha256:"));
        assert_eq!(d, digest_str("id: x\n"));
        assert_ne!(d, digest_str("id: y\n"));
    }

    #[test]
    fn playbook_ref_roundtrips_json() {
        let r = PlaybookRef {
            origin: Origin::Project {
                workspace_id: Some("ws-1".into()),
            },
            id: "review".into(),
            version: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: PlaybookRef = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn global_origin_omits_workspace_field() {
        let r = PlaybookRef {
            origin: Origin::Global,
            id: "x".into(),
            version: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"kind\":\"global\""));
        assert!(!s.contains("workspace_id"));
    }

    #[test]
    fn origin_maps_to_scope() {
        assert_eq!(Origin::Global.scope(), Scope::Global);
        assert_eq!(
            Origin::Project { workspace_id: None }.scope(),
            Scope::Project
        );
    }

    #[test]
    fn origin_scope_label_matches_scope() {
        assert_eq!(origin_scope_label(&Origin::Global), "global");
        assert_eq!(
            origin_scope_label(&Origin::Project { workspace_id: None }),
            "project"
        );
        assert_eq!(
            origin_scope_label(&Origin::Project {
                workspace_id: Some("ws".into())
            }),
            "project"
        );
    }

    #[test]
    fn scope_candidates_explicit_scopes_pin_one_origin() {
        let project = Origin::Project { workspace_id: None };
        // Explicit scopes ignore the parent entirely.
        assert_eq!(
            scope_candidates(ProfileScope::Global, &project),
            vec![Origin::Global]
        );
        assert_eq!(
            scope_candidates(ProfileScope::Global, &Origin::Global),
            vec![Origin::Global]
        );
        assert_eq!(
            scope_candidates(ProfileScope::Project, &Origin::Global),
            vec![Origin::Project { workspace_id: None }]
        );
        assert_eq!(
            scope_candidates(ProfileScope::Project, &project),
            vec![Origin::Project { workspace_id: None }]
        );
    }

    #[test]
    fn scope_candidates_auto_follows_parent_then_global() {
        // Auto under a project parent: project first, then global.
        assert_eq!(
            scope_candidates(
                ProfileScope::Auto,
                &Origin::Project {
                    workspace_id: Some("ws".into())
                }
            ),
            vec![Origin::Project { workspace_id: None }, Origin::Global]
        );
        // Auto under a global parent: global only (no project to fall back to).
        assert_eq!(
            scope_candidates(ProfileScope::Auto, &Origin::Global),
            vec![Origin::Global]
        );
    }
}
