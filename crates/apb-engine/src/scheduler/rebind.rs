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
    yaml_serde::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))
}

pub(crate) fn write_overlay(run_dir: &Path, overlay: &RebindOverlay) -> Result<(), EngineError> {
    let yaml = yaml_serde::to_string(overlay).map_err(|e| EngineError::Yaml(e.to_string()))?;
    apb_core::fsutil::atomic_write_under(run_dir, &overlay_path(run_dir), yaml.as_bytes())?;
    Ok(())
}

/// Whether any profile of the run at `run_dir` restricts its agent's
/// environment: a manifest entry (node or supervisor) or a rebound one under
/// `process_env: allowlist`. An unreadable manifest or overlay counts as
/// restricted, which is the safe side.
pub(crate) fn run_uses_allowlist(run_dir: &Path) -> bool {
    let allowlisted = |p: &ManifestProfile| {
        p.process_env
            .as_ref()
            .is_some_and(apb_core::process_env::ProcessEnvPolicy::is_allowlist)
    };
    let in_manifest = match crate::manifest::read(run_dir) {
        Ok(Some(m)) => m.profiles.iter().any(allowlisted),
        Ok(None) => false,
        Err(_) => true,
    };
    in_manifest || read_overlay(run_dir).map_or(true, |o| o.nodes.values().any(allowlisted))
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
    Ok(
        match crate::run_origin::origin_of_events(&read_all(run_dir)?) {
            crate::run_origin::RunScope::Global => PlaybookOrigin::Global,
            crate::run_origin::RunScope::Project | crate::run_origin::RunScope::Unreadable => {
                PlaybookOrigin::Project
            }
        },
    )
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
    let outcome = match resolve_rebind(root, run_dir, &command)? {
        RebindOutcome::Bound(profile) => match skip_refusal(root, run_dir, &command.node, &profile)
        {
            Some(reason) => RebindOutcome::Rejected(reason),
            None => RebindOutcome::Bound(profile),
        },
        rejected => rejected,
    };
    match outcome {
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

/// Why a rebind of `node` to `candidate` is refused because the node
/// settled items by its list skip in this run: the work the candidate gives
/// the node differs from the work it is bound to now, or either cannot be
/// computed. `None` for a node that skipped nothing.
fn skip_refusal(
    root: &Path,
    run_dir: &Path,
    node: &str,
    candidate: &ManifestProfile,
) -> Option<String> {
    use crate::list::skip::{WORK_CHANGED, WORK_UNKNOWN};
    // A journal that does not read cannot say whether the node skipped:
    // refused, never let through.
    let events = match read_all(run_dir) {
        Ok(ev) => ev,
        Err(e) => {
            return Some(format!(
                "{WORK_UNKNOWN}: node {node}: the run's journal does not read, so whether it settled items by its skip cannot be told: {e}"
            ));
        }
    };
    let skipped = events.iter().any(|e| {
        matches!(&e.payload, EventPayload::ListItemSettled { node: n, skipped_from: Some(_), .. } if n == node)
    });
    if !skipped {
        return None;
    }
    let unknown = |why: String| {
        format!(
            "{WORK_UNKNOWN}: node {node} settled items by its skip in this run, and its work cannot be compared: {why}"
        )
    };
    let playbook = match std::fs::read_to_string(run_dir.join("playbook.yaml"))
        .map_err(|e| e.to_string())
        .and_then(|y| Playbook::from_yaml(&y).map_err(|e| e.to_string()))
    {
        Ok(p) => p,
        Err(why) => return Some(unknown(why)),
    };
    let Some(node_def) = playbook.node(node) else {
        return Some(unknown(format!("node {node} is not in the definition")));
    };
    let cfg = match crate::run_config::read_run_config(run_dir) {
        Ok(c) => c,
        Err(e) => return Some(unknown(e.to_string())),
    };
    let decisions = crate::decision::DecisionRunner::for_run(root, run_dir, &events, &[]);
    let state = crate::state::RunState::fold(&events);
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let workers = node_def.list().map_or(1, |l| {
        l.workers(super::cache::resolved_max_parallel(&playbook, &cfg))
    });
    let exec = super::cache::visit_execution(
        &playbook,
        node_def,
        &super::cache::tree_dir(&state, run_dir, root),
        run_dir,
        &run_id,
        &state,
        &cfg,
        workers,
    );
    let src = super::cache::WorkSource {
        run_dir,
        events: &events,
        playbook: &playbook,
        node: node_def,
        cfg: &cfg,
        decisions: decisions.as_ref().map(|d| d.settings()),
        exec: &exec,
    };
    let now = super::cache::work_inputs(&src).map(|w| super::cache::work_fingerprint(&w));
    let then = super::cache::work_with_profile(&src, candidate)
        .map(|w| super::cache::work_fingerprint(&w));
    match (now, then) {
        (Ok(a), Ok(b)) if a == b => None,
        (Ok(_), Ok(_)) => Some(format!(
            "{WORK_CHANGED}: node {node} settled items by its skip in this run; a rebind may not change its work"
        )),
        (Err(why), _) | (_, Err(why)) => Some(unknown(why)),
    }
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

/// Sets or clears a node's tier-routing entry in the rebind overlay (issue
/// #165 Part 14.5): routing swaps the executor through the same overlay a
/// supervisor rebind writes, never through a third path. Returns whether the
/// overlay changed (a resumed execution that replays the same route writes
/// nothing).
pub(crate) fn set_routed(
    run_dir: &Path,
    node: &str,
    entry: Option<ManifestProfile>,
) -> Result<bool, EngineError> {
    let mut overlay = read_overlay(run_dir)?;
    let current = overlay.nodes.get(node);
    let changed = match (&entry, current) {
        (Some(e), Some(c)) => e != c,
        (Some(_), None) => true,
        (None, Some(c)) => c.routed_tier.is_some(),
        (None, None) => false,
    };
    if !changed {
        return Ok(false);
    }
    match entry {
        Some(e) => {
            overlay.nodes.insert(node.to_string(), e);
        }
        None => {
            overlay.nodes.remove(node);
        }
    }
    write_overlay(run_dir, &overlay)?;
    Ok(true)
}

#[cfg(test)]
mod skip_tests {
    use super::*;

    const PLAYBOOK: &str = "schema: 2\nid: lst\nname: List\nversion: 1.0.0\ndefaults: { profile: main }\nnodes:\n  - { id: start, type: start }\n  - id: work\n    type: agent_task\n    prompt: \"Handle one item.\"\n    list:\n      items: { inline: [ { id: a } ] }\n      skip: { passed_in: previous_success }\n  - { id: done, type: finish, outcome: success }\nedges:\n  - { from: start, to: work }\n  - { from: work, to: done }\n";

    fn profile(model: &str, argv: &[&str], program: &str, fingerprint: &str) -> ManifestProfile {
        serde_json::from_value(serde_json::json!({
            "scope": "project",
            "name": "main",
            "profile_digest": "sha256:p",
            "bundle_digest": "sha256:b",
            "soul": "",
            "soul_requirement": "any",
            "skills": [],
            "chain": [{
                "agent_id": "claude",
                "model": model,
                "spec": { "argv": argv },
                "soul_delivery": "native",
                "canonical_executable": program,
                "executable_fingerprint": fingerprint,
            }],
        }))
        .unwrap()
    }

    fn bound() -> ManifestProfile {
        profile(
            "haiku",
            &["-p", "--model", "{model}", "--", "{prompt}"],
            "/opt/claude",
            "10:1",
        )
    }

    /// A run directory whose node `work` settled an item by its skip, bound
    /// to [`bound`]; `pinned` writes the scripts pin.
    fn run_dir(pinned: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("playbook.yaml"), PLAYBOOK).unwrap();
        let provenance = if pinned {
            r#"{"seq":1,"ts":0,"type":"run_provenance","origin":"project","scripts_digest":"none"}"#
        } else {
            r#"{"seq":1,"ts":0,"type":"run_provenance","origin":"project"}"#
        };
        std::fs::write(
            dir.path().join("events.jsonl"),
            format!(
                "{}\n{provenance}\n{}\n",
                r#"{"seq":0,"ts":0,"type":"run_started","playbook":"lst","version":"1.0.0","definition":"sha256:def"}"#,
                r#"{"seq":2,"ts":0,"type":"list_item_settled","node":"work","item":"a","state":"passed","round":0,"carried":true,"skipped_from":"r0","skip_digest":"d"}"#
            ),
        )
        .unwrap();
        crate::run_config::write_run_config(dir.path(), &crate::run_config::RunConfig::default())
            .unwrap();
        let p = bound();
        let manifest = RunExecutionManifest {
            node_bindings: [("work".to_string(), p.key())].into(),
            profiles: vec![p],
            ..RunExecutionManifest::default()
        };
        crate::manifest::write(dir.path(), &manifest).unwrap();
        dir
    }

    fn refusal(dir: &Path, candidate: &ManifestProfile) -> Option<String> {
        skip_refusal(dir, dir, "work", candidate)
    }

    /// The candidate differs from the bound profile only in its model: the
    /// rebind would change the work of a node that skipped items.
    #[test]
    fn a_rebind_of_a_node_with_skipped_items_is_refused() {
        let dir = run_dir(true);
        assert_eq!(
            refusal(dir.path(), &bound()),
            None,
            "the same work is allowed"
        );
        let why = refusal(
            dir.path(),
            &profile(
                "opus",
                &["-p", "--model", "{model}", "--", "{prompt}"],
                "/opt/claude",
                "10:1",
            ),
        )
        .unwrap();
        assert!(
            why.starts_with("list_skip_work_changed: node work"),
            "{why}"
        );
    }

    /// Only the invocation specification differs.
    #[test]
    fn a_rebind_changing_only_the_executor_is_refused() {
        let dir = run_dir(true);
        let why = refusal(
            dir.path(),
            &profile(
                "haiku",
                &["-p", "--verbose", "--model", "{model}", "--", "{prompt}"],
                "/opt/claude",
                "10:1",
            ),
        )
        .unwrap();
        assert!(why.starts_with("list_skip_work_changed"), "{why}");
    }

    /// Only the canonical executable of the one invocation differs; the
    /// executable's fingerprint alone (the same binary updated in place)
    /// changes no work.
    #[test]
    fn a_rebind_changing_only_the_program_path_is_refused() {
        let dir = run_dir(true);
        let why = refusal(
            dir.path(),
            &profile(
                "haiku",
                &["-p", "--model", "{model}", "--", "{prompt}"],
                "/usr/local/bin/claude",
                "10:1",
            ),
        )
        .unwrap();
        assert!(why.starts_with("list_skip_work_changed"), "{why}");
        assert_eq!(
            refusal(
                dir.path(),
                &profile(
                    "haiku",
                    &["-p", "--model", "{model}", "--", "{prompt}"],
                    "/opt/claude",
                    "99:2"
                )
            ),
            None
        );
    }

    /// The work cannot be computed (the run pinned no scripts digest): the
    /// rebind is refused `list_skip_work_unknown`, never let through.
    #[test]
    fn a_rebind_whose_work_cannot_be_compared_is_refused() {
        let dir = run_dir(false);
        let why = refusal(dir.path(), &bound()).unwrap();
        assert!(
            why.starts_with("list_skip_work_unknown: node work"),
            "{why}"
        );
    }

    /// A node that skipped nothing is rebound as before.
    #[test]
    fn a_node_that_skipped_nothing_is_not_refused() {
        let dir = run_dir(true);
        let other = profile("opus", &["x"], "/opt/other", "1:1");
        assert_eq!(skip_refusal(dir.path(), dir.path(), "done", &other), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal that does not read refuses the rebind: whether the node
    /// settled items by its skip cannot be told, so it is never let through.
    #[test]
    fn a_journal_that_does_not_read_refuses_the_rebind() {
        let run = tempfile::tempdir().unwrap();
        std::fs::write(
            run.path().join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"p\",\"version\":\"1.0.0\"}\nnot json\n{\"seq\":2,\"ts\":0,\"type\":\"run_finished\",\"outcome\":\"succeeded\"}\n",
        )
        .unwrap();
        assert!(read_all(run.path()).is_err(), "the journal is unreadable");
        let candidate: ManifestProfile = serde_json::from_value(serde_json::json!({
            "scope": "project", "name": "main", "profile_digest": "sha256:p", "bundle_digest": "sha256:b",
            "soul": "", "soul_requirement": "any", "skills": [], "chain": [],
        }))
        .unwrap();
        let refusal = skip_refusal(run.path(), run.path(), "l", &candidate).expect("refused");
        assert!(
            refusal.starts_with(crate::list::skip::WORK_UNKNOWN)
                && refusal.contains("does not read"),
            "{refusal}"
        );
    }
}
