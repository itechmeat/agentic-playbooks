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
    /// Whether the profile runs with the minimal agent environment (its
    /// `environment`, issue #136 item 4; historically the `hermetic` flag),
    /// snapshotted so post-start reads (retry, fallback, resume) use the run's
    /// value, not the live profile. Old manifests written before this field
    /// parse as `false` (serde default), so a run started with the old full
    /// environment keeps it on resume.
    #[serde(default)]
    pub hermetic: bool,
    /// The profile's `zcode_mode`, snapshotted like `hermetic`: the mode its
    /// zcode steps get when the run grants autonomy. Absent (and in old
    /// manifests) means `yolo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zcode_mode: Option<apb_core::profile::ZcodeMode>,
    // --- tier routing (issue #165 Part 12) ---
    /// The profile's executor tiers, lightest first, resolved at run start
    /// like the chain. Empty for a profile without tiers (and old
    /// manifests), which is never routed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<ManifestTier>,
    /// Set only on a rebind-overlay entry written by tier routing: the tier
    /// the node was routed to. A supervisor's own rebind never has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_tier: Option<String>,
    /// How many leading chain steps of a routed entry are tiers below the
    /// profile's own executor: an agent failure there goes up a tier at
    /// once instead of spending same-executor retries.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cascade: u32,
    // --- end tier routing ---
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A profile tier as a run snapshots it (issue #165 Part 12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTier {
    pub name: String,
    /// The work the tier is for (the routing question's criterion).
    #[serde(rename = "for")]
    pub for_work: String,
    /// The tier's own executor; `None` for `use: executor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<ResolvedInvocation>,
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
/// functions, and an optional call budget counted per executor attempt
/// (`connector::call::attempt_floor`).
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
    /// The decision-model settings of the run (issue #165 Part 3), present
    /// only when at least one use is above off at start: providers (ids,
    /// kinds, URLs, pinned models, data class, the key REFERENCE, never a
    /// key), per-use modes and thresholds, budget and privacy. Retry and
    /// resume read it from here, so a later edit of `decisions.yaml` does not
    /// reach a started run. Absent in older manifests and in every run on a
    /// machine without `decisions.yaml`, which therefore serializes as before.
    ///
    /// Read leniently: the settings types refuse unknown fields, so a block a
    /// newer apb wrote (a new privacy knob, a new provider kind) reads as
    /// `None` here (the layer off for the run) instead of making the whole
    /// manifest unreadable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_decisions"
    )]
    pub decisions: Option<apb_core::decisions::EffectiveDecisions>,
    // --- host execution mode (0.23.0) ---
    /// Who executes the run's agent steps, resolved once at start (see
    /// `apb_core::execution`). Absent means `cli`, so a CLI run's manifest is
    /// byte-identical to one written before the field existed. A resume keeps
    /// the mode because it is read from here, never re-resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ManifestExecution>,
    // --- end host execution mode ---
    // --- 0.24.0 irreversible consent ---
    /// Who consented to the run's irreversible effects, present only when the
    /// run's tree declares `irreversible` (see [`crate::consent`]). Written
    /// once at start: a resume keeps it, a sub-playbook inherits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<crate::consent::RunConsent>,
    // --- end 0.24.0 irreversible consent ---
}

// --- host execution mode (0.23.0) ---
/// The execution block of a run manifest. `mode` is kept as the raw string so
/// a mode a newer apb writes does not make the whole manifest unreadable:
/// [`RunExecutionManifest::execution_mode`] refuses to drive such a run with a
/// version message instead of silently executing it in another mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestExecution {
    pub mode: String,
    /// Where the mode came from (`apb_core::execution::ExecutionSource`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The MCP client name of the session that started the run, the host
    /// every submission is attributed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// A `cli` run started by an MCP host session: an agent step whose CLI
    /// chain cannot start at all becomes a host task (`execution_fallback`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback_to_host: bool,
}

impl ManifestExecution {
    /// The block for a resolved execution; `None` for a plain `cli` run
    /// without the fallback, which keeps the manifest unchanged.
    pub fn from_resolved(r: &apb_core::execution::ResolvedExecution) -> Option<Self> {
        (r.mode != apb_core::execution::ExecutionMode::Cli || r.fallback_to_host).then(|| Self {
            mode: r.mode.as_str().to_string(),
            source: Some(r.source.as_str().to_string()),
            client: r.client.clone(),
            fallback_to_host: r.fallback_to_host,
        })
    }
}

impl RunExecutionManifest {
    /// The run's execution mode. An unknown mode (written by a newer apb) is
    /// an error naming the version mismatch: a binary that does not know the
    /// mode must not drive the run in another one.
    pub fn execution_mode(&self) -> Result<apb_core::execution::ExecutionMode, EngineError> {
        match &self.execution {
            None => Ok(apb_core::execution::ExecutionMode::Cli),
            Some(e) => apb_core::execution::ExecutionMode::parse(&e.mode).ok_or_else(|| {
                EngineError::Conflict(format!(
                    "the run manifest names execution mode `{}`, which this apb {} does not know; it was written by a newer apb: upgrade apb and retry",
                    e.mode,
                    env!("CARGO_PKG_VERSION"),
                ))
            }),
        }
    }

    /// Whether the run executes its agent steps as host tasks.
    pub fn is_host_mode(&self) -> Result<bool, EngineError> {
        Ok(self.execution_mode()? == apb_core::execution::ExecutionMode::Host)
    }

