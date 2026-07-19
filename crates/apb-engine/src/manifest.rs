//! RunExecutionManifest (spec 2026-07-12, section 3.6): the immutable
//! snapshot of a run's profiles, skills, and invocations.
//!
//! Written once at start. All profile/SOUL/skill reads after start (retry,
//! fallback, resume, server restart) come from the run snapshot and this
//! manifest, not from live directories - editing a profile/skill after start
//! does not affect the run. The binary fingerprint in the chain lets resume
//! catch a swapped executable (environment drift).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use apb_core::profile::SoulRequirement;
use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::invocation::ResolvedInvocation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestSkill {
    pub name: String,
    pub scope: String,
    pub digest: String,
}

/// A profile recorded in the manifest: identity + digests + role content +
/// executor chain (already filtered by SOUL requirement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestProfile {
    pub scope: String,
    pub name: String,
    pub profile_digest: String,
    pub bundle_digest: String,
    pub soul: String,
    pub soul_requirement: SoulRequirement,
    pub skills: Vec<ManifestSkill>,
    pub chain: Vec<ResolvedInvocation>,
    /// Run-local ephemeral executor override (completion-plan Task 4): the
    /// chain is replaced by a single ad-hoc invocation (agent+model), while
    /// SOUL and skills are taken from the node's profile. Such an entry is
    /// per-node (not deduplicated by `<scope>/<name>`) and is excluded from
    /// bundle trust (the executor is ad-hoc, not part of the profile).
    #[serde(default)]
    pub ephemeral: bool,
}

impl ManifestProfile {
    /// The `<scope>/<name>` key - the profile's identity across all surfaces (spec 3.3).
    pub fn key(&self) -> String {
        format!("{}/{}", self.scope, self.name)
    }
}

/// A connector account recorded in the manifest: identity + non-secret field
/// values + the env-var names holding secret field values + its content
/// digest. Secret fields never carry the raw secret value into the manifest -
/// `env` maps the field name to the ENV VAR NAME that holds it at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestAccount {
    pub name: String,
    pub default: bool,
    /// Non-secret field values.
    pub fields: BTreeMap<String, String>,
    /// Secret field name -> ENV VAR NAME (not the secret value itself).
    pub env: BTreeMap<String, String>,
    /// Secret field name -> the shell command line that produces the secret
    /// at call time (spec 4.1), never the secret value itself. Empty for an
    /// env-sourced or non-secret account; disjoint from `env` (a secret field
    /// is exactly one of the two forms).
    #[serde(default)]
    pub cmd: BTreeMap<String, String>,
    /// `account_digest`.
    pub digest: String,
}

/// A connector recorded in the manifest: identity + digest + the accounts
/// snapshotted for this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestConnector {
    pub name: String,
    pub digest: String,
    pub accounts: Vec<ManifestAccount>,
}

/// A single node's grant against one connector: which accounts, which
/// functions, and an optional per-run call budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestConnectorGrant {
    pub connector: String,
    /// Account names granted to this node.
    pub accounts: Vec<String>,
    pub functions: Vec<String>,
    pub max_calls: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunExecutionManifest {
    /// Profiles used, one per unique `(scope, name)`.
    pub profiles: Vec<ManifestProfile>,
    /// Binding from `node_id` (or `supervisor`) -> profile key `<scope>/<name>`.
    pub node_bindings: BTreeMap<String, String>,
    /// Connectors used, one per unique connector name.
    #[serde(default)]
    pub connectors: Vec<ManifestConnector>,
    /// Binding from `node_id` -> the grants that node holds.
    #[serde(default)]
    pub connector_grants: BTreeMap<String, Vec<ManifestConnectorGrant>>,
}

impl RunExecutionManifest {
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty() && self.connectors.is_empty()
    }

    pub fn for_node(&self, node_id: &str) -> Option<&ManifestProfile> {
        let key = self.node_bindings.get(node_id)?;
        self.profiles.iter().find(|p| &p.key() == key)
    }

    pub fn connector(&self, name: &str) -> Option<&ManifestConnector> {
        self.connectors.iter().find(|c| c.name == name)
    }

    pub fn grants_for(&self, node_id: &str) -> &[ManifestConnectorGrant] {
        self.connector_grants
            .get(node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

fn manifest_path(run_dir: &Path) -> std::path::PathBuf {
    run_dir.join("manifest.yaml")
}

/// Writes the manifest exactly once, crash-safe. We write the FULL content
/// to a temp file (0600 on unix), fsync it, then publish it at the target
/// path via a hard link: `link()` is atomic and does NOT overwrite an
/// existing path (no-clobber - a concurrent/repeat writer gets
/// AlreadyExists), and by the time of publishing the file is already intact.
/// Finally we fsync the directory so the directory-entry write survives a
/// crash. An interruption BEFORE the link leaves only the temp file (cleaned
/// up by the next writer), never an empty/corrupt immutable manifest at the
/// target path (spec 3.6).
pub fn write(run_dir: &Path, manifest: &RunExecutionManifest) -> Result<(), EngineError> {
    let path = manifest_path(run_dir);
    let dir = path.parent().unwrap_or(run_dir);
    std::fs::create_dir_all(dir)?;
    let yaml = serde_yaml_ng::to_string(manifest).map_err(|e| EngineError::Yaml(e.to_string()))?;

    let tmp = dir.join(format!(".manifest.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    {
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(yaml.as_bytes())?;
        f.sync_all()?;
    }

    let publish = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp); // the temp file is no longer needed either way
    match publish {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(EngineError::Invalid(
                "run manifest already exists (immutable)".into(),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    // fsync the directory: the manifest's directory entry must survive a crash.
    if let Ok(dir_f) = std::fs::File::open(dir) {
        let _ = dir_f.sync_all();
    }
    Ok(())
}

/// Reads the run manifest. `Ok(None)` means there is no manifest (the
/// executor path without profiles).
pub fn read(run_dir: &Path) -> Result<Option<RunExecutionManifest>, EngineError> {
    let path = manifest_path(run_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let m = serde_yaml_ng::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))?;
    Ok(Some(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_account_cmd_defaults_to_empty_and_roundtrips() {
        let acct = ManifestAccount {
            name: "a".to_string(),
            default: false,
            fields: BTreeMap::from([("base_url".to_string(), "https://x".to_string())]),
            env: BTreeMap::new(),
            cmd: BTreeMap::from([("token".to_string(), "gh auth token".to_string())]),
            digest: "sha256:x".to_string(),
        };
        let yaml = serde_yaml_ng::to_string(&acct).unwrap();
        let back: ManifestAccount = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back, acct);
        // An older manifest without `cmd` still parses (serde default).
        let legacy = "name: a\ndefault: false\nfields: {}\nenv: {}\ndigest: sha256:x\n";
        let parsed: ManifestAccount = serde_yaml_ng::from_str(legacy).unwrap();
        assert!(parsed.cmd.is_empty());
    }
}
