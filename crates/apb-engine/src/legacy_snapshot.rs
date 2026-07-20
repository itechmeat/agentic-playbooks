//! Ephemeral manifest for resuming runs that started BEFORE profiles existed
//! (completion-plan spec, Task 2). Such a run carries `runs/<id>/playbook.yaml`
//! with schema-1 `executors` and no `manifest.yaml`. Resume must build a
//! manifest from the snapshot executors instead of failing.
//!
//! The legacy types here are self-contained (independent of the main schema)
//! and parse ONLY the run snapshot - the public `Playbook::from_yaml` is not
//! weakened: it still rejects executors and directs live definitions to
//! migration.

use std::collections::BTreeMap;
use std::path::Path;

use apb_core::config::GlobalConfig;
use apb_core::profile::SoulRequirement;
use apb_core::schema::Playbook;
use serde::Deserialize;

use crate::error::EngineError;
use crate::invocation::{filter_chain, program_for, resolve_invocation};
use crate::manifest::{ManifestProfile, RunExecutionManifest};

/// Loads the run's immutable playbook snapshot from `<run_dir>/playbook.yaml`.
/// Returns `None` when the snapshot is missing or fails to parse.
///
/// Parsing goes through the shared run-snapshot compatibility parser
/// (`parse_snapshot_playbook` below), so schema-1 snapshots that the strict
/// `Playbook::from_yaml` rejects still yield progress (the same tolerance the
/// resume path uses), without weakening the strict parser for live
/// definitions.
///
/// F3: a missing snapshot is a silent `None` (very old runs never captured
/// one). An existing-but-unparseable snapshot also collapses to `None`, but is
/// not silent: it writes one stderr warning naming the run dir. apb-engine has
/// no tracing facility and we add no dependency for this one branch; snapshots
/// are immutable and engine-written, so a parse failure is a filesystem-level
/// fault worth a terminal signal rather than an authoring one.
///
/// Lives here rather than in `progress.rs` (its original home) so that
/// `progress.rs` and `question.rs` - each of which needs this function - stay
/// a one-way dependency edge onto this module instead of a mutual cycle on
/// each other (spec 2026-07-20, Task 5 dependency-cycle fix). `progress.rs`
/// re-exports it (`pub use crate::legacy_snapshot::load_run_playbook`) so the
/// `apb_engine::progress::load_run_playbook` path external callers
/// (apb-mcp, apb-server) already use keeps working unchanged.
pub fn load_run_playbook(run_dir: &Path) -> Option<Playbook> {
    let yaml = std::fs::read_to_string(run_dir.join("playbook.yaml")).ok()?;
    match parse_snapshot_playbook(&yaml) {
        Ok(pb) => Some(pb),
        Err(e) => {
            eprintln!(
                "apb: warning: run snapshot {} unparseable: {e}",
                run_dir.display()
            );
            None
        }
    }
}

/// Legacy executor schema 1: agent + model + an optional fallback chain.
#[derive(Debug, Clone, Deserialize)]
struct LegacyExec {
    agent: String,
    model: String,
    #[serde(default)]
    fallbacks: Vec<LegacyFallback>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyFallback {
    agent: String,
    model: String,
}

#[derive(Debug, Default, Deserialize)]
struct LegacyDefaults {
    #[serde(default)]
    executor: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Deserialize)]
struct LegacySupervisor {
    #[serde(default)]
    executor: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Deserialize)]