    /// Whether a `cli` step whose CLI chain cannot start becomes a host task.
    pub fn falls_back_to_host(&self) -> bool {
        self.execution.as_ref().is_some_and(|e| e.fallback_to_host)
    }

    /// The MCP client name of the host session that started the run.
    pub fn host_client(&self) -> Option<&str> {
        self.execution.as_ref().and_then(|e| e.client.as_deref())
    }
}

/// The execution mode of the run in `run_dir`: `cli` without a manifest.
pub fn run_execution_mode(
    run_dir: &Path,
) -> Result<apb_core::execution::ExecutionMode, EngineError> {
    match read(run_dir)? {
        Some(m) => m.execution_mode(),
        None => Ok(apb_core::execution::ExecutionMode::Cli),
    }
}
// --- end host execution mode ---

fn lenient_decisions<'de, D>(
    d: D,
) -> Result<Option<apb_core::decisions::EffectiveDecisions>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let v = Option::<serde_yaml_ng::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_yaml_ng::from_value(v).ok()))
}

impl RunExecutionManifest {
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
            && self.connectors.is_empty()
            && self.decisions.is_none()
            // host execution mode (0.23.0): a run without agent steps of its
            // own still passes its execution on to its sub-playbooks.
            && self.execution.is_none()
            // 0.24.0: the consent a sub-playbook inherits.
            && self.consent.is_none()
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

// --- 0.24.0 irreversible consent ---
/// Replaces the consent a run's manifest records: the one exception to the
/// write-once rule, for a resume that asked the person (a run of an
/// irreversible tree that has no valid consent, such as one started by an
/// older apb) or one that drops a consent it cannot honour. The rest of the
/// manifest is kept as read. The run-origin stamp covers the manifest, so it
/// is renewed, but only for a directory that carried a valid stamp before:
/// a run directory apb did not create here does not become one.
pub fn replace_consent(
    run_dir: &Path,
    run_id: &str,
    consent: Option<crate::consent::RunConsent>,
) -> Result<(), EngineError> {
    let was_local = apb_core::run_origin::verify(run_dir, run_id);
    let existing = read(run_dir)?;
    if existing.is_none() && consent.is_none() {
        return Ok(());
    }
    let mut manifest = existing.unwrap_or_default();
    manifest.consent = consent;
    let yaml = serde_yaml_ng::to_string(&manifest).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write_private(&manifest_path(run_dir), yaml.as_bytes())?;
    if was_local {
        apb_core::run_origin::stamp(run_dir, run_id)?;
    }
    Ok(())
}
// --- end 0.24.0 irreversible consent ---

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
    fn a_cli_manifest_carries_no_execution_block() {
        let m = RunExecutionManifest::default();
        assert!(!serde_yaml_ng::to_string(&m).unwrap().contains("execution"));
        assert_eq!(
            m.execution_mode().unwrap(),
            apb_core::execution::ExecutionMode::Cli
        );
    }

    #[test]
    fn a_host_manifest_round_trips_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let m = RunExecutionManifest {
            execution: Some(ManifestExecution {
                mode: "host".into(),
                source: Some("argument".into()),
                client: Some("some-host".into()),
                fallback_to_host: false,
            }),
            ..Default::default()
        };
        write(dir.path(), &m).unwrap();
        assert_eq!(
            run_execution_mode(dir.path()).unwrap(),
            apb_core::execution::ExecutionMode::Host
        );
    }

    #[test]
    fn an_execution_mode_from_a_newer_apb_refuses_with_a_version_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\nexecution:\n  mode: warp\n  future: 1\n",
        )
        .unwrap();
        let m = read(dir.path()).unwrap().unwrap();
        let err = m.execution_mode().unwrap_err().to_string();
        assert!(err.contains("`warp`") && err.contains("newer apb"), "{err}");
    }

    #[test]
    fn a_decisions_block_from_a_newer_apb_leaves_the_manifest_readable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            manifest_path(dir.path()),
            "profiles: []\nnode_bindings: {}\ndecisions:\n  mode: shadow\n  timeout_ms: 3000\n  providers: []\n  budget: { max_requests_per_run: 5, max_usd_per_run: 1.0 }\n  privacy: { send: [prompts], redact: true, max_state_bytes: 24000, debug_state: false, future_knob: 1 }\n  uses: {}\n",
        )
        .unwrap();
        let m = read(dir.path()).unwrap().unwrap();
        assert!(m.decisions.is_none());
    }

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

    #[test]
    fn manifest_profile_hermetic_defaults_false_and_roundtrips() {
        let mp = ManifestProfile {
            scope: "project".into(),
            name: "architect".into(),
            profile_digest: "sha256:a".into(),
            bundle_digest: "sha256:b".into(),
            soul: "role".into(),
            soul_requirement: SoulRequirement::Any,
            skills: Vec::new(),
            chain: Vec::new(),
            ephemeral: false,
            hermetic: true,
            zcode_mode: None,
            tiers: Vec::new(),
            routed_tier: None,
            cascade: 0,
        };
        let yaml = serde_yaml_ng::to_string(&mp).unwrap();
        let back: ManifestProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back, mp);
        // An older manifest without `hermetic` still parses (serde default).
        let legacy = "scope: project\nname: architect\nprofile_digest: sha256:a\nbundle_digest: sha256:b\nsoul: role\nsoul_requirement: any\nskills: []\nchain: []\n";
        let parsed: ManifestProfile = serde_yaml_ng::from_str(legacy).unwrap();
        assert!(!parsed.hermetic);
    }
}
