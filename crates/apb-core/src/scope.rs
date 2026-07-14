//! Playbook scope, origin, and addressing (spec section 3).
//!
//! Before this module, a playbook definition's source and its execution
//! location were the same project root. This introduces the independent
//! concepts of `Scope` (where the definition is stored) and `PlaybookRef` (how
//! to reference it), as well as `digest_str` - the content fingerprint that
//! trust is bound to (spec 3.1).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

/// Content fingerprint of a definition; the basis for trust binding (spec
/// 3.1). Format is `sha256:<hex>`, so the algorithm is visible in the data
/// itself and can change without ambiguity.
pub fn digest_str(yaml: &str) -> String {
    let mut h = Sha256::new();
    h.update(yaml.as_bytes());
    format!("sha256:{:x}", h.finalize())
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
}
