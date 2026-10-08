//! Forward patches (issue #192): `supervisor_patch_playbook` with
//! `scope: next_runs`. The supervisor improves the playbook for the NEXT
//! runs from what it saw in this one: the patch may change any node,
//! executed or not, because nothing migrates - the run keeps its version.
//! The patch becomes the playbook's candidate (see [`apb_core::candidate`])
//! and must prove itself in trial runs before it becomes `current`.
//!
//! Guards (every refusal is an [`EngineError::Invalid`] naming its code):
//! - `workaround_refused`: only an `improvement` makes sense for later runs;
//! - `window_closed`: the run is live, or ended at most
//!   [`FORWARD_PATCH_WINDOW_MS`] ago;
//! - `max_patches`: the run's `max_patches_per_run` (in-run patches handled
//!   plus forward patches created) is spent;
//! - `goal_changed`, `effects_changed`, `irreversible_added`,
//!   `trust_fields_changed`: the definition guard ([`definition_guard`]);
//! - the validator and the frozen flag, as for every patch version.
//!
//! Nothing is written to the run's journal (the drive loop is its only
//! writer, and the run may have ended): the version's provenance sidecar
//! records the run, the classification, the scope, the rationale and the
//! evidence.

use std::path::Path;

use apb_core::candidate::{CreatedCandidate, ForwardPatch, SCOPE_NEXT_RUNS, create_forward_patch};
use apb_core::registry::{Registry, is_safe_segment};
use apb_core::schema::Playbook;
use apb_core::scope::Origin;
use apb_core::versioning::list_versions_with_provenance;

use crate::error::EngineError;
use crate::event::{EventPayload, read_all};
use crate::state::RunState;

/// How long after its end a run's supervisor may still make a forward patch
/// from it: long enough to read the run report and write the patch, short
/// enough that a stale token does not keep the definition open.
pub const FORWARD_PATCH_WINDOW_MS: u128 = 30 * 60 * 1000;

/// The `max_patches_per_run` default, as for in-run patches.
const DEFAULT_MAX_PATCHES: u32 = 5;

/// The supervisor's forward patch request.
#[derive(Debug, Clone, Default)]
pub struct ForwardPatchRequest {
    pub yaml: String,
    pub classification: String,
    pub rationale: Option<String>,
    pub evidence: Vec<String>,
}

/// What a created forward patch reports back.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardPatchResult {
    /// The new patch version, now the candidate.
    pub version: String,
    /// The version it was made from (the run's active version).
    pub base_version: String,
    /// The candidate it replaced, if any.
    pub replaced: Option<String>,
}

fn refused(code: &str, detail: impl std::fmt::Display) -> EngineError {
    EngineError::Invalid(format!("forward patch refused ({code}): {detail}"))
}

/// Creates a forward patch from run `run_id` (see the module docs).
pub fn create(
    root: &Path,
    run_id: &str,
    req: &ForwardPatchRequest,
) -> Result<ForwardPatchResult, EngineError> {
    if req.classification == "workaround" {
        return Err(refused(
            "workaround_refused",
            "a workaround fits the circumstances of one run; a forward patch must be an improvement",
        ));
    }
    if req.classification != "improvement" {
        return Err(refused(
            "classification",
            format!("`{}` must be `improvement`", req.classification),
        ));
    }
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".apb/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let events = read_all(&run_dir)?;
    let driver_dead = crate::liveness::driver_alive(&run_dir, run_id) == Some(false);
    check_window(&events, driver_dead, apb_core::clock::now_ms())?;

    let (id, base_version) = crate::scheduler::run_playbook_ref(root, run_id)?;
    let base = Registry::open(root)?
        .load(&id, Some(&base_version))?
        .playbook;
    let patched = Playbook::from_yaml(&req.yaml)
        .map_err(|e| refused("schema", format!("the patched YAML does not parse: {e}")))?;
    definition_guard(root, &base, &patched)?;

    // The base check, the patch budget and the write are one step under the
    // candidate lock: two concurrent forward patches from one run cannot
    // both pass a budget of one, and the base cannot move in between.
    let playbook_dir = apb_core::candidate::playbook_dir(root, &id);
    let lock = apb_core::candidate::lock(&playbook_dir)?;
    check_base(&playbook_dir, &base_version)?;
    check_patch_budget(root, &run_dir, &events, &id, run_id)?;
    let CreatedCandidate { version, replaced } = create_forward_patch(
        root,
        &id,
        &base_version,
        &req.yaml,
        &ForwardPatch {
            run_id: run_id.to_string(),
            classification: req.classification.clone(),
            rationale: req.rationale.clone(),
            evidence: req.evidence.clone(),
        },
        &lock,
    )
    .map_err(|e| refused("invalid", e))?;
    Ok(ForwardPatchResult {
        version,
        base_version,
        replaced,
    })
}

