//! Mid-run executor rebinding (issue #45 finding 5): a sanctioned escape hatch
//! for switching a node's profile when its bound agent service is wedged, while
//! preserving the anti-TOCTOU pinning the immutable run manifest gives.
//!
//! The original `manifest.yaml` is never mutated; a rebind is recorded in a
//! journaled overlay (`runs/<id>/rebinds.yaml`) that resolution consults AFTER
//! the manifest. The new profile is re-snapshotted and its bundle re-verified
//! against the policy gate's digest by the SAME code path run start uses
//! ([`snapshot_loaded_profile`]), so an untrusted or drifted bundle is refused
//! and the accepted binding is pinned from then on.
//!
//! Split out of `scheduler` for navigability; shares the parent module's imports
//! via `use super::*`.

use super::*;

use apb_core::profile::{ProfileScope, QualifiedProfileRef};
use serde::Deserialize;

/// The journaled rebind overlay: `node_id -> the rebound profile`, each pinned
/// at rebind time. Consulted after the immutable manifest during resolution, so
/// the manifest stays the record of what the run started with while the
/// EFFECTIVE binding for a rebound node changes for its future attempts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RebindOverlay {
    #[serde(default)]
    pub(crate) nodes: BTreeMap<String, ManifestProfile>,
}

fn overlay_path(run_dir: &Path) -> PathBuf {
    run_dir.join("rebinds.yaml")
}

pub(crate) fn read_overlay(run_dir: &Path) -> Result<RebindOverlay, EngineError> {
    let path = overlay_path(run_dir);
    if !path.is_file() {
        return Ok(RebindOverlay::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_yaml_ng::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))
}

fn write_overlay(run_dir: &Path, overlay: &RebindOverlay) -> Result<(), EngineError> {
    let yaml = serde_yaml_ng::to_string(overlay).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write(&overlay_path(run_dir), yaml.as_bytes())?;
    Ok(())
}

/// The EFFECTIVE manifest profile for a node: the rebind overlay entry if the
/// node has been rebound, otherwise the immutable manifest's binding. Every node
/// execution, cache-key, and interaction-ceiling read goes through here so a
/// rebound node picks up its new profile on the next attempt while an untouched
/// node reads straight from the manifest.
pub(crate) fn effective_for_node(
    run_dir: &Path,
    manifest: &RunExecutionManifest,
    node_id: &str,
) -> Result<Option<ManifestProfile>, EngineError> {
    if let Some(profile) = read_overlay(run_dir)?.nodes.get(node_id) {
        return Ok(Some(profile.clone()));
    }
    Ok(manifest.for_node(node_id).cloned())
}

/// The origin (project vs global) the run was started under, read from its
/// `RunProvenance` event. Drives `scope: auto` resolution of a rebind's new
/// profile exactly as run start resolves node profiles. Absent provenance (old
/// logs) defaults to project.
pub(crate) fn run_origin(run_dir: &Path) -> Result<PlaybookOrigin, EngineError> {
    for event in read_all(run_dir)? {
        if let EventPayload::RunProvenance {
            origin: Some(origin),
            ..
        } = &event.payload
        {
            return Ok(if origin == "global" {
                PlaybookOrigin::Global
            } else {
                PlaybookOrigin::Project
            });
        }
    }
    Ok(PlaybookOrigin::Project)
}

/// A rebind request handed to the drive loop from a `Control::Rebind`.
pub(crate) struct RebindCommand {
    pub(crate) node: String,
    pub(crate) profile: String,
    pub(crate) scope: ProfileScope,
    /// The bundle digest the policy gate trust-verified; the re-snapshot must
    /// match it exactly (anti-TOCTOU pin).
    pub(crate) bundle: String,
    pub(crate) reason: Option<String>,
}

/// Outcome of resolving a rebind: a fully snapshotted, bundle-verified profile,
/// or a machine-readable refusal reason (journaled, non-fatal).
enum RebindOutcome {
    Bound(Box<ManifestProfile>),
    Rejected(String),
}