struct LegacyNode {
    id: String,
    #[serde(rename = "type", default)]
    node_type: Option<String>,
    #[serde(default)]
    executor: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct LegacyPlaybook {
    #[serde(default)]
    executors: BTreeMap<String, LegacyExec>,
    #[serde(default)]
    defaults: LegacyDefaults,
    #[serde(default)]
    supervisor: Option<LegacySupervisor>,
    #[serde(default)]
    nodes: Vec<LegacyNode>,
}

/// Whether the snapshot has traces of schema-1 executors (the same criterion
/// as `Playbook::from_yaml`, but local to the run snapshot).
pub fn has_legacy_executors(snapshot_yaml: &str) -> bool {
    let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(snapshot_yaml) else {
        return false;
    };
    if v.get("executors")
        .is_some_and(|e| !e.is_null() && e.as_mapping().map(|m| !m.is_empty()).unwrap_or(true))
    {
        return true;
    }
    let has_exec = |m: Option<&serde_yaml_ng::Value>| m.and_then(|x| x.get("executor")).is_some();
    if has_exec(v.get("defaults")) || has_exec(v.get("supervisor")) {
        return true;
    }
    v.get("nodes")
        .and_then(|n| n.as_sequence())
        .is_some_and(|nodes| nodes.iter().any(|n| n.get("executor").is_some()))
}

/// The one read-only compatibility parser for a persisted run snapshot, shared
/// by the resume path and by progress/report consumers. A snapshot carrying
/// schema-1 `executors` is parsed directly (those removed fields are silently
/// ignored by the current `Playbook`), the same tolerance resume relies on; any
/// other snapshot goes through the strict `Playbook::from_yaml`. This does NOT
/// weaken the strict parser: live definitions still route legacy executors to
/// migration.
pub fn parse_snapshot_playbook(snapshot_yaml: &str) -> Result<Playbook, EngineError> {
    if has_legacy_executors(snapshot_yaml) {
        serde_yaml_ng::from_str::<Playbook>(snapshot_yaml)
            .map_err(|e| EngineError::Yaml(format!("legacy snapshot playbook: {e}")))
    } else {
        Playbook::from_yaml(snapshot_yaml).map_err(|e| EngineError::Yaml(e.to_string()))
    }
}

/// Resolves a snapshot executor reference into `LegacyExec`: a string is a
/// name from the `executors` map; an object is an inline definition.
/// Anything unrecognized is an error.
fn resolve_exec(
    val: &serde_yaml_ng::Value,
    executors: &BTreeMap<String, LegacyExec>,
) -> Result<LegacyExec, EngineError> {
    if let Some(name) = val.as_str() {
        executors.get(name).cloned().ok_or_else(|| {
            EngineError::Invalid(format!(
                "legacy snapshot references executor `{name}` not defined in the snapshot"
            ))
        })
    } else if let Ok(ex) = serde_yaml_ng::from_value::<LegacyExec>(val.clone()) {
        Ok(ex)
    } else {
        Err(EngineError::Invalid(
            "legacy snapshot has an unrecognized executor form".into(),
        ))
    }
}

/// Reduces a legacy hint (executor key or `<id>-<node>`) to a safe segment:
/// lowercase `[a-z0-9-]`, no longer than `64 - len("legacy-")`, so that the
/// whole name `legacy-<hint>` fits the profile format `[a-z0-9][a-z0-9-]*`
/// and stays <=64. An empty/unconvertible hint -> a short deterministic
/// hash. The name is only an identity within the manifest (never written to
/// disk), but we keep the format for consistency and in case it is ever used
/// as a key.
fn safe_legacy_hint(hint: &str) -> String {
    const MAX: usize = 64 - "legacy-".len(); // 57
    let mut s: String = hint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s: String = s.trim_matches('-').chars().take(MAX).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() {
        apb_core::content::sha256_hex(hint.as_bytes())
            .trim_start_matches("sha256:")
            .chars()
            .take(6)
            .collect()
    } else {
        s
    }
}

/// Builds a `ManifestProfile` from a legacy executor: empty SOUL, no skills,
/// a primary + fallbacks chain via `resolve_invocation`. Name is
/// `legacy-<hint>`, scope is `legacy` (identity within the manifest only;
/// the profile is never written to disk).
fn manifest_profile_for(
    hint: &str,
    ex: &LegacyExec,
    global: &GlobalConfig,
) -> Result<ManifestProfile, EngineError> {
    let mut chain = Vec::new();
    let program = program_for(&ex.agent, global);
    chain.push(resolve_invocation(&ex.agent, &ex.model, &program, global)?);
    for f in &ex.fallbacks {
        let p = program_for(&f.agent, global);
        chain.push(resolve_invocation(&f.agent, &f.model, &p, global)?);
    }
    // Empty SOUL -> the SOUL-requirement filter cuts nothing.
    let chain = filter_chain(chain, SoulRequirement::Any, true)?;
    Ok(ManifestProfile {
        scope: "legacy".into(),
        name: format!("legacy-{}", safe_legacy_hint(hint)),
        profile_digest: "legacy".into(),
        bundle_digest: "legacy".into(),
        soul: String::new(),
        soul_requirement: SoulRequirement::Any,
        skills: Vec::new(),
        chain,
        ephemeral: false,
    })
}

/// Builds an ephemeral manifest from the run's snapshot executors. Every
/// agent_task that has an `executor` (or inherits `defaults.executor`) is
/// bound to a `legacy-<key>` profile. `supervisor.executor` produces the
/// `supervisor` binding. Profiles are deduplicated by the `<scope>/<name>` key.
pub fn build_ephemeral_manifest(
    _run_dir: &Path,
    snapshot_yaml: &str,
) -> Result<RunExecutionManifest, EngineError> {
    let playbook: LegacyPlaybook = serde_yaml_ng::from_str(snapshot_yaml)
        .map_err(|e| EngineError::Yaml(format!("legacy snapshot: {e}")))?;
    let global = GlobalConfig::load().map_err(EngineError::Invalid)?;

    let mut manifest = RunExecutionManifest::default();

    // Reference -> (hint for the profile name, LegacyExec). For string
    // references, hint is the executor's own name (one binding per named
    // executor); for inline definitions, it is the node id (one profile per
    // node).
    let push = |manifest: &mut RunExecutionManifest,
                binding: &str,
                hint: &str,
                ex: &LegacyExec,
                global: &GlobalConfig|
     -> Result<(), EngineError> {
        let mp = manifest_profile_for(hint, ex, global)?;
        let key = mp.key();
        manifest
            .node_bindings
            .insert(binding.to_string(), key.clone());
        if !manifest.profiles.iter().any(|p| p.key() == key) {
            manifest.profiles.push(mp);
        }
        Ok(())
    };

    for node in &playbook.nodes {
        if node.node_type.as_deref() != Some("agent_task") {
            continue;
        }
        let (hint, exec_val) = match &node.executor {
            Some(v) => {
                // A string reference -> the executor's name as hint (dedup
                // by name); inline -> the node id.
                let hint = v
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| node.id.clone());
                (hint, v.clone())
            }
            None => match &playbook.defaults.executor {
                Some(v) => {
                    let hint = v
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| node.id.clone());
                    (hint, v.clone())
                }
                // A node with no executor - the original run's loader would
                // have rejected it; we do not create a binding (execute_node
                // will report the absence).
                None => continue,
            },
        };
        let ex = resolve_exec(&exec_val, &playbook.executors)?;
        push(&mut manifest, &node.id, &hint, &ex, &global)?;
    }

    if let Some(sup) = &playbook.supervisor {
        let sup_val = sup
            .executor
            .clone()
            .or_else(|| playbook.defaults.executor.clone());
        if let Some(v) = sup_val {
            let hint = v
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "supervisor".to_string());
            let ex = resolve_exec(&v, &playbook.executors)?;
            push(&mut manifest, "supervisor", &hint, &ex, &global)?;
        }
    }

    Ok(manifest)
}