/// A forward patch builds on the line a person approved: its base (the
/// run's version) must be `current`, or the candidate now on trial (the
/// new patch then replaces it, and its lineage still reaches `current`). A
/// run of an older version may not plant a candidate that would undo a
/// newer one: `stale_base`.
fn check_base(playbook_dir: &Path, base_version: &str) -> Result<(), EngineError> {
    let current = std::fs::read_to_string(playbook_dir.join("current"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if base_version == current {
        return Ok(());
    }
    let candidate = apb_core::candidate::read_candidate(playbook_dir);
    if candidate.as_deref() == Some(base_version)
        && apb_core::candidate::lineage_reaches(playbook_dir, base_version, &current)
    {
        return Ok(());
    }
    Err(refused(
        "stale_base",
        format!(
            "the run ran {base_version}, but `current` is {current}: a forward patch builds on `current` or the candidate on trial"
        ),
    ))
}

/// A live run is open; an ended one only within [`FORWARD_PATCH_WINDOW_MS`]
/// of its terminal event. A run whose driver died never journals its end:
/// it counts as ended at its last journal line (`driver_dead`).
fn check_window(
    events: &[crate::event::Event],
    driver_dead: bool,
    now_ms: u128,
) -> Result<(), EngineError> {
    let ended = if RunState::fold(events).run_status.is_terminal() {
        events
            .iter()
            .rev()
            .find(|e| {
                matches!(
                    e.payload,
                    EventPayload::RunFinished { .. } | EventPayload::RunAborted { .. }
                )
            })
            .map(|e| e.ts)
            .unwrap_or(0)
    } else if driver_dead {
        events.last().map(|e| e.ts).unwrap_or(0)
    } else {
        return Ok(());
    };
    if now_ms.saturating_sub(ended) > FORWARD_PATCH_WINDOW_MS {
        return Err(refused(
            "window_closed",
            format!(
                "the run ended more than {} minutes ago",
                FORWARD_PATCH_WINDOW_MS / 60_000
            ),
        ));
    }
    Ok(())
}

/// `max_patches_per_run` covers both scopes: the in-run patches the drive
/// handled (applied or rejected) plus the forward patches already created
/// from this run.
fn check_patch_budget(
    root: &Path,
    run_dir: &Path,
    events: &[crate::event::Event],
    id: &str,
    run_id: &str,
) -> Result<(), EngineError> {
    let limit = crate::run_config::read_run_config(run_dir)
        .ok()
        .and_then(|c| c.max_patches_per_run)
        .unwrap_or(DEFAULT_MAX_PATCHES);
    let in_run = events
        .iter()
        .filter(|e| {
            matches!(
                e.payload,
                EventPayload::PatchApplied { .. } | EventPayload::PatchRejected { .. }
            )
        })
        .count();
    let forward = list_versions_with_provenance(root, id)
        .map(|infos| {
            infos
                .iter()
                .filter_map(|v| v.provenance.as_ref())
                .filter(|p| {
                    p.run_id.as_deref() == Some(run_id)
                        && p.scope.as_deref() == Some(SCOPE_NEXT_RUNS)
                })
                .count()
        })
        .unwrap_or(0);
    let used = in_run + forward;
    if used >= usize::try_from(limit).unwrap_or(usize::MAX) {
        return Err(refused(
            "max_patches",
            format!("{used} patches from this run already (limit {limit})"),
        ));
    }
    Ok(())
}

fn json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// What a forward patch may not touch: the goal (the run's contract, a
/// person's to change), the declared and effective effects (`secrets`,
/// `irreversible` and the rest; a patch may drop effects, never add them),
/// the irreversible sources a run start asks consent for, and the fields
/// trust and policy rest on: `requires`, the `supervisor` block (its
/// capabilities and promotion and trial policies), the decision opt-ins in
/// `defaults` (`host_decisions`, `retry_advice`), the run's `worktree`,
/// each node's connector grants and each sub-playbook a node runs.
///
/// A node may still bind another profile: profile trust is not inherited
/// from the base version, the run gate checks every profile bundle of the
/// candidate on its own.
pub fn definition_guard(
    root: &Path,
    base: &Playbook,
    patched: &Playbook,
) -> Result<(), EngineError> {
    if base.goal != patched.goal {
        return Err(refused(
            "goal_changed",
            "the goal and its criteria are the run's contract: only a person may change them",
        ));
    }
    if base.effects != patched.effects {
        return Err(refused(
            "effects_changed",
            "the declared `effects` are not a supervisor's to change",
        ));
    }
    let base_effects = apb_core::effects::effective(base);
    let added: Vec<String> = apb_core::effects::effective(patched)
        .difference(&base_effects)
        .map(|e| json(e).as_str().unwrap_or_default().to_string())
        .collect();
    if !added.is_empty() {
        return Err(refused(
            "effects_changed",
            format!("the patch adds effects: {}", added.join(", ")),
        ));
    }
    for node in &patched.nodes {
        let before = base.node(&node.id);
        if before.map_or(!node.effects.is_empty(), |b| b.effects != node.effects) {
            return Err(refused(
                "effects_changed",
                format!("node `{}` changes its declared effects", node.id),
            ));
        }
    }
    let origin = Origin::Project { workspace_id: None };
    let base_sources = crate::gate::consent_sources(root, base, &origin, None);
    let new_sources: Vec<String> = crate::gate::consent_sources(root, patched, &origin, None)
        .into_iter()
        .filter(|s| !base_sources.contains(s))
        .collect();
    if !new_sources.is_empty() {
        return Err(refused(
            "irreversible_added",
            format!(
                "the patch adds irreversible steps: {}",
                new_sources.join(", ")
            ),
        ));
    }
    if json(&base.requires) != json(&patched.requires) {
        return Err(refused("trust_fields_changed", "`requires` changed"));
    }
    if json(&base.defaults.host_decisions) != json(&patched.defaults.host_decisions)
        || json(&base.defaults.retry_advice) != json(&patched.defaults.retry_advice)
    {
        return Err(refused(
            "trust_fields_changed",
            "the decision opt-ins in `defaults` (`host_decisions`, `retry_advice`) changed",
        ));
    }
    if base.worktree != patched.worktree {
        return Err(refused("trust_fields_changed", "`worktree` changed"));
    }
    if json(&base.supervisor) != json(&patched.supervisor) {
        return Err(refused(
            "trust_fields_changed",
            "the `supervisor` block (capabilities, promotion and trial policies) changed",
        ));
    }
    for node in &patched.nodes {
        let before = base.node(&node.id);
        let grants = |n: &apb_core::schema::Node| json(&n.kind.connector_bindings());
        let child = |n: &apb_core::schema::Node| match &n.kind {
            apb_core::schema::NodeKind::Playbook { playbook, .. } => json(playbook),
            _ => serde_json::Value::Null,
        };
        let (grants_before, child_before) = before
            .map_or((json(&Vec::<()>::new()), serde_json::Value::Null), |b| {
                (grants(b), child(b))
            });
        if grants(node) != grants_before {
            return Err(refused(
                "trust_fields_changed",
                format!("node `{}` changes its connector grants", node.id),
            ));
        }
        if child(node) != child_before {
            return Err(refused(
                "trust_fields_changed",
                format!("node `{}` changes the sub-playbook it runs", node.id),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    fn ev(seq: u64, ts: u128, payload: EventPayload) -> Event {
        Event { seq, ts, payload }
    }

    #[test]
    fn the_window_is_open_while_live_and_for_a_while_after_the_end() {
        let started = ev(
            0,
            0,
            EventPayload::RunStarted {
                playbook: "p".into(),
                version: "1.0.0".into(),
            },
        );
        let live = vec![started.clone()];
        assert!(check_window(&live, false, u128::MAX).is_ok());
        // A run whose driver died counts as ended at its last journal line.
        assert!(check_window(&live, true, FORWARD_PATCH_WINDOW_MS).is_ok());
        let err = check_window(&live, true, 1 + FORWARD_PATCH_WINDOW_MS).unwrap_err();
        assert!(err.to_string().contains("window_closed"), "{err}");
        let ended = vec![
            started,
            ev(
                1,
                1_000,
                EventPayload::RunFinished {
                    outcome: "succeeded".into(),
                },
            ),
        ];
        assert!(check_window(&ended, false, 1_000 + FORWARD_PATCH_WINDOW_MS).is_ok());
        let err = check_window(&ended, false, 1_001 + FORWARD_PATCH_WINDOW_MS).unwrap_err();
        assert!(err.to_string().contains("window_closed"), "{err}");
    }
}