/// Applies a `Control::Rebind`: re-runs the run-start verification for the new
/// bundle, and on success writes the overlay + journals `ProfileRebound`. A
/// resolution or drift failure is journaled `RebindRejected` and is non-fatal
/// (the node keeps its old binding), mirroring a rejected patch. Only genuine
/// I/O faults propagate.
pub(crate) fn apply_rebind(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    command: RebindCommand,
) -> Result<(), EngineError> {
    match resolve_rebind(root, run_dir, &command)? {
        RebindOutcome::Bound(profile) => {
            let mut overlay = read_overlay(run_dir)?;
            overlay
                .nodes
                .insert(command.node.clone(), (*profile).clone());
            write_overlay(run_dir, &overlay)?;
            log.append(EventPayload::ProfileRebound {
                node: command.node,
                profile: profile.key(),
                bundle: profile.bundle_digest,
                reason: command.reason.unwrap_or_default(),
            })?;
        }
        RebindOutcome::Rejected(reason) => {
            log.append(EventPayload::RebindRejected {
                node: command.node,
                reason,
            })?;
        }
    }
    Ok(())
}

fn resolve_rebind(
    root: &Path,
    run_dir: &Path,
    command: &RebindCommand,
) -> Result<RebindOutcome, EngineError> {
    // The run must have a manifest and the node must be an executor-bound node:
    // rebinding a node that never had a profile binding is meaningless.
    let Some(manifest) = crate::manifest::read(run_dir)? else {
        return Ok(RebindOutcome::Rejected(
            "run has no execution manifest to rebind against".into(),
        ));
    };
    if manifest.for_node(&command.node).is_none() {
        return Ok(RebindOutcome::Rejected(format!(
            "node `{}` has no profile binding to rebind",
            command.node
        )));
    }

    let origin = run_origin(run_dir)?;
    let global = GlobalConfig::load().map_err(EngineError::Invalid)?;
    let pref = QualifiedProfileRef {
        name: command.profile.clone(),
        scope: command.scope,
    };
    let loaded = match profile_store::resolve_profile(root, origin, &pref) {
        Ok(loaded) => loaded,
        Err(e) => {
            return Ok(RebindOutcome::Rejected(format!(
                "profile `{}` did not resolve: {e}",
                command.profile
            )));
        }
    };
    let scope = profile_store::scope_str(loaded.scope).to_string();
    let name = loaded.name.clone();
    let key = format!("{scope}/{name}");

    // If this profile is already snapshotted for the run (another node binds it,
    // or a prior rebind used it), reuse that pinned snapshot rather than writing
    // over an existing dest - but only when its pinned bundle still matches the
    // gate's digest. A mismatch is drift and is refused.
    let overlay = read_overlay(run_dir)?;
    if let Some(existing) = manifest
        .profiles
        .iter()
        .find(|p| p.key() == key)
        .or_else(|| overlay.nodes.values().find(|p| p.key() == key))
    {
        if existing.bundle_digest == command.bundle {
            return Ok(RebindOutcome::Bound(Box::new(existing.clone())));
        }
        return Ok(RebindOutcome::Rejected(format!(
            "profile `{key}` bundle drifted since the rebind gate"
        )));
    }

    // Fresh key: clear any stale partial snapshot left by a prior rejected
    // attempt (the key is absent from both the manifest and the overlay, so this
    // dir belongs to no live binding), then re-snapshot and re-verify by the same
    // path run start uses.
    let dest = run_dir.join("profiles").join(&scope).join(&name);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    match snapshot_loaded_profile(
        root,
        &global,
        run_dir,
        &scope,
        &name,
        &key,
        &loaded,
        None,
        ExpectedBundle::Verify(Some(&command.bundle)),
    ) {
        Ok(profile) => Ok(RebindOutcome::Bound(Box::new(profile))),
        // A resolution/verification failure (bundle drift, a skill that no longer
        // resolves) is a refusal, not a run-fatal error - mirror a rejected patch.
        Err(EngineError::Invalid(reason)) => Ok(RebindOutcome::Rejected(reason)),
        Err(other) => Err(other),
    }
}
